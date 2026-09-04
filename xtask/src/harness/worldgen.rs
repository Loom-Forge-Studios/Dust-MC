//! `harness worldgen` — how far is the world Dust generates from the world
//! Minecraft generates for the same seed?
//!
//! # What Dust generates today
//!
//! A superflat. Bedrock at the world's floor, three rows of dirt, one of grass
//! at y -60, air above, `minecraft:plains` everywhere, and every column of
//! every chunk identical — `dust_server::net::world::FlatWorld`, which says so
//! in its own module note. A world read off disk falls back to it column by
//! column, because a world is a disc in an infinite plane and a player can
//! walk off the edge of it.
//!
//! This verb does not fix that. It measures it, before a line of noise is
//! written, for the same reason `harness light` measured opacity before
//! anything was changed: the number decides the order of the work, and three
//! times on this project the number has disagreed with the intuition.
//!
//! # Counts, and five of them
//!
//! **A percentage hides which half it is about.** A world that is 96% air
//! agrees with any other world that is 96% air, and a single "blocks match"
//! figure would read as a score for the generator while being a fact about how
//! much sky is in view. So five things are scored, each of them something a
//! player standing in the world would notice, and each of them a count:
//!
//! * **surface height** — is the ground at the right y? Per column, from
//!   `MOTION_BLOCKING`, which is the map `spawn_at` already puts players on.
//! * **surface block** — is it the right block underfoot? Per column, asked at
//!   *Minecraft's* surface y, because "what is the player standing on" is a
//!   question about where the ground actually is.
//! * **biome** — per 4x4x4 biome cell, and beside it how many distinct biomes
//!   each side has in view at all.
//! * **caves** — of the cells Minecraft carved open below its own surface, how
//!   many are open in Dust. Kept apart from the reverse — cells Dust opened
//!   that Minecraft did not — for the reason `harness light` keeps over-lit
//!   cells apart from under-lit ones: a list they shared would let an
//!   unexplained hundred hide inside an explained ten thousand.
//! * **blocks** — every cell, state for state. The total the other four are
//!   slices of.
//!
//! # The ladder
//!
//! Seven models over the same chunks in one run, each row the one above it
//! plus a single named change, in the order vanilla's own pipeline runs:
//!
//! ```text
//!   0  the flat world Dust serves today
//!   1  + the world's own sea level
//!   2  + Dust's biome source                    the multi-noise climate
//!   3  + Minecraft's surface height             the density functions
//!   4  + Minecraft's carvers                    caves
//!   5  + Minecraft's blocks at and below it     surface rules, ores, trees
//!   6  + Minecraft's blocks above it            plants -- the control
//! ```
//!
//! Rows 3 to 6 read their answer out of the region file. **None of them is a
//! mode a server could run in** — that is the whole point of them, and it is
//! the same device `harness light`'s last rung uses. What each row *buys* is
//! what that stage of worldgen is worth, in cells, on this world.
//!
//! Row 2 used to be one of them and is not any more. It is Dust's own
//! multi-noise biome source now, so it is a mode a server *could* run in, and
//! the gap between it and row 3 — which hands Minecraft's biomes over — is
//! what the biome source still gets wrong. That is how a stage graduates: the
//! rung that stood in for it becomes the ceiling above it.
//!
//! **Row 6 is a control and has to be exact.** It hands every block and every
//! biome over, so anything short of 100% on all five scores is the scorer
//! lying rather than the generator failing. It is checked in CI against
//! synthetic chunks, in both directions: the control agrees, and a single
//! changed block makes it disagree.
//!
//! # Cost is a score here, more than anywhere else
//!
//! This code runs for every chunk a player walks toward, forever. A generator
//! that is exact and too slow to serve is not a generator, so every rung is
//! also timed and weighed: columns per second, and the bytes the built column
//! holds, split into block storage and light arrays. The flat world's answer
//! to both is unrepresentative *by construction* — it builds one column ever
//! and shares it — and that is itself the number worth having written down
//! before the template goes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use dust_gen::biome::BiomeParameters;
use dust_gen::terrain::{Columns, Generator, Material};
use dust_server::net::source::RegistryNames;
use dust_server::net::world::{self, FlatWorld, Palette};
use dust_world::anvil::{Ids as _, Names as _};
use dust_world::chunk::Chunk;
use dust_world::coords::ChunkPos;
use dust_world::heightmap::{HeightmapKind, WorldHeight};

use super::{cache, digest, region};

const USAGE: &str = "\
harness worldgen --version <v> [--seed <n>] [--radius <r>] [--at <x>,<z>]...

Reads a world Minecraft generated, builds the same chunks with Dust's own
generator, and counts how far apart they are: surface height, surface block,
biome, caves, and every block state. Needs a world captured first --
`cargo xtask harness capture --version <v> --seed <n> --radius <r>`.

A measurement and not a gate: exit 0 unless the run itself failed.

  --version <v>   Minecraft version, e.g. 1.21.1.
  --seed <n>      The provisioned world's seed. Default 0. Score more than
                  one: seed 0 spawns inland and seed 1 in open ocean, and
                  they disagree about nearly every number here.
  --at <x>,<z>    Centre a square on this chunk instead of 0,0. Repeatable,
                  and repeating it is how a biome source gets scored: a square
                  anywhere holds one climate, so several small squares far
                  apart reach biomes a wide square never would. Score the same
                  squares the capture generated.
  --radius <r>    Chunks either side of each centre. Default 4 (a 9x9).
";

/// Sea level in a 1.21.1 overworld: the y of the topmost water block.
///
/// Not a constant Dust may read out of Minecraft — it is a property of the
/// overworld's dimension settings, which live in a data pack the operator's
/// world carries. It is here as the *model's* sea level, one rung of a ladder,
/// and the rung above it stops needing it.
const SEA_LEVEL: i32 = 63;

#[derive(Debug)]
pub struct Options {
    pub version: String,
    pub seed: i64,
    pub radius: i32,
    pub centres: Vec<(i32, i32)>,
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut version = None;
    let mut seed = 0i64;
    let mut radius = 4i32;
    let mut centres: Vec<(i32, i32)> = Vec::new();
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--version" => {
                at = super::take_value(&mut seen, "--version", args, at + 1)?;
                version = Some(seen.last().expect("just stored").1.clone());
            }
            "--seed" => {
                at = super::take_value(&mut seen, "--seed", args, at + 1)?;
                seed = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--seed needs a signed 64-bit integer")?;
            }
            "--radius" => {
                at = super::take_value(&mut seen, "--radius", args, at + 1)?;
                radius = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--radius needs a whole number")?;
            }
            "--at" => {
                at = super::take_repeated_value(&mut seen, "--at", args, at + 1)?;
                let value = seen.last().expect("just stored").1.clone();
                let (x, z) = value
                    .split_once(',')
                    .ok_or("--at needs two chunk coordinates, as `x,z`")?;
                centres.push((
                    x.trim()
                        .parse()
                        .map_err(|_| "--at's x is not a whole number")?,
                    z.trim()
                        .parse()
                        .map_err(|_| "--at's z is not a whole number")?,
                ));
            }
            other => return Err(format!("unknown worldgen option `{other}`\n\n{USAGE}")),
        }
    }
    Ok(Options {
        version: version.ok_or_else(|| {
            format!("worldgen needs --version, e.g. `--version 1.21.1`\n\n{USAGE}")
        })?,
        seed,
        radius,
        centres: if centres.is_empty() {
            vec![(0, 0)]
        } else {
            centres
        },
    })
}

pub fn run(options: &Options) -> ExitCode {
    match measure(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("harness worldgen: {e}");
            ExitCode::from(2)
        }
    }
}

