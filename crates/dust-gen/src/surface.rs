//! Surface rules: the one block a player walks on, and the four under it.
//!
//! Vanilla's noise stage answers three questions — rock, fluid or air — and
//! decision record 0026 wired that to the socket. It leaves a world made of
//! stone. **Surface rules are what put grass over dirt, sand on a beach,
//! gravel on a shore and snow on a peak**, and a terrain with the right shape
//! and the wrong surface reads as wrong from the first screenshot.
//!
//! # Where the rules come from
//!
//! `noise_settings/<dimension>.json` carries a `surface_rule` — thirty-two
//! kilobytes of it for the overworld — and **nothing in this file is Mojang's
//! but the shape of the interpreter**. The blocks, the thresholds, the noises,
//! the biome lists and the order they are tried in all arrive at run time from
//! the operator's own unpacked data pack, the same road decision records 0006,
//! 0007, 0008 and 0026 put every other number on. A pack that puts mycelium on
//! a plains gets mycelium on a plains.
//!
//! # The column walk is the rule
//!
//! A surface rule is not a function of a position. It is a function of a
//! position **and how deep into the rock that position is**, which is why
//! vanilla walks each column from its top down and carries three running
//! numbers: how many solid blocks have been passed since the last air
//! (`stone_depth_above`), how many are left before the rock ends
//! (`stone_depth_below`), and where the last fluid surface was
//! (`water_height`). Every `stone_depth` and `water` condition reads one of
//! those. Sampling the rule at a point without the walk would answer a
//! different question.
//!
//! # What is not here, and what it costs
//!
//! * **`minecraft:temperature`.** Minecraft asks the biome whether it is cold
//!   enough to snow at that block, which is a legacy simplex noise and two
//!   temperature modifiers this file does not have. It answers `false` and
//!   [`Painter::declined`] counts every time it was asked, because a condition
//!   that is never reached is not a gap. In the overworld pack it sits under
//!   `biome in {frozen_ocean, deep_frozen_ocean}` and `hole`, and the branch
//!   it falls through to is water where Minecraft may have ice.
//! * **The eroded-badlands and frozen-ocean extensions.** Those are not
//!   surface *rules* — they run beside them in `SurfaceSystem` and are what
//!   makes an iceberg and a badlands pillar. Named in decision record 0028 and
//!   counted rather than guessed at.
//! * **Aquifers.** A different stage of the same record. Everything below the
//!   sea level that is not rock is still the dimension's fluid.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::noise::build::{BlockSpec, BuildError, NoiseSettings};
use crate::noise::density::{Evaluator, Graph};
use crate::noise::rng::{Positional, Xoroshiro};

/// The most result blocks a rule may name.
///
/// Four codes are spoken for by the noise stage — air, the default block, the
/// default fluid and the lava an aquifer writes — and a material is a `u8`
/// because a chunk's worth is ninety-six kibibytes of scratch that is reused
/// for every column a server ever generates.
pub const MAX_PALETTE: usize = 252;

/// The noise a column's surface depth is drawn from.
const SURFACE_NOISE: &str = "minecraft:surface";
/// The noise the second depth of a `stone_depth` check is drawn from.
const SURFACE_SECONDARY_NOISE: &str = "minecraft:surface_secondary";
/// The noise that shifts a badlands column's clay bands up or down.
const CLAY_BANDS_NOISE: &str = "minecraft:clay_bands_offset";
/// The name the clay bands' own stream is hashed from.
const CLAY_BANDS: &str = "clay_bands";

/// The seven blocks a badlands band is made of.
///
/// These are the one table in this file that is not read from the pack,
/// because vanilla does not put them in one: `SurfaceSystem.generateBands`
/// names them in Java and the `minecraft:bandlands` rule carries no argument
/// at all. They are block *names*, which Dust already writes at a world's
/// floor and in its heightmap predicate; they are not a table of values, which
/// is what decision records 0006 to 0008 keep out of the tree.
const BAND_TERRACOTTA: &str = "minecraft:terracotta";
const BAND_ORANGE: &str = "minecraft:orange_terracotta";
const BAND_YELLOW: &str = "minecraft:yellow_terracotta";
const BAND_BROWN: &str = "minecraft:brown_terracotta";
const BAND_RED: &str = "minecraft:red_terracotta";
const BAND_WHITE: &str = "minecraft:white_terracotta";
const BAND_LIGHT_GRAY: &str = "minecraft:light_gray_terracotta";
/// How many bands a badlands column repeats through.
const BANDS: usize = 192;

/// The one block name this file has to recognise rather than merely write.
///
/// A rule that puts air at the top of a column lowers where its sky reaches,
/// and `steep` reads that. Everything else the rules name is opaque to them.
const AIR: &str = "minecraft:air";

/// One rule of the tree.
#[derive(Debug, Clone)]
enum Rule {
    /// A block, always. Index into [`Rules::palette`].
    Block(u8),
    /// The badlands clay band at this y.
    Bandlands,
    /// The first child that answers wins. `start..end` into `Rules::children`.
    Sequence { start: u32, end: u32 },
    /// `then` if `test` holds, and no answer otherwise.
    Condition { test: u32, then: u32 },
}

/// One condition of the tree.
#[derive(Debug, Clone)]
enum Condition {
    /// The biome at this block is one of `start..end` into `Rules::biomes`.
    Biome {
        start: u32,
        end: u32,
    },
    /// A noise read at this column, between two thresholds.
    NoiseThreshold {
        noise: u32,
        min: f64,
        max: f64,
    },
    /// True below one y, false above another, and a die in between.
    VerticalGradient {
        true_at_and_below: i32,
        false_at_and_above: i32,
        factory: u32,
    },
    YAbove {
        anchor: i32,
        multiplier: i32,
        add_stone_depth: bool,
    },
    Water {
        offset: i32,
        multiplier: i32,
        add_stone_depth: bool,
    },
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary_range: i32,
        ceiling: bool,
    },
    /// The column's surface depth came out at or below zero.
    Hole,
    /// At or above the coarse surface this column's density predicts.
    AbovePreliminarySurface,
    /// A neighbouring column is four blocks taller.
    Steep,
    /// Cold enough to snow. See the module note: this answers `false`.
    Temperature,
    Not(u32),
}

