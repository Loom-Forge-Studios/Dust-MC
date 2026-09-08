//! A column built from noise, which is what a player walks on when there is
//! no world file to read.
//!
//! [`super::world::FlatWorld`] served every column of every world until this
//! existed, and its own note said what it was: bedrock, three rows of dirt and
//! grass at y -60, everywhere, forever. Decision record 0012 measured what
//! that costs a player — 20,736 of 20,736 columns at the wrong height on both
//! seeds it looked at — and put the terrain second in the order of the work.
//! This is that terrain, wired to the socket.
//!
//! # What a player gets, and what they do not
//!
//! `dust_gen::terrain` is vanilla's **noise stage**, `dust_gen::aquifer`
//! decides what a pocket under it holds, `dust_gen::surface` paints the
//! dimension's own surface rules over both, and `dust_gen::carver` cuts the
//! caves and canyons through the result — vanilla's own four stages in
//! vanilla's own order. So a player lands on grass over dirt, sand on a beach,
//! gravel on a shore and deepslate below; the mountains, valleys, overhangs,
//! coastlines, oceans and sea floors are where Minecraft puts them, with the
//! biome Minecraft would have put there; and a cave under the sea is somewhere
//! to walk rather than somewhere to drown. Decision records 0032, 0035 and
//! 0039 are what each of those is worth.
//!
//! `dust_gen::feature` then places what the biomes in view name, which today is
//! `minecraft:ore` and nothing else — so a player who digs finds coal, iron,
//! copper, gold, redstone, lapis, diamond and emerald where Minecraft put them,
//! in among the tuff, andesite, diorite and granite the same feature type
//! places. `[worldgen.ores]` is applied to those placements at boot; with the
//! defaults it is applied by not running, which is what decision record 0006
//! asks for and what lets vanilla parity be tested against a server that has
//! the setting compiled in.
//!
//! What is still missing is **trees, plants and structures** — no oaks, no
//! grass, no mineshafts, no villages. Record 0043 prices what is left.
//!
//! # Why the light needs the four columns around it
//!
//! Sky light does not stop at a chunk boundary. A flat world could be lit with
//! its own floors on all four sides because every column of it is the same
//! column; a real one cannot, and a cliff at x 16 lit as though the next chunk
//! were the same shape is a seam a player sees. So a column's neighbours are
//! generated for their sky floors — terrain only, no biomes and no light — and
//! remembered, exactly as [`super::source::AnvilWorld`] remembers the floors it
//! reads. Each position's floor is then computed once: a view distance of
//! eight is 289 columns and 72 more around its edge, not 289 times four.
//!
//! **The cache has exactly one writer**, [`GeneratedWorld::sky_floor`], and
//! that is what makes a generated world a function of its seed rather than of
//! the order its columns were built in. Decision record 0036 has the 15
//! columns in 900 that said so.

use std::collections::HashMap;
use std::sync::Mutex;

use dust_world::chunk::Chunk;
use dust_world::column_light::{Skirt, SkyFloor};
use dust_world::coords::ChunkPos;
use dust_world::heightmap::WorldHeight;
use dust_world::propagation::{EmissionModel, OpacityModel};

use dust_gen::terrain::{Generator, Material};

use super::world::{FlatWorld, Palette};

/// Sky floors kept for columns that have been generated, capped and cleared
/// wholesale for the reason [`super::source`]'s own cache is.
const SKY_FLOOR_CACHE_CAP: usize = 4096;

/// What the noise stage cannot build without.
#[derive(Debug)]
pub struct MissingBlock {
    pub name: String,
}

impl std::fmt::Display for MissingBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the dimension's settings name {} and the block registry has no such state",
            self.name
        )
    }
}

impl std::error::Error for MissingBlock {}