/// One rung of the ladder.
///
/// Ordered as vanilla's own pipeline runs, so the deltas read as what each
/// stage of worldgen is worth rather than as an arbitrary sequence. Cheapest
/// first would have put the sea-level constant beside the biome source and
/// said nothing about the order the work should be done in, which is what this
/// verb is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rung {
    /// `FlatWorld`'s own column, unchanged and called through the server's own
    /// code rather than restated here. **A differential whose reference
    /// restates the thing under test proves nothing**, and the inverse holds
    /// too: a model that claims to be the shipping generator has to *be* it.
    Flat,
    /// The same six lines with the grass at the world's sea level instead of
    /// four blocks off its floor. The fill has to go somewhere and dirt is
    /// what the flat world fills with.
    FlatAtSeaLevel,
    /// The biome of every cell, from **Dust's own** multi-noise biome source:
    /// six climate values sampled out of the operator's density functions and
    /// matched against their parameter list. Changes no block.
    Biomes,
    /// Dust's own terrain: `final_density` over the interpolation lattice, the
    /// dimension's default block where it is positive, its default fluid below
    /// the sea level the settings name, air elsewhere.
    ///
    /// **The last rung a server could run in**, and the first one that is a
    /// world rather than a plain. Surface rules have not run, so the ground is
    /// the default block and not grass; that is vanilla's own noise stage and
    /// decision record 0012 puts the rules after it.
    Density,
    /// Dust's terrain with the dimension's own surface rules painted over it:
    /// grass on dirt, sand on a beach, gravel on a shore, snow and ice where
    /// it is cold, and the deepslate the rules put under the stone.
    ///
    /// **The last rung a server could run in** until the one below it.
    /// Aquifers have not run, so every pocket below the sea level holds water
    /// where vanilla would leave most of it dry.
    Surface,
    /// The same terrain and the same rules, with the dimension's own aquifers
    /// deciding what an enclosed pocket holds before the rules are painted —
    /// which is where vanilla puts them.
    ///
    /// **The last rung a server could run in** until the one below it.
    /// Carvers have still not run, so a cave that is only a cave because
    /// something dug it is not there at all.
    Aquifers,
    /// The same world with the dimension's own carvers cut through it after the
    /// surface rules, which is where vanilla's chunk statuses put them: 289
    /// neighbouring chunks' worth of tunnels and canyons, of which this chunk
    /// keeps the part that lands inside it.
    ///
    /// Carvers have run but nothing has been placed in the world they cut, so
    /// there is no ore in it at all.
    Carve,
    /// The same world with its features placed: `minecraft:ore`, which is the
    /// coal and iron a player needs and the tuff, andesite, diorite and granite
    /// that are the four largest single blocks Minecraft has where Dust does
    /// not. Nine chunks' worth of origins, because a vein whose origin is next
    /// door still reaches thirteen blocks in.
    ///
    /// **The last rung a server could run in.** Everything below it reads the
    /// region file.
    Feature,
    /// The flat stack again, with each column's grass at the y Minecraft's
    /// `MOTION_BLOCKING` puts it. The terrain's *shape*, and nothing else:
    /// stone is still dirt, an ocean is still filled in solid.
    Heights,
    /// Cells Minecraft left open below its own surface are open here too.
    Carvers,
    /// Every cell at or below the surface takes Minecraft's own block —
    /// surface rules, aquifers and ores at once, carving included, since air
    /// is a material.
    ///
    /// **And trees.** `MOTION_BLOCKING` counts leaves, so a tree sits *below*
    /// this rung's surface and is handed over here rather than in the row
    /// beneath it. What is left above is what blocks nothing: grass, flowers.
    BelowTheSurface,
    /// Every cell takes Minecraft's own block. What is above the surface and
    /// blocks no motion: short grass, flowers.
    /// **The control**, and the reason it is last: it hands everything over,
    /// so anything short of exact is this file's fault and not the
    /// generator's.
    Everything,
}

impl Rung {
    /// The ladder, in order.
    const ALL: [Self; 12] = [
        Self::Flat,
        Self::FlatAtSeaLevel,
        Self::Biomes,
        Self::Density,
        Self::Surface,
        Self::Aquifers,
        Self::Carve,
        Self::Feature,
        Self::Heights,
        Self::Carvers,
        Self::BelowTheSurface,
        Self::Everything,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Flat => "the flat world Dust serves today",
            Self::FlatAtSeaLevel => "+ the world's own sea level",
            Self::Biomes => "+ Dust's biome source                    (the multi-noise climate)",
            Self::Density => "+ Dust's terrain                         (the density functions)",
            Self::Surface => "+ Dust's surface rules                   (the block underfoot)",
            Self::Aquifers => {
                "+ Dust's aquifers                        (whether a cave drowns you)"
            }
            Self::Carve => "+ Dust's carvers                         (the caves themselves)",
            Self::Feature => "+ Dust's features                        (ore, and the stone in it)",
            Self::Heights => "+ Minecraft's surface height             (the ceiling above it)",
            Self::Carvers => "+ Minecraft's carvers                    (caves)",
            Self::BelowTheSurface => {
                "+ Minecraft's blocks at and below it     (surface rules, ores, trees)"
            }
            Self::Everything => "+ Minecraft's blocks above it            (plants) <- the control",
        }
    }

    /// Whether this rung reads its answer out of the region file, and is
    /// therefore not a mode any server could run in.
    fn reads_the_region_file(self) -> bool {
        !matches!(
            self,
            Self::Flat
                | Self::FlatAtSeaLevel
                | Self::Biomes
                | Self::Density
                | Self::Surface
                | Self::Aquifers
                | Self::Carve
                | Self::Feature
        )
    }
}

/// The block states every model here is built from, resolved once.
///
/// The three air states are here because vanilla has three and Dust's own
/// heightmap fallback knows about one — the omission decision record 0010
/// named. A carver fills a cave with `minecraft:cave_air`, so a rung that
/// asked only about `minecraft:air` would say Minecraft carved nothing.
struct Blocks {
    palette: Palette,
    airs: [u32; 3],
}

impl Blocks {
    fn resolve() -> Result<Self, String> {
        let state = |name: &str| {
            dust_registry::Block::from_name(name)
                .map(|block| block.default_state().id())
                .ok_or_else(|| format!("the generated block table has no {name}"))
        };
        Ok(Self {
            palette: Palette::resolve().map_err(|e| e.to_string())?,
            airs: [
                state("minecraft:air")?,
                state("minecraft:cave_air")?,
                state("minecraft:void_air")?,
            ],
        })
    }

    fn is_air(&self, state: u32) -> bool {
        self.airs.contains(&state)
    }
}

/// What one rung found, over every chunk of the square.
///
/// Five scores and not one, for the reason the module note gives. Every field
/// is a count; the percentages are printed from them and are never stored,
/// so nothing here can be read without its denominator beside it.
#[derive(Default)]
struct Scores {
    columns: u64,
    surface_agree: u64,
    /// Dust's surface y minus Minecraft's, for the columns that disagree.
    /// Signed, because too high and too low are different defects: too high
    /// is terrain that should have been carved or never raised, too low is a
    /// hill that is not there.
    surface_off_by: BTreeMap<i32, u64>,
    surface_block_agree: u64,
    /// What Minecraft has underfoot where Dust has something else. The
    /// worklist a surface-rule engine would be written against.
    surface_wanted: BTreeMap<String, u64>,
    /// What Minecraft is *standing on* in the columns whose surface **height**
    /// disagrees.
    ///
    /// Not the same list as the one above, and the difference is which stage
    /// owns the gap. `MOTION_BLOCKING` counts leaves and ice, so a column
    /// whose ground is exactly right still reads five blocks short when a tree
    /// is on it — and a terrain generator that has no trees would be blamed
    /// for the forest. This names it instead.
    surface_short_on: BTreeMap<String, u64>,
    biome_cells: u64,
    biome_agree: u64,
    minecrafts_biomes: BTreeSet<String>,
    dusts_biomes: BTreeSet<String>,
    /// `Minecraft's biome -> Dust's` for every cell they disagree on.
    ///
    /// Both sides and not just the wanted one, because a biome source is wrong
    /// in *pairs*: "435,459 cells short" says nothing a reader can act on, and
    /// "swamp where Minecraft has mangrove_swamp, 149 cells" names one region
    /// of the parameter list and one boundary to go and look at.
    biome_confusions: BTreeMap<String, u64>,
    /// Cells Minecraft left open strictly below its own surface.
    carved: u64,
    /// ... of which Dust also leaves open.
    carved_open: u64,
    /// Cells below the surface Dust leaves open that Minecraft filled. Kept
    /// apart from the line above: no stage of this ladder is supposed to
    /// produce one, so a run with any is a run with something unexplained in
    /// it.
    opened_wrongly: u64,
    /// Cells below Minecraft's surface that Minecraft left open and Dust
    /// filled with the dimension's fluid — what an aquifer would be worth.
    flooded: u64,
    cells: u64,
    block_agree: u64,
    /// What Minecraft has where Dust is wrong, most common first.
    wanted: BTreeMap<String, u64>,
    /// Wall time spent building the columns, the reads excluded.
    built: Duration,
    /// Bytes the built columns' paletted containers hold — block states and
    /// biomes, palette and packed storage both.
    block_bytes: u64,
    /// Bytes their light arrays hold. Unconditional and identical for every
    /// rung, which is the point of printing it apart: it is 96 KiB of every
    /// column whatever the terrain does.
    light_bytes: u64,
}

/// Everything a rung builds from that is the same for every chunk.
///
/// One struct rather than eight parameters, because the list stopped being
/// readable and a caller that swapped two of them would still compile.
struct Model<'a> {
    flat: &'a FlatWorld,
    blocks: &'a Blocks,
    names: &'a RegistryNames,
    height: WorldHeight,
    constants: Option<&'a dust_registry::BlockConstants>,
    /// The dimension's own `default_block` and `default_fluid`, resolved from
    /// the settings file rather than named here. A pack that generates a
    /// basalt world generates one.
    solid: u32,
    fluid: u32,
    /// The surface rules' result blocks, resolved once.
    surface: &'a [u32],
    /// `minecraft:lava`, which only an aquifer writes.
    lava: u32,
}

/// Resolve a block the noise settings named, properties and all.
fn spec_state(spec: &dust_gen::noise::build::BlockSpec) -> Result<u32, String> {
    let block = dust_registry::Block::from_name(&spec.name)
        .ok_or_else(|| format!("the generated block table has no {}", spec.name))?;
    let mut state = block.default_state();
    for (property, value) in &spec.properties {
        state = state
            .with(property, value)
            .ok_or_else(|| format!("{} has no {property}={value}", spec.name))?;
    }
    Ok(state.id())
}