impl Condition {
    /// Whether the answer can only change when the column does.
    ///
    /// The ones that can are worth caching and the ones that cannot are not:
    /// vanilla makes exactly this split with two lazy base classes, and it is
    /// not an optimisation so much as the difference between seven noise
    /// samples per column and seven per **block**.
    fn stable_over_a_column(&self, all: &[Condition]) -> bool {
        match self {
            Self::NoiseThreshold { .. } | Self::Hole | Self::Steep => true,
            Self::Not(inner) => all[*inner as usize].stable_over_a_column(all),
            _ => false,
        }
    }
}

/// A dimension's compiled surface rules.
///
/// Shared and immutable, like the density graph it is compiled beside: two
/// threads painting two chunks share every noise and every node and hold
/// nothing between them but a [`Painter`] each.
#[derive(Debug, Clone)]
pub struct Rules {
    rules: Vec<Rule>,
    conditions: Vec<Condition>,
    /// Which conditions are worth caching for the length of a column.
    stable: Vec<bool>,
    children: Vec<u32>,
    root: u32,
    /// The biome names every `biome` condition names, flattened, and the ids
    /// they were bound to. `u32::MAX` is a name this world's registry does not
    /// have, which can never match rather than matching everything.
    biome_names: Vec<String>,
    biome_ids: Vec<u32>,
    /// The distinct result blocks, in the order the tree first names them.
    palette: Vec<BlockSpec>,
    /// Indices into the graph's noise table.
    surface_noise: u32,
    secondary_noise: u32,
    clay_bands_noise: u32,
    /// One per `vertical_gradient`, in compile order, seeded by name.
    gradients: Vec<Positional>,
    /// The world's own positional factory, which a surface depth rolls from.
    factory: Positional,
    /// Palette indices, 192 of them, and the badlands column is this repeated.
    bands: Vec<u8>,
    /// Which palette entry is air, if any. Resolved once because it is asked
    /// about every block a rule claims and a name comparison there would be
    /// the cost of the stage.
    air: Option<u8>,
}

impl Rules {
    /// The result blocks, which a caller resolves against its own registry.
    ///
    /// A material code of `4 + i` is `palette()[i]`.
    pub fn palette(&self) -> &[BlockSpec] {
        &self.palette
    }

    /// The biome names the rules ask about, so a caller can bind them.
    pub fn biome_names(&self) -> &[String] {
        &self.biome_names
    }

    /// Point every `biome` condition at the ids a running registry uses.
    ///
    /// Returns the names it could not find. They are left unbound rather than
    /// defaulted, because a name that matched everything would put beach sand
    /// across a continent and a name that matched nothing costs one biome's
    /// surface.
    pub fn bind_biomes(&mut self, id_of: impl Fn(&str) -> Option<u32>) -> Vec<String> {
        let mut missing = Vec::new();
        for (name, slot) in self.biome_names.iter().zip(&mut self.biome_ids) {
            match id_of(name) {
                Some(id) => *slot = id,
                None => {
                    *slot = u32::MAX;
                    missing.push(name.clone());
                }
            }
        }
        missing
    }
}

/// One thread's scratch for painting columns.
#[derive(Debug, Clone)]
pub struct Painter<'a> {
    rules: &'a Rules,
    graph: &'a Graph,
    settings: &'a NoiseSettings,
    /// `initial_density_without_jaggedness`, which the preliminary surface
    /// level is walked down. Without it that condition cannot be answered and
    /// the whole surface branch is refused rather than guessed.
    initial_density: Option<usize>,
    evaluator: Evaluator<'a>,
    /// The top non-air y of each of the chunk's 256 columns, kept current as
    /// the columns are painted because `steep` reads its neighbours'.
    heights: Vec<i32>,
    /// The preliminary surface level at the chunk's four section corners.
    corners: [i32; 4],
    /// Per-column cache: 0 unknown, 1 false, 2 true.
    cache: Vec<u8>,
    block_x: i32,
    block_z: i32,
    surface_depth: i32,
    secondary: f64,
    min_surface_level: i32,
    block_y: i32,
    stone_above: i32,
    stone_below: i32,
    water_height: i32,
    /// The biome at this block, fetched the first time a rule asks for one.
    ///
    /// **Lazily, and that is the whole cost of the stage.** A biome is a
    /// climate search and the walk visits every solid block of every column;
    /// looking one up per block cost eighteen times the rest of the generator
    /// put together. Most blocks never reach a `biome` condition at all — the
    /// rules refuse everything below the preliminary surface two conditions in
    /// — so the ones that do are a thin shell and not a column.
    biome: Option<u32>,
    /// The last quart cell looked up, because consecutive blocks of one column
    /// mostly land in it.
    memo: Option<((i32, i32, i32), u32)>,
    zoom_seed: i64,
    declined: u64,
    bandlands: u64,
}

/// The y a column with no rock at all reports, which is one below the world.
const NO_GROUND: i32 = i32::MIN;

impl<'a> Painter<'a> {
    pub fn new(
        rules: &'a Rules,
        graph: &'a Graph,
        settings: &'a NoiseSettings,
        initial_density: Option<usize>,
    ) -> Self {
        Self {
            rules,
            graph,
            settings,
            initial_density,
            evaluator: Evaluator::new(graph),
            heights: vec![0; 256],
            corners: [0; 4],
            cache: vec![0; rules.conditions.len()],
            block_x: 0,
            block_z: 0,
            surface_depth: 0,
            secondary: 0.0,
            min_surface_level: 0,
            block_y: 0,
            stone_above: 0,
            stone_below: 0,
            water_height: NO_GROUND,
            biome: None,
            memo: None,
            zoom_seed: 0,
            declined: 0,
            bandlands: 0,
        }
    }

    /// How many times `minecraft:temperature` was asked and answered `false`
    /// without looking, and how many blocks a badlands band decided.
    ///
    /// Printed rather than assumed: a gap nobody's world reaches is not a gap,
    /// and this is the number that says which it is.
    pub fn declined(&self) -> (u64, u64) {
        (self.declined, self.bandlands)
    }

    /// Paint one chunk's materials in place.
    ///
    /// `materials` is the noise stage's output — air, the default block and
    /// the default fluid — and comes back with surface codes written over the
    /// blocks the rules claimed. Codes at or above three index
    /// [`Rules::palette`].
    /// Put the counts back to what they were, for a caller that painted a
    /// chunk it is not going to serve.
    pub fn set_declined(&mut self, declined: (u64, u64)) {
        (self.declined, self.bandlands) = declined;
    }