/// A world generated from noise.
#[derive(Debug)]
pub struct GeneratedWorld {
    generator: Generator,
    /// The plain underneath, kept for the two things every source is asked
    /// for — the block palette and the world height — and for nothing else.
    /// It is a template column's worth of bookkeeping and no position is ever
    /// served from it.
    flat: FlatWorld,
    palette: Palette,
    opacity: OpacityModel,
    emission: EmissionModel,
    height: WorldHeight,
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    /// The dimension's own two blocks, resolved once at boot.
    solid: u32,
    fluid: u32,
    /// `minecraft:lava`, which an aquifer writes and no data pack names.
    lava: u32,
    /// The surface rules' own result blocks, resolved once at boot rather than
    /// per block. A generated column asks this table about ninety thousand
    /// times; a name lookup there would be the whole cost of the stage.
    surface: Vec<u32>,
    /// What a cell gets when the biome source has no answer for it, which is
    /// the same biome the flat world serves.
    default_biome: u32,
    biome_registry_size: u32,
    floors: Mutex<HashMap<(i32, i32), SkyFloor>>,
    /// `OCEAN_FLOOR_WG` per generated chunk, which is what the feature stage
    /// reads before it draws a vein.
    ///
    /// Kept here rather than in the generator's own scratch because of the
    /// *order* a join builds columns in. A vein whose origin is two chunks away
    /// still reaches in, so every column served asks about the 5x5 window
    /// around it, and the only way to answer for a chunk is to build its
    /// terrain. The scratch's own cache is five rows deep and direct-mapped:
    /// right for a scan, useless for the nearest-first spiral a join streams
    /// in, which made every column pay twenty-five terrain fills instead of
    /// one — 128 ms a column, 37 s for a 289-column join. A shared map keyed by
    /// position turns that back into one fill per chunk of the region, at
    /// 512 bytes each.
    heights: Mutex<HashMap<(i32, i32), [i16; 256]>>,
}

