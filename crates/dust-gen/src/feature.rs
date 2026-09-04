//! The feature stage: what a chunk gets *after* its caves are cut.
//!
//! Vanilla's `ChunkStatus.FEATURES` runs `ChunkGenerator.applyBiomeDecoration`,
//! which walks eleven decoration steps and, in each, places every placed
//! feature the biomes in view name. `worldgen/biome/*.json` says which features
//! a biome runs and in what order, `worldgen/placed_feature/*.json` says where
//! one lands, and `worldgen/configured_feature/*.json` says what it builds.
//! All three are the operator's own data; the parts that are code rather than
//! data were read out of the operator's own server jar. Nothing Mojang's is
//! committed.
//!
//! # What this stage runs, and what it counts instead
//!
//! One configured-feature type: `minecraft:ore`. That is thirty of the pack's
//! one hundred and ninety-six, and it is the whole of the underground-ores
//! step — the coal and iron a player needs before anything else, and the tuff,
//! andesite, diorite and granite that were the four largest single entries in
//! "Minecraft has where Dust is wrong" the day this was written. Every other
//! type is read, indexed, ordered and then **skipped by name with a count**
//! ([`Features::skipped`]), so the next stage starts from a list rather than a
//! survey.
//!
//! Skipping is free of consequence for the features that do run, and that is
//! not luck: `setFeatureSeed` re-seeds the stream *per feature* from the
//! chunk's decoration seed and the feature's own global index. A feature that
//! is not run consumes nothing that another feature would have drawn. What a
//! skipped feature does cost is its blocks, which is what the count is for.
//!
//! # Three things a guess would have got wrong
//!
//! **The stream is neither of the two generators this crate already had.**
//! `applyBiomeDecoration` builds `WorldgenRandom` over an
//! `XoroshiroRandomSource`, and `WorldgenRandom` overrides only `next(bits)`
//! and `setSeed` — so every draw is `java.util.Random`'s own arithmetic
//! (`nextInt`'s rejection loop, a 24-bit `nextFloat`, a **two-draw**
//! `nextDouble`) reading xoroshiro's top bits. See [`crate::noise::rng::Worldgen`].
//!
//! **`OreFeature` uses both a real sine and a table one, in one feature.** The
//! angle the vein is drawn along comes from `java.lang.Math.sin` and `cos` on a
//! double; the radius of each of its nodes comes from `Mth.sin`, the
//! 65,536-entry lookup table. Using either one for both puts every vein
//! somewhere else, and every test that only asks whether ore exists still
//! passes.
//!
//! **A feature writes into the eight chunks around its own.** The FEATURES
//! step declares a block-state write radius of one, so a chunk holds its own
//! features *and* whatever the eight around it spilled in — a size-64 ore vein
//! reaches thirteen blocks. This runs all nine origins and keeps the writes
//! that land in the middle one, because the alternative is veins sliced flat at
//! every chunk boundary, which no test would fail and every player would see.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::noise::build::{read_json, BlockSpec, BuildError, NoiseSettings};
use crate::noise::rng::{mth_ceil, mth_floor, mth_lerp, mth_sin, Worldgen};

/// `GenerationStep.Decoration.values().length`.
pub const STEPS: usize = 11;

/// Material codes, which are a `u8`, so this is the whole space a mask covers.
const CODES: usize = 256;

/// How wide the window of columns the feature stage reads is, in chunks, either
/// side of the one being built.
///
/// **Two, not one.** Features run for the nine chunks that may write into this
/// one, and `OreFeature` asks `OCEAN_FLOOR_WG` over a box that reaches thirteen
/// blocks past its own origin — so a vein whose origin is in the far corner of
/// the ring asks about columns twenty-six blocks beyond that, which is the
/// chunk after next. A one-chunk window would answer "no ground here" for those
/// columns and quietly refuse veins vanilla drew.
pub const WINDOW_RADIUS: i32 = 2;

/// Columns across the window: five chunks.
pub const WINDOW: usize = (2 * WINDOW_RADIUS as usize + 1) * 16;

/// Chunk rows of column heights kept between calls. Five is what a stage that
/// reads its own row and the two either side of it needs.
const CACHE_ROWS: i32 = 2 * WINDOW_RADIUS + 1;

/// Chunks per cached row. A caller that builds columns in any scan order up to
/// this wide asks the terrain for each chunk once; a wider or a random one asks
/// for some of them twice. Sixty-four rows of sixteen-bit heights is 98 KiB.
const CACHE_COLUMNS: i32 = 64;

fn malformed(path: &Path, detail: &str) -> BuildError {
    BuildError::Malformed {
        path: path.to_path_buf(),
        detail: detail.to_owned(),
    }
}

fn split_id(id: &str) -> (&str, &str) {
    match id.split_once(':') {
        Some((namespace, name)) => (namespace, name),
        None => ("minecraft", id),
    }
}

/// A `{"Name": ..., "Properties": {...}}` block, as a settings file writes one.
fn block_spec(value: &Value, path: &Path) -> Result<BlockSpec, BuildError> {
    let name = value
        .get("Name")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(path, "a block state carries `Name`"))?
        .to_owned();
    let mut properties = Vec::new();
    if let Some(object) = value.get("Properties").and_then(Value::as_object) {
        for (key, entry) in object {
            let text = entry
                .as_str()
                .ok_or_else(|| malformed(path, "a block property is a string"))?;
            properties.push((key.clone(), text.to_owned()));
        }
    }
    properties.sort();
    Ok(BlockSpec { name, properties })
}

/// A set over material codes, which is what "may this target replace what is
/// already here" is once the palette is known.
///
/// Four words rather than a `HashSet` of names: the question is asked once per
/// candidate cell of every vein, and the answer cannot change after boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CodeSet([u64; CODES / 64]);

impl CodeSet {
    fn insert(&mut self, code: u8) {
        self.0[usize::from(code) >> 6] |= 1u64 << (code & 63);
    }

    fn contains(self, code: u8) -> bool {
        self.0[usize::from(code) >> 6] >> (code & 63) & 1 == 1
    }

    fn is_empty(self) -> bool {
        self.0 == [0; CODES / 64]
    }
}

/// One `OreConfiguration.TargetBlockState`: what it may replace, and what it
/// leaves behind.
#[derive(Debug, Clone)]
struct Target {
    /// The material codes vanilla's `RuleTest` answers true for. Resolved at
    /// boot from the tag or the block the pack names, over the palette this
    /// generator can actually write.
    replaces: CodeSet,
    /// The material code written, which is `4 + index` into the combined
    /// palette.
    code: u8,
    /// The blocks the pack's `RuleTest` names, kept until the whole palette is
    /// known and `replaces` can be built over it.
    names: Vec<String>,
}