    pub fn paint(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        materials: &mut [u8],
        biomes: &mut crate::biome::Sampler<'_>,
        zoom_seed: i64,
    ) {
        let min_y = self.settings.min_y;
        let top = min_y + self.settings.height;
        let base_x = chunk_x * 16;
        let base_z = chunk_z * 16;
        self.zoom_seed = zoom_seed;
        self.memo = None;

        // The highest row of the chunk that holds anything at all, found by
        // reading whole rows rather than 256 columns of sky. Above a mountain
        // that is two hundred and fifty rows nobody has to look at twice, and
        // the answer is the same.
        let ceiling = (min_y..top)
            .rev()
            .find(|&y| {
                let row = (y - min_y) as usize * 256;
                materials[row..row + 256].iter().any(|&code| code != 0)
            })
            .unwrap_or(min_y - 1);
        for column in 0..256usize {
            self.heights[column] = (min_y..=ceiling)
                .rev()
                .find(|&y| materials[(y - min_y) as usize * 256 + column] != 0)
                .unwrap_or(min_y - 1);
        }
        self.fill_corners(chunk_x, chunk_z);

        // x outermost and z inside it, which is the order vanilla walks and
        // therefore the order `steep` sees its neighbours' heights change in.
        for local_x in 0..16i32 {
            for local_z in 0..16i32 {
                let column = (local_x + local_z * 16) as usize;
                self.start_column(base_x + local_x, base_z + local_z);
                let mut stone_above = 0;
                let mut water_height = NO_GROUND;
                let mut stone_floor = i32::MAX;
                let mut lowered = false;
                let mut y = self.heights[column] + 1;
                while y >= min_y {
                    let at = |y: i32| materials[(y - min_y) as usize * 256 + column];
                    let code = at(y);
                    if code == 0 {
                        stone_above = 0;
                        water_height = NO_GROUND;
                    } else if code == 2 || code == 3 {
                        // Any fluid, lava included: vanilla's walk asks
                        // `!getFluidState().isEmpty()` and not "is it water",
                        // so a `water` condition over a lava lake reads the
                        // lava's own surface.
                        if water_height == NO_GROUND {
                            water_height = y + 1;
                        }
                    } else {
                        if stone_floor >= y {
                            // How far down the rock runs, found once and then
                            // carried: the same answer for every block of one
                            // run, and a fresh scan per block would be the
                            // column walked squared.
                            stone_floor = i32::MIN / 2;
                            let mut below = y - 1;
                            while below >= min_y - 1 {
                                if below < min_y
                                    || at(below) == 0
                                    || at(below) == 2
                                    || at(below) == 3
                                {
                                    stone_floor = below + 1;
                                    break;
                                }
                                below -= 1;
                            }
                        }
                        stone_above += 1;
                        self.start_block(y, stone_above, y - stone_floor + 1, water_height);
                        // Only the dimension's own block is the rules' to
                        // claim. A cell the noise stage made fluid or air is
                        // not offered to them, which is why a surface rule can
                        // not fill an ocean.
                        if code == 1 {
                            if let Some(index) = self.apply(self.rules.root, biomes) {
                                materials[(y - min_y) as usize * 256 + column] = 4 + index;
                                // The only write that can move a column's top
                                // is one that puts air there, and the rules
                                // that do sit under `hole` in a frozen ocean.
                                // Recomputing every column for them would be
                                // 256 scans of a chunk to catch one.
                                if y >= self.heights[column] && self.rules.air == Some(index) {
                                    lowered = true;
                                }
                            }
                        }
                    }
                    y -= 1;
                }
                if lowered {
                    // A rule wrote air at the top of the column, which lowers
                    // the height its neighbours' `steep` reads.
                    self.heights[column] = (min_y..=self.heights[column])
                        .rev()
                        .find(|&y| {
                            let code = materials[(y - min_y) as usize * 256 + column];
                            code != 0 && self.rules.air != Some(code.wrapping_sub(4))
                        })
                        .unwrap_or(min_y - 1);
                }
            }
        }
    }

    /// The preliminary surface level at the chunk's four section corners.
    ///
    /// Four samples and not 256: vanilla asks at the corners of the **section**
    /// the block is in and lerps between them, so a chunk has exactly four
    /// answers however many blocks it holds.
    fn fill_corners(&mut self, chunk_x: i32, chunk_z: i32) {
        for (slot, (dx, dz)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
            self.corners[slot] =
                self.preliminary_surface_level((chunk_x + dx) * 16, (chunk_z + dz) * 16);
        }
    }

    /// Walk the un-jagged density down a cell at a time and stop where it
    /// first says rock.
    fn preliminary_surface_level(&mut self, x: i32, z: i32) -> i32 {
        let Some(root) = self.initial_density else {
            return i32::MAX;
        };
        // Quart-aligned, because that is the grid vanilla asks on.
        let x = (x >> 2) << 2;
        let z = (z >> 2) << 2;
        let min_y = self.settings.min_y;
        let mut y = min_y + self.settings.height;
        while y >= min_y {
            if self.evaluator.compute(root, x, y, z) > 0.390625 {
                return y;
            }
            y -= self.settings.cell_height;
        }
        i32::MAX
    }

    fn start_column(&mut self, x: i32, z: i32) {
        self.block_x = x;
        self.block_z = z;
        self.cache.fill(0);
        let surface = &self.graph.noises[self.rules.surface_noise as usize];
        let roll = self.rules.factory.at(x, 0, z).next_f64();
        self.surface_depth =
            (surface.value(f64::from(x), 0.0, f64::from(z)) * 2.75 + 3.0 + roll * 0.25) as i32;
        self.secondary = self.graph.noises[self.rules.secondary_noise as usize].value(
            f64::from(x),
            0.0,
            f64::from(z),
        );
        let fx = f64::from(x & 15) / 16.0;
        let fz = f64::from(z & 15) / 16.0;
        let level = lerp2(
            fx,
            fz,
            f64::from(self.corners[0]),
            f64::from(self.corners[1]),
            f64::from(self.corners[2]),
            f64::from(self.corners[3]),
        );
        self.min_surface_level = (level.floor() as i32)
            .saturating_add(self.surface_depth)
            .saturating_sub(8);
    }