/// Build Dust's biome source for this world, out of the operator's own data.
///
/// Two files, both outside the repository and both Mojang's: the density
/// functions and noise parameters that `cargo xtask extract` unpacks from the
/// server jar, and the `dust-biomes.tsv` the same command writes. Refused
/// rather than defaulted when either is missing — a rung that quietly copied
/// Minecraft's biomes instead would report a perfect biome score for a
/// generator that had not run.
fn generator(
    version: &str,
    seed: i64,
    names: &RegistryNames,
    constants: Option<&dust_registry::BlockConstants>,
) -> Result<Generator, String> {
    let cache = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join(".dust-extract");
    let table = cache
        .join(format!("oracle-{version}"))
        .join(dust_gen::biome::FILE);
    let text = std::fs::read_to_string(&table).map_err(|e| {
        format!(
            "could not read {}: {e}\n\n`cargo xtask extract --version {version} --only worldgen` \
             writes it",
            table.display()
        )
    })?;
    let mut parameters =
        BiomeParameters::parse(&text).map_err(|e| format!("{}: {e}", table.display()))?;

    // The table carries the id the extraction saw beside the name, and the
    // name is what is checked here. They agree today because both came out of
    // the same jar; the day they do not, this says which biome moved instead
    // of putting a jungle in the tundra.
    let moved = parameters.rebind(|name| names.biome(name));
    if !moved.is_empty() {
        println!(
            "{} biome(s) in {} are not where this build's registry has them:",
            moved.len(),
            table.display()
        );
        for entry in &moved {
            match entry.now {
                Some(now) => println!("  {} moved from {} to {now}", entry.name, entry.was),
                None => println!(
                    "  {} is id {} in the table and is not in the registry at all",
                    entry.name, entry.was
                ),
            }
        }
    }

    let data = cache.join(format!("data-{version}/data"));
    println!(
        "biome source: {} regions over {} biomes from {}, density functions from {}",
        parameters.len(),
        parameters.distinct_biomes(),
        table.display(),
        data.display()
    );
    let mut generator =
        Generator::new(&data, "overworld", seed, parameters).map_err(|e| e.to_string())?;
    let unbound = generator.bind_surface_biomes(|name| names.biome(name));
    match generator.surface() {
        Some(rules) => println!(
            "surface rules: {} result block(s) over {} biome name(s) from {}",
            rules.palette().len(),
            rules.biome_names().len(),
            data.join("minecraft/worldgen/noise_settings/overworld.json")
                .display()
        ),
        None => {
            println!("surface rules: none in this dimension's settings, so the ground is stone")
        }
    }
    if !unbound.is_empty() {
        println!(
            "  {} biome name(s) the rules ask about are not in this registry: {}",
            unbound.len(),
            unbound.join(", ")
        );
    }
    let settings = generator.settings();
    println!(
        "terrain: {}x{} cells over y {}..{}, sea level {}, {} and {}",
        settings.cell_width,
        settings.cell_height,
        settings.min_y,
        settings.min_y + settings.height,
        settings.sea_level,
        settings.default_block.name,
        settings.default_fluid.name
    );
    match generator.carvers() {
        Some(carvers) => println!(
            "carvers: {} — {}, from {}",
            carvers.len(),
            carvers.names().collect::<Vec<_>>().join(", "),
            data.join("minecraft/worldgen/configured_carver").display()
        ),
        None => println!("carvers: none — no biome of this dimension names one"),
    }

    // The feature stage asks two things of the running build rather than
    // deciding them: which registry id each biome name has, and which of the
    // palette's blocks count towards `OCEAN_FLOOR_WG` — the heightmap
    // `OreFeature` consults before it draws a vein at all. The second comes out
    // of the operator's own `constants.tsv` column of the same name. Without
    // it, no feature runs, and this says so rather than guessing.
    let ocean_floor = constants.and_then(|table| table.flag("OCEAN_FLOOR_WG"));
    let unbound = generator.bind_features(
        |name| names.biome(name),
        |spec| {
            let table = constants?;
            let flag = ocean_floor?;
            let state = spec_state(spec).ok()?;
            Some(table.is_set(flag, state))
        },
    );
    match generator.features() {
        Some(features) => {
            let (running, read) = features.coverage();
            println!(
                "features: {running} of {read} placed feature(s) run, {} block(s) added to the \
                 palette, from {}",
                features.palette().len(),
                data.join("minecraft/worldgen/placed_feature").display()
            );
            if !features.ocean_floor_bound() {
                println!(
                    "  no feature runs: {} block(s) have no OCEAN_FLOOR_WG answer in this \
                     checkout ({}). `cargo xtask extract --version {version} --only constants`",
                    unbound.len(),
                    unbound.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
                );
            }
            let mut skipped: Vec<(&String, &usize)> = features.skipped().iter().collect();
            skipped.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let total: usize = skipped.iter().map(|(_, n)| **n).sum();
            if total > 0 {
                let head: Vec<String> = skipped
                    .iter()
                    .take(6)
                    .map(|(kind, n)| format!("{kind} {n}"))
                    .collect();
                println!(
                    "  {total} placed feature(s) this generator does not run, by kind: {}{}",
                    head.join(", "),
                    if skipped.len() > 6 {
                        format!(", and {} more kind(s)", skipped.len() - 6)
                    } else {
                        String::new()
                    }
                );
            }
        }
        None => println!("features: none — no biome of this dimension names one this generator runs"),
    }
    Ok(generator)
}

fn measure(options: &Options) -> Result<(), String> {
    let dirs = cache::Layout::resolve()?;
    let run_dir = dirs.server_dir(&options.version, options.seed);
    let region_dir = run_dir.join("world/region");
    if !region_dir.is_dir() {
        return Err(format!(
            "no world at {}; run `cargo xtask harness capture --version {} --seed {} \
             --radius {}` first",
            region_dir.display(),
            options.version,
            options.seed,
            options.radius
        ));
    }
    let names = RegistryNames::new()
        .ok_or_else(|| "the generated registry has no biome table".to_owned())?;
    let height = WorldHeight::OVERWORLD;
    let blocks = Blocks::resolve()?;
    let plains = names
        .biome("minecraft:plains")
        .ok_or_else(|| "the biome registry has no minecraft:plains".to_owned())?;
    // The shipping generator itself, built the way the server builds it. Rung
    // zero is this object's own output and not a copy of its rules.
    let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());

    let constants = constants_table(&options.version)?;
    match &constants {
        Some((path, table)) => println!(
            "block constants: {} — {} states",
            path.display(),
            table.len()
        ),
        None => println!(
            "no block constants in this checkout, so the surface is `not air` rather than \
             Minecraft's own MOTION_BLOCKING; `cargo xtask extract --version {} --only constants` \
             writes one",
            options.version
        ),
    }

    let expected = digest::expected_chunks_over(options.radius, &options.centres);

    // Only chunks vanilla finished. A chunk below `full` holds a partial
    // answer that looks like a complete one, which is why `digest::scan` and
    // `harness light` both refuse it.
    let mut vanilla = Vec::with_capacity(expected.len());
    for &(x, z) in &expected {
        match read_chunk(&region_dir, x, z, height, &names)? {
            Some(chunk) => vanilla.push(chunk),
            None => continue,
        }
    }
    let skipped = expected.len() - vanilla.len();
    if skipped > 0 {
        println!(
            "{skipped} of {} chunk(s) skipped: vanilla has not finished them",
            expected.len()
        );
    }
    if vanilla.is_empty() {
        return Err("every chunk was skipped; capture a world first".to_owned());
    }
    println!(
        "comparing {} chunk(s) of Minecraft {} seed {} against Dust's generator",
        vanilla.len(),
        options.version,
        options.seed
    );

    // Minecraft's surface, worked out once and shared by every rung: the
    // question is asked of the *same* world seven times and a per-rung
    // recompute would be seven chances for the models to be scored against
    // slightly different ground.
    let mut surfaces = Vec::with_capacity(vanilla.len());
    for chunk in &mut vanilla {
        chunk.recompute_heightmaps(world::heightmap_predicate(
            blocks.palette.air,
            constants.as_ref().map(|(_, table)| table),
        ));
        surfaces.push(surface_of(chunk));
    }

    let source = generator(
        &options.version,
        options.seed,
        &names,
        constants.as_ref().map(|(_, table)| table),
    )?;
    // One palette: the surface rules' blocks and then the features'. Resolving
    // only the first would map an ore's material code onto whichever surface
    // block sat at that index.
    let surface_states = source
        .block_palette()
        .iter()
        .map(spec_state)
        .collect::<Result<Vec<u32>, String>>()?;
    let model = Model {
        flat: &flat,
        blocks: &blocks,
        names: &names,
        height,
        constants: constants.as_ref().map(|(_, table)| table),
        solid: spec_state(&source.settings().default_block)?,
        fluid: spec_state(&source.settings().default_fluid)?,
        lava: spec_state(&dust_gen::aquifer::Aquifer::lava_block())?,
        surface: &surface_states,
    };

    let mut ladder = Vec::new();
    for rung in Rung::ALL {
        let mut scores = Scores::default();
        // One set of scratch per rung rather than per chunk: the `flat_cache`
        // nodes hold a column's continentalness across its 96 biome cells, the
        // material buffer is 96 KiB, and rebuilding either per chunk would
        // still be correct and would throw both away at every boundary. It is
        // scratch space, not state — the graph and every noise table are
        // shared and immutable.
        let mut sampler = source.columns();
        for (chunk, surface) in vanilla.iter().zip(&surfaces) {
            let started = Instant::now();
            let built = build(rung, chunk, surface, &model, &mut sampler);
            scores.built += started.elapsed();
            weigh(&built, &mut scores);
            score(
                &built,
                chunk,
                surface,
                &blocks,
                &names,
                height,
                model.fluid,
                &mut scores,
            );
        }
        report(rung, &scores);
        if matches!(
            rung,
            Rung::Surface | Rung::Aquifers | Rung::Carve | Rung::Feature
        ) {
            // Counted, not assumed. `minecraft:temperature` answers `false`
            // without looking and the badlands bands are the one table in the
            // rules that is not in the pack, so both are worth a number: a gap
            // nobody's world reaches is not a gap, and one that every column
            // reaches is the next piece of work.
            println!(
                "      {} of those {} missing cave cell(s) are pockets Dust flooded with {}, \
                 which is what an aquifer would be worth",
                scores.flooded,
                scores.carved - scores.carved_open,
                source.settings().default_fluid.name
            );
            if rung == Rung::Aquifers {
                println!(
                    "      aquifers: {}",
                    if source.has_aquifer() {
                        "on, from this dimension's own `aquifers_enabled`"
                    } else {
                        "off — this dimension's settings say it has none, so this rung is                          the one above it"
                    }
                );
            }
            if rung == Rung::Carve {
                match sampler.carving() {
                    Some(counts) => println!(
                        "      carvers: {} start(s) rolled out of {} neighbour chunk(s) drawn; \
                         {} cell(s) got as far as the block test and {} changed, {} of them held \
                         up by the aquifer's own barrier; {} carved column(s) stood on dirt \
                         under a grass block, which is the one clause of `carveBlock` this \
                         generator declines (decision record 0039)",
                        counts.starts,
                        counts.neighbours,
                        counts.reached,
                        counts.carved,
                        counts.barred,
                        counts.grass_floors,
                    ),
                    None => println!(
                        "      carvers: none — no biome of this dimension names one, so this \
                         rung is                          the one above it"
                    ),
                }
            }
            let (temperature, bandlands) = sampler.declined();
            println!(
                "      the rules asked `minecraft:temperature` {temperature} time(s), which this \
                 generator answers `false` without looking, and a badlands band decided \
                 {bandlands} block(s)"
            );
        }
        ladder.push((rung, scores));
    }
    summary(&ladder);
    Ok(())
}