/// `minecraft:ore`, as `OreFeature` runs it.
#[derive(Debug, Clone)]
struct Ore {
    size: i32,
    discard_on_air: f32,
    targets: Vec<Target>,
}

/// A height provider, with both its anchors already resolved.
///
/// Vanilla resolves them per call against a `WorldGenerationContext` that is
/// constant for a dimension, so this resolves them once. The *draws* are not
/// folded away with them: `UniformHeight` on a one-block range still calls
/// `nextInt(1)`, and that draw moves the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Height {
    Uniform { min: i32, max: i32 },
    Trapezoid { min: i32, max: i32, plateau: i32 },
}

impl Height {
    fn sample(self, rng: &mut Worldgen) -> i32 {
        match self {
            Self::Uniform { min, max } => {
                if min > max {
                    // Vanilla warns and answers `min` without drawing.
                    return min;
                }
                rng.between_inclusive(min, max)
            }
            Self::Trapezoid { min, max, plateau } => {
                if min > max {
                    return min;
                }
                let range = max - min;
                if plateau >= range {
                    return rng.between_inclusive(min, max);
                }
                // Java's integer division truncates, so the two halves are
                // unequal for an odd range and the distribution is skewed.
                // Two draws, low half second.
                let low = (range - plateau) / 2;
                let high = range - low;
                min + rng.between_inclusive(0, high) + rng.between_inclusive(0, low)
            }
        }
    }
}

/// One entry of a placed feature's `placement` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    /// `minecraft:count` with a constant.
    Count(i32),
    /// `minecraft:count` with a `minecraft:uniform` provider.
    CountUniform { min: i32, max: i32 },
    /// `minecraft:rarity_filter`.
    Rarity(i32),
    /// `minecraft:in_square`.
    InSquare,
    /// `minecraft:height_range`.
    HeightRange(Height),
    /// `minecraft:biome`.
    Biome,
}

/// One `worldgen/placed_feature` entry, in the order the sorter numbered it.
#[derive(Debug, Clone)]
struct Placed {
    name: String,
    /// The configured feature's type, which is what a skipped feature is
    /// counted under.
    kind: String,
    /// `None` when this generator does not run it.
    chain: Option<Vec<Modifier>>,
    ore: Option<Ore>,
}

/// What one chunk's feature stage did, counted rather than assumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Placed features whose seed was set — nine chunks' worth per chunk built.
    pub seeded: u64,
    /// Positions a placement chain offered to a feature.
    pub offered: u64,
    /// Positions the biome filter refused.
    pub off_biome: u64,
    /// Veins that got past the `OCEAN_FLOOR_WG` test and were drawn.
    pub veins: u64,
    /// Cells a vein reached the block test at.
    pub reached: u64,
    /// Cells that changed.
    pub written: u64,
    /// Cells refused because the vein's own earlier writes had taken them.
    pub taken: u64,
    /// Times the air-exposure check asked about a cell outside the chunk being
    /// built, which this generator answers "not air" without looking. See the
    /// decision record.
    pub air_outside: u64,
}

/// A dimension's features, compiled for one seed.
///
/// Shared and immutable. The per-thread half is [`Placer`].
#[derive(Debug, Clone)]
pub struct Features {
    /// Every distinct placed feature the pack's biomes name, in first-appearance
    /// order — which is the order `FeatureSorter` numbers them and therefore
    /// the order the topological sort breaks ties in.
    placed: Vec<Placed>,
    /// Per decoration step, indices into `placed` in the order
    /// `FeatureSorter.buildFeaturesPerStep` sorted them. **The position in this
    /// list is the number `setFeatureSeed` takes**, so a step whose order is
    /// wrong puts every one of its features on a different stream.
    steps: Vec<Vec<u32>>,
    /// Per biome, the placed features it names, as a bitset over `placed`.
    /// Indexed by the dense slot `by_id` maps a registry id to.
    biome_sets: Vec<Box<[u64]>>,
    /// Biome name per dense slot, kept so `bind_biomes` can say what it could
    /// not bind.
    biome_names: Vec<String>,
    /// Registry id to dense biome slot, filled by [`Features::bind_biomes`].
    /// `u16::MAX` is "no biome of this pack".
    by_id: Vec<u16>,
    /// Blocks the features write, appended to the surface rules' own palette.
    palette: Vec<BlockSpec>,
    /// Material codes that count towards `OCEAN_FLOOR_WG`, filled by
    /// [`Features::bind_ocean_floor`].
    ocean_floor: CodeSet,
    /// Whether that binding has happened and answered for every block.
    ocean_floor_bound: bool,
    /// The dimension's own default block, which is the code an ore replaces
    /// most of the time and never reaches the palette.
    default_block: BlockSpec,
    /// Configured-feature types this generator does not run, and how many
    /// placed features name one.
    skipped: BTreeMap<String, usize>,
    seed: i64,
    min_y: i32,
    height: i32,
}