    fn start_block(&mut self, y: i32, stone_above: i32, stone_below: i32, water_height: i32) {
        self.block_y = y;
        self.stone_above = stone_above;
        self.stone_below = stone_below;
        self.water_height = water_height;
        self.biome = None;
    }

    /// The biome `BiomeManager` would report at this block.
    fn biome(&mut self, biomes: &mut crate::biome::Sampler<'_>) -> u32 {
        if let Some(known) = self.biome {
            return known;
        }
        let cell =
            crate::biome::blurred_quart(self.zoom_seed, self.block_x, self.block_y, self.block_z);
        let found = match self.memo {
            Some((remembered, biome)) if remembered == cell => biome,
            _ => {
                let biome = biomes.biome(cell.0, cell.1, cell.2).unwrap_or(u32::MAX);
                self.memo = Some((cell, biome));
                biome
            }
        };
        self.biome = Some(found);
        found
    }

    /// Try one rule and hand back the palette index it claims, if any.
    fn apply(&mut self, rule: u32, biomes: &mut crate::biome::Sampler<'_>) -> Option<u8> {
        match self.rules.rules[rule as usize] {
            Rule::Block(index) => Some(index),
            Rule::Bandlands => {
                self.bandlands += 1;
                let noise = &self.graph.noises[self.rules.clay_bands_noise as usize];
                let shift = (noise.value(f64::from(self.block_x), 0.0, f64::from(self.block_z))
                    * 4.0)
                    .round() as i32;
                let bands = self.rules.bands.len() as i32;
                let at = (self.block_y + shift + bands).rem_euclid(bands);
                Some(self.rules.bands[at as usize])
            }
            Rule::Sequence { start, end } => {
                for index in start..end {
                    let child = self.rules.children[index as usize];
                    if let Some(found) = self.apply(child, biomes) {
                        return Some(found);
                    }
                }
                None
            }
            Rule::Condition { test, then } => {
                if self.test(test, biomes) {
                    self.apply(then, biomes)
                } else {
                    None
                }
            }
        }
    }

    fn test(&mut self, condition: u32, biomes: &mut crate::biome::Sampler<'_>) -> bool {
        let stable = self.rules.stable[condition as usize];
        if stable {
            match self.cache[condition as usize] {
                1 => return false,
                2 => return true,
                _ => {}
            }
        }
        let answer = self.evaluate(condition, biomes);
        if stable {
            self.cache[condition as usize] = if answer { 2 } else { 1 };
        }
        answer
    }

    fn evaluate(&mut self, condition: u32, biomes: &mut crate::biome::Sampler<'_>) -> bool {
        match self.rules.conditions[condition as usize] {
            Condition::Biome { start, end } => {
                let biome = self.biome(biomes);
                self.rules.biome_ids[start as usize..end as usize].contains(&biome)
            }
            Condition::NoiseThreshold { noise, min, max } => {
                let value = self.graph.noises[noise as usize].value(
                    f64::from(self.block_x),
                    0.0,
                    f64::from(self.block_z),
                );
                value >= min && value <= max
            }
            Condition::VerticalGradient {
                true_at_and_below,
                false_at_and_above,
                factory,
            } => {
                let y = self.block_y;
                if y <= true_at_and_below {
                    return true;
                }
                if y >= false_at_and_above {
                    return false;
                }
                let chance = map(
                    f64::from(y),
                    f64::from(true_at_and_below),
                    f64::from(false_at_and_above),
                    1.0,
                    0.0,
                );
                let mut stream =
                    self.rules.gradients[factory as usize].at(self.block_x, y, self.block_z);
                f64::from(stream.next_f32()) < chance
            }
            Condition::YAbove {
                anchor,
                multiplier,
                add_stone_depth,
            } => {
                let depth = if add_stone_depth { self.stone_above } else { 0 };
                self.block_y + depth >= anchor + self.surface_depth * multiplier
            }
            Condition::Water {
                offset,
                multiplier,
                add_stone_depth,
            } => {
                if self.water_height == NO_GROUND {
                    return true;
                }
                let depth = if add_stone_depth { self.stone_above } else { 0 };
                self.block_y + depth >= self.water_height + offset + self.surface_depth * multiplier
            }
            Condition::StoneDepth {
                offset,
                add_surface_depth,
                secondary_range,
                ceiling,
            } => {
                let depth = if ceiling {
                    self.stone_below
                } else {
                    self.stone_above
                };
                let surface = if add_surface_depth {
                    self.surface_depth
                } else {
                    0
                };
                let secondary = if secondary_range == 0 {
                    0
                } else {
                    map(self.secondary, -1.0, 1.0, 0.0, f64::from(secondary_range)) as i32
                };
                depth <= 1 + offset + surface + secondary
            }
            Condition::Hole => self.surface_depth <= 0,
            Condition::AbovePreliminarySurface => self.block_y >= self.min_surface_level,
            Condition::Steep => self.steep(),
            Condition::Temperature => {
                self.declined += 1;
                false
            }
            Condition::Not(inner) => !self.test(inner, biomes),
        }
    }

    fn steep(&self) -> bool {
        let x = (self.block_x & 15) as usize;
        let z = (self.block_z & 15) as usize;
        let at = |x: usize, z: usize| self.heights[x + z * 16];
        let low_z = z.saturating_sub(1);
        let high_z = (z + 1).min(15);
        if at(x, high_z) >= at(x, low_z) + 4 {
            return true;
        }
        let low_x = x.saturating_sub(1);
        let high_x = (x + 1).min(15);
        at(low_x, z) >= at(high_x, z) + 4
    }
}

fn lerp(delta: f64, start: f64, end: f64) -> f64 {
    start + delta * (end - start)
}

fn lerp2(dx: f64, dz: f64, v00: f64, v10: f64, v01: f64, v11: f64) -> f64 {
    lerp(dz, lerp(dx, v00, v10), lerp(dx, v01, v11))
}

/// `Mth.map`: `value`'s place in one interval, read off another.
fn map(value: f64, from_low: f64, from_high: f64, to_low: f64, to_high: f64) -> f64 {
    to_low + (value - from_low) * (to_high - to_low) / (from_high - from_low)
}

// ---------------------------------------------------------------------------
// Compiling
// ---------------------------------------------------------------------------