/// Minecraft's `MOTION_BLOCKING` top for each of a chunk's 256 columns.
///
/// `None` where the column holds nothing the map counts — the top of a world
/// is a boundary and not a position, and a column with no ground has no
/// surface rather than a surface at the floor.
fn surface_of(chunk: &Chunk) -> [Option<i32>; 256] {
    let map = chunk.heightmaps().get(HeightmapKind::MotionBlocking);
    let mut out = [None; 256];
    for z in 0..16u32 {
        for x in 0..16u32 {
            out[(x + z * 16) as usize] = map.highest_taken(x, z);
        }
    }
    out
}

/// Build one chunk with one rung's model.
fn build(
    rung: Rung,
    vanilla: &Chunk,
    surface: &[Option<i32>; 256],
    model: &Model,
    generator: &mut Columns,
) -> Chunk {
    let Model {
        flat,
        blocks,
        names,
        height,
        constants,
        solid,
        fluid,
        lava,
        surface: surface_blocks,
    } = *model;
    // Rung zero is the server's own column, cloned rather than rebuilt: that
    // clone is also what `Source::column` hands out for a position a real
    // world does not contain, so it is the shipping answer and not a model of
    // one.
    if rung == Rung::Flat {
        let mut column = flat.column().clone();
        // **Asked the same question as the other side, and it caught the
        // harness the first time it ran.** The control rung reproduced every
        // block of the world and still reported 352 columns whose surface was
        // wrong, all of them too high, because the built chunk's heightmaps
        // were recomputed with `state != air` while Minecraft's were
        // recomputed with Minecraft's own `MOTION_BLOCKING` predicate. Short
        // grass and flowers are the difference — decision record 0010's
        // finding, in a second place, found the same way. A comparison that
        // asks the two sides different questions measures itself.
        //
        // It changes nothing for a flat world: bedrock, dirt and grass are
        // what all six of vanilla's predicates agree about. It is done anyway,
        // because a line that has to be remembered later is a line that will
        // not be.
        column.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, constants));
        return column;
    }

    let air = blocks.palette.air;
    let plains = flat.column().get_biome(0, height.min_y(), 0);
    let mut chunk = Chunk::uniform(
        vanilla.pos(),
        height,
        dust_registry::STATE_COUNT,
        names.biome_registry_size(),
        air,
        plains,
    );

    let top = height.min_y() + height.height() as i32;
    if matches!(
        rung,
        Rung::Density | Rung::Surface | Rung::Aquifers | Rung::Carve | Rung::Feature
    ) {
        // The world's own floor is bedrock on every rung of this ladder,
        // including the control, because vanilla's is: the bedrock gradient is
        // true at and below the bottom without a die being rolled. What is
        // above it here is the noise stage and nothing else.
        let materials = match rung {
            Rung::Feature => generator.features(vanilla.pos().x, vanilla.pos().z),
            Rung::Carve => generator.carve(vanilla.pos().x, vanilla.pos().z),
            Rung::Aquifers => generator.aquifer(vanilla.pos().x, vanilla.pos().z),
            Rung::Surface => generator.surface(vanilla.pos().x, vanilla.pos().z),
            _ => generator.terrain(vanilla.pos().x, vanilla.pos().z),
        };
        for z in 0..16u32 {
            for x in 0..16u32 {
                for y in height.min_y()..top {
                    let state = if y == height.min_y() {
                        blocks.palette.bedrock
                    } else {
                        let at = (y - height.min_y()) as usize * 256 + (z * 16 + x) as usize;
                        match Material::from_code(materials[at]) {
                            Material::Air => air,
                            Material::Solid => solid,
                            Material::Fluid => fluid,
                            Material::Lava => lava,
                            Material::Surface(index) => surface_blocks[index as usize],
                        }
                    };
                    if state != air {
                        chunk.set_block(x, y, z, state);
                    }
                }
            }
        }
    } else {
        for z in 0..16u32 {
            for x in 0..16u32 {
                let column = (x + z * 16) as usize;
                // Where this model puts the grass. The flat rungs put it at sea
                // level everywhere; the rest take it from Minecraft, and a column
                // Minecraft left empty gets no ground at all rather than a guess.
                let ground = match rung {
                    Rung::Flat
                    | Rung::Density
                    | Rung::Surface
                    | Rung::Aquifers
                    | Rung::Carve
                    | Rung::Feature => {
                        unreachable!("handled above")
                    }
                    Rung::FlatAtSeaLevel | Rung::Biomes => Some(SEA_LEVEL),
                    _ => surface[column],
                };
                for y in height.min_y()..top {
                    let state = if y == height.min_y() {
                        blocks.palette.bedrock
                    } else {
                        match rung {
                            Rung::Flat
                            | Rung::Density
                            | Rung::Surface
                            | Rung::Aquifers
                            | Rung::Carve
                            | Rung::Feature => {
                                unreachable!("handled above")
                            }
                            Rung::FlatAtSeaLevel | Rung::Biomes | Rung::Heights => {
                                stack(y, ground, blocks)
                            }
                            Rung::Carvers => {
                                let v = vanilla.get_block(x, y, z);
                                if ground.is_some_and(|g| y <= g) && blocks.is_air(v) {
                                    v
                                } else {
                                    stack(y, ground, blocks)
                                }
                            }
                            Rung::BelowTheSurface => {
                                if ground.is_some_and(|g| y <= g) {
                                    vanilla.get_block(x, y, z)
                                } else {
                                    air
                                }
                            }
                            Rung::Everything => vanilla.get_block(x, y, z),
                        }
                    };
                    if state != air {
                        chunk.set_block(x, y, z, state);
                    }
                }
            }
        }
    }

    // Biome cells are 4x4x4 and the loops below walk them at their own stride
    // rather than per block: sixty-four writes a section instead of four
    // thousand and ninety-six, for the same answer.
    if rung.reads_the_region_file() {
        for y in (height.min_y()..top).step_by(4) {
            for z in (0..16u32).step_by(4) {
                for x in (0..16u32).step_by(4) {
                    chunk.set_biome(x, y, z, vanilla.get_biome(x, y, z));
                }
            }
        }
    } else if matches!(
        rung,
        Rung::Biomes
            | Rung::Density
            | Rung::Surface
            | Rung::Aquifers
            | Rung::Carve
            | Rung::Feature
    ) {
        // Quart coordinates: the cell index, which is the block position
        // shifted down by two. The x and z of the chunk are added first,
        // because a climate is a fact about where in the world the column is
        // and every chunk would otherwise be given the climate at the origin.
        let base_x = vanilla.pos().x * 4;
        let base_z = vanilla.pos().z * 4;
        // **Column outermost, y innermost, and that is not a style choice.**
        // Four of the six climate functions are wrapped in a `flat_cache`,
        // which means they do not depend on y and the sampler holds them for
        // as long as the column does not move. Walking y on the outside moves
        // the column on every cell and throws that away.
        for z in (0..16u32).step_by(4) {
            for x in (0..16u32).step_by(4) {
                let quart_x = base_x + (x as i32 >> 2);
                let quart_z = base_z + (z as i32 >> 2);
                for y in (height.min_y()..top).step_by(4) {
                    if let Some(biome) = generator.biomes().biome(quart_x, y >> 2, quart_z) {
                        chunk.set_biome(x, y, z, biome);
                    }
                }
            }
        }
    }

    chunk.recompute_heightmaps(world::heightmap_predicate(air, constants));
    chunk
}

/// The flat stack at one y: bedrock is the caller's, dirt below the ground,
/// grass on it, air above.
fn stack(y: i32, ground: Option<i32>, blocks: &Blocks) -> u32 {
    match ground {
        Some(g) if y == g => blocks.palette.grass,
        Some(g) if y < g => blocks.palette.dirt,
        _ => blocks.palette.air,
    }
}