impl Features {
    /// Compile the features every biome in `biomes` names.
    ///
    /// `biomes` must be in the biome source's own order — the parameter list's
    /// first-appearance order, not a sorted one. `FeatureSorter` numbers
    /// features by first appearance scanning biomes in that order, and the
    /// number is the sort's tie-break, so a sorted list would order some step
    /// differently and re-seed every feature in it.
    ///
    /// `None` when no biome names a feature this generator runs.
    pub fn over(
        data_root: &Path,
        settings: &NoiseSettings,
        seed: i64,
        biomes: &[String],
        palette: &[BlockSpec],
    ) -> Result<Option<Self>, BuildError> {
        let mut index_of: BTreeMap<String, u32> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        // Vertex is `(step, feature index)`, which is vanilla's own comparator,
        // and the same feature at two steps is two vertices.
        let mut edges: BTreeMap<(u16, u32), BTreeSet<(u16, u32)>> = BTreeMap::new();
        let mut biome_chains: Vec<Vec<u32>> = Vec::new();
        let mut max_steps = 0usize;

        for biome in biomes {
            let path = biome_path(data_root, biome);
            let per_step = features_of_biome(&path)?;
            max_steps = max_steps.max(per_step.len());
            let mut chain: Vec<(u16, u32)> = Vec::new();
            let mut named: Vec<u32> = Vec::new();
            for (step, list) in per_step.iter().enumerate() {
                for name in list {
                    let next = u32::try_from(order.len()).expect("a pack has fewer than 4G features");
                    let index = *index_of.entry(name.clone()).or_insert_with(|| {
                        order.push(name.clone());
                        next
                    });
                    chain.push((u16::try_from(step).expect("eleven steps"), index));
                    named.push(index);
                }
            }
            for (position, vertex) in chain.iter().enumerate() {
                let successors = edges.entry(*vertex).or_default();
                if position + 1 < chain.len() {
                    successors.insert(chain[position + 1]);
                }
            }
            named.sort_unstable();
            named.dedup();
            biome_chains.push(named);
        }
        if order.is_empty() {
            return Ok(None);
        }

        let sorted = topological(&edges)?;
        let mut steps: Vec<Vec<u32>> = vec![Vec::new(); max_steps];
        for (step, index) in sorted {
            steps[usize::from(step)].push(index);
        }

        // Everything the pack names, read once each. A feature this generator
        // does not run is still read, still numbered and still ordered: its
        // position is what the ones around it are seeded from.
        let mut palette_extra: Vec<BlockSpec> = Vec::new();
        let mut tags: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
        let mut placed = Vec::with_capacity(order.len());
        for name in &order {
            let entry = read_placed(
                data_root,
                name,
                settings.min_y,
                settings.height,
                palette,
                &mut palette_extra,
                &mut tags,
                &mut skipped,
            )?;
            placed.push(entry);
        }
        if placed.iter().all(|entry| entry.chain.is_none()) {
            return Ok(None);
        }

        // The whole palette is known now: the surface rules' blocks, then the
        // ones the features themselves write. An ore that replaces
        // `deepslate_ore_replaceables` replaces the tuff an earlier ore wrote,
        // which is why this cannot be done while the palette is still growing.
        let mut by_name: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
        for (index, spec) in palette.iter().chain(palette_extra.iter()).enumerate() {
            if let Ok(code) = u8::try_from(index + 4) {
                by_name.entry(spec.name.as_str()).or_default().push(code);
            }
        }
        let lava = crate::aquifer::Aquifer::lava_block();
        let default_codes: [(&str, u8); 3] = [
            (settings.default_block.name.as_str(), 1),
            (settings.default_fluid.name.as_str(), 2),
            (lava.name.as_str(), 3),
        ];
        for entry in &mut placed {
            let Some(ore) = entry.ore.as_mut() else {
                continue;
            };
            for target in &mut ore.targets {
                for name in &target.names {
                    if let Some(codes) = by_name.get(name.as_str()) {
                        for &code in codes {
                            target.replaces.insert(code);
                        }
                    }
                    for &(default, code) in &default_codes {
                        if default == name {
                            target.replaces.insert(code);
                        }
                    }
                }
            }
        }

        let words = placed.len().div_ceil(64);
        let mut biome_sets = Vec::with_capacity(biome_chains.len());
        for named in &biome_chains {
            let mut set = vec![0u64; words].into_boxed_slice();
            for &index in named {
                set[index as usize >> 6] |= 1u64 << (index & 63);
            }
            biome_sets.push(set);
        }

        Ok(Some(Self {
            placed,
            steps,
            biome_sets,
            biome_names: biomes.to_vec(),
            by_id: Vec::new(),
            palette: palette_extra,
            ocean_floor: CodeSet::default(),
            ocean_floor_bound: false,
            default_block: settings.default_block.clone(),
            skipped,
            seed,
            min_y: settings.min_y,
            height: settings.height,
        }))
    }

    /// The blocks the features write, which extend the surface rules' palette:
    /// a material code of `4 + surface.len() + i` is `palette()[i]`.
    pub fn palette(&self) -> &[BlockSpec] {
        &self.palette
    }

    /// Configured-feature types this generator does not run, and how many
    /// placed features name one.
    pub fn skipped(&self) -> &BTreeMap<String, usize> {
        &self.skipped
    }

    /// How many placed features are run, and how many were read in total.
    pub fn coverage(&self) -> (usize, usize) {
        (
            self.placed.iter().filter(|e| e.chain.is_some()).count(),
            self.placed.len(),
        )
    }

    /// Point the biome filter at a registry's own ids.
    ///
    /// Separate from [`Features::over`] for the same reason
    /// `Rules::bind_biomes` is: a generator is compiled from a data pack and
    /// bound to a *running* registry, and the two are not the same thing.
    /// Returns the biome names this build's registry does not have.
    pub fn bind_biomes(&mut self, id_of: impl Fn(&str) -> Option<u32>) -> Vec<String> {
        let mut unbound = Vec::new();
        self.by_id.clear();
        for (slot, name) in self.biome_names.iter().enumerate() {
            match id_of(name) {
                Some(id) => {
                    let id = id as usize;
                    if self.by_id.len() <= id {
                        self.by_id.resize(id + 1, u16::MAX);
                    }
                    self.by_id[id] = u16::try_from(slot).unwrap_or(u16::MAX);
                }
                None => unbound.push(name.clone()),
            }
        }
        unbound
    }

    /// Say which of the combined palette's blocks count towards
    /// `OCEAN_FLOOR_WG`, which is the heightmap `OreFeature` asks before it
    /// draws a vein at all.
    ///
    /// Asked of the caller rather than guessed: the answer is a per-block-state
    /// column of the operator's own `dust-constants.tsv`, and a generator that
    /// decided for itself that "not air and not fluid" means "blocks motion"
    /// would be right for every block in a vanilla overworld's palette but one,
    /// and would say nothing when a pack added another. `surface` is the
    /// palette the surface rules own, which comes first in the combined one.
    ///
    /// Returns the blocks the caller could not answer for. **Until this is
    /// called and answers for every block, no feature runs at all** — an
    /// unbound generator carves and stops, and [`Features::ocean_floor_bound`]
    /// says so.
    pub fn bind_ocean_floor(
        &mut self,
        surface: &[BlockSpec],
        blocks: impl Fn(&BlockSpec) -> Option<bool>,
    ) -> Vec<String> {
        let mut unknown = Vec::new();
        let mut set = CodeSet::default();
        // The dimension's own default block gets code 1 and never reaches the
        // palette, so it is asked for by name like the rest rather than assumed
        // to be stone. Air, the default fluid and lava are codes 0, 2 and 3, and
        // none of the three blocks motion.
        match blocks(&self.default_block) {
            Some(true) => set.insert(1),
            Some(false) => {}
            None => unknown.push(self.default_block.name.clone()),
        }
        for (index, spec) in surface.iter().chain(self.palette.iter()).enumerate() {
            let Ok(code) = u8::try_from(index + 4) else {
                unknown.push(spec.name.clone());
                continue;
            };
            match blocks(spec) {
                Some(true) => set.insert(code),
                Some(false) => {}
                None => unknown.push(spec.name.clone()),
            }
        }
        self.ocean_floor_bound = unknown.is_empty();
        self.ocean_floor = if unknown.is_empty() {
            set
        } else {
            CodeSet::default()
        };
        unknown
    }