/// Compile a dimension's `surface_rule`.
///
/// `noise` registers a noise by name and hands back its index in the graph's
/// own table, so a noise the density functions already reached is read once
/// and built once.
pub fn compile(
    value: &Value,
    origin: &Path,
    settings: &NoiseSettings,
    seed: i64,
    mut noise: impl FnMut(&str) -> u32,
) -> Result<Rules, BuildError> {
    let mut builder = Builder {
        origin: origin.to_path_buf(),
        rules: Vec::new(),
        conditions: Vec::new(),
        children: Vec::new(),
        biome_names: Vec::new(),
        palette: Vec::new(),
        gradients: Vec::new(),
        noise_names: Vec::new(),
        min_y: settings.min_y,
        height: settings.height,
    };
    let root = builder.rule(value)?;
    if builder.palette.len() > MAX_PALETTE {
        return Err(builder.complain(format!(
            "{} result blocks; a material code holds {MAX_PALETTE}",
            builder.palette.len()
        )));
    }

    // Every noise the conditions named, then the three the walk itself reads.
    // Registered through the caller so they land in the one graph the density
    // functions were compiled into.
    let condition_noises: Vec<u32> = builder.noise_names.iter().map(|name| noise(name)).collect();
    for condition in &mut builder.conditions {
        if let Condition::NoiseThreshold { noise, .. } = condition {
            *noise = condition_noises[*noise as usize];
        }
    }
    let surface_noise = noise(SURFACE_NOISE);
    let secondary_noise = noise(SURFACE_SECONDARY_NOISE);
    let clay_bands_noise = noise(CLAY_BANDS_NOISE);

    let factory = crate::noise::build::positional_factory(seed);
    let gradients = builder
        .gradients
        .iter()
        .map(|name| {
            // `getOrCreateRandomFactory`: the world's factory hashed on the
            // rule's own name and then forked again. Two gradients with two
            // names roll different dice at the same block, which is what keeps
            // a bedrock roof from being a bedrock floor turned upside down.
            factory.from_hash_of(name).fork_positional()
        })
        .collect();
    let bands = clay_bands(
        &mut factory.from_hash_of(&namespaced(CLAY_BANDS)),
        &mut builder.palette,
    )?;
    if builder.palette.len() > MAX_PALETTE {
        return Err(builder.complain(format!(
            "{} result blocks once the badlands bands are counted; a material code holds \
             {MAX_PALETTE}",
            builder.palette.len()
        )));
    }

    let air = builder
        .palette
        .iter()
        .position(|spec| spec.name == AIR && spec.properties.is_empty())
        .map(|at| at as u8);
    let stable = {
        let all = &builder.conditions;
        all.iter().map(|c| c.stable_over_a_column(all)).collect()
    };
    let biome_ids = vec![u32::MAX; builder.biome_names.len()];
    Ok(Rules {
        rules: builder.rules,
        conditions: builder.conditions,
        stable,
        children: builder.children,
        root,
        biome_names: builder.biome_names,
        biome_ids,
        palette: builder.palette,
        surface_noise,
        secondary_noise,
        clay_bands_noise,
        gradients,
        factory,
        bands,
        air,
    })
}

/// The 192 bands a badlands column repeats through.
///
/// The draws are the contract: five `nextInt` calls per orange run, three
/// makeBands passes in a fixed order, then a white run whose stride is another
/// draw. Reordering any of them is a different badlands, and it is the sort of
/// difference that still looks like a badlands.
fn clay_bands(random: &mut Xoroshiro, palette: &mut Vec<BlockSpec>) -> Result<Vec<u8>, BuildError> {
    let index = |name: &str, palette: &mut Vec<BlockSpec>| -> u8 {
        let spec = BlockSpec {
            name: name.to_owned(),
            properties: Vec::new(),
        };
        match palette.iter().position(|entry| *entry == spec) {
            Some(at) => at as u8,
            None => {
                palette.push(spec);
                (palette.len() - 1) as u8
            }
        }
    };
    let terracotta = index(BAND_TERRACOTTA, palette);
    let orange = index(BAND_ORANGE, palette);
    let yellow = index(BAND_YELLOW, palette);
    let brown = index(BAND_BROWN, palette);
    let red = index(BAND_RED, palette);
    let white = index(BAND_WHITE, palette);
    let light_gray = index(BAND_LIGHT_GRAY, palette);

    let mut bands = vec![terracotta; BANDS];
    let mut at = 0usize;
    while at < BANDS {
        at += random.next_i32_below(5) as usize + 1;
        if at < BANDS {
            bands[at] = orange;
        }
        at += 1;
    }
    make_bands(random, &mut bands, 1, yellow);
    make_bands(random, &mut bands, 2, brown);
    make_bands(random, &mut bands, 1, red);

    let runs = random.next_i32_between_inclusive(9, 15);
    let mut made = 0;
    let mut at = 0usize;
    while made < runs && at < BANDS {
        bands[at] = white;
        if at >= 1 && random.next_bool() {
            bands[at - 1] = light_gray;
        }
        if at + 1 < BANDS && random.next_bool() {
            bands[at + 1] = light_gray;
        }
        made += 1;
        at += random.next_i32_below(16) as usize + 4;
    }
    Ok(bands)
}

fn make_bands(random: &mut Xoroshiro, bands: &mut [u8], thickness: i32, band: u8) {
    let runs = random.next_i32_between_inclusive(6, 15);
    for _ in 0..runs {
        let length = thickness + random.next_i32_below(3);
        let start = random.next_i32_below(bands.len() as i32) as usize;
        let mut step = 0usize;
        while start + step < bands.len() && (step as i32) < length {
            bands[start + step] = band;
            step += 1;
        }
    }
}

