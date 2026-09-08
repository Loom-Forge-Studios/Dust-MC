//! Reading the `--server` worldgen tree into the vanilla ore baseline.
//!
//! `dust-gen::ore_density` scales a world's ore placements. It deliberately
//! does not know what vanilla's are — see that module's header — so the numbers
//! have to come from the operator's own jar, and what lands in the repository
//! is the Rust that results, never Mojang's JSON.
//!
//! A placement is two files. `placed_feature/ore_diamond_buried.json` says how
//! often and how high; the `configured_feature` it names says how big a vein is
//! and which block states it puts down. Both halves are needed for one
//! `Baseline`.
//!
//! # Which placed features are ore placements
//!
//! Not the ones whose name starts with `ore_`. A placed feature is an ore
//! placement when the configured feature it names has type `minecraft:ore` or
//! `minecraft:scattered_ore`, which is a fact about what it does rather than
//! about what it is called. On 1.21.1 the two readings happen to agree exactly
//! — 40 placements either way — and that agreement is worth knowing precisely
//! because it is the kind of thing that stops being true in a datapack.
//!
//! # How a placement becomes an ore group
//!
//! The group is the knob an operator turns, and D6's whole design is that
//! asking for more diamond means all four of vanilla's diamond placements. So
//! every placement has to land in a group, and the grouping cannot come from
//! parsing names: a name rule can only be right about the names whoever wrote
//! it thought of, and it breaks silently on the first datapack that names
//! things differently — which is the case the setting exists to serve.
//!
//! The rule is therefore about what a feature *places*:
//!
//! 1. Two placements are in the same group when they share at least one target
//!    block state. `ore_diamond_buried` places `minecraft:diamond_ore` and
//!    `minecraft:deepslate_diamond_ore`; so does `ore_diamond_large`; that is
//!    what makes them both diamond. Sharing is transitive, so the groups are
//!    the connected components of "places a block the other one also places".
//! 2. The group's *name* is derived from the block ids in it, by taking the
//!    longest run of `_`-separated segments they all end with — falling back to
//!    the longest run they all begin with when they end differently — and then
//!    dropping a trailing `ore` or `ores` segment if anything is left. So
//!    `{coal_ore, deepslate_coal_ore}` is `coal`, `{nether_gold_ore}` is
//!    `nether_gold`, `{ancient_debris}` is `ancient_debris`, and
//!    `{infested_stone, infested_deepslate}` — which share no suffix at all —
//!    is `infested`.
//! 3. A `minecraft:` namespace is elided, because a bare resource location
//!    means `minecraft:` everywhere else too. Any other namespace is kept, so a
//!    datapack's ore is `spelunkery:rock_salt` rather than colliding with a
//!    vanilla name.
//!
//! On 1.21.1 that yields 22 groups over 40 placements, and the 11 names
//! `dust_config::ore::VANILLA_ORE_GROUPS` was hand-written with come out of the
//! data exactly — which is checked here, so that list is a claim this
//! extraction tests rather than a second source of truth.
//!
//! **What the rule does not catch.**
//!
//! - Two ores that a datapack places from one feature — say a vein of copper
//!   that also drops the odd gold — become *one* group, because the rule has
//!   nothing else to go on. On 1.21.1 no two placements overlap partially:
//!   every pair of target sets is identical or disjoint, and a partial overlap
//!   is reported during extraction, so if it ever stops being true it is
//!   visible rather than quiet.
//! - `ore_gravel` and `ore_gravel_nether` both place `minecraft:gravel`, so
//!   they are one group across two dimensions. An operator turning `gravel`
//!   turns both. That follows from the rule and is not a special case.
//! - The rule says nothing about whether a group is an *ore* in the player's
//!   sense. `dirt`, `gravel`, `clay`, `andesite`, `diorite`, `granite`, `tuff`,
//!   `infested`, `magma_block`, `soul_sand` and `blackstone` are terrain blobs
//!   that happen to be placed by the ore feature. They are emitted as groups
//!   like any other, because the alternative is a hand-typed list of "real"
//!   ores — the exact thing the grouping rule exists to avoid — and because an
//!   operator who wants less gravel has as much right to the knob as one who
//!   wants more diamond. The distinction is not hidden: every group carries the
//!   block ids it was derived from, so the generated table says outright that
//!   `dirt` places `minecraft:dirt`, and the extraction prints which derived
//!   groups `VANILLA_ORE_GROUPS` knows about and which it does not.
//! - A placement whose targets yield no name at all is **reported by id and
//!   left out of the table**. It is never silently dropped: a dropped placement
//!   is an ore an operator's setting does nothing to, which D6's record calls
//!   the worst outcome available.
//!
//! # Heights
//!
//! `HeightRange` in `dust-gen` is a plain `min_y`/`max_y` pair, and the data is
//! not. A `height_range` step carries a distribution — `uniform` or `trapezoid`
//! — and each bound is written one of three ways: `absolute`, `above_bottom` or
//! `below_top`. The two relative spellings need the dimension's own vertical
//! extent, which is **read from the data** and not assumed: `dimension_type`
//! gives one `min_y`/`height` and the generator's `noise_settings` gives
//! another, and the context Minecraft resolves an anchor against is the
//! intersection — the higher floor and the lower ceiling. That is not a detail:
//! for the Nether, `dimension_type` says 256 tall and the noise settings say
//! 128, so `below_top: 10` is y=117 and not y=246. Anything that cannot be
//! resolved is an error naming the file rather than a guess.
//!
//! The *shape* of the distribution has nowhere to go in a `min_y`/`max_y` pair,
//! and 13 of the 40 placements are trapezoids. Rather than lose it, the table
//! carries it as a string that nothing yet reads — a trapezoid narrowed by a
//! height override is not the same distribution as a uniform one, and the day
//! that matters the fact should already be in the table instead of needing
//! another extraction.
//!
//! # Two passes, on purpose
//!
//! [`parse`] reads the tree twice. The first pass is everything above: it
//! interprets, resolves and groups. The second, [`source_rows`], walks the same
//! files and copies out the *literals* — the count as written, the anchor
//! spellings, the vein size, the target names, the two pairs of numbers each
//! dimension's height was narrowed from — and interprets none of them.
//!
//! Both go into the generated table, and `dust-gen` checks one against the
//! other. The reason is the same one the block extractor's golden sample gives:
//! a table that is internally consistent proves the reader agrees with itself,
//! not that it agrees with Minecraft. Only a row that never went through the
//! resolution can fail when the resolution is systematically wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::registries::Registries;

/// The `world_preset` that defines a vanilla world.
///
/// The dimensions in this preset are the ones a `Baseline` can be resolved
/// against. A placement generating in a dimension this preset does not have is
/// an error rather than a guess, because the guess would be a vertical extent.
const WORLD_PRESET: &str = "minecraft:normal";

/// Which biome tag says a biome belongs to which of the preset's dimensions.
///
/// This is the one link in this module that the `--server` output does not
/// state. The preset names a `biome_source` with `"preset":
/// "minecraft:overworld"`, and the file that preset points at contains
/// `{"preset": "minecraft:overworld"}` — the biome list never leaves the jar.
/// The `is_*` biome tags do enumerate it, but nothing in the data connects a
/// tag to a dimension.
///
/// So it is written down, kept to three pairs, and checked: every dimension in
/// the preset must appear here, every tag named here must exist, and no biome
/// may be in two of them. A future version that adds a dimension fails the
/// extraction instead of quietly resolving its ores against the Overworld.
const DIMENSION_BIOME_TAGS: &[(&str, &str)] = &[
    ("minecraft:overworld", "minecraft:is_overworld"),
    ("minecraft:the_nether", "minecraft:is_nether"),
    ("minecraft:the_end", "minecraft:is_end"),
];

/// The configured feature types that place ore.
const ORE_FEATURE_TYPES: &[&str] = &["minecraft:ore", "minecraft:scattered_ore"];

/// How often a placement is attempted, in the two forms `dust-gen`'s
/// `Attempts` has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempts {
    PerChunk(u32),
    RarityFilter { one_in: u32 },
}

/// A dimension's vertical generation context, as anchors resolve against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dimension {
    pub id: String,
    pub min_y: i32,
    pub height: i32,
    /// The `dimension_type` and `noise_settings` figures this was narrowed
    /// from, kept so the generated table can be checked against them.
    pub dimension_type_min_y: i32,
    pub dimension_type_height: i32,
    pub noise_min_y: i32,
    pub noise_height: i32,
}