    /// Whether [`Features::bind_ocean_floor`] has answered for every block.
    pub fn ocean_floor_bound(&self) -> bool {
        self.ocean_floor_bound
    }

    /// One thread's scratch.
    pub fn placer(&self) -> Placer<'_> {
        let longest = self
            .placed
            .iter()
            .filter_map(|entry| entry.ore.as_ref())
            .map(|ore| ore.size.max(1) as usize)
            .max()
            .unwrap_or(1);
        Placer {
            features: self,
            rng: Worldgen::new(),
            nodes: vec![0.0; longest * 4],
            mask: Vec::new(),
            counts: Counts::default(),
        }
    }
}

/// Vanilla's own reverse-post-order depth-first topological sort, with its own
/// tie-break: roots and successors both ascending by `(step, feature index)`.
///
/// Written out rather than replaced by a sort on the pair, because a
/// topological order is not a total order and any two of them disagree about
/// some pair — and the index this produces is what every feature's seed is.
fn topological(
    edges: &BTreeMap<(u16, u32), BTreeSet<(u16, u32)>>,
) -> Result<Vec<(u16, u32)>, BuildError> {
    let successors: BTreeMap<(u16, u32), Vec<(u16, u32)>> = edges
        .iter()
        .map(|(vertex, set)| (*vertex, set.iter().copied().collect()))
        .collect();
    let mut visited: BTreeSet<(u16, u32)> = BTreeSet::new();
    let mut in_progress: BTreeSet<(u16, u32)> = BTreeSet::new();
    let mut ordered: Vec<(u16, u32)> = Vec::with_capacity(edges.len());
    for root in edges.keys() {
        if visited.contains(root) {
            continue;
        }
        in_progress.insert(*root);
        let mut stack: Vec<((u16, u32), usize)> = vec![(*root, 0)];
        while let Some(&mut (vertex, ref mut next)) = stack.last_mut() {
            let step = successors.get(&vertex).and_then(|list| list.get(*next));
            match step {
                Some(&successor) => {
                    *next += 1;
                    if visited.contains(&successor) {
                        continue;
                    }
                    if in_progress.contains(&successor) {
                        return Err(BuildError::Cycle {
                            name: format!("feature order at step {}", successor.0),
                        });
                    }
                    in_progress.insert(successor);
                    stack.push((successor, 0));
                }
                None => {
                    stack.pop();
                    in_progress.remove(&vertex);
                    visited.insert(vertex);
                    ordered.push(vertex);
                }
            }
        }
    }
    ordered.reverse();
    Ok(ordered)
}

fn biome_path(data_root: &Path, biome: &str) -> PathBuf {
    let (namespace, name) = split_id(biome);
    data_root.join(format!("{namespace}/worldgen/biome/{name}.json"))
}

/// Which placed features a biome names, per decoration step, in its own order.
fn features_of_biome(path: &Path) -> Result<Vec<Vec<String>>, BuildError> {
    let json = read_json(path)?;
    let Some(features) = json.get("features") else {
        return Ok(Vec::new());
    };
    let steps = features
        .as_array()
        .ok_or_else(|| malformed(path, "`features` is a list of steps"))?;
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        let list = step
            .as_array()
            .ok_or_else(|| malformed(path, "a decoration step is a list"))?;
        let mut named = Vec::with_capacity(list.len());
        for entry in list {
            let name = entry
                .as_str()
                .ok_or_else(|| malformed(path, "a placed feature is named by a string"))?;
            if let Some(tag) = name.strip_prefix('#') {
                // A `HolderSet` written as a tag has its own order, and a
                // guess at it would renumber the features around it.
                return Err(malformed(
                    path,
                    &format!("`#{tag}` names a feature tag, which this generator does not expand"),
                ));
            }
            named.push(name.to_owned());
        }
        out.push(named);
    }
    Ok(out)
}

/// Read one `worldgen/placed_feature` entry and the configured feature under it.
#[allow(clippy::too_many_arguments)]
fn read_placed(
    data_root: &Path,
    name: &str,
    min_y: i32,
    height: i32,
    surface: &[BlockSpec],
    extra: &mut Vec<BlockSpec>,
    tags: &mut BTreeMap<String, Vec<String>>,
    skipped: &mut BTreeMap<String, usize>,
) -> Result<Placed, BuildError> {
    let (namespace, id) = split_id(name);
    let path = data_root.join(format!("{namespace}/worldgen/placed_feature/{id}.json"));
    let json = read_json(&path)?;
    let feature = json
        .get("feature")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(&path, "a placed feature names a `feature`"))?
        .to_owned();
    let (kind, ore) = read_configured(data_root, &feature, surface, extra, tags)?;

    let mut chain = read_chain(&json, min_y, height, &path)?;
    // A chain this generator cannot run in full is not run at all, and the
    // whole feature is counted as skipped: half a placement is a vein in the
    // wrong place, which is worse than no vein.
    if ore.is_none() {
        chain = None;
    }
    // Every feature that runs must end by asking the biome. This stage places
    // the union of every biome's features in every chunk and lets that filter
    // decide, which is exact only because the filter is there -- vanilla
    // places the union of the biomes in the 3x3 around the chunk, and a
    // feature without the filter would then run where vanilla never offered it.
    if let Some(list) = &chain {
        if !list.iter().any(|m| matches!(m, Modifier::Biome)) {
            chain = None;
        }
    }
    if chain.is_none() {
        *skipped.entry(kind.clone()).or_insert(0) += 1;
    }
    Ok(Placed {
        name: name.to_owned(),
        kind,
        chain,
        ore,
    })
}

fn read_chain(
    json: &Value,
    min_y: i32,
    height: i32,
    path: &Path,
) -> Result<Option<Vec<Modifier>>, BuildError> {
    let placement = json
        .get("placement")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(path, "a placed feature carries a `placement` list"))?;
    let mut chain = Vec::with_capacity(placement.len());
    for entry in placement {
        let kind = entry
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed(path, "a placement modifier carries a `type`"))?;
        let modifier = match kind {
            "minecraft:count" => match entry.get("count") {
                Some(Value::Number(number)) => Modifier::Count(
                    number
                        .as_i64()
                        .and_then(|value| i32::try_from(value).ok())
                        .ok_or_else(|| malformed(path, "`count` is a whole number"))?,
                ),
                Some(object) if object.get("type").and_then(Value::as_str)
                    == Some("minecraft:uniform") =>
                {
                    Modifier::CountUniform {
                        min: whole(object, "min_inclusive", path)?,
                        max: whole(object, "max_inclusive", path)?,
                    }
                }
                _ => return Ok(None),
            },
            "minecraft:rarity_filter" => Modifier::Rarity(whole(entry, "chance", path)?),
            "minecraft:in_square" => Modifier::InSquare,
            "minecraft:biome" => Modifier::Biome,
            "minecraft:height_range" => {
                let Some(band) = read_height(entry.get("height"), min_y, height, path)? else {
                    return Ok(None);
                };
                Modifier::HeightRange(band)
            }
            _ => return Ok(None),
        };
        chain.push(modifier);
    }
    Ok(Some(chain))
}