impl GeneratedWorld {
    pub fn new(
        generator: Generator,
        flat: FlatWorld,
        opacity: OpacityModel,
        default_biome: u32,
        biome_registry_size: u32,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Result<Self, MissingBlock> {
        let settings = generator.settings();
        let solid = state_of(&settings.default_block)?;
        let fluid = state_of(&settings.default_fluid)?;
        // Resolved at boot beside the dimension's own two, and refused by name
        // if this build's registry has no lava — a generator that quietly
        // defaulted it would fill a deep cave with air and look right.
        let lava = state_of(&dust_gen::aquifer::Aquifer::lava_block())?;
        // One palette: the surface rules' blocks and then the ones the feature
        // stage writes. Two lists would map an ore's material code onto
        // whichever surface block happened to sit at that index.
        let surface = generator
            .block_palette()
            .iter()
            .map(state_of)
            .collect::<Result<Vec<u32>, MissingBlock>>()?;
        Ok(Self {
            emission: super::world::emission_of(constants.as_deref()),
            height: flat.height(),
            palette: flat.palette(),
            generator,
            flat,
            opacity,
            constants,
            solid,
            fluid,
            lava,
            surface,
            default_biome,
            biome_registry_size,
            floors: Mutex::new(HashMap::new()),
            heights: Mutex::new(HashMap::new()),
        })
    }

    pub fn palette(&self) -> Palette {
        self.palette
    }

    pub fn flat(&self) -> &FlatWorld {
        &self.flat
    }

    pub fn height(&self) -> WorldHeight {
        self.height
    }

    pub fn settings(&self) -> &dust_gen::noise::build::NoiseSettings {
        self.generator.settings()
    }

    /// One column, blocks, biomes, heightmaps and light.
    ///
    /// **This column's own floors are deliberately not put in the cache.** They
    /// were, for one line and one stated reason — "so a scan pays for each
    /// position once" — and that line made a generated world depend on the
    /// order it was built in. The floor of a *carved* column is not the floor
    /// of the same column's terrain, so whether the cache held one or the other
    /// depended on whether a neighbour had been built as a column yet, and the
    /// answer to that is a race as soon as more than one thread builds. Two
    /// worlds on seed 1, one reaching each column cold and one reaching it with
    /// its four neighbours already built, **disagreed on 15 of 900 columns**.
    /// See [`Self::sky_floor`], which is now the only writer, and decision
    /// record 0036.
    pub fn column(&self, pos: ChunkPos) -> Chunk {
        let mut chunk = self.build(pos, true);
        let skirt = Skirt {
            west: self.sky_floor(ChunkPos::new(pos.x - 1, pos.z)),
            east: self.sky_floor(ChunkPos::new(pos.x + 1, pos.z)),
            north: self.sky_floor(ChunkPos::new(pos.x, pos.z - 1)),
            south: self.sky_floor(ChunkPos::new(pos.x, pos.z + 1)),
        };
        let _ = super::world::light_column(&mut chunk, &self.opacity, &self.emission, skirt);
        chunk
    }

    /// Blocks, and the surface and biomes only when the caller is going to
    /// serve them.
    ///
    /// A neighbour is generated for one thing — where its sky reaches — and
    /// its biomes are 2.4 ms of climate nobody would read. **The surface rules
    /// are skipped with them, and for a reason rather than to save the time**:
    /// a rule replaces the block at a y, it does not move it, so a column's
    /// sky floor is the same before and after. The exception is the handful of
    /// rules that write air into a hole in a frozen ocean floor, which is one
    /// block of sky reach on a world of them.
    ///
    /// **The carvers go with them, and that one is an approximation rather than
    /// a free lunch.** A ravine that breaks a neighbour's surface lowers where
    /// its sky reaches by tens of blocks, and this skirt does not see it, so
    /// light crossing that seam is the light of an uncarved neighbour. It is
    /// the same approximation on every column rather than on whichever ones a
    /// build order happened to reach first, which is what makes the world a
    /// function of its seed again. Costed against the accurate alternative in
    /// one interleaved run: handing the skirt a fully carved neighbour is
    /// **1.5x to 2.1x** a join's whole generation, and being deterministic at
    /// all costs between **-6% and +8%**. Decision record 0036 has the table,
    /// declines the carved skirt here, and hands the accuracy question to the
    /// light oracle, which has never been pointed at generated terrain.
    fn build(&self, pos: ChunkPos, with_biomes: bool) -> Chunk {
        let mut chunk = Chunk::uniform(
            pos,
            self.height,
            dust_registry::STATE_COUNT,
            self.biome_registry_size,
            self.palette.air,
            self.default_biome,
        );
        let mut columns = self.generator.columns();
        let min_y = self.height.min_y();
        let top = min_y + self.height.height() as i32;
        {
            let materials = if with_biomes {
                let window = self.window_heights(&mut columns, pos);
                columns.features_over(pos.x, pos.z, &window)
            } else {
                columns.terrain(pos.x, pos.z)
            };
            for y in min_y..top {
                let row = (y - min_y) as usize * 256;
                for z in 0..16u32 {
                    for x in 0..16u32 {
                        // The world's own floor is bedrock. Vanilla writes its
                        // bottom five rows with a die, which is a surface rule
                        // and is not here; one row is not that rule, it is the
                        // floor, and without it a player digs into the void.
                        let state = if y == min_y {
                            self.palette.bedrock
                        } else {
                            match Material::from_code(materials[row + (z * 16 + x) as usize]) {
                                Material::Air => self.palette.air,
                                Material::Solid => self.solid,
                                Material::Fluid => self.fluid,
                                Material::Lava => self.lava,
                                Material::Surface(index) => self.surface[index as usize],
                            }
                        };
                        if state != self.palette.air {
                            chunk.set_block(x, y, z, state);
                        }
                    }
                }
            }
        }
        if with_biomes {
            // Column outermost and y innermost: four of the six climate
            // functions do not depend on y and the sampler holds them for as
            // long as the column does not move.
            let base_x = pos.x * 4;
            let base_z = pos.z * 4;
            for z in (0..16u32).step_by(4) {
                for x in (0..16u32).step_by(4) {
                    let quart_x = base_x + (x as i32 >> 2);
                    let quart_z = base_z + (z as i32 >> 2);
                    for y in (min_y..top).step_by(4) {
                        if let Some(biome) = columns.biomes().biome(quart_x, y >> 2, quart_z) {
                            chunk.set_biome(x, y, z, biome);
                        }
                    }
                }
            }
        }
        chunk.recompute_heightmaps(super::world::heightmap_predicate(
            self.palette.air,
            self.constants.as_deref(),
        ));
        chunk
    }

    /// `OCEAN_FLOOR_WG` over the window the feature stage reads, out of the
    /// shared cache, building only the chunks nothing has built yet.
    fn window_heights(
        &self,
        columns: &mut dust_gen::terrain::Columns<'_>,
        pos: ChunkPos,
    ) -> Vec<i16> {
        let radius = dust_gen::feature::WINDOW_RADIUS;
        let width = dust_gen::feature::WINDOW;
        let mut window = vec![0i16; width * width];
        for offset_z in -radius..=radius {
            for offset_x in -radius..=radius {
                let (near_x, near_z) = (pos.x + offset_x, pos.z + offset_z);
                let heights = self.chunk_heights(columns, near_x, near_z);
                let base_x = ((offset_x + radius) * 16) as usize;
                let base_z = ((offset_z + radius) * 16) as usize;
                for local_z in 0..16usize {
                    let row = (base_z + local_z) * width + base_x;
                    window[row..row + 16]
                        .copy_from_slice(&heights[local_z * 16..local_z * 16 + 16]);
                }
            }
        }
        window
    }

    fn chunk_heights(
        &self,
        columns: &mut dust_gen::terrain::Columns<'_>,
        chunk_x: i32,
        chunk_z: i32,
    ) -> [i16; 256] {
        if let Some(held) = self
            .heights
            .lock()
            .expect("the height map is never poisoned")
            .get(&(chunk_x, chunk_z))
        {
            return *held;
        }
        let heights = columns.ocean_floor_heights(chunk_x, chunk_z);
        let mut cache = self
            .heights
            .lock()
            .expect("the height map is never poisoned");
        if cache.len() >= SKY_FLOOR_CACHE_CAP {
            cache.clear();
        }
        cache.insert((chunk_x, chunk_z), heights);
        heights
    }

    /// Where the sky reaches in a *neighbouring* column, remembered.
    ///
    /// **The only writer of the cache, and that is the invariant rather than a
    /// tidiness.** A memo may hold one function's answers; this one held two —
    /// this terrain-only floor and, from [`Self::column`], the floor of the
    /// same column after carving — and which of them a reader got was decided
    /// by whichever thread arrived first. One writer means the key has one
    /// meaning, and a builder pool cannot change what a seed generates.
    fn sky_floor(&self, pos: ChunkPos) -> SkyFloor {
        if let Some(held) = self
            .floors
            .lock()
            .expect("the floor map is never poisoned")
            .get(&(pos.x, pos.z))
        {
            return *held;
        }
        let floors = SkyFloor::of(&self.build(pos, false));
        let mut cache = self.floors.lock().expect("the floor map is never poisoned");
        if cache.len() >= SKY_FLOOR_CACHE_CAP {
            cache.clear();
        }
        cache.insert((pos.x, pos.z), floors);
        floors
    }
}

/// Resolve a block a noise-settings file named, properties and all.
fn state_of(spec: &dust_gen::noise::build::BlockSpec) -> Result<u32, MissingBlock> {
    let missing = || MissingBlock {
        name: spec.name.clone(),
    };
    let block = dust_registry::Block::from_name(&spec.name).ok_or_else(missing)?;
    let mut state = block.default_state();
    for (property, value) in &spec.properties {
        state = state.with(property, value).ok_or_else(missing)?;
    }
    Ok(state.id())
}

/// Build a generated world out of whatever is under `[data] path`, or say why
/// there is none.
///
/// `Ok(None)` when the operator has not extracted the biome parameter list —
/// which is the ordinary case for a server that has only ever run flat, and is
/// not an error. `Err` when the files are there and do not answer: a data
/// directory that is half a world is a mistake an operator should be told
/// about at boot rather than by walking into it.
///
/// Nothing here is Mojang's. The density functions, the noise parameters and
/// the sea level come from the operator's own unpacked data pack, and the
/// biome parameter list from the table `cargo xtask extract --only worldgen`
/// writes out of their own server jar. Decision records 0006, 0007 and 0008.
#[allow(clippy::too_many_arguments)]
pub fn beside(
    data_path: &std::path::Path,
    seed: i64,
    flat: FlatWorld,
    opacity: OpacityModel,
    default_biome: u32,
    biome_registry_size: u32,
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ores: &dust_config::ore::OresConfig,
) -> Result<Option<(GeneratedWorld, Report)>, String> {
    let table = data_path.join(dust_gen::biome::FILE);
    let text = match std::fs::read_to_string(&table) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} could not be read: {e}", table.display())),
    };
    let mut parameters = dust_gen::biome::BiomeParameters::parse(&text)
        .map_err(|e| format!("{}: {e}", table.display()))?;

    // The table carries the id its extraction saw beside the biome's name, and
    // the name is what is checked here against the registry this server will
    // actually send. A version that renumbered a biome is then caught on the
    // row it renumbered rather than by a player standing in the wrong forest.
    use dust_world::anvil::Names as _;
    let names = super::source::RegistryNames::new()
        .ok_or_else(|| "the synced registries have no biome registry".to_owned())?;
    let moved = parameters.rebind(|name| names.biome(name));
    let regions = parameters.len();
    let biomes = parameters.distinct_biomes();

    let mut generator = dust_gen::terrain::Generator::new(data_path, "overworld", seed, parameters)
        .map_err(|e| {
            format!(
                "{} has {} beside it but no overworld to generate: {e}",
                data_path.display(),
                dust_gen::biome::FILE
            )
        })?;
    // The rules name biomes; this registry numbers them. A name it does not
    // have is reported and left unbound rather than matched against
    // everything, because a `biome_is` that matched everything would put a
    // beach across a continent.
    let mut unbound = generator.bind_surface_biomes(|name| names.biome(name));
    // The feature stage asks the running build two things rather than deciding
    // them: which id each biome name has, and which blocks count towards
    // `OCEAN_FLOOR_WG` -- the heightmap an ore consults before it draws a vein
    // at all. The second is a column of the operator's own constants table.
    // Without an answer for every block no feature runs, and the boot line says
    // so rather than putting ore in the sky.
    let ocean_floor = constants
        .as_deref()
        .and_then(|table| table.flag("OCEAN_FLOOR_WG").map(|flag| (table, flag)));
    unbound.extend(generator.bind_features(
        |name| names.biome(name),
        |spec| {
            let (table, flag) = ocean_floor?;
            Some(table.is_set(flag, state_of(spec).ok()?))
        },
    ));
    unbound.sort();
    unbound.dedup();
    // `[worldgen.ores]`, applied to the ores this world actually has rather
    // than to a table of vanilla's. Two things happen here and they are
    // different: a name the world has never heard of is an error naming the
    // nearest match, which is the check D6 says cannot be done until a world is
    // loaded; and the settings themselves are applied, which with the defaults
    // means nothing is applied at all and the pack's own placements run.
    let ore_groups = generator.ore_groups();
    let unknown_ores: Vec<String> = ores
        .validate_against(&ore_groups, "worldgen.ores")
        .into_iter()
        .map(|finding| format!("{}: {}", finding.path, finding.message))
        .collect();
    let ore_settings = generator.apply_ore_settings(ores);
    let surface_blocks = generator.surface().map_or(0, |rules| rules.palette().len());
    let features = generator
        .features()
        .filter(|features| features.ocean_floor_bound())
        .map_or((0, 0), dust_gen::feature::Features::coverage);
    let settings = generator.settings().clone();
    let world = GeneratedWorld::new(
        generator,
        flat,
        opacity,
        default_biome,
        biome_registry_size,
        constants,
    )
    .map_err(|e| e.to_string())?;
    Ok(Some((
        world,
        Report {
            regions,
            biomes,
            moved: moved.into_iter().map(|entry| entry.name).collect(),
            sea_level: settings.sea_level,
            default_block: settings.default_block.name,
            default_fluid: settings.default_fluid.name,
            surface_blocks,
            features,
            ore_groups: ore_groups.len(),
            ore_settings,
            unknown_ores,
            unbound,
        },
    )))
}