impl Dimension {
    fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }
}

/// One ore placement, resolved into what `Baseline` needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub id: String,
    pub configured_feature: String,
    pub group: String,
    pub dimension: String,
    pub attempts: Attempts,
    pub vein_size: u32,
    pub min_y: i32,
    pub max_y: i32,
    /// `minecraft:uniform` or `minecraft:trapezoid`. Nothing reads it yet; see
    /// the module header for why it is carried anyway.
    pub distribution: String,
    /// The ore feature's own chance of throwing a block away when it would be
    /// exposed to air. Carried for the same reason as `distribution`.
    pub discard_chance_on_air_exposure: f64,
}

/// An ore group: the knob, and the block states that define it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub targets: Vec<String>,
    pub placements: Vec<String>,
}

/// One placement's source facts, copied out of the JSON without being
/// interpreted. See the module header, "Two passes, on purpose".
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    pub placed_feature: String,
    pub configured_feature: String,
    /// The `count` exactly as written: `"4"`, `"0..=1"`, or `""` when the
    /// placement has no `minecraft:count` step at all.
    pub count: String,
    /// The `rarity_filter` chance as written, or `""` when there is no such
    /// step.
    pub rarity: String,
    pub size: u32,
    pub min_anchor: String,
    pub min_value: i32,
    pub max_anchor: String,
    pub max_value: i32,
    pub dimension: String,
    pub dimension_type_min_y: i32,
    pub dimension_type_height: i32,
    pub noise_min_y: i32,
    pub noise_height: i32,
    /// The target block ids, sorted, comma-joined.
    pub targets: String,
}

/// Everything the worldgen tree says about ore, once it has been checked.
#[derive(Debug)]
pub struct Ores {
    pub dimensions: Vec<Dimension>,
    pub placements: Vec<Placement>,
    pub groups: Vec<Group>,
    pub source: Vec<SourceRow>,
    /// Placements whose targets yielded no usable group name, by id. Reported
    /// rather than dropped in silence.
    pub ungrouped: Vec<String>,
}

/// One file, kept with its path so every error can name it.
#[derive(Debug)]
struct Entry {
    path: PathBuf,
    value: Value,
}

/// The `data/<namespace>/<category>/**` tree, indexed by resource location.
#[derive(Debug, Default)]
struct Registry {
    entries: BTreeMap<String, Entry>,
}

impl Registry {
    /// Load `data/<ns>/<category>/**/*.json`, keyed `<ns>:<path without .json>`.
    ///
    /// Every namespace, not just `minecraft`: a datapack's ores are the case
    /// this whole design exists for, and skipping their namespace would make
    /// the extractor right only about vanilla.
    fn load(data_root: &Path, category: &str) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        let namespaces = std::fs::read_dir(data_root)
            .map_err(|e| format!("could not read {}: {e}", data_root.display()))?;
        for namespace in namespaces {
            let namespace =
                namespace.map_err(|e| format!("could not read {}: {e}", data_root.display()))?;
            if !namespace.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = namespace.file_name().to_string_lossy().into_owned();
            let root = namespace.path().join(category);
            if root.is_dir() {
                collect(&root, &root, &name, &mut entries)?;
            }
        }
        Ok(Self { entries })
    }

    fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    fn require(&self, id: &str, wanted_by: &Path) -> Result<&Entry, String> {
        self.get(id).ok_or_else(|| {
            format!(
                "{} refers to {id}, which is not in this data tree",
                wanted_by.display()
            )
        })
    }
}

fn collect(
    root: &Path,
    dir: &Path,
    namespace: &str,
    into: &mut BTreeMap<String, Entry>,
) -> Result<(), String> {
    let read =
        std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    for item in read {
        let item = item.map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        let path = item.path();
        if path.is_dir() {
            collect(root, &path, namespace, into)?;
            continue;
        }
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{} escaped {}", path.display(), root.display()))?
            .with_extension("");
        let id = format!(
            "{namespace}:{}",
            relative.to_string_lossy().replace('\\', "/")
        );
        let text =
            std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_slice(&text)
            .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
        into.insert(id, Entry { path, value });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Small typed accessors. Every one names the file it failed on, because
// "expected an object" with no path is a message that costs an afternoon.
// ---------------------------------------------------------------------------

fn field<'a>(value: &'a Value, key: &str, path: &Path) -> Result<&'a Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("{} has no `{key}`", path.display()))
}

fn as_i32(value: &Value, what: &str, path: &Path) -> Result<i32, String> {
    value
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| format!("{}: {what} is not a whole number that fits", path.display()))
}

fn as_u32(value: &Value, what: &str, path: &Path) -> Result<u32, String> {
    value
        .as_i64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| {
            format!(
                "{}: {what} is not a non-negative whole number",
                path.display()
            )
        })
}

fn as_str<'a>(value: &'a Value, what: &str, path: &Path) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("{}: {what} is not a string", path.display()))
}

fn as_array<'a>(value: &'a Value, what: &str, path: &Path) -> Result<&'a Vec<Value>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{}: {what} is not an array", path.display()))
}

/// The one placement step of the given type, if there is one.
///
/// Two steps of the same type would make "the count" ambiguous, so that is an
/// error rather than a first match.
fn step<'a>(steps: &'a [Value], kind: &str, path: &Path) -> Result<Option<&'a Value>, String> {
    let mut found = steps
        .iter()
        .filter(|s| s.get("type").and_then(Value::as_str) == Some(kind));
    let first = found.next();
    if found.next().is_some() {
        return Err(format!(
            "{} has more than one `{kind}` placement step, so there is no single value \
             to read",
            path.display()
        ));
    }
    Ok(first)
}

// ---------------------------------------------------------------------------
// Dimensions
// ---------------------------------------------------------------------------