fn whole(value: &Value, key: &str, path: &Path) -> Result<i32, BuildError> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
        .ok_or_else(|| malformed(path, &format!("`{key}` is a whole number")))
}

/// A vertical anchor, resolved against the dimension the way
/// `WorldGenerationContext` resolves it. `min_y` and `height` are constants for
/// a dimension, so this is done once rather than per placement.
fn anchor(value: Option<&Value>, min_y: i32, height: i32, path: &Path) -> Result<i32, BuildError> {
    let object = value.ok_or_else(|| malformed(path, "a height provider carries anchors"))?;
    if let Some(y) = object.get("absolute").and_then(Value::as_i64) {
        return Ok(y as i32);
    }
    if let Some(offset) = object.get("above_bottom").and_then(Value::as_i64) {
        return Ok(min_y + offset as i32);
    }
    if let Some(offset) = object.get("below_top").and_then(Value::as_i64) {
        return Ok(height - 1 + min_y - offset as i32);
    }
    Err(malformed(
        path,
        "an anchor is `absolute`, `above_bottom` or `below_top`",
    ))
}

/// A height provider, both of its anchors resolved against the dimension.
///
/// `None` for a provider type this generator does not run, which takes the
/// whole placed feature out with it rather than substituting a uniform one.
fn read_height(
    value: Option<&Value>,
    min_y: i32,
    height: i32,
    path: &Path,
) -> Result<Option<Height>, BuildError> {
    let Some(object) = value else {
        return Ok(None);
    };
    let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
    let min = anchor(object.get("min_inclusive"), min_y, height, path)?;
    let max = anchor(object.get("max_inclusive"), min_y, height, path)?;
    Ok(match kind {
        "minecraft:uniform" => Some(Height::Uniform { min, max }),
        "minecraft:trapezoid" => Some(Height::Trapezoid {
            min,
            max,
            plateau: object.get("plateau").and_then(Value::as_i64).unwrap_or(0) as i32,
        }),
        _ => None,
    })
}

/// Read one `worldgen/configured_feature` entry.
///
/// Answers the type it is, always, and the ore it configures when this
/// generator runs that type.
fn read_configured(
    data_root: &Path,
    name: &str,
    surface: &[BlockSpec],
    extra: &mut Vec<BlockSpec>,
    tags: &mut BTreeMap<String, Vec<String>>,
) -> Result<(String, Option<Ore>), BuildError> {
    let (namespace, id) = split_id(name);
    let path = data_root.join(format!("{namespace}/worldgen/configured_feature/{id}.json"));
    let json = read_json(&path)?;
    let kind = json
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(&path, "a configured feature carries a `type`"))?
        .to_owned();
    if kind != "minecraft:ore" {
        return Ok((kind, None));
    }
    let config = json
        .get("config")
        .ok_or_else(|| malformed(&path, "an ore carries a `config`"))?;
    let size = whole(config, "size", &path)?;
    let discard_on_air = config
        .get("discard_chance_on_air_exposure")
        .and_then(Value::as_f64)
        .ok_or_else(|| malformed(&path, "an ore carries `discard_chance_on_air_exposure`"))?
        as f32;
    let list = config
        .get("targets")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(&path, "an ore carries a list of `targets`"))?;
    let mut targets = Vec::with_capacity(list.len());
    for entry in list {
        let spec = block_spec(
            entry
                .get("state")
                .ok_or_else(|| malformed(&path, "a target carries a `state`"))?,
            &path,
        )?;
        let code = palette_code(&spec, surface, extra, &path)?;
        let names = target_names(
            data_root,
            entry
                .get("target")
                .ok_or_else(|| malformed(&path, "a target carries a `target`"))?,
            &path,
            tags,
        )?;
        // The mask cannot be built until the whole palette is known, because
        // an ore may replace a block an ore two features earlier wrote. It is
        // filled in a second pass, over the finished palette.
        targets.push(Target {
            replaces: CodeSet::default(),
            code,
            names,
        });
    }
    Ok((
        kind,
        Some(Ore {
            size,
            discard_on_air,
            targets,
        }),
    ))
}

/// Which block names a `RuleTest` answers true for.
fn target_names(
    data_root: &Path,
    value: &Value,
    path: &Path,
    tags: &mut BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>, BuildError> {
    let kind = value
        .get("predicate_type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(path, "a rule test carries a `predicate_type`"))?;
    match kind {
        "minecraft:tag_match" => {
            let tag = value
                .get("tag")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed(path, "a tag match carries a `tag`"))?;
            block_tag(data_root, tag, tags, 0)
        }
        "minecraft:block_match" => Ok(vec![value
            .get("block")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed(path, "a block match carries a `block`"))?
            .to_owned()]),
        other => Err(malformed(
            path,
            &format!("`{other}` is not a rule test this generator runs"),
        )),
    }
}

fn block_tag(
    data_root: &Path,
    tag: &str,
    tags: &mut BTreeMap<String, Vec<String>>,
    depth: usize,
) -> Result<Vec<String>, BuildError> {
    if depth > 8 {
        return Err(BuildError::Cycle {
            name: tag.to_owned(),
        });
    }
    if let Some(known) = tags.get(tag) {
        return Ok(known.clone());
    }
    let (namespace, name) = split_id(tag.trim_start_matches('#'));
    let path = data_root.join(format!("{namespace}/tags/block/{name}.json"));
    let json = read_json(&path)?;
    let values = json
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(&path, "a block tag carries `values`"))?;
    let mut members = Vec::new();
    for entry in values {
        let text = match entry {
            Value::String(text) => text.as_str(),
            other => other
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed(&path, "a tag entry names a block"))?,
        };
        match text.strip_prefix('#') {
            Some(nested) => members.extend(block_tag(data_root, nested, tags, depth + 1)?),
            None => members.push(text.to_owned()),
        }
    }
    members.sort();
    members.dedup();
    tags.insert(tag.to_owned(), members.clone());
    Ok(members)
}