/// What a built column holds, in bytes.
///
/// The paletted containers are counted as they are stored — the palette's
/// entries plus the packed longs — because that is what a server holding a
/// view distance of columns actually pays. The light arrays are counted apart
/// and are the same number for every rung: `LightArray` is an unconditional
/// `Box<[u8; 2048]>` and a section has two, so every column carries 96 KiB of
/// light whether or not anything ever lights it.
fn weigh(chunk: &Chunk, scores: &mut Scores) {
    for section in chunk.sections() {
        for container in [section.states(), section.biomes()] {
            scores.block_bytes += container.palette().len() as u64 * 4;
            scores.block_bytes += container.storage().as_longs().len() as u64 * 8;
        }
        scores.light_bytes += 2 * dust_world::light::BYTES as u64;
    }
}

/// Score one built chunk against the one Minecraft wrote.
#[allow(clippy::too_many_arguments)]
fn score(
    dust: &Chunk,
    vanilla: &Chunk,
    surface: &[Option<i32>; 256],
    blocks: &Blocks,
    names: &RegistryNames,
    height: WorldHeight,
    model_fluid: u32,
    scores: &mut Scores,
) {
    let dust_surface = surface_of(dust);
    let top = height.min_y() + height.height() as i32;
    for z in 0..16u32 {
        for x in 0..16u32 {
            let column = (x + z * 16) as usize;
            scores.columns += 1;
            let want = surface[column];
            let got = dust_surface[column];
            if want == got {
                scores.surface_agree += 1;
            } else if let (Some(want), Some(got)) = (want, got) {
                *scores.surface_off_by.entry(got - want).or_default() += 1;
                *scores
                    .surface_short_on
                    .entry(block_name(vanilla.get_block(x, want, z)))
                    .or_default() += 1;
            }
            // Asked at Minecraft's y and not at Dust's. A column whose ground
            // is thirty blocks too low can still be grass on top of dirt, and
            // scoring it there would call that a match while the player is
            // standing in the air above it.
            if let Some(y) = want {
                let wanted = vanilla.get_block(x, y, z);
                if dust.get_block(x, y, z) == wanted {
                    scores.surface_block_agree += 1;
                } else {
                    *scores.surface_wanted.entry(block_name(wanted)).or_default() += 1;
                }
            } else {
                // No ground at all is a match when Dust has none either.
                if got.is_none() {
                    scores.surface_block_agree += 1;
                }
            }

            for y in height.min_y()..top {
                let wanted = vanilla.get_block(x, y, z);
                let built = dust.get_block(x, y, z);
                scores.cells += 1;
                if built == wanted {
                    scores.block_agree += 1;
                } else {
                    *scores.wanted.entry(block_name(wanted)).or_default() += 1;
                }
                let Some(ground) = want else { continue };
                if y >= ground {
                    continue;
                }
                match (blocks.is_air(wanted), blocks.is_air(built)) {
                    (true, true) => {
                        scores.carved += 1;
                        scores.carved_open += 1;
                    }
                    (true, false) => {
                        scores.carved += 1;
                        // ... and of those, the ones Dust filled with the
                        // dimension's own fluid rather than its rock. That is
                        // the aquifer's share of the cave score: vanilla runs
                        // its noise caves through an aquifer that leaves most
                        // of them dry, and a generator without one floods
                        // every pocket below the sea level. Kept apart because
                        // it is a *different* stage from carving and a single
                        // "caves missing" count cannot say which owns it.
                        if built == model_fluid {
                            scores.flooded += 1;
                        }
                    }
                    (false, true) => scores.opened_wrongly += 1,
                    (false, false) => {}
                }
            }
        }
    }

    for y in (height.min_y()..top).step_by(4) {
        for z in (0..16u32).step_by(4) {
            for x in (0..16u32).step_by(4) {
                let wanted = vanilla.get_biome(x, y, z);
                let built = dust.get_biome(x, y, z);
                scores.biome_cells += 1;
                if wanted == built {
                    scores.biome_agree += 1;
                } else {
                    let pair = format!(
                        "{} where Minecraft has {}",
                        biome_name(names, built),
                        biome_name(names, wanted)
                    );
                    *scores.biome_confusions.entry(pair).or_default() += 1;
                }
                if let Some(name) = names.biome_name(wanted) {
                    scores.minecrafts_biomes.insert(name.to_owned());
                }
                if let Some(name) = names.biome_name(built) {
                    scores.dusts_biomes.insert(name.to_owned());
                }
            }
        }
    }
}

/// A biome's name, or its number when the registry does not know it.
///
/// Never a bare number silently: an id with no name is a table and a registry
/// that have come apart, which is the thing `BiomeParameters::rebind` exists to
/// say out loud.
fn biome_name(names: &RegistryNames, id: u32) -> String {
    names
        .biome_name(id)
        .map_or_else(|| format!("biome #{id}"), str::to_owned)
}

fn block_name(state: u32) -> String {
    dust_registry::BlockState::from_id(state)
        .map(|s| s.block().name().to_owned())
        .unwrap_or_else(|| format!("state {state}"))
}

#[expect(clippy::cast_precision_loss, reason = "counts here are far below 2^53")]
fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        100.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

fn report(rung: Rung, scores: &Scores) {
    println!();
    println!("--- {} ---", rung.name());
    println!(
        "  surface height  {:>10} of {:>10} column(s)  ({:.3}%)",
        scores.surface_agree,
        scores.columns,
        percent(scores.surface_agree, scores.columns)
    );
    if !scores.surface_off_by.is_empty() {
        let mut rows: Vec<(&i32, &u64)> = scores.surface_off_by.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let shown: Vec<String> = rows
            .iter()
            .take(6)
            .map(|(delta, count)| format!("{delta:+} x {count}"))
            .collect();
        println!("      off by (Dust minus Minecraft): {}", shown.join(", "));
        histogram(
            "      and Minecraft's own surface there was:",
            &scores.surface_short_on,
        );
    }
    println!(
        "  surface block   {:>10} of {:>10} column(s)  ({:.3}%)",
        scores.surface_block_agree,
        scores.columns,
        percent(scores.surface_block_agree, scores.columns)
    );
    histogram("      Minecraft has underfoot:", &scores.surface_wanted);
    println!(
        "  biome           {:>10} of {:>10} cell(s)    ({:.3}%)  Minecraft has {} kind(s) here, \
         Dust {}",
        scores.biome_agree,
        scores.biome_cells,
        percent(scores.biome_agree, scores.biome_cells),
        scores.minecrafts_biomes.len(),
        scores.dusts_biomes.len()
    );
    histogram(
        "      Dust has, where they disagree:",
        &scores.biome_confusions,
    );
    println!(
        "  caves           {:>10} of {:>10} carved cell(s) open ({:.3}%); {} cell(s) Dust opened \
         that Minecraft filled",
        scores.carved_open,
        scores.carved,
        percent(scores.carved_open, scores.carved),
        scores.opened_wrongly
    );
    println!(
        "  blocks          {:>10} of {:>10} cell(s)    ({:.3}%)",
        scores.block_agree,
        scores.cells,
        percent(scores.block_agree, scores.cells)
    );
    histogram("      Minecraft has where Dust is wrong:", &scores.wanted);
}

fn histogram(label: &str, counts: &BTreeMap<String, u64>) {
    if counts.is_empty() {
        return;
    }
    println!("{label}");
    let mut rows: Vec<(&String, &u64)> = counts.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (name, count) in rows.iter().take(6) {
        println!("        {name:<34} {count}");
    }
    if rows.len() > 6 {
        println!("        ... and {} more kind(s)", rows.len() - 6);
    }
}

/// The whole ladder side by side, printed last because it is the conclusion.
///
/// **Shortfalls, in cells, and not one percentage.** The first run of this
/// verb printed rates here and the flat world scored **100% on caves** — a
/// world with no rock in it above y -60 contains every cave Minecraft carved,
/// and the rate said so without a word about the six million cells of stone it
/// had turned into sky. The count beside it, "false caves", is the one that
/// reads that row correctly. Every column here is a number of things wrong.
///
/// The cost columns are the reason this table is wider than `harness light`'s.
/// Worldgen runs for every chunk a player walks toward, forever, and a rung
/// that closes the gap at ten times the cost is a different answer from one
/// that closes it free.
fn summary(ladder: &[(Rung, Scores)]) {
    println!();
    println!("what each one leaves wrong, over the same chunks (counts, not rates):");
    println!();
    println!(
        "    surface    surface      biome      caves      false      blocks    cols/s  KiB/col  \
         model"
    );
    println!(
        "      short      block      short    missing      caves       short                     "
    );
    for (rung, scores) in ladder {
        #[expect(clippy::cast_precision_loss, reason = "counts here are far below 2^53")]
        let per_second = if scores.built.as_secs_f64() > 0.0 {
            scores.columns as f64 / 256.0 / scores.built.as_secs_f64()
        } else {
            f64::INFINITY
        };
        let columns = (scores.columns / 256).max(1);
        #[expect(clippy::cast_precision_loss, reason = "counts here are far below 2^53")]
        let kib = scores.block_bytes as f64 / columns as f64 / 1024.0;
        println!(
            "  {:>9} {:>10} {:>10} {:>10} {:>10} {:>11} {:>9.0} {:>8.1}  {}",
            scores.columns - scores.surface_agree,
            scores.columns - scores.surface_block_agree,
            scores.biome_cells - scores.biome_agree,
            scores.carved - scores.carved_open,
            scores.opened_wrongly,
            scores.cells - scores.block_agree,
            per_second,
            kib,
            rung.name()
        );
    }
    let (columns, cells, biomes, carved) = ladder
        .first()
        .map(|(_, s)| (s.columns, s.cells, s.biome_cells, s.carved))
        .unwrap_or_default();
    println!();
    println!(
        "  Out of {columns} column(s), {cells} cell(s), {biomes} biome cell(s) and {carved} \
         cell(s) Minecraft carved."
    );
    println!();
    println!("  Each row is the one above it plus a single named change, and every change");
    println!("  from the third row down hands Dust an answer it read out of the region");
    println!("  file -- none of those is a mode a server could run in.");
    println!();
    println!("  The last row is a control: it hands over every block and every biome, so");
    println!("  a non-zero anywhere in it is this verb's fault and not the generator's.");
    println!();
    println!("  \"false caves\" is cells below Minecraft's own surface that Dust leaves open");
    println!("  and Minecraft filled. It is what stops the caves column being read as a");
    println!("  score: a world of pure air has none missing and all of them false.");
    println!();
    println!("  cols/s is this verb's own building, the region reads excluded, on whatever");
    println!("  machine ran it -- read the ratio between rows and not the number. KiB/col");
    let light = ladder
        .first()
        .map(|(_, scores)| scores.light_bytes / (scores.columns / 256).max(1) / 1024)
        .unwrap_or(0);
    println!("  is block states and biomes as they are stored. Light is {light} KiB more per");
    println!("  column, the same for every row, and unconditional.");
}