/// What [`beside`] found, for the one line a server says about it at boot.
#[derive(Debug)]
pub struct Report {
    pub regions: usize,
    pub biomes: usize,
    /// Biomes whose id in the table is not the id this build's registry has.
    pub moved: Vec<String>,
    pub sea_level: i32,
    pub default_block: String,
    pub default_fluid: String,
    /// How many distinct blocks the dimension's surface rules can write. Zero
    /// means the settings carried no rules and the ground is bare stone.
    pub surface_blocks: usize,
    /// Placed features this generator runs, and how many the pack's biomes name
    /// altogether. `(0, 0)` means no feature runs -- either the pack names none
    /// this generator knows, or nothing answered for `OCEAN_FLOOR_WG`.
    pub features: (usize, usize),
    /// How many ore groups this world's data defines — the knobs
    /// `[worldgen.ores]` may turn.
    pub ore_groups: usize,
    /// What `[worldgen.ores]` did to them, which with the defaults is nothing.
    pub ore_settings: dust_gen::feature::OreSettings,
    /// `[worldgen.ores.overrides]` entries naming an ore this world does not
    /// generate, each with the nearest name it does. An operator who wrote one
    /// has a server that started and a setting that did nothing, which is the
    /// outcome decision record 0006 calls the worst available.
    pub unknown_ores: Vec<String>,
    /// Biomes the rules ask about that this registry does not have.
    pub unbound: Vec<String>,
}