fn namespaced(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

struct Builder {
    origin: PathBuf,
    rules: Vec<Rule>,
    conditions: Vec<Condition>,
    children: Vec<u32>,
    biome_names: Vec<String>,
    palette: Vec<BlockSpec>,
    /// The name each `vertical_gradient` rolls its die from.
    gradients: Vec<String>,
    /// Noise names in the order the conditions reached them.
    noise_names: Vec<String>,
    min_y: i32,
    height: i32,
}

impl Builder {
    fn complain(&self, detail: String) -> BuildError {
        BuildError::Malformed {
            path: self.origin.clone(),
            detail,
        }
    }

    fn push_rule(&mut self, rule: Rule) -> u32 {
        self.rules.push(rule);
        (self.rules.len() - 1) as u32
    }

    fn push_condition(&mut self, condition: Condition) -> u32 {
        self.conditions.push(condition);
        (self.conditions.len() - 1) as u32
    }

    fn kind<'v>(
        &self,
        value: &'v Value,
        what: &str,
    ) -> Result<(&'v serde_json::Map<String, Value>, String), BuildError> {
        let object = value
            .as_object()
            .ok_or_else(|| self.complain(format!("a surface {what} is an object")))?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| self.complain(format!("a surface {what} needs a `type`")))?
            .to_owned();
        Ok((object, kind))
    }

    fn rule(&mut self, value: &Value) -> Result<u32, BuildError> {
        let (object, kind) = self.kind(value, "rule")?;
        match kind.as_str() {
            "minecraft:block" => {
                let state = object.get("result_state").ok_or_else(|| {
                    self.complain("a `block` rule needs a `result_state`".to_owned())
                })?;
                let spec = block_spec(state)
                    .ok_or_else(|| self.complain("a `result_state` needs a `Name`".to_owned()))?;
                let index = match self.palette.iter().position(|entry| *entry == spec) {
                    Some(at) => at,
                    None => {
                        self.palette.push(spec);
                        self.palette.len() - 1
                    }
                };
                Ok(self.push_rule(Rule::Block(index as u8)))
            }
            "minecraft:bandlands" => Ok(self.push_rule(Rule::Bandlands)),
            "minecraft:sequence" => {
                let list = object
                    .get("sequence")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        self.complain("a `sequence` rule needs a `sequence`".to_owned())
                    })?;
                let mut compiled = Vec::with_capacity(list.len());
                for entry in list {
                    compiled.push(self.rule(entry)?);
                }
                let start = self.children.len() as u32;
                self.children.extend(compiled);
                let end = self.children.len() as u32;
                Ok(self.push_rule(Rule::Sequence { start, end }))
            }
            "minecraft:condition" => {
                let test = object
                    .get("if_true")
                    .ok_or_else(|| {
                        self.complain("a `condition` rule needs an `if_true`".to_owned())
                    })?
                    .clone();
                let then = object
                    .get("then_run")
                    .ok_or_else(|| {
                        self.complain("a `condition` rule needs a `then_run`".to_owned())
                    })?
                    .clone();
                let test = self.condition(&test)?;
                let then = self.rule(&then)?;
                Ok(self.push_rule(Rule::Condition { test, then }))
            }
            other => Err(BuildError::UnknownType {
                name: self.origin.display().to_string(),
                kind: format!("surface rule `{other}`"),
            }),
        }
    }

    fn condition(&mut self, value: &Value) -> Result<u32, BuildError> {
        let (object, kind) = self.kind(value, "condition")?;
        let integer = |name: &str, fallback: i32| -> i32 {
            object
                .get(name)
                .and_then(Value::as_i64)
                .map_or(fallback, |value| value as i32)
        };
        let flag =
            |name: &str| -> bool { object.get(name).and_then(Value::as_bool).unwrap_or(false) };
        let condition = match kind.as_str() {
            "minecraft:biome" => {
                let list = object
                    .get("biome_is")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        self.complain("a `biome` condition needs a `biome_is`".to_owned())
                    })?;
                let start = self.biome_names.len() as u32;
                for entry in list {
                    let name = entry
                        .as_str()
                        .ok_or_else(|| self.complain("a `biome_is` entry is a name".to_owned()))?;
                    self.biome_names.push(namespaced(name));
                }
                let end = self.biome_names.len() as u32;
                Condition::Biome { start, end }
            }
            "minecraft:noise_threshold" => {
                let name = object.get("noise").and_then(Value::as_str).ok_or_else(|| {
                    self.complain("a `noise_threshold` needs a `noise`".to_owned())
                })?;
                let name = namespaced(name);
                let slot = match self.noise_names.iter().position(|entry| *entry == name) {
                    Some(at) => at as u32,
                    None => {
                        self.noise_names.push(name);
                        (self.noise_names.len() - 1) as u32
                    }
                };
                let number = |field: &str, fallback: f64| {
                    object
                        .get(field)
                        .and_then(Value::as_f64)
                        .unwrap_or(fallback)
                };
                Condition::NoiseThreshold {
                    noise: slot,
                    min: number("min_threshold", f64::NEG_INFINITY),
                    max: number("max_threshold", f64::INFINITY),
                }
            }
            "minecraft:vertical_gradient" => {
                let name = object
                    .get("random_name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        self.complain("a `vertical_gradient` needs a `random_name`".to_owned())
                    })?;
                let below = self.anchor(object.get("true_at_and_below"))?;
                let above = self.anchor(object.get("false_at_and_above"))?;
                self.gradients.push(namespaced(name));
                Condition::VerticalGradient {
                    true_at_and_below: below,
                    false_at_and_above: above,
                    factory: (self.gradients.len() - 1) as u32,
                }
            }
            "minecraft:y_above" => Condition::YAbove {
                anchor: self.anchor(object.get("anchor"))?,
                multiplier: integer("surface_depth_multiplier", 0),
                add_stone_depth: flag("add_stone_depth"),
            },
            "minecraft:water" => Condition::Water {
                offset: integer("offset", 0),
                multiplier: integer("surface_depth_multiplier", 0),
                add_stone_depth: flag("add_stone_depth"),
            },
            "minecraft:stone_depth" => Condition::StoneDepth {
                offset: integer("offset", 0),
                add_surface_depth: flag("add_surface_depth"),
                secondary_range: integer("secondary_depth_range", 0),
                ceiling: object.get("surface_type").and_then(Value::as_str) == Some("ceiling"),
            },
            "minecraft:hole" => Condition::Hole,
            "minecraft:above_preliminary_surface" => Condition::AbovePreliminarySurface,
            "minecraft:steep" => Condition::Steep,
            "minecraft:temperature" => Condition::Temperature,
            "minecraft:not" => {
                let inner = object
                    .get("invert")
                    .ok_or_else(|| self.complain("a `not` condition needs an `invert`".to_owned()))?
                    .clone();
                Condition::Not(self.condition(&inner)?)
            }
            other => {
                return Err(BuildError::UnknownType {
                    name: self.origin.display().to_string(),
                    kind: format!("surface condition `{other}`"),
                })
            }
        };
        Ok(self.push_condition(condition))
    }

    /// A vertical anchor, resolved against this world's own floor and height.
    ///
    /// `above_bottom` and `below_top` are relative on purpose: a pack that
    /// changes `min_y` moves the bedrock band with it, and an anchor resolved
    /// against a number written here would not move.
    fn anchor(&self, value: Option<&Value>) -> Result<i32, BuildError> {
        let object = value
            .and_then(Value::as_object)
            .ok_or_else(|| self.complain("a vertical anchor is an object".to_owned()))?;
        let read = |name: &str| object.get(name).and_then(Value::as_i64).map(|v| v as i32);
        if let Some(y) = read("absolute") {
            return Ok(y);
        }
        if let Some(y) = read("above_bottom") {
            return Ok(self.min_y + y);
        }
        if let Some(y) = read("below_top") {
            return Ok(self.min_y + self.height - 1 - y);
        }
        Err(self
            .complain("a vertical anchor is `absolute`, `above_bottom` or `below_top`".to_owned()))
    }
}