/// The constants `cargo xtask extract --only constants` wrote for this version,
/// if this checkout has one. A developer route, exactly as in `harness light`.
fn constants_table(
    version: &str,
) -> Result<Option<(PathBuf, dust_registry::BlockConstants)>, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join(format!(".dust-extract/oracle-{version}/constants.tsv"));
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let table = dust_registry::BlockConstants::parse(&text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some((path, table)))
}

/// One chunk of the captured world, or `None` if vanilla has not finished it.
fn read_chunk(
    region_dir: &Path,
    x: i32,
    z: i32,
    height: WorldHeight,
    names: &RegistryNames,
) -> Result<Option<Chunk>, String> {
    let path = region::region_file_path(region_dir, x, z);
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(None);
    };
    let Some((compression, payload)) = region::read_chunk(&bytes, x, z)? else {
        return Ok(None);
    };
    let decompressed = region::decompress(compression, &payload)?;
    let named =
        dust_nbt::read::from_bytes(&decompressed).map_err(|e| format!("chunk {x},{z}: {e}"))?;
    let dust_nbt::Tag::Compound(root) = &named.tag else {
        return Err(format!("chunk {x},{z} is not a compound"));
    };
    let status = root.get("Status").and_then(|tag| match tag {
        dust_nbt::Tag::String(s) => Some(s.as_str()),
        _ => None,
    });
    if !matches!(status, Some("minecraft:full" | "full")) {
        return Ok(None);
    }
    let _ = ChunkPos::new(x, z);
    dust_world::anvil::chunk(root, height, names)
        .map(Some)
        .map_err(|e| format!("chunk {x},{z}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::harness::testing::scratch_dir;

    /// A stand-in world with one hill, one cave and one tree's worth of
    /// leaves, built here rather than read from a file: this harness tests
    /// against bytes it constructed, never against Mojang's.
    fn synthetic(blocks: &Blocks, height: WorldHeight, plains: u32, other: u32) -> Chunk {
        synthetic_at(ChunkPos::new(0, 0), blocks, height, plains, other)
    }

    /// The same stand-in, somewhere in particular.
    ///
    /// A chunk's position is an input to a biome source and to nothing else
    /// here, which is why it is a parameter rather than always the origin.
    fn synthetic_at(
        pos: ChunkPos,
        blocks: &Blocks,
        height: WorldHeight,
        plains: u32,
        other: u32,
    ) -> Chunk {
        let stone = dust_registry::Block::from_name("minecraft:stone")
            .expect("the block table has stone")
            .default_state()
            .id();
        let mut chunk = Chunk::uniform(
            pos,
            height,
            dust_registry::STATE_COUNT,
            64,
            blocks.palette.air,
            plains,
        );
        for z in 0..16u32 {
            for x in 0..16u32 {
                // A slope, so the surface is not one number everywhere and a
                // model that got the height right by accident cannot pass.
                let ground = 60 + (x as i32 % 5) + (z as i32 % 3);
                chunk.set_block(x, height.min_y(), z, blocks.palette.bedrock);
                for y in (height.min_y() + 1)..ground {
                    chunk.set_block(x, y, z, stone);
                }
                chunk.set_block(x, ground, z, blocks.palette.grass);
            }
        }
        // A cave: cave_air, which is the state a carver leaves and the one a
        // model that only knows `minecraft:air` would miss.
        for y in 20..24 {
            for x in 3..7u32 {
                chunk.set_block(x, y, 5, blocks.airs[1]);
            }
        }
        // One biome cell that is not plains, so a biome score of 100% has to
        // have been earned.
        chunk.set_biome(8, 0, 8, other);
        chunk.recompute_heightmaps(|_kind, state| state != blocks.palette.air);
        chunk
    }

    fn parts() -> (Blocks, WorldHeight, RegistryNames) {
        (
            Blocks::resolve().expect("the block table"),
            WorldHeight::OVERWORLD,
            RegistryNames::new().expect("the biome table"),
        )
    }

    /// A data pack written here, holding nothing of Mojang's.
    ///
    /// Six climate functions, of which two carry information: temperature is a
    /// gradient in y, so what it says is arithmetic a reader can check, and
    /// vegetation is a real Perlin noise in x and z, so the machinery that
    /// actually samples one is exercised rather than stubbed. The other four
    /// are constants.
    ///
    /// This is the same device the synthetic chunk above is: the harness is
    /// tested against bytes it wrote.
    fn scratch_pack(root: &Path) {
        scratch_pack_naming(root, "{}");
    }

    /// The same pack with every biome naming `carvers`, so a test can put a
    /// real tunnel through it. `{}` is no carvers at all, which is what the
    /// rest of these tests want: a tunnel would move the very cells they count.
    fn scratch_pack_naming(root: &Path, carvers: &str) {
        let write = |relative: &str, text: &str| {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        };
        write(
            "minecraft/worldgen/noise/scratch.json",
            r#"{"firstOctave": -7, "amplitudes": [1.0, 1.0]}"#,
        );
        write(
            "minecraft/worldgen/noise_settings/overworld.json",
            r#"{"noise": {"height": 384, "min_y": -64, "size_horizontal": 1, "size_vertical": 2},
               "sea_level": 63,
               "default_block": {"Name": "minecraft:stone"},
               "default_fluid": {"Name": "minecraft:water", "Properties": {"level": "0"}},
               "aquifers_enabled": false,
               "noise_router": {
                 "temperature": {
                   "type": "minecraft:y_clamped_gradient",
                   "from_y": -64, "to_y": 320, "from_value": -1.0, "to_value": 1.0
                 },
                 "vegetation": {
                   "type": "minecraft:flat_cache",
                   "argument": {
                     "type": "minecraft:shifted_noise",
                     "noise": "minecraft:scratch",
                     "shift_x": 0.0, "shift_y": 0.0, "shift_z": 0.0,
                     "xz_scale": 1.0, "y_scale": 0.0
                   }
                 },
                 "continents": 0.0,
                 "erosion": 0.0,
                 "depth": 0.0,
                 "ridges": 0.0,
                 "final_density": {
                   "type": "minecraft:interpolated",
                   "argument": {
                     "type": "minecraft:y_clamped_gradient",
                     "from_y": -64, "to_y": 144, "from_value": 1.0, "to_value": -1.0
                   }
                 }
               }}"#,
        );
        // Every biome any of these tables names, so that `Generator::new` can
        // ask each one which carvers it runs. They have to agree with one
        // another or the generator refuses; see decision record 0039.
        for biome in ["snowy_plains", "desert", "savanna", "jungle", "plains"] {
            write(
                &format!("minecraft/worldgen/biome/{biome}.json"),
                &format!(r#"{{"carvers": {carvers}}}"#),
            );
        }
        // The tag the scratch carver names, and the one block in it.
        write(
            "minecraft/tags/block/scratch_replaceables.json",
            r#"{"values": ["minecraft:stone"]}"#,
        );
        write(
            "minecraft/worldgen/configured_carver/scratch_cave.json",
            SCRATCH_CARVER,
        );
    }

    /// A cave carver that always starts, working between y 0 and y 30 — inside
    /// this pack's stone, and below its sea level, so what it leaves behind in
    /// a dimension with no aquifers is the global fluid picker's water.
    const SCRATCH_CARVER: &str = r##"{"type": "minecraft:cave", "config": {
        "probability": 1.0,
        "y": {"type": "minecraft:uniform",
              "min_inclusive": {"absolute": 0},
              "max_inclusive": {"absolute": 30}},
        "yScale": {"type": "minecraft:uniform", "min_inclusive": 0.1, "max_exclusive": 0.9},
        "lava_level": {"above_bottom": 8},
        "replaceable": "#minecraft:scratch_replaceables",
        "horizontal_radius_multiplier": {"type": "minecraft:uniform",
            "min_inclusive": 0.7, "max_exclusive": 1.4},
        "vertical_radius_multiplier": {"type": "minecraft:uniform",
            "min_inclusive": 0.8, "max_exclusive": 1.3},
        "floor_level": {"type": "minecraft:uniform",
            "min_inclusive": -1.0, "max_exclusive": -0.4}}}"##;

    /// Two biomes split on temperature alone, so which one a cell gets is a
    /// statement about its y that can be worked out by hand.
    fn scratch_table(cold: u32, warm: u32) -> String {
        let axes = "\t-10000\t10000".repeat(5);
        format!(
            "# biome_id\tbiome\ttemperature_min\ttemperature_max\thumidity_min\thumidity_max\
             \tcontinentalness_min\tcontinentalness_max\terosion_min\terosion_max\
             \tdepth_min\tdepth_max\tweirdness_min\tweirdness_max\toffset\n\
             {cold}\tminecraft:snowy_plains\t-10000\t0{axes}\t0\n\
             {warm}\tminecraft:desert\t0\t10000{axes}\t0\n"
        )
    }

    fn scratch_source(root: &Path, cold: u32, warm: u32) -> Generator {
        scratch_pack(root);
        let parameters =
            BiomeParameters::parse(&scratch_table(cold, warm)).expect("the table parses");
        assert_eq!(parameters.len(), 2);
        Generator::new(root, "overworld", 1234, parameters).expect("the pack compiles")
    }

    /// The y the scratch pack's density crosses zero at: solid below, and
    /// water from there to the sea level.
    const SCRATCH_GROUND: i32 = 40;

    fn model<'a>(
        flat: &'a FlatWorld,
        blocks: &'a Blocks,
        names: &'a RegistryNames,
        height: WorldHeight,
    ) -> Model<'a> {
        Model {
            flat,
            blocks,
            names,
            height,
            constants: None,
            // The scratch pack carries no `surface_rule`, so nothing here can
            // reach this table. An empty one panics rather than painting the
            // wrong block, which is the right way round.
            surface: &[],
            solid: dust_registry::Block::from_name("minecraft:stone")
                .expect("stone")
                .default_state()
                .id(),
            fluid: dust_registry::Block::from_name("minecraft:water")
                .expect("water")
                .default_state()
                .id(),
            lava: dust_registry::Block::from_name("minecraft:lava")
                .expect("lava")
                .default_state()
                .id(),
        }
    }

    /// The control rung reproduces the world exactly, on all five scores.
    ///
    /// Without this the ladder's last row could read 99.99% forever and nobody
    /// would know whether that was the generator or the scorer.
    #[test]
    fn the_control_rung_is_exact_on_every_score() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let other = names.biome("minecraft:desert").expect("desert");
        let mut vanilla = synthetic(&blocks, height, plains, other);
        vanilla.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));
        let surface = surface_of(&vanilla);
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());

        let scratch = scratch_dir("worldgen-control");
        let source = scratch_source(&scratch, plains, other);
        let built = build(
            Rung::Everything,
            &vanilla,
            &surface,
            &model(&flat, &blocks, &names, height),
            &mut source.columns(),
        );
        let mut scores = Scores::default();
        let water = dust_registry::Block::from_name("minecraft:water")
            .expect("water")
            .default_state()
            .id();
        score(
            &built,
            &vanilla,
            &surface,
            &blocks,
            &names,
            height,
            water,
            &mut scores,
        );
        assert_eq!(scores.block_agree, scores.cells, "blocks");
        assert_eq!(scores.surface_agree, scores.columns, "surface height");
        assert_eq!(scores.surface_block_agree, scores.columns, "surface block");
        assert_eq!(scores.biome_agree, scores.biome_cells, "biomes");
        assert_eq!(scores.carved_open, scores.carved, "caves");
        assert!(scores.carved > 0, "the stand-in has a cave to find");
        assert_eq!(scores.opened_wrongly, 0);
        assert_eq!(scores.minecrafts_biomes.len(), 2, "plains and desert");
    }

    /// ... and it is exact because the world is, not because the comparison
    /// cannot see.
    ///
    /// **The negative half of the control.** One block changed in the world
    /// the control was built from has to move every score it touches. Watching
    /// this fail is what says the assertions above are load-bearing.
    #[test]
    fn one_changed_block_moves_the_control_off_exact() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let other = names.biome("minecraft:desert").expect("desert");
        let mut vanilla = synthetic(&blocks, height, plains, other);
        vanilla.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));
        let surface = surface_of(&vanilla);
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());
        let scratch = scratch_dir("worldgen-one-block");
        let source = scratch_source(&scratch, plains, other);
        let mut built = build(
            Rung::Everything,
            &vanilla,
            &surface,
            &model(&flat, &blocks, &names, height),
            &mut source.columns(),
        );
        // Take the top block of one column away. The surface drops, the block
        // underfoot changes and one cell of the world differs.
        let y = surface[0].expect("the stand-in has ground everywhere");
        built.set_block(0, y, 0, blocks.palette.air);
        built.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));

        let mut scores = Scores::default();
        let water = dust_registry::Block::from_name("minecraft:water")
            .expect("water")
            .default_state()
            .id();
        score(
            &built,
            &vanilla,
            &surface,
            &blocks,
            &names,
            height,
            water,
            &mut scores,
        );
        assert_eq!(scores.block_agree, scores.cells - 1, "exactly one cell");
        assert_eq!(scores.surface_agree, scores.columns - 1);
        assert_eq!(scores.surface_block_agree, scores.columns - 1);
        assert_eq!(scores.surface_off_by.get(&-1).copied(), Some(1));
    }

    /// The carver rung finds a cave filled with `cave_air`.
    ///
    /// A model that asked only about `minecraft:air` would report the cave
    /// missing, which is decision record 0010's omission in a second place.
    #[test]
    fn a_cave_of_cave_air_counts_as_carved() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let other = names.biome("minecraft:desert").expect("desert");
        let mut vanilla = synthetic(&blocks, height, plains, other);
        vanilla.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));
        let surface = surface_of(&vanilla);
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());

        let scratch = scratch_dir("worldgen-cave");
        let source = scratch_source(&scratch, plains, other);
        for (rung, open) in [(Rung::Heights, false), (Rung::Carvers, true)] {
            let built = build(
                rung,
                &vanilla,
                &surface,
                &model(&flat, &blocks, &names, height),
                &mut source.columns(),
            );
            let mut scores = Scores::default();
            score(
                &built,
                &vanilla,
                &surface,
                &blocks,
                &names,
                height,
                dust_registry::Block::from_name("minecraft:water")
                    .expect("water")
                    .default_state()
                    .id(),
                &mut scores,
            );
            assert_eq!(scores.carved, 16, "4x4 cells of cave_air");
            assert_eq!(
                scores.carved_open > 0,
                open,
                "{rung:?} should {} find the cave",
                if open { "" } else { "not" }
            );
            // The height rung takes its shape from Minecraft, so the surface
            // is exact even where the material is not.
            assert_eq!(scores.surface_agree, scores.columns, "{rung:?} surface");
        }
    }

    /// The flat rung is the world the server serves, not a restatement of it.
    #[test]
    fn the_flat_rung_is_the_servers_own_column() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());
        let vanilla = synthetic(&blocks, height, plains, plains);
        let surface = surface_of(&vanilla);
        let scratch = scratch_dir("worldgen-flat");
        let source = scratch_source(&scratch, plains, plains);
        let built = build(
            Rung::Flat,
            &vanilla,
            &surface,
            &model(&flat, &blocks, &names, height),
            &mut source.columns(),
        );
        assert_eq!(&built, flat.column());
    }

    /// The terrain rung is built from the data pack's density and not from
    /// the region file.
    ///
    /// The scratch pack's `final_density` falls from +1 at y -64 to -1 at
    /// y 144, so it crosses zero at y 40, and it is wrapped in `interpolated`
    /// — which for a straight line is exact, so every assertion below is that
    /// arithmetic and not a recorded output. Above the crossing and below the
    /// sea level of 63 the answer is water; above both it is air.
    ///
    /// Watched to fail: the synthetic chunk this is scored beside is solid
    /// stone to y 100 with a cave in it, so a rung that copied the region file
    /// would disagree with every line here.
    /// **The carve rung is Dust's carvers and not another name for the rung
    /// above it.**
    ///
    /// The same chunk built twice from one pack, once at each rung. A wiring
    /// mistake in `build` — the carve arm calling `aquifer`, or falling into
    /// the region-file branch — would give a ladder that looks right and says
    /// the carvers are worth nothing.
    #[test]
    fn the_carve_rung_cuts_cells_the_rung_above_it_left_solid() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let cold = names.biome("minecraft:snowy_plains").expect("snowy plains");
        let warm = names.biome("minecraft:desert").expect("desert");
        let mut vanilla = synthetic(&blocks, height, plains, plains);
        vanilla.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));
        let surface = surface_of(&vanilla);
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());
        let scratch = scratch_dir("worldgen-carve-rung");
        scratch_pack_naming(&scratch, r#"{"air": ["minecraft:scratch_cave"]}"#);
        let parameters =
            BiomeParameters::parse(&scratch_table(cold, warm)).expect("the table parses");
        let source =
            Generator::new(&scratch, "overworld", 1234, parameters).expect("the pack compiles");
        assert_eq!(
            source.carvers().map(dust_gen::carver::Carvers::len),
            Some(1),
            "the fixture's biomes name one carver"
        );

        let model = model(&flat, &blocks, &names, height);
        let uncarved = build(
            Rung::Aquifers,
            &vanilla,
            &surface,
            &model,
            &mut source.columns(),
        );
        let carved = build(
            Rung::Carve,
            &vanilla,
            &surface,
            &model,
            &mut source.columns(),
        );

        // The fixture's carver works between y 0 and y 30, which is inside
        // this pack's stone and below its sea level. With no aquifers the
        // carve state is the global fluid picker's, which is water down there,
        // so what a cut cell becomes is water and never the other way about.
        let stone = dust_registry::Block::from_name("minecraft:stone")
            .expect("stone")
            .default_state()
            .id();
        let mut cut = 0;
        for y in height.min_y()..height.min_y() + height.height() as i32 {
            for z in 0..16u32 {
                for x in 0..16u32 {
                    let was = uncarved.get_block(x, y, z);
                    let now = carved.get_block(x, y, z);
                    if was == now {
                        continue;
                    }
                    assert_eq!(was, stone, "a carver only ever takes a block away");
                    assert_ne!(now, stone);
                    cut += 1;
                }
            }
        }
        assert!(cut > 0, "the carve rung cut nothing at all");
    }

    #[test]
    fn the_density_rung_builds_the_stack_the_data_pack_states() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let mut vanilla = synthetic(&blocks, height, plains, plains);
        vanilla.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));
        let surface = surface_of(&vanilla);
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());
        let scratch = scratch_dir("worldgen-density-rung");
        let source = scratch_source(&scratch, plains, plains);
        let built = build(
            Rung::Density,
            &vanilla,
            &surface,
            &model(&flat, &blocks, &names, height),
            &mut source.columns(),
        );

        let stone = dust_registry::Block::from_name("minecraft:stone")
            .expect("stone")
            .default_state()
            .id();
        let water = dust_registry::Block::from_name("minecraft:water")
            .expect("water")
            .default_state()
            .id();
        assert_eq!(
            built.get_block(3, height.min_y(), 9),
            blocks.palette.bedrock
        );
        assert_eq!(built.get_block(3, height.min_y() + 1, 9), stone);
        assert_eq!(built.get_block(3, SCRATCH_GROUND - 1, 9), stone);
        assert_eq!(
            built.get_block(3, SCRATCH_GROUND, 9),
            water,
            "the density is exactly zero at the crossing, which is not solid"
        );
        assert_eq!(built.get_block(3, SEA_LEVEL - 1, 9), water);
        assert_eq!(
            built.get_block(3, SEA_LEVEL, 9),
            blocks.palette.air,
            "the sea level names the surface the water reaches to"
        );
        // And it is not the world it was scored against: the synthetic chunk
        // is stone up to a sloping grass surface near y 62, where this is
        // water, and it has a cave of `cave_air` at y 20 where this is rock.
        assert_ne!(
            built.get_block(3, SEA_LEVEL - 2, 9),
            vanilla.get_block(3, SEA_LEVEL - 2, 9)
        );
        assert_eq!(built.get_block(3, 21, 5), stone, "the region file's cave");
        assert_eq!(vanilla.get_block(3, 21, 5), blocks.airs[1]);
    }

    /// The biome rung answers out of Dust's own climate, and the answer is the
    /// arithmetic the data pack states."""
    ///
    /// The scratch pack's temperature is a gradient from -1 at y -64 to +1 at
    /// y 320, so it crosses zero at y 128, and the two biomes are split there.
    /// Every assertion below is that arithmetic and not a recorded output.
    #[test]
    fn the_biome_rung_reads_the_climate_and_not_the_region_file() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let cold = names.biome("minecraft:snowy_plains").expect("snowy plains");
        let warm = names.biome("minecraft:desert").expect("desert");
        let mut vanilla = synthetic(&blocks, height, plains, plains);
        vanilla.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));
        let surface = surface_of(&vanilla);
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());
        let scratch = scratch_dir("worldgen-biome-rung");
        let source = scratch_source(&scratch, cold, warm);

        let built = build(
            Rung::Biomes,
            &vanilla,
            &surface,
            &model(&flat, &blocks, &names, height),
            &mut source.columns(),
        );

        // Below the crossing and above it, on the cell either side of it.
        assert_eq!(
            built.get_biome(0, -64, 0),
            cold,
            "the world's floor is cold"
        );
        assert_eq!(built.get_biome(0, 124, 0), cold, "the cell below y 128");
        assert_eq!(
            built.get_biome(0, 128, 0),
            cold,
            "at y 128 the gradient is exactly zero, which is inside both spans, and the \
             first row of the table wins the tie"
        );
        assert_eq!(built.get_biome(0, 132, 0), warm, "the cell above it");
        assert_eq!(built.get_biome(12, 316, 12), warm, "the world's ceiling");

        // **The negative half.** Vanilla's every cell here is plains. A rung
        // that had gone on copying the region file would agree with it
        // perfectly, and every assertion above would still be about a copy.
        assert_ne!(
            built.get_biome(0, -64, 0),
            vanilla.get_biome(0, -64, 0),
            "the rung must not be reading the world it is being scored against"
        );
    }

    /// A climate is a fact about where in the world a column is.
    ///
    /// Dropping the chunk's own x and z is the single likeliest mistake in the
    /// rung above — it compiles, it runs, and it gives every chunk in the world
    /// the climate at the origin, which no score short of a biome count would
    /// notice. So the two chunks are built from one source and compared.
    #[test]
    fn two_chunks_far_apart_are_given_different_climates() {
        let (blocks, height, names) = parts();
        let plains = names.biome("minecraft:plains").expect("plains");
        let dry = names.biome("minecraft:savanna").expect("savanna");
        let wet = names.biome("minecraft:jungle").expect("jungle");
        let flat = FlatWorld::new(blocks.palette, plains, names.biome_registry_size());
        let scratch = scratch_dir("worldgen-biome-position");
        scratch_pack(&scratch);
        // Split on vegetation, which the scratch pack samples as a real noise
        // in x and z, so the answer moves with the column.
        let axes = "\t-10000\t10000";
        let table = format!(
            "# biome_id\tbiome\ttemperature_min\ttemperature_max\thumidity_min\thumidity_max\
             \tcontinentalness_min\tcontinentalness_max\terosion_min\terosion_max\
             \tdepth_min\tdepth_max\tweirdness_min\tweirdness_max\toffset\n\
             {dry}\tminecraft:savanna{axes}\t-10000\t0{axes}{axes}{axes}{axes}\t0\n\
             {wet}\tminecraft:jungle{axes}\t0\t10000{axes}{axes}{axes}{axes}\t0\n"
        );
        let source = Generator::new(
            &scratch,
            "overworld",
            1234,
            BiomeParameters::parse(&table).expect("the table parses"),
        )
        .expect("the pack compiles");
        let mut sampler = source.columns();

        let here = ChunkPos::new(0, 0);
        let far = ChunkPos::new(1000, -1000);
        let built = [here, far].map(|pos| {
            let mut vanilla = synthetic_at(pos, &blocks, height, plains, plains);
            vanilla.recompute_heightmaps(world::heightmap_predicate(blocks.palette.air, None));
            let surface = surface_of(&vanilla);
            build(
                Rung::Biomes,
                &vanilla,
                &surface,
                &model(&flat, &blocks, &names, height),
                &mut sampler,
            )
        });

        let biomes = |chunk: &Chunk| -> Vec<u32> {
            let top = height.min_y() + height.height() as i32;
            (height.min_y()..top)
                .step_by(4)
                .flat_map(|y| {
                    (0..16u32)
                        .step_by(4)
                        .flat_map(move |z| (0..16u32).step_by(4).map(move |x| (x, y, z)))
                })
                .map(|(x, y, z)| chunk.get_biome(x, y, z))
                .collect()
        };
        let near = biomes(&built[0]);
        let distant = biomes(&built[1]);
        assert_eq!(near.len(), 1536, "4 x 4 x 96 cells");
        assert_ne!(
            near, distant,
            "16,000 blocks apart and the same climate means the chunk's own position \
             was thrown away"
        );
    }

    #[test]
    fn the_ladder_changes_one_thing_per_rung_and_ends_at_the_control() {
        assert_eq!(Rung::ALL.len(), 12);
        assert_eq!(Rung::ALL[0], Rung::Flat);
        assert_eq!(*Rung::ALL.last().expect("eleven"), Rung::Everything);
        assert!(!Rung::Flat.reads_the_region_file());
        assert!(!Rung::FlatAtSeaLevel.reads_the_region_file());
        assert!(
            !Rung::Biomes.reads_the_region_file(),
            "the biome rung answers for itself now, so it is a mode a server could run in"
        );
        assert!(
            !Rung::Density.reads_the_region_file(),
            "the terrain rung answers for itself too"
        );
        assert!(
            !Rung::Surface.reads_the_region_file(),
            "the surface rung reads the operator's own data pack"
        );
        assert!(
            !Rung::Aquifers.reads_the_region_file(),
            "the aquifer rung is the operator's own pack and this crate's own \
             arithmetic"
        );
        assert!(
            !Rung::Carve.reads_the_region_file(),
            "the carve rung is the operator's own pack and this crate's own \
             arithmetic, and is the last rung a server could run in"
        );
        assert!(Rung::Heights.reads_the_region_file());
    }

    #[test]
    fn parsing_takes_the_documented_spelling_and_refuses_the_rest() {
        let args: Vec<String> = ["--version", "1.21.1", "--seed", "1", "--radius", "3"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let options = parse(&args).expect("parses");
        assert_eq!(options.seed, 1);
        assert_eq!(options.radius, 3);
        assert_eq!(options.centres, vec![(0, 0)]);
        assert!(parse(&[]).is_err(), "no --version");
        assert!(parse(&["--nope".to_owned()]).is_err());
    }
}