/// The material code a block written by a feature gets, extending the surface
/// rules' palette rather than starting a second one.
fn palette_code(
    spec: &BlockSpec,
    surface: &[BlockSpec],
    extra: &mut Vec<BlockSpec>,
    path: &Path,
) -> Result<u8, BuildError> {
    if let Some(index) = surface.iter().position(|entry| entry == spec) {
        return code_of(index, path);
    }
    if let Some(index) = extra.iter().position(|entry| entry == spec) {
        return code_of(surface.len() + index, path);
    }
    extra.push(spec.clone());
    code_of(surface.len() + extra.len() - 1, path)
}

fn code_of(index: usize, path: &Path) -> Result<u8, BuildError> {
    u8::try_from(index + 4).map_err(|_| {
        malformed(
            path,
            "the surface rules and the features together want more than 252 blocks, \
             which is more than a material code holds",
        )
    })
}

/// The scratch one thread needs to place a chunk's features.
#[derive(Debug, Clone)]
pub struct Placer<'a> {
    features: &'a Features,
    rng: Worldgen,
    /// `OreFeature`'s node array, reused: four doubles per node and the largest
    /// vein in a vanilla pack is sixty-four of them.
    nodes: Vec<f64>,
    /// `OreFeature`'s `BitSet`, reused.
    mask: Vec<u64>,
    counts: Counts,
}

impl<'a> Placer<'a> {
    /// The compiled features this scratch belongs to.
    pub fn features(&self) -> &'a Features {
        self.features
    }
}

impl Placer<'_> {
    pub fn counts(&self) -> Counts {
        self.counts
    }

    pub fn reset_counts(&mut self) {
        self.counts = Counts::default();
    }

    /// Place every feature of the chunk and of the eight around it, keeping the
    /// writes that land in this one.
    ///
    /// `heights` is `OCEAN_FLOOR_WG` over the [`WINDOW`] by [`WINDOW`] columns
    /// centred on this chunk, row-major from the north-west corner of the
    /// chunk [`WINDOW_RADIUS`] to the north and west.
    pub fn place(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        materials: &mut [u8],
        heights: &[i16],
        biomes: &mut crate::biome::Sampler<'_>,
        zoom_seed: i64,
    ) {
        if !self.features.ocean_floor_bound() {
            return;
        }
        for offset_z in -1..=1 {
            for offset_x in -1..=1 {
                self.chunk(
                    chunk_x,
                    chunk_z,
                    chunk_x + offset_x,
                    chunk_z + offset_z,
                    materials,
                    heights,
                    biomes,
                    zoom_seed,
                );
            }
        }
    }

    /// One origin chunk's whole decoration, written into `materials`, which is
    /// the chunk at `(centre_x, centre_z)`.
    #[allow(clippy::too_many_arguments)]
    fn chunk(
        &mut self,
        centre_x: i32,
        centre_z: i32,
        chunk_x: i32,
        chunk_z: i32,
        materials: &mut [u8],
        heights: &[i16],
        biomes: &mut crate::biome::Sampler<'_>,
        zoom_seed: i64,
    ) {
        let features = self.features;
        let decoration =
            self.rng
                .set_decoration_seed(features.seed, chunk_x * 16, chunk_z * 16);
        for (step, list) in features.steps.iter().enumerate() {
            for (position, &which) in list.iter().enumerate() {
                let entry = &features.placed[which as usize];
                let (Some(chain), Some(ore)) = (&entry.chain, &entry.ore) else {
                    continue;
                };
                self.counts.seeded += 1;
                self.rng.set_feature_seed(
                    decoration,
                    i32::try_from(position).expect("a step holds fewer than 2G features"),
                    i32::try_from(step).expect("eleven steps"),
                );
                let origin = (chunk_x * 16, features.min_y, chunk_z * 16);
                let mut site = Site {
                    which,
                    centre_x,
                    centre_z,
                    min_y: features.min_y,
                    max_y: features.min_y + features.height,
                    materials,
                    heights,
                    zoom_seed,
                };
                run(
                    features,
                    &mut self.rng,
                    &mut self.nodes,
                    &mut self.mask,
                    &mut self.counts,
                    chain,
                    0,
                    origin,
                    ore,
                    &mut site,
                    biomes,
                );
            }
        }
    }
}

/// One origin chunk's view of the world while its features run.
struct Site<'a> {
    /// Which placed feature, so the biome filter can ask whether the biome at a
    /// position names it.
    which: u32,
    centre_x: i32,
    centre_z: i32,
    min_y: i32,
    max_y: i32,
    materials: &'a mut [u8],
    heights: &'a [i16],
    zoom_seed: i64,
}

impl Site<'_> {
    /// `OCEAN_FLOOR_WG` at a column of the nine-chunk window, which is where
    /// `OreFeature` decides whether to draw a vein at all.
    fn height_at(&self, x: i32, z: i32) -> i32 {
        let column_x = x - (self.centre_x - WINDOW_RADIUS) * 16;
        let column_z = z - (self.centre_z - WINDOW_RADIUS) * 16;
        if !(0..WINDOW as i32).contains(&column_x) || !(0..WINDOW as i32).contains(&column_z) {
            // Outside the write radius, so nothing here can be written anyway.
            return i32::MIN;
        }
        i32::from(self.heights[(column_z as usize) * WINDOW + column_x as usize])
    }

    /// The index into the chunk's material buffer, or `None` when the cell is
    /// outside the chunk being built or outside the world.
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        if y < self.min_y || y >= self.max_y {
            return None;
        }
        let local_x = x - self.centre_x * 16;
        let local_z = z - self.centre_z * 16;
        if !(0..16).contains(&local_x) || !(0..16).contains(&local_z) {
            return None;
        }
        Some((y - self.min_y) as usize * 256 + local_x as usize + local_z as usize * 16)
    }
}