/// A block name and the properties a rule wrote beside it.
fn block_spec(value: &Value) -> Option<BlockSpec> {
    let object = value.as_object()?;
    let name = object.get("Name").and_then(Value::as_str)?;
    let mut properties: Vec<(String, String)> = object
        .get("Properties")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    properties.sort();
    Some(BlockSpec {
        name: namespaced(name),
        properties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biome::BiomeParameters;
    use crate::terrain::{Generator, Material};

    /// A pack with the three noises a surface walk reads, a terrain that falls
    /// away with height, and whatever `surface_rule` the caller wants.
    fn pack(name: &str, rule: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("dust-gen-surface-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for noise in [
            "surface",
            "surface_secondary",
            "clay_bands_offset",
            "scratch",
        ] {
            write(
                &root,
                &format!("minecraft/worldgen/noise/{noise}.json"),
                r#"{"firstOctave": -6, "amplitudes": [1.0, 1.0, 1.0]}"#,
            );
        }
        write(
            &root,
            "minecraft/worldgen/noise_settings/overworld.json",
            &format!(
                r#"{{"noise": {{"height": 384, "min_y": -64,
                                "size_horizontal": 1, "size_vertical": 2}},
                    "sea_level": 63,
                    "default_block": {{"Name": "minecraft:stone"}},
                    "default_fluid": {{"Name": "minecraft:water"}},
                    "aquifers_enabled": false,
                    "surface_rule": {rule},
                    "noise_router": {{"temperature": 0.0, "vegetation": 0.0,
                                      "continents": 0.0, "erosion": 0.0,
                                      "depth": 0.0, "ridges": 0.0,
                                      "initial_density_without_jaggedness": {TERRAIN},
                                      "final_density": {{"type": "minecraft:interpolated",
                                                         "argument": {TERRAIN}}}}}}}"#
            ),
        );
        // The one biome this fixture's parameter table names, so that
        // `Generator::new` can ask it which carvers it runs. It names none;
        // these tests are about the block underfoot and a tunnel through it
        // would move the very cells they count.
        write(
            &root,
            "minecraft/worldgen/biome/plains.json",
            r#"{"carvers": {}}"#,
        );
        root
    }

    /// Ground that falls away with height and is rough enough that two columns
    /// differ, which is the same shape the terrain tests use.
    const TERRAIN: &str = r#"{
        "type": "minecraft:add",
        "argument1": {"type": "minecraft:y_clamped_gradient",
                      "from_y": -64, "to_y": 224,
                      "from_value": 1.0, "to_value": -1.0},
        "argument2": {"type": "minecraft:mul", "argument1": 0.8,
                      "argument2": {"type": "minecraft:noise",
                                    "noise": "minecraft:scratch",
                                    "xz_scale": 1.0, "y_scale": 1.0}}
      }"#;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// Grass on the top block of rock, dirt for the three under it, stone
    /// below — the smallest rule that is a real surface.
    const STACK: &str = r#"{"type": "minecraft:sequence", "sequence": [
        {"type": "minecraft:condition",
         "if_true": {"type": "minecraft:stone_depth", "surface_type": "floor",
                     "add_surface_depth": false, "offset": 0,
                     "secondary_depth_range": 0},
         "then_run": {"type": "minecraft:block",
                      "result_state": {"Name": "minecraft:grass_block"}}},
        {"type": "minecraft:condition",
         "if_true": {"type": "minecraft:stone_depth", "surface_type": "floor",
                     "add_surface_depth": false, "offset": 3,
                     "secondary_depth_range": 0},
         "then_run": {"type": "minecraft:block",
                      "result_state": {"Name": "minecraft:dirt"}}}]}"#;

    fn parameters() -> BiomeParameters {
        let axes = "\t-10000\t10000".repeat(6);
        let table = format!(
            "# biome_id\tbiome\ttemperature_min\ttemperature_max\thumidity_min\thumidity_max\
             \tcontinentalness_min\tcontinentalness_max\terosion_min\terosion_max\
             \tdepth_min\tdepth_max\tweirdness_min\tweirdness_max\toffset\n\
             0\tminecraft:plains{axes}\t0\n"
        );
        BiomeParameters::parse(&table).expect("the scratch table parses")
    }

    fn painted(root: &Path) -> (Generator, Vec<u8>) {
        let generator =
            Generator::new(root, "overworld", 42, parameters()).expect("the pack compiles");
        let mut columns = generator.columns();
        let materials = columns.surface(0, 0).to_vec();
        (generator, materials)
    }

    fn at(materials: &[u8], x: usize, y: i32, z: usize) -> Material {
        Material::from_code(materials[(y + 64) as usize * 256 + z * 16 + x])
    }

    /// The name of what is at a position, resolved through the rules' palette.
    fn name(rules: &Rules, materials: &[u8], x: usize, y: i32, z: usize) -> String {
        match at(materials, x, y, z) {
            Material::Air => "air".to_owned(),
            Material::Solid => "stone".to_owned(),
            Material::Fluid => "water".to_owned(),
            Material::Lava => "lava".to_owned(),
            Material::Surface(index) => rules.palette()[index as usize].name.clone(),
        }
    }

    /// The top of every rock column is grass, the three under it are dirt, and
    /// the fifth down is still the default block.
    ///
    /// **Watched to fail in both directions.** `offset: 3` is what makes the
    /// dirt three deep; the same rule with the depths swapped puts grass three
    /// blocks down, and the fifth assertion is what catches a walk that has
    /// stopped counting depth at all and paints the whole column.
    #[test]
    fn grass_sits_on_dirt_which_sits_on_the_default_block() {
        let root = pack("stack", STACK);
        let (generator, materials) = painted(&root);
        let rules = generator.surface().expect("the pack has rules");
        let mut checked = 0;
        for z in 0..16usize {
            for x in 0..16usize {
                // The topmost rock, which is where the rules start.
                let top = (-64..320)
                    .rev()
                    .find(|&y| at(&materials, x, y, z) != Material::Air)
                    .expect("every column has ground");
                if at(&materials, x, top, z) == Material::Fluid {
                    continue;
                }
                assert_eq!(name(rules, &materials, x, top, z), "minecraft:grass_block");
                for down in 1..=3 {
                    assert_eq!(
                        name(rules, &materials, x, top - down, z),
                        "minecraft:dirt",
                        "at {x},{},{z}",
                        top - down
                    );
                }
                assert_eq!(name(rules, &materials, x, top - 4, z), "stone");
                checked += 1;
            }
        }
        assert!(checked > 0, "no column of this chunk had dry ground on it");
    }

    /// The rules only ever claim the dimension's own block.
    ///
    /// Air stays air and the fluid stays fluid, however loudly a rule says
    /// otherwise: a rule that painted a whole column would fill an ocean.
    #[test]
    fn a_rule_that_always_answers_still_leaves_the_air_and_the_fluid_alone() {
        let root = pack(
            "greedy",
            r#"{"type": "minecraft:block", "result_state": {"Name": "minecraft:gravel"}}"#,
        );
        let (generator, materials) = painted(&root);
        let rules = generator.surface().expect("the pack has rules");
        let mut air = 0;
        let mut fluid = 0;
        let mut gravel = 0;
        for y in -64..320 {
            for z in 0..16usize {
                for x in 0..16usize {
                    match at(&materials, x, y, z) {
                        Material::Air => air += 1,
                        Material::Fluid => fluid += 1,
                        Material::Lava => panic!("this dimension has no aquifer to write lava"),
                        Material::Solid => {
                            panic!("a rule that always answers left stone at {x},{y},{z}")
                        }
                        Material::Surface(index) => {
                            assert_eq!(rules.palette()[index as usize].name, "minecraft:gravel");
                            gravel += 1;
                        }
                    }
                }
            }
        }
        assert!(
            air > 0 && fluid > 0 && gravel > 0,
            "{air} air, {fluid} fluid, {gravel} gravel"
        );
    }

    /// A `y_above` rule is a horizontal line through the world, and the line
    /// is where the rule says and not one block off.
    #[test]
    fn a_y_above_rule_cuts_exactly_where_it_says() {
        let root = pack(
            "line",
            r#"{"type": "minecraft:condition",
                "if_true": {"type": "minecraft:y_above",
                            "anchor": {"absolute": 20},
                            "surface_depth_multiplier": 0,
                            "add_stone_depth": false},
                "then_run": {"type": "minecraft:block",
                             "result_state": {"Name": "minecraft:calcite"}}}"#,
        );
        let (generator, materials) = painted(&root);
        let rules = generator.surface().expect("the pack has rules");
        for z in 0..16usize {
            for x in 0..16usize {
                assert_eq!(name(rules, &materials, x, 20, z), "minecraft:calcite");
                assert_eq!(name(rules, &materials, x, 19, z), "stone");
            }
        }
    }

    /// A biome name the registry does not have is reported and matches
    /// nothing, rather than matching everything.
    #[test]
    fn an_unbound_biome_name_is_named_and_claims_no_block() {
        let root = pack(
            "biome",
            r#"{"type": "minecraft:condition",
                "if_true": {"type": "minecraft:biome",
                            "biome_is": ["minecraft:nowhere"]},
                "then_run": {"type": "minecraft:block",
                             "result_state": {"Name": "minecraft:mud"}}}"#,
        );
        let mut generator =
            Generator::new(&root, "overworld", 42, parameters()).expect("the pack compiles");
        let missing = generator.bind_surface_biomes(|name| {
            if name == "minecraft:plains" {
                Some(0)
            } else {
                None
            }
        });
        assert_eq!(missing, vec!["minecraft:nowhere".to_owned()]);
        let mut columns = generator.columns();
        let materials = columns.surface(0, 0).to_vec();
        assert!(
            (-64..320).all(|y| (0..16).all(|c| at(&materials, c, y, 0) != Material::Surface(0))),
            "an unbound biome name claimed a block"
        );
    }

    /// The blur stays inside the cell it started in, and does not agree with
    /// the plain quart everywhere.
    ///
    /// Both halves matter. A blur that wandered further than a cell would put
    /// a desert's sand in a forest; one that never moved would be the quart
    /// grid with a hash bolted on, and the coast would run down a straight
    /// line. This is the check that says which.
    #[test]
    fn the_biome_blur_moves_a_boundary_without_leaving_the_neighbourhood() {
        let seed = crate::noise::rng::obfuscate_seed(1234);
        let mut moved = 0;
        let mut looked = 0;
        for x in -40..40 {
            for z in -40..40 {
                let (qx, qy, qz) = crate::biome::blurred_quart(seed, x, 64, z);
                assert!((qx - (x >> 2)).abs() <= 1, "{qx} is not beside {}", x >> 2);
                assert!((qz - (z >> 2)).abs() <= 1);
                assert!((qy - (64 >> 2)).abs() <= 1);
                if (qx, qy, qz) != (x >> 2, 64 >> 2, z >> 2) {
                    moved += 1;
                }
                looked += 1;
            }
        }
        assert!(
            moved > looked / 10,
            "the blur moved {moved} of {looked} positions, which is a quart grid"
        );
        // And it is a fact about *this* world. A blur that ignored the zoom
        // seed would wobble every coast in every world identically, which is
        // the sort of wrong that still looks like a coast.
        let other = crate::noise::rng::obfuscate_seed(4321);
        assert_ne!(other, seed);
        let differ = (-40..40)
            .flat_map(|x| (-40..40).map(move |z| (x, z)))
            .filter(|&(x, z)| {
                crate::biome::blurred_quart(seed, x, 64, z)
                    != crate::biome::blurred_quart(other, x, 64, z)
            })
            .count();
        assert!(
            differ > 0,
            "two world seeds blurred the same 6,400 positions alike"
        );
    }
}