impl Report {
    pub fn summary(&self, seed: i64) -> String {
        let surface = if self.surface_blocks == 0 {
            "no surface rules, so the ground is the default block".to_owned()
        } else {
            format!("surface rules over {} block(s)", self.surface_blocks)
        };
        let features = match self.features {
            (0, 0) => "no features".to_owned(),
            (running, read) => format!("{running} of {read} placed feature(s)"),
        };
        let mut line = format!(
            "generating from seed {seed}: {} climate region(s) over {} biome(s), \
             {} above sea level {}, {surface}, {features}",
            self.regions, self.biomes, self.default_fluid, self.sea_level,
        );
        if !self.moved.is_empty() {
            line.push_str(&format!(
                " — and {} biome(s) have moved since the table was written: {}",
                self.moved.len(),
                self.moved.join(", ")
            ));
        }
        if !self.ore_settings.is_empty() {
            let settings = &self.ore_settings;
            line.push_str(&format!(
                " — and [worldgen.ores] over {} ore group(s) scaled {}, switched off {}                  and left {} alone",
                self.ore_groups,
                settings.scaled.len(),
                settings.disabled.len(),
                settings.untouched.len()
            ));
            for note in &settings.notes {
                line.push_str(&format!(" — {note}"));
            }
        }
        if !self.unknown_ores.is_empty() {
            line.push_str(&format!(
                " — and {} ore setting(s) name nothing this world generates: {}",
                self.unknown_ores.len(),
                self.unknown_ores.join("; ")
            ));
        }
        if !self.unbound.is_empty() {
            line.push_str(&format!(
                " — and {} name(s) the rules or the features ask about are not in this \
                 registry: {}",
                self.unbound.len(),
                self.unbound.join(", ")
            ));
        }
        line
    }
}