/// The vertical context each of the preset's dimensions resolves anchors in.
///
/// Minecraft narrows the dimension's own bounds by the generator's: the floor
/// is the higher of the two and the ceiling the lower of the two. The Nether is
/// the case that proves it matters — 256 tall by its `dimension_type`, 128 by
/// its noise settings, and `below_top: 10` means y=117.
fn dimensions(data_root: &Path) -> Result<Vec<Dimension>, String> {
    let presets = Registry::load(data_root, "worldgen/world_preset")?;
    let types = Registry::load(data_root, "dimension_type")?;
    let noise = Registry::load(data_root, "worldgen/noise_settings")?;

    let preset = presets.get(WORLD_PRESET).ok_or_else(|| {
        format!(
            "this data tree has no `{WORLD_PRESET}` world preset, so nothing in it says \
             what dimensions a vanilla world has"
        )
    })?;
    let listed = field(&preset.value, "dimensions", &preset.path)?
        .as_object()
        .ok_or_else(|| format!("{}: `dimensions` is not an object", preset.path.display()))?;

    let mut out = Vec::with_capacity(listed.len());
    for (id, entry) in listed {
        let type_id = as_str(
            field(entry, "type", &preset.path)?,
            "the dimension type",
            &preset.path,
        )?;
        let dimension_type = types.require(type_id, &preset.path)?;
        let dimension_type_min_y = as_i32(
            field(&dimension_type.value, "min_y", &dimension_type.path)?,
            "min_y",
            &dimension_type.path,
        )?;
        let dimension_type_height = as_i32(
            field(&dimension_type.value, "height", &dimension_type.path)?,
            "height",
            &dimension_type.path,
        )?;

        let generator = field(entry, "generator", &preset.path)?;
        let settings_id = generator
            .get("settings")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!(
                    "{}: dimension {id} has a generator with no `settings`, so its \
                     generation height is not readable from the data",
                    preset.path.display()
                )
            })?;
        let settings = noise.require(settings_id, &preset.path)?;
        let noise_block = field(&settings.value, "noise", &settings.path)?;
        let noise_min_y = as_i32(
            field(noise_block, "min_y", &settings.path)?,
            "noise.min_y",
            &settings.path,
        )?;
        let noise_height = as_i32(
            field(noise_block, "height", &settings.path)?,
            "noise.height",
            &settings.path,
        )?;

        out.push(Dimension {
            id: id.clone(),
            min_y: dimension_type_min_y.max(noise_min_y),
            height: dimension_type_height.min(noise_height),
            dimension_type_min_y,
            dimension_type_height,
            noise_min_y,
            noise_height,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Which dimension each biome generates in, from the `is_*` biome tags.
///
/// See [`DIMENSION_BIOME_TAGS`] for why this correspondence is written down
/// rather than derived, and what is checked instead.
fn biome_dimensions(
    data_root: &Path,
    dimensions: &[Dimension],
) -> Result<BTreeMap<String, String>, String> {
    let tags = Registry::load(data_root, "tags/worldgen/biome")?;
    let mut out: BTreeMap<String, String> = BTreeMap::new();

    for dimension in dimensions {
        let tag_id = DIMENSION_BIOME_TAGS
            .iter()
            .find(|(d, _)| *d == dimension.id)
            .map(|(_, t)| *t)
            .ok_or_else(|| {
                format!(
                    "the `{WORLD_PRESET}` preset has a dimension `{}` this extractor has \
                     no biome tag for. Add it to DIMENSION_BIOME_TAGS in \
                     xtask/src/extract/worldgen.rs; guessing its vertical extent would \
                     put its ores at the wrong depths.",
                    dimension.id
                )
            })?;
        let tag = tags.get(tag_id).ok_or_else(|| {
            format!(
                "this data tree has no biome tag `{tag_id}`, which dimension `{}` needs",
                dimension.id
            )
        })?;
        for value in as_array(field(&tag.value, "values", &tag.path)?, "values", &tag.path)? {
            let biome = as_str(value, "a tag entry", &tag.path)?;
            if let Some(already) = out.insert(biome.to_owned(), dimension.id.clone()) {
                if already != dimension.id {
                    return Err(format!(
                        "{biome} is in the biome tags of both `{already}` and `{}`, so \
                         which dimension's height its features resolve against is not \
                         decidable",
                        dimension.id
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// Which biomes ask for each placed feature.
fn biomes_by_feature(data_root: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let biomes = Registry::load(data_root, "worldgen/biome")?;
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, entry) in &biomes.entries {
        let steps = as_array(
            field(&entry.value, "features", &entry.path)?,
            "features",
            &entry.path,
        )?;
        for step in steps {
            for feature in as_array(step, "a generation step", &entry.path)? {
                // A biome may inline a placed feature rather than naming one.
                // An inlined feature has no id, so nothing can configure it and
                // it cannot be a table row. Skipping it is not a silent drop:
                // the ore scan below works from named placed features and would
                // never have seen it either way.
                if let Some(name) = feature.as_str() {
                    out.entry(name.to_owned()).or_default().insert(id.clone());
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The interpreting pass
// ---------------------------------------------------------------------------

/// Everything loaded from the tree, so the two passes read the same files.
#[derive(Debug)]
struct Tree {
    placed: Registry,
    configured: Registry,
    dimensions: Vec<Dimension>,
    /// Which dimension each ore placement generates in, by placed feature id.
    dimension_of: BTreeMap<String, String>,
}

impl Tree {
    fn dimension(&self, id: &str) -> Option<&Dimension> {
        let name = self.dimension_of.get(id)?;
        self.dimensions.iter().find(|d| &d.id == name)
    }
}

/// Read the whole ore baseline out of a `--server` data tree.
///
/// `data_root` is the `data/` directory the generator wrote — the one with a
/// namespace directory under it.
pub fn parse(data_root: &Path) -> Result<Ores, String> {
    let dimensions = dimensions(data_root)?;
    let biome_dimension = biome_dimensions(data_root, &dimensions)?;
    let biomes_of = biomes_by_feature(data_root)?;
    let placed = Registry::load(data_root, "worldgen/placed_feature")?;
    let configured = Registry::load(data_root, "worldgen/configured_feature")?;

    // Which placed features are ore placements, and which configured feature
    // each one uses. Both passes start from this and nothing else.
    let mut ore_features: BTreeMap<String, String> = BTreeMap::new();
    for (id, entry) in &placed.entries {
        let Some(feature_id) = entry.value.get("feature").and_then(Value::as_str) else {
            // An inlined configured feature: no id, so it can be neither a
            // table row nor something a setting could name.
            continue;
        };
        let feature = configured.require(feature_id, &entry.path)?;
        let kind = as_str(
            field(&feature.value, "type", &feature.path)?,
            "type",
            &feature.path,
        )?;
        if ORE_FEATURE_TYPES.contains(&kind) {
            ore_features.insert(id.clone(), feature_id.to_owned());
        }
    }

    if ore_features.is_empty() {
        return Err(format!(
            "{} contains no ore placements at all. Every placed feature naming a `{}` \
             configured feature would be one, and none does — which means this is not \
             the tree Minecraft's `--server` generator writes.",
            data_root.display(),
            ORE_FEATURE_TYPES.join("` or `")
        ));
    }

    let mut dimension_of = BTreeMap::new();
    for id in ore_features.keys() {
        let entry = &placed.entries[id];
        dimension_of.insert(
            id.clone(),
            dimension_of_placement(id, &entry.path, &biomes_of, &biome_dimension, &dimensions)?,
        );
    }
    let tree = Tree {
        placed,
        configured,
        dimensions,
        dimension_of,
    };

    let mut raw = Vec::with_capacity(ore_features.len());
    for (id, feature_id) in &ore_features {
        raw.push(read_placement(id, feature_id, &tree)?);
    }

    let (groups, ungrouped) = group(&raw)?;
    let named: BTreeMap<&str, &str> = groups
        .iter()
        .flat_map(|g| g.placements.iter().map(|p| (p.as_str(), g.name.as_str())))
        .collect();

    let mut placements = Vec::with_capacity(named.len());
    for one in &raw {
        let Some(group) = named.get(one.id.as_str()) else {
            continue;
        };
        let dimension = tree.dimension(&one.id).expect("attributed above");
        placements.push(Placement {
            id: one.id.clone(),
            configured_feature: one.configured_feature.clone(),
            group: (*group).to_owned(),
            dimension: dimension.id.clone(),
            attempts: one.attempts,
            vein_size: one.vein_size,
            min_y: one.min.resolve(dimension),
            max_y: one.max.resolve(dimension),
            distribution: one.distribution.clone(),
            discard_chance_on_air_exposure: one.discard_chance,
        });
    }

    check_vanilla_group_names(&groups)?;
    let source = source_rows(&tree, &ore_features, &named)?;

    Ok(Ores {
        dimensions: tree.dimensions,
        placements,
        groups,
        source,
        ungrouped,
    })
}

/// A vertical anchor, kept in the spelling the file used.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Anchor {
    anchor: String,
    value: i32,
}

impl Anchor {
    fn read(value: &Value, what: &str, path: &Path) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| format!("{}: {what} is not an object", path.display()))?;
        let [(anchor, number)] = &object.iter().collect::<Vec<_>>()[..] else {
            return Err(format!(
                "{}: {what} has {} keys and a vertical anchor has exactly one \
                 (`absolute`, `above_bottom` or `below_top`)",
                path.display(),
                object.len()
            ));
        };
        if !matches!(anchor.as_str(), "absolute" | "above_bottom" | "below_top") {
            return Err(format!(
                "{}: {what} is spelled `{anchor}`, which this extractor cannot resolve \
                 to a y. It knows `absolute`, `above_bottom` and `below_top`.",
                path.display()
            ));
        }
        Ok(Self {
            anchor: (*anchor).clone(),
            value: as_i32(number, what, path)?,
        })
    }

    /// The y this anchor means in a dimension.
    ///
    /// `below_top` counts down from the highest y that generates, which is one
    /// below the top of the range: a dimension `height` blocks tall starting at
    /// `min_y` has its last block at `min_y + height - 1`.
    fn resolve(&self, dimension: &Dimension) -> i32 {
        match self.anchor.as_str() {
            "above_bottom" => dimension.min_y + self.value,
            "below_top" => dimension.max_y() - self.value,
            _ => self.value,
        }
    }
}

/// One placement, interpreted but not yet grouped or resolved.
#[derive(Debug)]
struct Raw {
    id: String,
    configured_feature: String,
    attempts: Attempts,
    vein_size: u32,
    min: Anchor,
    max: Anchor,
    distribution: String,
    discard_chance: f64,
    targets: Vec<String>,
}

fn read_placement(id: &str, feature_id: &str, tree: &Tree) -> Result<Raw, String> {
    let entry = &tree.placed.entries[id];
    let path = &entry.path;
    let steps = as_array(field(&entry.value, "placement", path)?, "placement", path)?;

    let attempts = attempts(steps, path)?;

    let height = step(steps, "minecraft:height_range", path)?.ok_or_else(|| {
        format!(
            "{} has no `minecraft:height_range` step, so there is no vertical range to \
             put in a baseline",
            path.display()
        )
    })?;
    let height = field(height, "height", path)?;
    let distribution = as_str(
        field(height, "type", path)?,
        "the height distribution",
        path,
    )?;
    let min = Anchor::read(field(height, "min_inclusive", path)?, "min_inclusive", path)?;
    let max = Anchor::read(field(height, "max_inclusive", path)?, "max_inclusive", path)?;

    let feature = &tree.configured.entries[feature_id];
    let config = field(&feature.value, "config", &feature.path)?;
    let vein_size = as_u32(field(config, "size", &feature.path)?, "size", &feature.path)?;
    let discard_chance = config
        .get("discard_chance_on_air_exposure")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let targets = targets(config, &feature.path)?;

    Ok(Raw {
        id: id.to_owned(),
        configured_feature: feature_id.to_owned(),
        attempts,
        vein_size,
        min,
        max,
        distribution: distribution.to_owned(),
        discard_chance,
        targets,
    })
}

/// The block ids an ore feature's config places, sorted and deduplicated.
fn targets(config: &Value, path: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for target in as_array(field(config, "targets", path)?, "targets", path)? {
        let state = field(target, "state", path)?;
        out.push(as_str(field(state, "Name", path)?, "a target's state Name", path)?.to_owned());
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        return Err(format!(
            "{} places nothing: its `targets` is empty, so no ore group could be derived \
             from it",
            path.display()
        ));
    }
    Ok(out)
}

/// The dimension a placement generates in, from the biomes that ask for it.
///
/// An unresolvable answer is an error naming the file even when both bounds are
/// `absolute` and it would have been harmless. Accepting it there would mean
/// the check is absent in exactly the tree where a datapack later adds a
/// relative bound.
fn dimension_of_placement(
    id: &str,
    path: &Path,
    biomes_of: &BTreeMap<String, BTreeSet<String>>,
    biome_dimension: &BTreeMap<String, String>,
    dimensions: &[Dimension],
) -> Result<String, String> {
    let mut found: BTreeSet<&str> = BTreeSet::new();
    for biome in biomes_of.get(id).into_iter().flatten() {
        if let Some(dimension) = biome_dimension.get(biome) {
            found.insert(dimension.as_str());
        }
    }
    let known = |name: &str| dimensions.iter().any(|d| d.id == name);
    match found.iter().copied().collect::<Vec<_>>()[..] {
        [one] if known(one) => Ok(one.to_owned()),
        [] => Err(format!(
            "{} is not asked for by any biome this extractor can place in a dimension, \
             so there is no vertical extent to resolve its height bounds against",
            path.display()
        )),
        _ => Err(format!(
            "{} is asked for by biomes in {} different dimensions ({}). One placement \
             cannot have two vertical extents.",
            path.display(),
            found.len(),
            found.iter().copied().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// How often the placement is attempted.
///
/// Three shapes have to come out of two variants:
///
/// - `minecraft:count` with a number is `PerChunk(n)`.
/// - `minecraft:rarity_filter` with a chance is `RarityFilter { one_in }`.
/// - **Neither step at all** is `PerChunk(1)`. That is not a default and not a
///   guess: a placed feature runs once per chunk from a single origin position,
///   and a `count` step is what *multiplies* that position into several.
///   Absence therefore means one attempt, not none — and reading it as none
///   would silently delete both of vanilla's ancient debris placements, which
///   are the two written this way.
///
/// A `count` can also be a value provider rather than a number. A uniform
/// provider over `0..=1` is one attempt with probability one half, which is
/// exactly what `RarityFilter { one_in: 2 }` already means, so it crosses over
/// without losing anything — vanilla's `ore_gold_lower` is written that way. A
/// provider over any other range has no `Attempts` to land in and is an error
/// naming the file, because the alternative is to keep its mean and throw away
/// its spread.
fn attempts(steps: &[Value], path: &Path) -> Result<Attempts, String> {
    let count = step(steps, "minecraft:count", path)?;
    let rarity = step(steps, "minecraft:rarity_filter", path)?;

    if count.is_some() && rarity.is_some() {
        return Err(format!(
            "{} has both a `minecraft:count` and a `minecraft:rarity_filter` step. \
             Attempts is one or the other, and multiplying a combination of them is not \
             something this extractor can claim to have got right.",
            path.display()
        ));
    }

    if let Some(rarity) = rarity {
        let chance = as_u32(field(rarity, "chance", path)?, "the rarity chance", path)?;
        if chance == 0 {
            return Err(format!(
                "{}: a rarity chance of 0 is one chunk in none",
                path.display()
            ));
        }
        return Ok(Attempts::RarityFilter { one_in: chance });
    }

    let Some(count) = count else {
        return Ok(Attempts::PerChunk(1));
    };
    let count = field(count, "count", path)?;

    if count.is_i64() {
        return Ok(Attempts::PerChunk(as_u32(count, "the count", path)?));
    }

    match count.get("type").and_then(Value::as_str) {
        Some("minecraft:constant") => Ok(Attempts::PerChunk(as_u32(
            field(count, "value", path)?,
            "the count",
            path,
        )?)),
        Some("minecraft:uniform") => {
            let low = as_i32(
                field(count, "min_inclusive", path)?,
                "the count's min_inclusive",
                path,
            )?;
            let high = as_i32(
                field(count, "max_inclusive", path)?,
                "the count's max_inclusive",
                path,
            )?;
            if low == high {
                let n = u32::try_from(low).map_err(|_| {
                    format!(
                        "{}: a count of {low} is not a number of attempts",
                        path.display()
                    )
                })?;
                Ok(Attempts::PerChunk(n))
            } else if low == 0 && high == 1 {
                Ok(Attempts::RarityFilter { one_in: 2 })
            } else {
                Err(format!(
                    "{}: its count is uniform over {low}..={high}. `Attempts` holds a \
                     fixed count or a one-in-N chance, and neither is that distribution \
                     — only `0..=1`, which is one attempt in two chunks, crosses over \
                     exactly.",
                    path.display()
                ))
            }
        }
        other => Err(format!(
            "{}: its count is a `{}` value provider, which this extractor cannot turn \
             into a number of attempts per chunk.",
            path.display(),
            other.unwrap_or("(untyped)")
        )),
    }
}

// ---------------------------------------------------------------------------
// The copying pass
// ---------------------------------------------------------------------------

/// Copy each placement's source facts out of the JSON without interpreting any
/// of them.
///
/// This shares the loaded files with the pass above, and the dimension each
/// placement was attributed to, and nothing else. In particular it does not
/// call [`attempts`], [`Anchor`] or [`targets`] — it reads the same fields with
/// its own hands, so that a systematically wrong reading up there shows up as a
/// disagreement down here rather than as two tables that match.
///
/// **What it does not catch.** The dimension attribution is shared, so a
/// placement filed under the wrong dimension would produce a consistent pair of
/// rows. That one is guarded differently: a placement whose biomes span two
/// dimensions, or none, stops the extraction outright.
fn source_rows(
    tree: &Tree,
    ore_features: &BTreeMap<String, String>,
    grouped: &BTreeMap<&str, &str>,
) -> Result<Vec<SourceRow>, String> {
    let mut out = Vec::with_capacity(ore_features.len());
    for (id, feature_id) in ore_features {
        if !grouped.contains_key(id.as_str()) {
            continue;
        }
        let entry = &tree.placed.entries[id];
        let path = &entry.path;
        let steps = as_array(field(&entry.value, "placement", path)?, "placement", path)?;

        let mut count = String::new();
        let mut rarity = String::new();
        let mut min = (String::new(), 0);
        let mut max = (String::new(), 0);
        for step in steps {
            match step.get("type").and_then(Value::as_str) {
                Some("minecraft:count") => {
                    let value = field(step, "count", path)?;
                    count = match (value.as_i64(), value.get("value").and_then(Value::as_i64)) {
                        (Some(n), _) => n.to_string(),
                        (None, Some(n)) => n.to_string(),
                        (None, None) => format!(
                            "{}..={}",
                            field(value, "min_inclusive", path)?,
                            field(value, "max_inclusive", path)?
                        ),
                    };
                }
                Some("minecraft:rarity_filter") => {
                    rarity = field(step, "chance", path)?.to_string();
                }
                Some("minecraft:height_range") => {
                    let height = field(step, "height", path)?;
                    min = one_key(field(height, "min_inclusive", path)?, path)?;
                    max = one_key(field(height, "max_inclusive", path)?, path)?;
                }
                _ => {}
            }
        }
        if min.0.is_empty() || max.0.is_empty() {
            return Err(format!("{} has no readable height bounds", path.display()));
        }

        let feature = &tree.configured.entries[feature_id];
        let config = field(&feature.value, "config", &feature.path)?;
        let size = as_u32(field(config, "size", &feature.path)?, "size", &feature.path)?;
        let mut names: Vec<String> = as_array(
            field(config, "targets", &feature.path)?,
            "targets",
            &feature.path,
        )?
        .iter()
        .map(|t| {
            t.get("state")
                .and_then(|s| s.get("Name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
        names.sort();
        names.dedup();

        let dimension = tree.dimension(id).expect("attributed above");
        out.push(SourceRow {
            placed_feature: id.clone(),
            configured_feature: feature_id.clone(),
            count,
            rarity,
            size,
            min_anchor: min.0,
            min_value: min.1,
            max_anchor: max.0,
            max_value: max.1,
            dimension: dimension.id.clone(),
            dimension_type_min_y: dimension.dimension_type_min_y,
            dimension_type_height: dimension.dimension_type_height,
            noise_min_y: dimension.noise_min_y,
            noise_height: dimension.noise_height,
            targets: names.join(","),
        });
    }
    Ok(out)
}

/// The single key and value of a one-entry object, as text and a number.
fn one_key(value: &Value, path: &Path) -> Result<(String, i32), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{}: a height bound is not an object", path.display()))?;
    let mut pairs = object.iter();
    match (pairs.next(), pairs.next()) {
        (Some((key, number)), None) => Ok((key.clone(), as_i32(number, key, path)?)),
        _ => Err(format!(
            "{}: a height bound has {} keys and needs exactly one",
            path.display(),
            object.len()
        )),
    }
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

/// Gather placements into ore groups by the block states they place.
///
/// The rule itself is `dust_gen::ore_density::group`, and living there rather
/// than here is the point: the generator has to key its knob by the same name
/// this table writes, and two implementations of a naming rule are two chances
/// for `[worldgen.ores.overrides.diamond]` to name nothing. This function is
/// the extractor's half — turning indices back into ids, and refusing a world
/// where one name would mean two ores.
fn group(raw: &[Raw]) -> Result<(Vec<Group>, Vec<String>), String> {
    let placed: Vec<Vec<String>> = raw.iter().map(|one| one.targets.clone()).collect();
    let grouping = dust_gen::ore_density::group(&placed);

    if !grouping.overlapping.is_empty() {
        let ids: Vec<&str> = grouping
            .overlapping
            .iter()
            .map(|&i| raw[i].id.as_str())
            .collect();
        println!(
            "note: these placements share some but not all of their target blocks, so \
             they are one group: {}",
            ids.join(", ")
        );
    }

    let mut seen = BTreeSet::new();
    for one in &grouping.groups {
        if !seen.insert(one.name.as_str()) {
            return Err(format!(
                "two ore groups both derive the name `{}`, from different block states \
                 ({}). One knob cannot mean two ores.",
                one.name.as_str(),
                one.targets.join(", ")
            ));
        }
    }

    let groups = grouping
        .groups
        .iter()
        .map(|one| Group {
            name: one.name.as_str().to_owned(),
            targets: one.targets.clone(),
            placements: one.placements.iter().map(|&i| raw[i].id.clone()).collect(),
        })
        .collect();
    let mut ungrouped: Vec<String> = grouping
        .of_placement
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.is_none())
        .map(|(index, _)| raw[index].id.clone())
        .collect();
    ungrouped.sort();
    Ok((groups, ungrouped))
}

/// Every name `dust_config::ore::VANILLA_ORE_GROUPS` claims vanilla has must
/// actually come out of the data.
///
/// That list is documentation and a validation aid, not a source of truth, and
/// this is what keeps it from drifting into being wrong. If the grouping rule
/// ever produced `diamonds` where the configuration promises `diamond`, an
/// operator's `[worldgen.ores.overrides.diamond]` would be rejected by the
/// world at boot and nothing else would have noticed.
///
/// The reverse is not an error. The data has 11 groups the list does not name —
/// `dirt`, `gravel`, `andesite` and friends — and they are groups on purpose.
fn check_vanilla_group_names(groups: &[Group]) -> Result<(), String> {
    let derived: BTreeSet<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    let missing: Vec<&str> = dust_config::ore::VANILLA_ORE_GROUPS
        .iter()
        .copied()
        .filter(|name| !derived.contains(name))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "dust_config::ore::VANILLA_ORE_GROUPS names {} ore group(s) this data tree does \
         not produce: {}. Either the grouping rule changed or the list did; a name in \
         that list that no world has is a setting an operator can write and nothing can \
         apply.",
        missing.len(),
        missing.join(", ")
    ))
}

// ---------------------------------------------------------------------------
// The Phase 6 vocabulary: density functions, noise router, biome parameters
// ---------------------------------------------------------------------------

/// One density-function type as the vanilla files use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DensityFunctionType {
    /// Namespaced type id, e.g. `minecraft:add`.
    pub name: String,
    /// How many times the type appears across every density function in the
    /// data pack — including nested appearances, since these trees nest.
    pub uses: usize,
    /// Top-level argument keys seen on objects of this type, `type` excluded,
    /// sorted.
    pub arguments: Vec<String>,
}

/// One dimension's biome-parameter definition, summarised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiomeParameterDimension {
    pub dimension: String,
    /// How many parameter entries the generator wrote for this dimension.
    pub entries: usize,
    pub distinct_biomes: usize,
    /// Entries whose parameters carry a `[min, max]` range rather than a point.
    pub ranged_entries: usize,
}

/// A verbatim biome parameter point from the smallest dimension, emitted as
/// the golden sample of the format. Parameter values are f64; the seven names
/// and their order are [`PARAMETER_NAMES`].
#[derive(Debug, Clone, PartialEq)]
pub struct BiomePoint {
    pub biome: String,
    /// Values in [`PARAMETER_NAMES`] order.
    pub values: [f64; PARAMETER_COUNT],
}

/// The multi-noise parameter names, alphabetically for stability.
pub const PARAMETER_NAMES: &[&str] = &[
    "continentalness",
    "depth",
    "erosion",
    "humidity",
    "offset",
    "temperature",
    "weirdness",
];
pub const PARAMETER_COUNT: usize = PARAMETER_NAMES.len();

/// Everything the vocabulary pass collects.
#[derive(Debug)]
pub struct WorldgenVocabulary {
    /// Every density-function type the data uses, name-sorted, each checked
    /// against the `worldgen/density_function_type` registry in the report.
    pub density_function_types: Vec<DensityFunctionType>,
    /// The noise-router slot names, identical across every noise setting —
    /// checked, not assumed, since one deviant file would mean a version
    /// changed terrain's wiring.
    pub noise_router_slots: Vec<String>,
    pub dimensions: Vec<BiomeParameterDimension>,
    /// The nether's five points, verbatim: the golden sample of what a
    /// parameter entry looks like when it is points all the way down.
    pub nether_points: Vec<BiomePoint>,
}

/// Read the density-function and biome-parameter vocabulary.
///
/// Deliberately *not* the full overworld parameter expansion — 7,593 ranged
/// entries naming 53 biomes. Those are world-generation *data*, which a real
/// server reads from the world's datapacks at boot; baking them into the
/// binary would be committing Mojang's content by volume and freezing what a
/// datapack exists to change. What Phase 6 needs before any of that is the
/// vocabulary: which density functions exist and what they are called with,
/// which slots a noise router has, and what shape a biome parameter entry
/// takes. That, plus the five-point nether as a worked example, is what lands.
pub fn vocabulary(
    data_root: &Path,
    reports_root: &Path,
    registries: &Registries,
) -> Result<WorldgenVocabulary, String> {
    // Density functions: walk every file, counting how many objects of each
    // type appear and which top-level argument keys they carry.
    let mut type_shapes: BTreeMap<String, (usize, BTreeSet<String>)> = BTreeMap::new();

    // Argument keys are owned rather than borrowed from the file being read:
    // the map outlives each file, and a map tied to one file's lifetime is
    // exactly how a borrow error gets written.
    fn recurse_types(value: &Value, shapes: &mut BTreeMap<String, (usize, BTreeSet<String>)>) {
        match value {
            Value::Object(fields) => {
                if let Some(name) = fields.get("type").and_then(Value::as_str) {
                    let (count, arguments) = shapes.entry(name.to_owned()).or_default();
                    *count += 1;
                    arguments.extend(fields.keys().filter(|k| *k != "type").cloned());
                }
                fields.values().for_each(|v| recurse_types(v, shapes));
            }
            Value::Array(items) => items.iter().for_each(|v| recurse_types(v, shapes)),
            _ => {}
        }
    }

    let function_root = data_root.join("worldgen/density_function");
    if function_root.is_dir() {
        for path in collect_json(&function_root)? {
            let text = std::fs::read(&path)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            let value: Value = serde_json::from_slice(&text)
                .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
            recurse_types(&value, &mut type_shapes);
        }
    } else {
        return Err(format!(
            "{} holds no density_function tree, so there is no terrain vocabulary to read",
            data_root.display()
        ));
    }

    // Every type the terrain uses must be a registered density-function type:
    // two reports agreeing again, and the check that stops a typo'd `type`
    // from becoming part of the committed vocabulary.
    let type_registry = registries
        .registries
        .iter()
        .find(|r| r.name == "minecraft:worldgen/density_function_type")
        .ok_or("the registry report has no minecraft:worldgen/density_function_type")?;

    Ok(WorldgenVocabulary {
        density_function_types: type_shapes
            .into_iter()
            .filter_map(|(name, (uses, arguments))| {
                if !type_registry.entries.iter().any(|e| e.name == name) {
                    println!(
                        "note: {name} shapes terrain but is not in the \
                         density_function_type registry on this version"
                    );
                    return None;
                }
                Some(DensityFunctionType {
                    name,
                    uses,
                    arguments: arguments.into_iter().collect(),
                })
            })
            .collect(),
        noise_router_slots: noise_router_slots(data_root)?,
        dimensions: biome_parameter_dimensions(reports_root)?,
        nether_points: biome_points(reports_root, "nether")?,
    })
}

/// Every `.json` file under `directory`, sorted.
///
/// The ore passes walk namespaces through [`Registry::load`]; the vocabulary
/// pass reads fixed paths (`density_function/`, the biome-parameter report),
/// so it walks directly.
fn collect_json(directory: &Path) -> Result<Vec<PathBuf>, String> {
    fn walk(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(directory)
            .map_err(|e| format!("could not read {}: {e}", directory.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("could not read {}: {e}", directory.display()))?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().is_some_and(|e| e == "json") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(directory, &mut out)?;
    out.sort();
    Ok(out)
}

/// The slot names of the noise router, checked to be identical across every
/// `noise_settings` file: one shared wiring diagram rather than seven guesses.
fn noise_router_slots(data_root: &Path) -> Result<Vec<String>, String> {
    let root = data_root.join("worldgen/noise_settings");
    if !root.is_dir() {
        return Err(format!(
            "{} holds no noise_settings, so the router slots cannot be checked",
            root.display()
        ));
    }
    let mut shared: Option<BTreeSet<String>> = None;
    let mut files = 0usize;
    for path in collect_json(&root)? {
        let text =
            std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_slice(&text)
            .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
        let router = field(&value, "noise_router", &path)?
            .as_object()
            .ok_or_else(|| format!("{}: noise_router is not an object", path.display()))?;
        let slots: BTreeSet<String> = router.keys().cloned().collect();
        match &shared {
            Some(previous) => {
                if *previous != slots {
                    return Err(format!(
                        "{} wires its noise router differently ({:?} versus {:?}). A \\
                         version that changes terrain's wiring stops here, where it is \\
                         news.",
                        path.display(),
                        slots,
                        previous
                    ));
                }
            }
            None => shared = Some(slots),
        }
        files += 1;
    }
    if files == 0 || shared.is_none() {
        return Err(format!(
            "{} held no readable noise settings",
            root.display()
        ));
    }
    let mut slots: Vec<String> = shared.expect("checked above").into_iter().collect();
    slots.sort();
    Ok(slots)
}

fn biome_parameter_dimensions(reports_root: &Path) -> Result<Vec<BiomeParameterDimension>, String> {
    let mut out = Vec::new();
    for (dimension, summary) in summarize_biome_parameters(reports_root)? {
        out.push(BiomeParameterDimension {
            dimension,
            entries: summary.0,
            distinct_biomes: summary.2,
            ranged_entries: summary.1,
        });
    }
    out.sort_by(|a, b| a.dimension.cmp(&b.dimension));
    Ok(out)
}

/// `(entries, ranged_entries, distinct_biomes)` per dimension.
type ParameterSummary = BTreeMap<String, (usize, usize, usize)>;

fn summarize_biome_parameters(reports_root: &Path) -> Result<ParameterSummary, String> {
    let root = reports_root.join("biome_parameters/minecraft");
    if !root.is_dir() {
        return Err(format!(
            "{} holds no biome_parameters report, so the multi-noise vocabulary cannot \\
             be read",
            reports_root.display()
        ));
    }
    let mut out = ParameterSummary::default();
    for path in collect_json(&root)? {
        let dimension = format!(
            "minecraft:{}",
            path.with_extension("")
                .strip_prefix(&root)
                .map_err(|_| format!("{} escaped {}", path.display(), root.display()))?
                .to_string_lossy()
        );
        let text =
            std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_slice(&text)
            .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
        let biomes = field(&value, "biomes", &path)?;
        let mut entries = 0usize;
        let mut ranged = 0usize;
        let mut distinct = std::collections::BTreeSet::new();
        for entry in as_array(biomes, "biomes", &path)? {
            entries += 1;
            distinct.insert(
                as_str(field(entry, "biome", &path)?, "the entry's biome", &path)?.to_owned(),
            );
            let parameters = field(entry, "parameters", &path)?;
            if parameters
                .as_object()
                .ok_or_else(|| format!("{}: parameters is not an object", path.display()))?
                .values()
                .any(|v| v.is_array())
            {
                ranged += 1;
            }
        }
        out.insert(dimension, (entries, ranged, distinct.len()));
    }
    if out.is_empty() {
        return Err(format!(
            "{} held no dimension reports, which means this run's generators predate \\
             the biome_parameters report",
            root.display()
        ));
    }
    Ok(out)
}

/// One dimension's parameter entries verbatim, as golden rows.
fn biome_points(reports_root: &Path, dimension: &str) -> Result<Vec<BiomePoint>, String> {
    let path = reports_root
        .join("biome_parameters/minecraft")
        .join(format!("{dimension}.json"));
    let text =
        std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let value: Value = serde_json::from_slice(&text)
        .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
    let mut out = Vec::new();
    for entry in as_array(field(&value, "biomes", &path)?, "biomes", &path)? {
        let biome = as_str(field(entry, "biome", &path)?, "the entry's biome", &path)?.to_owned();
        let parameters = field(entry, "parameters", &path)?;
        let object = parameters
            .as_object()
            .ok_or_else(|| format!("{}: parameters is not an object", path.display()))?;
        let mut values = [0.0; PARAMETER_COUNT];
        for (index, name) in PARAMETER_NAMES.iter().enumerate() {
            let number = object.get(*name).ok_or_else(|| {
                format!("{}: {} has no `{name}` parameter", path.display(), biome)
            })?;
            if !number.is_number() {
                // Only recorded for dimensions whose entries are plain points;
                // a ranged dimension is summarised above instead.
                return Err(format!(
                    "{}: {}'s {name} is {number}, not a point value",
                    path.display(),
                    biome
                ));
            }
            values[index] = number.as_f64().unwrap_or(f64::NAN);
        }
        out.push(BiomePoint { biome, values });
    }
    out.sort_by(|a, b| a.biome.cmp(&b.biome));
    Ok(out)
}

/// One row of `dust-biomes.tsv`: a biome and the region of climate space it
/// claims, already quantised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiomeRegion {
    /// The id the running registry gives this name, at the moment of
    /// extraction. `dust_gen::biome::BiomeParameters::rebind` checks it again
    /// at load and says which biome moved if it ever stops being true.
    pub id: u32,
    pub biome: String,
    /// Inclusive spans, in `dust_gen::biome::AXES` order.
    pub axes: [(i64, i64); 6],
    pub offset: i64,
}

/// The six climate axes, in the order `dust_gen::biome::AXES` holds them, with
/// the name each has in the biome-parameter report.
///
/// The two spellings differ on two of the six and that is not a typo: the
/// noise router calls them `vegetation` and `ridges`, and the parameter list
/// calls the same two axes `humidity` and `weirdness`. Naming them together
/// here is what stops the pairing being re-derived — or mis-derived — twice.
const REPORT_AXES: [&str; 6] = [
    "temperature",
    "humidity",
    "continentalness",
    "erosion",
    "depth",
    "weirdness",
];

/// Minecraft's `Climate.quantizeCoord`.
///
/// The multiply is in **f32** and the narrowing is a truncation, because
/// Minecraft's is: `(long)(f * 10000.0F)`. Doing it in f64 would be more
/// accurate and would put some rows one ten-thousandth off the region vanilla
/// actually compares against.
fn quantise(value: f64) -> i64 {
    ((value as f32) * 10_000.0f32) as i64
}

/// One parameter, which the report writes as a number when it is a point and
/// as `[min, max]` when it is a span.
fn span(value: &Value, what: &str, path: &Path) -> Result<(i64, i64), String> {
    if let Some(number) = value.as_f64() {
        let at = quantise(number);
        return Ok((at, at));
    }
    let pair = value
        .as_array()
        .ok_or_else(|| format!("{}: {what} is neither a number nor a range", path.display()))?;
    if pair.len() != 2 {
        return Err(format!(
            "{}: {what} is a range of {} value(s), not two",
            path.display(),
            pair.len()
        ));
    }
    let min = quantise(
        pair[0]
            .as_f64()
            .ok_or_else(|| format!("{}: {what}'s low end is not a number", path.display()))?,
    );
    let max = quantise(
        pair[1]
            .as_f64()
            .ok_or_else(|| format!("{}: {what}'s high end is not a number", path.display()))?,
    );
    if min > max {
        return Err(format!("{}: {what} runs backwards", path.display()));
    }
    Ok((min, max))
}

/// Read one dimension's whole biome-parameter list, ready to write out.
///
/// **This is world data and it does not enter the repository.** It is written
/// to the extraction cache as a TSV the operator copies beside their own data,
/// exactly as `dust-constants.tsv` and `dust-items.tsv` are, and for the reason
/// decision records 0006, 0007 and 0008 give: a biome parameter list is
/// Mojang's content and a datapack's to change.
pub fn biome_regions(
    reports_root: &Path,
    dimension: &str,
    id_of: impl Fn(&str) -> Option<u32>,
) -> Result<Vec<BiomeRegion>, String> {
    let path = reports_root
        .join("biome_parameters/minecraft")
        .join(format!("{dimension}.json"));
    let text =
        std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let value: Value = serde_json::from_slice(&text)
        .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
    let mut out = Vec::new();
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    for entry in as_array(field(&value, "biomes", &path)?, "biomes", &path)? {
        let biome = as_str(field(entry, "biome", &path)?, "the entry's biome", &path)?.to_owned();
        let parameters = field(entry, "parameters", &path)?;
        let mut axes = [(0i64, 0i64); 6];
        for (slot, name) in axes.iter_mut().zip(REPORT_AXES) {
            *slot = span(field(parameters, name, &path)?, name, &path)?;
        }
        let offset = quantise(
            field(parameters, "offset", &path)?
                .as_f64()
                .ok_or_else(|| format!("{}: {biome}'s offset is not a number", path.display()))?,
        );
        let Some(id) = id_of(&biome) else {
            unknown.insert(biome);
            continue;
        };
        out.push(BiomeRegion {
            id,
            biome,
            axes,
            offset,
        });
    }
    // Refused rather than skipped. A biome the registry has never heard of is
    // a row that can never be chosen, so a world generated from this table
    // would quietly hold a hole exactly where that biome belongs — and the
    // shape of the hole would be an ocean or a jungle, not an error.
    if !unknown.is_empty() {
        return Err(format!(
            "{} names {} biome(s) the synced registry has no id for ({}). The parameter \
             list and the registry came out of the same jar, so this means one of the two \
             readers is looking at the wrong tree.",
            path.display(),
            unknown.len(),
            unknown.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if out.is_empty() {
        return Err(format!("{} named no biome regions", path.display()));
    }
    Ok(out)
}

/// The TSV `dust_gen::biome::BiomeParameters::parse` reads.
///
/// Row order is the report's order, which is the order vanilla's own parameter
/// list is built in, because the search resolves an exact tie in favour of the
/// first row.
pub fn biome_table(regions: &[BiomeRegion]) -> String {
    let mut out = String::from("# biome_id\tbiome");
    for axis in REPORT_AXES {
        out.push_str(&format!("\t{axis}_min\t{axis}_max"));
    }
    out.push_str("\toffset\n");
    for region in regions {
        out.push_str(&format!("{}\t{}", region.id, region.biome));
        for (min, max) in region.axes {
            out.push_str(&format!("\t{min}\t{max}"));
        }
        out.push_str(&format!("\t{}\n", region.offset));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(kind: &str, value: i32) -> Anchor {
        Anchor {
            anchor: kind.to_owned(),
            value,
        }
    }

    fn nether() -> Dimension {
        Dimension {
            id: "minecraft:the_nether".to_owned(),
            min_y: 0,
            height: 128,
            dimension_type_min_y: 0,
            dimension_type_height: 256,
            noise_min_y: 0,
            noise_height: 128,
        }
    }

    #[test]
    fn a_climate_value_is_quantised_the_way_minecraft_quantises_it() {
        assert_eq!(quantise(0.0), 0);
        assert_eq!(quantise(1.0), 10_000);
        assert_eq!(quantise(-1.2), -12_000);

        // The width is not a style choice, and 1.21.1's own overworld report
        // is what proves it. Of the 37 distinct parameter values in that file,
        // two quantise differently in the two widths — and they are real
        // bounds, on the temperature and weirdness axes of biomes a player
        // walks through. Minecraft multiplies in f32; f64 is one
        // ten-thousandth off on every region bounded at +/- 0.7666.
        assert_eq!(quantise(0.7666), 7666);
        assert_eq!(quantise(-0.7666), -7666);
        assert_eq!(
            (0.7666f64 * 10_000.0f64) as i64,
            7665,
            "which is the answer this must not give"
        );
    }

    #[test]
    fn a_parameter_is_read_as_a_point_or_as_a_range() {
        let path = Path::new("overworld.json");
        assert_eq!(
            span(&serde_json::json!(0.0), "depth", path).expect("a point"),
            (0, 0)
        );
        assert_eq!(
            span(&serde_json::json!([-1.2, -1.05]), "continentalness", path).expect("a range"),
            (-12_000, -10_500)
        );
        assert!(span(&serde_json::json!([0.5, -0.5]), "erosion", path).is_err());
        assert!(span(&serde_json::json!([0.5]), "erosion", path).is_err());
        assert!(span(&serde_json::json!("warm"), "temperature", path).is_err());
    }

    #[test]
    fn the_table_is_the_one_the_reader_parses_and_keeps_the_reports_order() {
        let regions = vec![
            BiomeRegion {
                id: 33,
                biome: "minecraft:mushroom_fields".to_owned(),
                axes: [(-10_000, 10_000); 6],
                offset: 0,
            },
            BiomeRegion {
                id: 1,
                biome: "minecraft:plains".to_owned(),
                axes: [(0, 1_000); 6],
                offset: 500,
            },
        ];
        let text = biome_table(&regions);
        let parsed = dust_gen::biome::BiomeParameters::parse(&text)
            .expect("the writer and the reader agree on the format");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.distinct_biomes(), 2);
        assert_eq!(parsed.regions()[0].name, "minecraft:mushroom_fields");
        assert_eq!(parsed.regions()[0].id, 33);
        assert_eq!(parsed.regions()[1].offset, 500);
        assert_eq!(parsed.regions()[1].axes[0].min, 0);
        assert_eq!(parsed.regions()[1].axes[5].max, 1_000);
    }

    #[test]
    fn below_top_counts_down_from_the_generators_ceiling_not_the_dimensions() {
        // The Nether is 256 blocks tall by its dimension_type and 128 by its
        // noise settings, and Minecraft resolves against the narrower of the
        // two. Reading the wrong one puts nether quartz at y=246, above the
        // bedrock roof, where the ore simply would not exist.
        let nether = nether();
        assert_eq!(nether.max_y(), 127);
        assert_eq!(anchor("below_top", 10).resolve(&nether), 117);
        assert_eq!(anchor("above_bottom", 10).resolve(&nether), 10);
        assert_eq!(anchor("absolute", 10).resolve(&nether), 10);
    }

    #[test]
    fn an_anchor_spelling_this_extractor_does_not_know_is_an_error() {
        let value = serde_json::json!({ "somewhere_else": 4 });
        let err =
            Anchor::read(&value, "min_inclusive", Path::new("f.json")).expect_err("must fail");
        assert!(
            err.contains("somewhere_else") && err.contains("f.json"),
            "{err}"
        );
    }

    fn steps(json: serde_json::Value) -> Vec<Value> {
        json.as_array().expect("array").clone()
    }

    #[test]
    fn no_count_and_no_rarity_filter_is_one_attempt_per_chunk() {
        // Vanilla's two ancient debris placements are written this way. Reading
        // absence as zero would delete both without a word.
        let steps = steps(serde_json::json!([
            { "type": "minecraft:in_square" },
            { "type": "minecraft:biome" }
        ]));
        assert_eq!(
            attempts(&steps, Path::new("f.json")).expect("reads"),
            Attempts::PerChunk(1)
        );
    }

    #[test]
    fn a_uniform_count_over_zero_and_one_is_a_one_in_two_rarity_filter() {
        // Exactly, not approximately: a uniform integer over {0, 1} and one
        // attempt with probability 1/2 are the same distribution.
        let steps = steps(serde_json::json!([{
            "type": "minecraft:count",
            "count": { "type": "minecraft:uniform", "min_inclusive": 0, "max_inclusive": 1 }
        }]));
        assert_eq!(
            attempts(&steps, Path::new("f.json")).expect("reads"),
            Attempts::RarityFilter { one_in: 2 }
        );
    }

    #[test]
    fn a_uniform_count_over_any_other_range_is_an_error_naming_the_file() {
        // 0..=3 has mean 1.5, which `RarityFilter` cannot express and
        // `PerChunk` would round away. Keeping the mean and dropping the spread
        // is the guess this refuses to make.
        let steps = steps(serde_json::json!([{
            "type": "minecraft:count",
            "count": { "type": "minecraft:uniform", "min_inclusive": 0, "max_inclusive": 3 }
        }]));
        let err = attempts(&steps, Path::new("ore_x.json")).expect_err("must fail");
        assert!(err.contains("ore_x.json") && err.contains("0..=3"), "{err}");
    }

    #[test]
    fn a_count_and_a_rarity_filter_together_are_an_error() {
        let steps = steps(serde_json::json!([
            { "type": "minecraft:count", "count": 4 },
            { "type": "minecraft:rarity_filter", "chance": 9 }
        ]));
        let err = attempts(&steps, Path::new("ore_x.json")).expect_err("must fail");
        assert!(err.contains("ore_x.json"), "{err}");
    }

    #[test]
    fn two_steps_of_the_same_type_are_an_error_rather_than_a_first_match() {
        let steps = steps(serde_json::json!([
            { "type": "minecraft:count", "count": 4 },
            { "type": "minecraft:count", "count": 9 }
        ]));
        let err = attempts(&steps, Path::new("ore_x.json")).expect_err("must fail");
        assert!(err.contains("more than one"), "{err}");
    }

    fn names(targets: &[&str]) -> Option<String> {
        dust_gen::ore_density::group_name(
            &targets.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn a_group_is_named_by_what_its_blocks_have_in_common() {
        assert_eq!(
            names(&["minecraft:coal_ore", "minecraft:deepslate_coal_ore"]).as_deref(),
            Some("coal")
        );
        assert_eq!(
            names(&["minecraft:nether_gold_ore"]).as_deref(),
            Some("nether_gold")
        );
        // No `_ore` to drop, and none is invented.
        assert_eq!(
            names(&["minecraft:ancient_debris"]).as_deref(),
            Some("ancient_debris")
        );
        // These share no suffix at all, so the common prefix carries it.
        assert_eq!(
            names(&["minecraft:infested_deepslate", "minecraft:infested_stone"]).as_deref(),
            Some("infested")
        );
        // A namespace that is not Minecraft's stays, so a datapack's ore cannot
        // collide with a vanilla knob.
        assert_eq!(
            names(&[
                "spelunkery:rock_salt_ore",
                "spelunkery:deepslate_rock_salt_ore"
            ])
            .as_deref(),
            Some("spelunkery:rock_salt")
        );
    }

    #[test]
    fn a_group_with_nothing_in_common_has_no_name() {
        // Reported by id at the call site, never dropped in silence.
        assert_eq!(names(&["minecraft:stone", "minecraft:diamond_ore"]), None);
        assert_eq!(names(&["a:copper_ore", "b:copper_ore"]), None);
    }

    #[test]
    fn dropping_ore_never_leaves_an_empty_name() {
        // A block literally called `minecraft:ore` keeps its name rather than
        // becoming the empty string, which would be a knob nobody can type.
        assert_eq!(names(&["minecraft:ore"]).as_deref(), Some("ore"));
    }

    fn raw(id: &str, targets: &[&str]) -> Raw {
        Raw {
            id: id.to_owned(),
            configured_feature: format!("{id}_feature"),
            attempts: Attempts::PerChunk(1),
            vein_size: 8,
            min: anchor("absolute", 0),
            max: anchor("absolute", 16),
            distribution: "minecraft:uniform".to_owned(),
            discard_chance: 0.0,
            targets: targets.iter().map(|t| (*t).to_owned()).collect(),
        }
    }

    #[test]
    fn placements_that_share_a_block_are_one_group_however_they_are_named() {
        // The point of the whole rule: nothing here says "diamond", and the
        // names deliberately do not look alike.
        let placements = [
            raw(
                "test:sparkly",
                &["minecraft:diamond_ore", "minecraft:deepslate_diamond_ore"],
            ),
            raw(
                "test:shiny_rocks",
                &["minecraft:deepslate_diamond_ore", "minecraft:diamond_ore"],
            ),
            raw("test:rusty", &["minecraft:iron_ore"]),
        ];
        let (groups, ungrouped) = group(&placements).expect("groups");
        assert!(ungrouped.is_empty());
        assert_eq!(groups.len(), 2);
        let diamond = groups
            .iter()
            .find(|g| g.name == "diamond")
            .expect("diamond");
        assert_eq!(diamond.placements, ["test:sparkly", "test:shiny_rocks"]);
        assert_eq!(
            groups
                .iter()
                .find(|g| g.name == "iron")
                .expect("iron")
                .placements
                .len(),
            1
        );
    }

    #[test]
    fn a_partial_overlap_merges_rather_than_splitting_a_placement_in_two() {
        // A placement cannot be half in one group: it has one knob. So sharing
        // is transitive, and the merge is the documented cost of the rule.
        let placements = [
            raw("test:a", &["minecraft:copper_ore"]),
            raw("test:b", &["minecraft:copper_ore", "minecraft:gold_ore"]),
            raw("test:c", &["minecraft:gold_ore"]),
        ];
        let (groups, _) = group(&placements).expect("groups");
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].placements.len(), 3);
    }

    #[test]
    fn a_placement_with_no_derivable_name_is_reported_and_not_dropped() {
        let placements = [
            raw(
                "test:nameless",
                &["minecraft:stone", "minecraft:diamond_ore"],
            ),
            raw("test:fine", &["minecraft:iron_ore"]),
        ];
        let (groups, ungrouped) = group(&placements).expect("groups");
        assert_eq!(ungrouped, ["test:nameless"]);
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn two_groups_that_derive_the_same_name_stop_the_extraction() {
        // Two groups that share no block at all, whose blocks nonetheless have
        // the same common part. One knob cannot mean two ores, and picking one
        // silently is the shape of bug that ships.
        let placements = [
            raw("test:a", &["minecraft:coal_ore"]),
            raw(
                "test:b",
                &["minecraft:deepslate_coal_ore", "minecraft:mossy_coal_ore"],
            ),
        ];
        let err = group(&placements).expect_err("must fail");
        assert!(err.contains("coal"), "{err}");
    }

    #[test]
    fn a_vanilla_group_name_the_data_does_not_produce_stops_the_extraction() {
        let groups = vec![Group {
            name: "diamonds".to_owned(),
            targets: vec!["minecraft:diamond_ore".to_owned()],
            placements: vec!["test:a".to_owned()],
        }];
        let err = check_vanilla_group_names(&groups).expect_err("must fail");
        assert!(err.contains("diamond"), "{err}");
    }

    // What these tests do not catch: nothing here reads a real Minecraft file.
    // They fix the rules — absence means one attempt, `below_top` counts down
    // from the generator's ceiling, sharing a block means sharing a knob — but
    // whether Mojang's tree means what this module reads it as meaning is
    // answered by the extraction refusing to emit what it cannot verify, and by
    // the source rows, which are copied by a second pass and checked against
    // the derived table in `dust-gen`.
}