/// Walk one placement chain, depth first, which is the order Java's own lazy
/// `flatMap` pipeline draws in: a modifier's draws happen when it is asked, and
/// the whole of the chain below a position runs before the next position is
/// asked for.
#[allow(clippy::too_many_arguments)]
fn run(
    features: &Features,
    rng: &mut Worldgen,
    nodes: &mut [f64],
    mask: &mut Vec<u64>,
    counts: &mut Counts,
    chain: &[Modifier],
    depth: usize,
    position: (i32, i32, i32),
    ore: &Ore,
    site: &mut Site<'_>,
    biomes: &mut crate::biome::Sampler<'_>,
) {
    let Some(modifier) = chain.get(depth) else {
        counts.offered += 1;
        place_ore(ore, rng, nodes, mask, counts, position, site);
        return;
    };
    match *modifier {
        Modifier::Count(count) => {
            for _ in 0..count {
                run(
                    features, rng, nodes, mask, counts, chain, depth + 1, position, ore, site,
                    biomes,
                );
            }
        }
        Modifier::CountUniform { min, max } => {
            let count = rng.between_inclusive(min, max);
            for _ in 0..count {
                run(
                    features, rng, nodes, mask, counts, chain, depth + 1, position, ore, site,
                    biomes,
                );
            }
        }
        Modifier::Rarity(chance) => {
            // A float reciprocal and a strict `<`, both of them vanilla's:
            // `1.0f / 3` is 0.33333334 and not a third.
            if rng.next_f32() < 1.0f32 / chance as f32 {
                run(
                    features, rng, nodes, mask, counts, chain, depth + 1, position, ore, site,
                    biomes,
                );
            }
        }
        Modifier::InSquare => {
            let x = rng.next_i32_below(16) + position.0;
            let z = rng.next_i32_below(16) + position.2;
            run(
                features,
                rng,
                nodes,
                mask,
                counts,
                chain,
                depth + 1,
                (x, position.1, z),
                ore,
                site,
                biomes,
            );
        }
        Modifier::HeightRange(height) => {
            let y = height.sample(rng);
            run(
                features,
                rng,
                nodes,
                mask,
                counts,
                chain,
                depth + 1,
                (position.0, y, position.2),
                ore,
                site,
                biomes,
            );
        }
        Modifier::Biome => {
            if features.names_here(site, position, biomes) {
                run(
                    features, rng, nodes, mask, counts, chain, depth + 1, position, ore, site,
                    biomes,
                );
            } else {
                counts.off_biome += 1;
            }
        }
    }
}

impl Features {
    /// Whether the biome at a position names the placed feature being placed —
    /// vanilla's `BiomeFilter`, which draws nothing.
    ///
    /// The biome is the blurred one `BiomeManager.getBiome` answers with, not
    /// the raw quart, because that is the lookup a `WorldGenLevel` goes
    /// through.
    fn names_here(
        &self,
        site: &Site<'_>,
        position: (i32, i32, i32),
        biomes: &mut crate::biome::Sampler<'_>,
    ) -> bool {
        let (quart_x, quart_y, quart_z) =
            crate::biome::blurred_quart(site.zoom_seed, position.0, position.1, position.2);
        let Some(id) = biomes.biome(quart_x, quart_y, quart_z) else {
            return false;
        };
        let slot = match self.by_id.get(id as usize) {
            Some(&slot) if slot != u16::MAX => usize::from(slot),
            _ => return false,
        };
        let which = site.which as usize;
        self.biome_sets[slot][which >> 6] >> (which & 63) & 1 == 1
    }
}

/// `OreFeature.place`, then `doPlace`.
fn place_ore(
    ore: &Ore,
    rng: &mut Worldgen,
    nodes: &mut [f64],
    mask: &mut Vec<u64>,
    counts: &mut Counts,
    origin: (i32, i32, i32),
    site: &mut Site<'_>,
) {
    // `Mth.PI` is the float nearest pi, which is what the angle is drawn in.
    let angle = rng.next_f32() * std::f32::consts::PI;
    let half = ore.size as f32 / 8.0;
    let pad = mth_ceil((ore.size as f32 / 16.0 * 2.0 + 1.0) / 2.0);
    // `java.lang.Math.sin` on a double here, and `Mth.sin` on a float below.
    // Both, in one feature, and neither will do for the other.
    let sin = f64::from(angle).sin();
    let cos = f64::from(angle).cos();
    let x_start = f64::from(origin.0) + sin * f64::from(half);
    let x_end = f64::from(origin.0) - sin * f64::from(half);
    let z_start = f64::from(origin.2) + cos * f64::from(half);
    let z_end = f64::from(origin.2) - cos * f64::from(half);
    let y_start = f64::from(origin.1 + rng.next_i32_below(3) - 2);
    let y_end = f64::from(origin.1 + rng.next_i32_below(3) - 2);

    let base_x = origin.0 - mth_ceil(half) - pad;
    let base_y = origin.1 - 2 - pad;
    let base_z = origin.2 - mth_ceil(half) - pad;
    let wide = 2 * (mth_ceil(half) + pad);
    let tall = 2 * (2 + pad);

    let mut reachable = false;
    'scan: for x in base_x..=base_x + wide {
        for z in base_z..=base_z + wide {
            if base_y <= site.height_at(x, z) {
                reachable = true;
                break 'scan;
            }
        }
    }
    if !reachable {
        return;
    }
    counts.veins += 1;

    let size = ore.size;
    if size <= 0 {
        return;
    }
    let size_usize = size as usize;
    if nodes.len() < size_usize * 4 {
        return;
    }
    for k in 0..size_usize {
        let along = k as f32 / size as f32;
        let x = mth_lerp(f64::from(along), x_start, x_end);
        let y = mth_lerp(f64::from(along), y_start, y_end);
        let z = mth_lerp(f64::from(along), z_start, z_end);
        let spread = rng.next_f64() * f64::from(size) / 16.0;
        // `Mth.sin`, and the `+ 1.0` is a float add before anything widens.
        let radius =
            (f64::from(mth_sin(std::f32::consts::PI * along) + 1.0) * spread + 1.0) / 2.0;
        nodes[k * 4] = x;
        nodes[k * 4 + 1] = y;
        nodes[k * 4 + 2] = z;
        nodes[k * 4 + 3] = radius;
    }

    // Nodes that swallow another node absorb it.
    for k in 0..size_usize.saturating_sub(1) {
        if nodes[k * 4 + 3] <= 0.0 {
            continue;
        }
        for other in k + 1..size_usize {
            if nodes[other * 4 + 3] <= 0.0 {
                continue;
            }
            let dx = nodes[k * 4] - nodes[other * 4];
            let dy = nodes[k * 4 + 1] - nodes[other * 4 + 1];
            let dz = nodes[k * 4 + 2] - nodes[other * 4 + 2];
            let dr = nodes[k * 4 + 3] - nodes[other * 4 + 3];
            if dr * dr > dx * dx + dy * dy + dz * dz {
                if dr > 0.0 {
                    nodes[other * 4 + 3] = -1.0;
                } else {
                    nodes[k * 4 + 3] = -1.0;
                }
            }
        }
    }

    // Vanilla's `BitSet` is allocated `wide * tall * wide` and then indexed
    // with a stride that can run past it; `BitSet` grows silently rather than
    // throwing, so the highest index the formula can produce is what this has
    // to hold. Reproduced rather than tidied: the collisions the mismatch
    // causes are cells vanilla does not write.
    let span = if wide <= 0 || tall <= 0 {
        0
    } else {
        (wide as usize) + (tall as usize) * (wide as usize)
            + (wide as usize) * (tall as usize) * (wide as usize)
            + 1
    };
    let words = span.div_ceil(64);
    mask.clear();
    mask.resize(words, 0);

    for k in 0..size_usize {
        let radius = nodes[k * 4 + 3];
        if radius < 0.0 {
            continue;
        }
        let cx = nodes[k * 4];
        let cy = nodes[k * 4 + 1];
        let cz = nodes[k * 4 + 2];
        let low_x = mth_floor(cx - radius).max(base_x);
        let low_y = mth_floor(cy - radius).max(base_y);
        let low_z = mth_floor(cz - radius).max(base_z);
        let high_x = mth_floor(cx + radius).max(low_x);
        let high_y = mth_floor(cy + radius).max(low_y);
        let high_z = mth_floor(cz + radius).max(low_z);
        for x in low_x..=high_x {
            let fx = (f64::from(x) + 0.5 - cx) / radius;
            if fx * fx >= 1.0 {
                continue;
            }
            for y in low_y..=high_y {
                let fy = (f64::from(y) + 0.5 - cy) / radius;
                if fx * fx + fy * fy >= 1.0 {
                    continue;
                }
                for z in low_z..=high_z {
                    let fz = (f64::from(z) + 0.5 - cz) / radius;
                    if fx * fx + fy * fy + fz * fz >= 1.0 {
                        continue;
                    }
                    if y < site.min_y || y >= site.max_y {
                        continue;
                    }
                    let index = (x - base_x) as usize
                        + (y - base_y) as usize * wide as usize
                        + (z - base_z) as usize * wide as usize * tall as usize;
                    if index >= span {
                        continue;
                    }
                    if mask[index >> 6] >> (index & 63) & 1 == 1 {
                        counts.taken += 1;
                        continue;
                    }
                    mask[index >> 6] |= 1u64 << (index & 63);
                    let Some(cell) = site.index(x, y, z) else {
                        // Outside the chunk being built. Vanilla writes it into
                        // the neighbour; that neighbour builds it for itself.
                        continue;
                    };
                    counts.reached += 1;
                    let current = site.materials[cell];
                    for target in &ore.targets {
                        if !target.replaces.contains(current) {
                            continue;
                        }
                        let skip = if !(ore.discard_on_air > 0.0) {
                            true
                        } else if !(ore.discard_on_air < 1.0) {
                            false
                        } else {
                            rng.next_f32() >= ore.discard_on_air
                        };
                        if !skip && adjacent_to_air(site, counts, x, y, z) {
                            continue;
                        }
                        site.materials[cell] = target.code;
                        counts.written += 1;
                        break;
                    }
                }
            }
        }
    }
}

/// `Feature.isAdjacentToAir`, in `Direction.values()` order: down, up, north,
/// south, west, east.
fn adjacent_to_air(site: &Site<'_>, counts: &mut Counts, x: i32, y: i32, z: i32) -> bool {
    const AROUND: [(i32, i32, i32); 6] = [
        (0, -1, 0),
        (0, 1, 0),
        (0, 0, -1),
        (0, 0, 1),
        (-1, 0, 0),
        (1, 0, 0),
    ];
    for (dx, dy, dz) in AROUND {
        match site.index(x + dx, y + dy, z + dz) {
            Some(cell) => {
                if site.materials[cell] == 0 {
                    return true;
                }
            }
            None => {
                // A cell in the next chunk along, which this generator has not
                // built. Answered "not air" and counted, because the only ores
                // that ask are the buried ones and the only cells that ask are
                // at a chunk's four walls.
                counts.air_outside += 1;
            }
        }
    }
    false
}

/// The column heights the feature stage reads, kept between chunks.
///
/// `OreFeature` asks `OCEAN_FLOOR_WG` of columns up to two chunks away, and the
/// only way to answer is to build that chunk's terrain. Building it again for
/// every chunk that asks would be twenty-five terrain fills per chunk; keeping
/// five rows of them makes it one, for any caller that builds columns in a scan
/// order up to [`CACHE_COLUMNS`] wide. A miss costs a fill and never a wrong
/// answer.
///
/// Five rows of sixty-four chunks of 256 sixteen-bit heights is 164 KiB per
/// generating thread, which is under two chunks' worth of the block storage the
/// same thread already holds.
#[derive(Debug, Clone)]
pub struct Heights {
    keys: Vec<(i32, i32)>,
    filled: Vec<bool>,
    values: Vec<i16>,
}

impl Heights {
    pub fn new() -> Self {
        let slots = (CACHE_ROWS * CACHE_COLUMNS) as usize;
        Self {
            keys: vec![(0, 0); slots],
            filled: vec![false; slots],
            values: vec![0; slots * 256],
        }
    }

    fn slot(chunk_x: i32, chunk_z: i32) -> usize {
        (chunk_z.rem_euclid(CACHE_ROWS) * CACHE_COLUMNS + chunk_x.rem_euclid(CACHE_COLUMNS))
            as usize
    }

    pub fn get(&self, chunk_x: i32, chunk_z: i32) -> Option<&[i16]> {
        let slot = Self::slot(chunk_x, chunk_z);
        if self.filled[slot] && self.keys[slot] == (chunk_x, chunk_z) {
            Some(&self.values[slot * 256..slot * 256 + 256])
        } else {
            None
        }
    }

    pub fn put(&mut self, chunk_x: i32, chunk_z: i32, heights: &[i16; 256]) {
        let slot = Self::slot(chunk_x, chunk_z);
        self.keys[slot] = (chunk_x, chunk_z);
        self.filled[slot] = true;
        self.values[slot * 256..slot * 256 + 256].copy_from_slice(heights);
    }
}

impl Default for Heights {
    fn default() -> Self {
        Self::new()
    }
}

impl Features {
    /// `OCEAN_FLOOR_WG` for every column of a chunk, off its materials.
    ///
    /// Vanilla's heightmap answers "the y above the highest block that blocks
    /// motion", and the value it stores is primed after the noise stage and
    /// kept up to date by the carvers — so this reads a carved chunk and not a
    /// noise one.
    pub fn column_heights(&self, materials: &[u8], out: &mut [i16; 256]) {
        let rows = materials.len() / 256;
        out.fill(self.min_y as i16);
        let mut remaining = 256usize;
        for row in (0..rows).rev() {
            if remaining == 0 {
                break;
            }
            let base = row * 256;
            let y = self.min_y + row as i32;
            for column in 0..256usize {
                if out[column] != self.min_y as i16 {
                    continue;
                }
                if self.ocean_floor.contains(materials[base + column]) {
                    out[column] = (y + 1) as i16;
                    remaining -= 1;
                }
            }
        }
    }

    /// Whether a code counts towards `OCEAN_FLOOR_WG`, for the checks.
    #[cfg(test)]
    fn floors(&self, code: u8) -> bool {
        self.ocean_floor.contains(code)
    }
}
