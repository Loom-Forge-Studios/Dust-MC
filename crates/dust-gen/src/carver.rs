//! The carvers: the tunnels and the canyons that are cut through finished
//! terrain, and the reason a cave is somewhere a player can walk.
//!
//! # Where the algorithm came from
//!
//! Three things about a carver live in the operator's data pack —
//! `worldgen/configured_carver/*.json` says how thick and how deep and how
//! often, `worldgen/biome/*.json` says which biome runs which of them, and
//! `tags/block/*.json` says what a carver is allowed to cut through. All three
//! are read at run time from the pack, like everything else in this crate.
//!
//! What is *done* with those numbers is `CaveWorldCarver.java`,
//! `CanyonWorldCarver.java`, `WorldCarver.java` and
//! `ChunkGenerator.applyCarvers`. That is code, and D8's route reaches code:
//! `javap -p -c` on the inner server jar in the operator's own `.dust-extract`,
//! read through the ProGuard mappings Mojang publishes beside it. Decision
//! record 0039 lists what came out of it and what a careful guess would have
//! got wrong. **Nothing Mojang's is committed**; every arithmetic step below is
//! this project's own, and every number the world is generated *from* still
//! arrives at run time.
//!
//! # What a carver is, mechanically
//!
//! A chunk is not carved by its own carvers. It is carved by the carvers of
//! every chunk within eight in each direction — 289 of them — because a tunnel
//! that starts nine chunks away is 112 steps long and can still arrive. So
//! generating one chunk means re-drawing 289 neighbourhoods' worth of tunnels
//! and keeping only the part that lands inside. That is not waste; it is what
//! makes a chunk's contents depend on nothing but its coordinates, which is
//! what makes the world the same however the chunks are visited.

use std::collections::BTreeMap;
use std::path::Path;

use crate::aquifer::{Flow, Fluid, Substance};
use crate::noise::build::{read_json, BlockSpec, BuildError, NoiseSettings};
use crate::noise::rng::{mth_cos, mth_sin, Legacy};
use serde_json::Value;

/// `WorldCarver.getRange`, which neither overworld carver overrides.
const RANGE: i32 = 4;

/// The step from the top of a chunk that `carveEllipsoid` refuses to touch on a
/// chunk that is not being upgraded from an older world. Dust has no old
/// worlds, so it is always this.
const TOP_MARGIN: i32 = 7;

/// One turn, as the `float` a carver multiplies a `nextFloat` by.
const TAU: f32 = 6.2831855;

/// A quarter turn, as the `float` a room's radius and a branch's yaw use.
const QUARTER_TURN: f32 = 1.5707964;

/// Half a turn, as the `float` a tunnel's thickness envelope uses.
///
/// Written as the literal the bytecode carries rather than as `PI`. The two are
/// the same bits at `f32`; the literal is what a reader checking this file
/// against `javap` output is holding.
#[allow(clippy::approx_constant)]
const HALF_TURN: f32 = 3.1415927;

/// A height, as a data pack writes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
}

impl Anchor {
    fn resolve(self, min_y: i32, height: i32) -> i32 {
        match self {
            Self::Absolute(y) => y,
            Self::AboveBottom(y) => min_y + y,
            Self::BelowTop(y) => min_y + height - 1 - y,
        }
    }

    fn parse(value: &Value, path: &Path) -> Result<Self, BuildError> {
        let object = value
            .as_object()
            .ok_or_else(|| malformed(path, "a vertical anchor is an object"))?;
        let read = |key: &str| -> Option<i32> { object.get(key)?.as_i64().map(|v| v as i32) };
        if let Some(y) = read("absolute") {
            Ok(Self::Absolute(y))
        } else if let Some(y) = read("above_bottom") {
            Ok(Self::AboveBottom(y))
        } else if let Some(y) = read("below_top") {
            Ok(Self::BelowTop(y))
        } else {
            Err(malformed(
                path,
                "a vertical anchor is `absolute`, `above_bottom` or `below_top`",
            ))
        }
    }
}

/// A `FloatProvider`, which is either a bare number or a named shape.
///
/// Only the two shapes the overworld's carvers use are here. A third is
/// **refused by name** rather than folded into the nearest of these two: a
/// carver whose thickness came from the wrong distribution would generate
/// caves that look like caves and are not this world's.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Floats {
    Constant(f32),
    Uniform { min: f32, max: f32 },
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl Floats {
    fn sample(self, rng: &mut Legacy) -> f32 {
        match self {
            Self::Constant(value) => value,
            // `Mth.randomBetween`.
            Self::Uniform { min, max } => rng.next_f32() * (max - min) + min,
            // `TrapezoidFloat.sample`: two draws, and the split between them is
            // what makes the middle of the range likelier than its ends.
            Self::Trapezoid { min, max, plateau } => {
                let span = max - min;
                let ramp = (span - plateau) / 2.0;
                let flat = span - ramp;
                min + rng.next_f32() * flat + rng.next_f32() * ramp
            }
        }
    }

    fn parse(value: &Value, path: &Path) -> Result<Self, BuildError> {
        if let Some(number) = value.as_f64() {
            return Ok(Self::Constant(number as f32));
        }
        let object = value
            .as_object()
            .ok_or_else(|| malformed(path, "a float provider is a number or an object"))?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed(path, "a float provider object names a `type`"))?;
        let number = |key: &str| -> Result<f32, BuildError> {
            object
                .get(key)
                .and_then(Value::as_f64)
                .map(|v| v as f32)
                .ok_or_else(|| malformed(path, &format!("`{kind}` wants a numeric `{key}`")))
        };
        match kind {
            "minecraft:constant" => Ok(Self::Constant(number("value")?)),
            "minecraft:uniform" => Ok(Self::Uniform {
                min: number("min_inclusive")?,
                max: number("max_exclusive")?,
            }),
            "minecraft:trapezoid" => Ok(Self::Trapezoid {
                min: number("min")?,
                max: number("max")?,
                plateau: number("plateau")?,
            }),
            other => Err(BuildError::UnknownType {
                name: path.display().to_string(),
                kind: other.to_owned(),
            }),
        }
    }
}

/// A `HeightProvider`. The overworld's carvers use one shape and the others are
/// refused by name, for the reason [`Floats`] gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heights {
    Uniform { min: Anchor, max: Anchor },
}

impl Heights {
    fn sample(self, rng: &mut Legacy, min_y: i32, height: i32) -> i32 {
        match self {
            Self::Uniform { min, max } => {
                let low = min.resolve(min_y, height);
                let high = max.resolve(min_y, height);
                if low > high {
                    // Vanilla logs an empty range once and answers the low
                    // bound. A range this generator cannot draw from is the
                    // pack's business and not a reason to stop the world.
                    return low;
                }
                // `Mth.randomBetweenInclusive`.
                rng.next_i32_below(high - low + 1) + low
            }
        }
    }

    fn parse(value: &Value, path: &Path) -> Result<Self, BuildError> {
        let object = value
            .as_object()
            .ok_or_else(|| malformed(path, "a height provider is an object"))?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("minecraft:uniform");
        match kind {
            "minecraft:uniform" => Ok(Self::Uniform {
                min: Anchor::parse(
                    object
                        .get("min_inclusive")
                        .ok_or_else(|| malformed(path, "a uniform height wants `min_inclusive`"))?,
                    path,
                )?,
                max: Anchor::parse(
                    object
                        .get("max_inclusive")
                        .ok_or_else(|| malformed(path, "a uniform height wants `max_inclusive`"))?,
                    path,
                )?,
            }),
            other => Err(BuildError::UnknownType {
                name: path.display().to_string(),
                kind: other.to_owned(),
            }),
        }
    }
}

/// A canyon's cross-section.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CanyonShape {
    distance_factor: Floats,
    thickness: Floats,
    width_smoothness: i32,
    horizontal_radius_factor: Floats,
    vertical_radius_default_factor: f32,
    vertical_radius_center_factor: f32,
}

/// Which of vanilla's two overworld carvers this is, and the numbers only that
/// one reads.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Cave {
        horizontal_radius_multiplier: Floats,
        vertical_radius_multiplier: Floats,
        floor_level: Floats,
    },
    Canyon {
        vertical_rotation: Floats,
        shape: CanyonShape,
    },
}

/// One entry of `worldgen/configured_carver`.
#[derive(Debug, Clone)]
struct Configured {
    name: String,
    probability: f32,
    y: Heights,
    y_scale: Floats,
    lava_level: Anchor,
    /// Block names this carver may cut through, sorted. Kept as names because
    /// the material codes it is turned into are a *caller's* palette and this
    /// object is built before one exists.
    replaceable: Vec<String>,
    shape: Shape,
}

/// A dimension's carvers, compiled against one pack and one biome list.
#[derive(Debug, Clone)]
pub struct Carvers {
    /// The carvers every biome of this dimension names, in the order the biome
    /// names them — which is the order they are seeded in, so it is load
    /// bearing and not a list.
    list: Vec<Configured>,
    min_y: i32,
    height: i32,
    sea_level: i32,
    seed: i64,
    /// Whether each material code is a block this carver may replace, one row
    /// per carver. A flat table because the alternative is a string compare per
    /// block of every cave in the world.
    replaces: Vec<[bool; 256]>,
    /// Which material codes are `grass_block` or `mycelium`, which is the one
    /// thing `carveBlock` looks at the block for beyond whether it may go.
    grassy: [bool; 256],
    /// The material code plain `minecraft:dirt` has, when this palette has one.
    dirt: Option<u8>,
}

/// What one chunk's carving did, counted rather than assumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Chunks whose carvers were drawn, which is 289 per chunk carved.
    pub neighbours: u64,
    /// Carvers that rolled a start.
    pub starts: u64,
    /// Cells the mask let through to `carve_block`.
    pub reached: u64,
    /// Cells that changed.
    pub carved: u64,
    /// Cells the aquifer's barrier answered rock for, which vanilla leaves
    /// alone. Counted because it is the only thing that says the aquifer is
    /// still deciding at this stage rather than the carver.
    pub barred: u64,
    /// Times a carved column stood under a grass block with dirt below it —
    /// the one thing in `carveBlock` this generator declines. See D39.
    pub grass_floors: u64,
}

fn malformed(path: &Path, detail: &str) -> BuildError {
    BuildError::Malformed {
        path: path.to_path_buf(),
        detail: detail.to_owned(),
    }
}

impl Carvers {
    /// Compile the carvers every biome in `biomes` names.
    ///
    /// `None` when no biome names any, which is what the end says.
    ///
    /// **Refused rather than merged when biomes disagree.** Vanilla asks the
    /// biome at each of the 289 neighbours' own corners, which is 289 climate
    /// lookups per chunk on top of the terrain. Every biome in a vanilla
    /// overworld names the same three carvers in the same order, so the lookup
    /// cannot change the answer and is not done. A pack where it *could* change
    /// the answer gets told so by name at boot rather than getting one biome's
    /// caves everywhere, which would look right.
    pub fn over(
        data_root: &Path,
        settings: &NoiseSettings,
        seed: i64,
        biomes: &[String],
        palette: &[BlockSpec],
    ) -> Result<Option<Self>, BuildError> {
        let mut wanted: Option<(String, Vec<String>)> = None;
        for biome in biomes {
            let named = carvers_of_biome(data_root, biome)?;
            match &wanted {
                None => wanted = Some((biome.clone(), named)),
                Some((_, names)) if *names == named => {}
                Some((first, names)) => {
                    return Err(BuildError::Malformed {
                        path: biome_path(data_root, biome),
                        detail: format!(
                            "this dimension's biomes do not agree on their carvers — `{first}` \
                             names {names:?} and `{biome}` names {named:?}. Dust carves a chunk \
                             from one list and would have to look the biome up 289 times per \
                             chunk to honour two; see decision record 0039",
                        ),
                    })
                }
            }
        }
        let names = match wanted {
            Some((_, names)) if !names.is_empty() => names,
            _ => return Ok(None),
        };
        let mut tags = BTreeMap::new();
        let list = names
            .iter()
            .map(|name| configured(data_root, name, &mut tags))
            .collect::<Result<Vec<Configured>, BuildError>>()?;

        // The material codes, resolved once. `Material::Air` is code 0 and is
        // in no carver's tag; `Material::Lava` is code 3 and is in none either,
        // which is why a lava lake is not re-carved into a bigger one.
        let mut block_of = vec![String::new(); 256];
        block_of[1] = settings.default_block.name.clone();
        block_of[2] = settings.default_fluid.name.clone();
        block_of[3] = crate::aquifer::Aquifer::lava_block().name;
        for (index, spec) in palette.iter().enumerate() {
            let code = 4 + index;
            if code < 256 {
                block_of[code] = spec.name.clone();
            }
        }
        let replaces = list
            .iter()
            .map(|carver| {
                let mut row = [false; 256];
                for (code, name) in block_of.iter().enumerate() {
                    row[code] = !name.is_empty() && carver.replaceable.binary_search(name).is_ok();
                }
                row
            })
            .collect();
        let mut grassy = [false; 256];
        let mut dirt = None;
        for (code, name) in block_of.iter().enumerate() {
            grassy[code] = name == "minecraft:grass_block" || name == "minecraft:mycelium";
            if name == "minecraft:dirt" {
                dirt = Some(code as u8);
            }
        }

        Ok(Some(Self {
            list,
            min_y: settings.min_y,
            height: settings.height,
            sea_level: settings.sea_level,
            seed,
            replaces,
            grassy,
            dirt,
        }))
    }

    /// How many carvers a chunk draws, per neighbour.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The names, in seeding order, for a boot line.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.list.iter().map(|carver| carver.name.as_str())
    }

    /// One thread's scratch: the carving mask, which is one bit per cell of one
    /// chunk and is reused rather than reallocated.
    pub fn cutter(&self) -> Cutter<'_> {
        let bits = 256 * self.height as usize;
        Cutter {
            carvers: self,
            mask: vec![0u64; bits.div_ceil(64)],
            counts: Counts::default(),
        }
    }
}

/// One thread's carving state.
#[derive(Debug, Clone)]
pub struct Cutter<'a> {
    carvers: &'a Carvers,
    mask: Vec<u64>,
    counts: Counts,
}

/// Everything one carve of one chunk writes to or reads from, gathered so the
/// recursion below carries one reference and not eleven.
struct Site<'a, 'f> {
    carvers: &'a Carvers,
    which: usize,
    chunk_x: i32,
    chunk_z: i32,
    materials: &'a mut [u8],
    mask: &'a mut [u64],
    flow: Option<&'a mut Flow<'f>>,
    counts: &'a mut Counts,
}

/// The two shapes a carver skips a cell by, which is the only thing that
/// differs between a tunnel and a canyon once the ellipsoid is being walked.
enum Skip<'a> {
    Cave { floor_level: f64 },
    Canyon { widths: &'a [f32] },
}

impl Skip<'_> {
    fn should_skip(&self, min_y: i32, rel_x: f64, rel_y: f64, rel_z: f64, y: i32) -> bool {
        match self {
            // A cave's floor is flat because this clause is not symmetric: it
            // answers "skip" for everything below the floor level outright,
            // before the ellipsoid is consulted at all.
            Self::Cave { floor_level } => {
                if rel_y <= *floor_level {
                    return true;
                }
                rel_x * rel_x + rel_y * rel_y + rel_z * rel_z >= 1.0
            }
            // A canyon is a stack of independently widened slices, which is
            // what makes its walls ribbed rather than smooth.
            Self::Canyon { widths } => {
                let index = (y - min_y - 1) as usize;
                let width = f64::from(widths[index.min(widths.len() - 1)]);
                (rel_x * rel_x + rel_z * rel_z) * width + rel_y * rel_y / 6.0 >= 1.0
            }
        }
    }
}

impl Cutter<'_> {
    /// What this thread's carving has done since it started.
    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Carve one chunk in place.
    ///
    /// `materials` is the chunk the noise stage, the aquifer and the surface
    /// rules have already finished with — carvers run *after* the surface in
    /// vanilla's own pipeline, which is why `carveBlock` has a clause about
    /// grass at all.
    /// Put the counts back to what they were.
    ///
    /// The feature stage carves neighbouring chunks purely to read their column
    /// heights, and those carvings are not this chunk's.
    pub fn set_counts(&mut self, counts: Counts) {
        self.counts = counts;
    }

    pub fn carve(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        materials: &mut [u8],
        mut flow: Option<&mut Flow<'_>>,
    ) {
        for word in self.mask.iter_mut() {
            *word = 0;
        }
        let carvers = self.carvers;
        // One generator for the whole chunk, re-seeded per neighbour and per
        // carver. Vanilla seeds this one from a unique seed it then never uses,
        // which is why a carved chunk is reproducible at all.
        let mut rng = Legacy::from_seed(0);
        for offset_x in -8..=8 {
            for offset_z in -8..=8 {
                let cx = chunk_x + offset_x;
                let cz = chunk_z + offset_z;
                self.counts.neighbours += 1;
                for (which, carver) in carvers.list.iter().enumerate() {
                    rng.set_large_feature_seed(carvers.seed.wrapping_add(which as i64), cx, cz);
                    if rng.next_f32() > carver.probability {
                        continue;
                    }
                    self.counts.starts += 1;
                    let mut site = Site {
                        carvers,
                        which,
                        chunk_x,
                        chunk_z,
                        materials,
                        mask: &mut self.mask,
                        flow: flow.as_deref_mut(),
                        counts: &mut self.counts,
                    };
                    match carver.shape {
                        Shape::Cave { .. } => cave(&mut site, carver, cx, cz, &mut rng),
                        Shape::Canyon { .. } => canyon(&mut site, carver, cx, cz, &mut rng),
                    }
                }
            }
        }
    }
}

/// `CaveWorldCarver.carve`.
fn cave(site: &mut Site<'_, '_>, cfg: &Configured, cx: i32, cz: i32, rng: &mut Legacy) {
    let Shape::Cave {
        horizontal_radius_multiplier,
        vertical_radius_multiplier,
        floor_level,
    } = cfg.shape
    else {
        return;
    };
    let min_y = site.carvers.min_y;
    let height = site.carvers.height;
    // `SectionPos.sectionToBlockCoord(getRange() * 2 - 1)`.
    let span = (RANGE * 2 - 1) * 16;
    // Three nested draws, innermost first. Written out because Java evaluates
    // the argument before the call and Rust would not, and the difference is
    // the whole stream from here on.
    let a = rng.next_i32_below(15);
    let b = rng.next_i32_below(a + 1);
    let tunnels = rng.next_i32_below(b + 1);
    for _ in 0..tunnels {
        let x = f64::from(cx * 16 + rng.next_i32_below(16));
        let y = f64::from(cfg.y.sample(rng, min_y, height));
        let z = f64::from(cz * 16 + rng.next_i32_below(16));
        let horizontal = f64::from(horizontal_radius_multiplier.sample(rng));
        let vertical = f64::from(vertical_radius_multiplier.sample(rng));
        let skip = Skip::Cave {
            floor_level: f64::from(floor_level.sample(rng)),
        };
        let mut branches = 1;
        // One tunnel in four starts in a room, and the room is drawn before the
        // branch count is rolled — so a world where rooms were skipped would
        // have different tunnels, not the same tunnels without rooms.
        if rng.next_i32_below(4) == 0 {
            let y_scale = f64::from(cfg.y_scale.sample(rng));
            let thickness = 1.0 + rng.next_f32() * 6.0;
            let radius = 1.5 + f64::from(mth_sin(QUARTER_TURN) * thickness);
            carve_ellipsoid(site, cfg, x + 1.0, y, z, radius, radius * y_scale, &skip);
            branches += rng.next_i32_below(4);
        }
        for _ in 0..branches {
            let yaw = rng.next_f32() * TAU;
            let pitch = (rng.next_f32() - 0.5) / 4.0;
            let thickness = cave_thickness(rng);
            let steps = span - rng.next_i32_below(span / 4);
            let seed = rng.next_i64();
            tunnel(
                site, cfg, seed, x, y, z, horizontal, vertical, thickness, yaw, pitch, 0, steps,
                1.0, &skip,
            );
        }
    }
}

/// `CaveWorldCarver.getThickness`.
fn cave_thickness(rng: &mut Legacy) -> f32 {
    let mut thickness = rng.next_f32() * 2.0 + rng.next_f32();
    // One tunnel in ten is a big one, and the multiplier is drawn from two
    // floats rather than one so the big ones are mostly only slightly big.
    if rng.next_i32_below(10) == 0 {
        thickness *= rng.next_f32() * rng.next_f32() * 3.0 + 1.0;
    }
    thickness
}

/// `CaveWorldCarver.createTunnel`.
#[allow(clippy::too_many_arguments)]
fn tunnel(
    site: &mut Site<'_, '_>,
    cfg: &Configured,
    seed: i64,
    mut x: f64,
    mut y: f64,
    mut z: f64,
    horizontal: f64,
    vertical: f64,
    thickness: f32,
    mut yaw: f32,
    mut pitch: f32,
    from: i32,
    steps: i32,
    y_scale: f64,
    skip: &Skip<'_>,
) {
    let mut rng = Legacy::from_seed(seed);
    let split = rng.next_i32_below(steps / 2) + steps / 4;
    let steep = rng.next_i32_below(6) == 0;
    let mut yaw_drift = 0.0f32;
    let mut pitch_drift = 0.0f32;
    for step in from..steps {
        // The envelope is a half sine over the tunnel's whole length, which is
        // why a tunnel tapers at both ends rather than stopping in a wall.
        let radius = 1.5 + f64::from(mth_sin(HALF_TURN * step as f32 / steps as f32) * thickness);
        let vertical_radius = radius * y_scale;
        let flat = mth_cos(pitch);
        x += f64::from(mth_cos(yaw) * flat);
        y += f64::from(mth_sin(pitch));
        z += f64::from(mth_sin(yaw) * flat);
        pitch *= if steep { 0.92 } else { 0.7 };
        pitch += pitch_drift * 0.1;
        yaw += yaw_drift * 0.1;
        pitch_drift *= 0.9;
        yaw_drift *= 0.75;
        pitch_drift += (rng.next_f32() - rng.next_f32()) * rng.next_f32() * 2.0;
        yaw_drift += (rng.next_f32() - rng.next_f32()) * rng.next_f32() * 4.0;
        if step == split && thickness > 1.0 {
            // A fork replaces the rest of this tunnel rather than adding to it,
            // and each half is thinner than one, so a fork never forks again.
            let a = rng.next_i64();
            let a_thickness = rng.next_f32() * 0.5 + 0.5;
            tunnel(
                site,
                cfg,
                a,
                x,
                y,
                z,
                horizontal,
                vertical,
                a_thickness,
                yaw - QUARTER_TURN,
                pitch / 3.0,
                step,
                steps,
                1.0,
                skip,
            );
            let b = rng.next_i64();
            let b_thickness = rng.next_f32() * 0.5 + 0.5;
            tunnel(
                site,
                cfg,
                b,
                x,
                y,
                z,
                horizontal,
                vertical,
                b_thickness,
                yaw + QUARTER_TURN,
                pitch / 3.0,
                step,
                steps,
                1.0,
                skip,
            );
            return;
        }
        if rng.next_i32_below(4) == 0 {
            continue;
        }
        if !can_reach(site.chunk_x, site.chunk_z, x, z, step, steps, thickness) {
            return;
        }
        carve_ellipsoid(
            site,
            cfg,
            x,
            y,
            z,
            radius * horizontal,
            vertical_radius * vertical,
            skip,
        );
    }
}

/// `CanyonWorldCarver.carve`.
fn canyon(site: &mut Site<'_, '_>, cfg: &Configured, cx: i32, cz: i32, rng: &mut Legacy) {
    let Shape::Canyon {
        vertical_rotation,
        shape,
    } = cfg.shape
    else {
        return;
    };
    let min_y = site.carvers.min_y;
    let height = site.carvers.height;
    let span = (RANGE * 2 - 1) * 16;
    let x = f64::from(cx * 16 + rng.next_i32_below(16));
    let y = f64::from(cfg.y.sample(rng, min_y, height));
    let z = f64::from(cz * 16 + rng.next_i32_below(16));
    let yaw = rng.next_f32() * TAU;
    let pitch = vertical_rotation.sample(rng);
    let y_scale = f64::from(cfg.y_scale.sample(rng));
    let thickness = shape.thickness.sample(rng);
    let steps = (span as f32 * shape.distance_factor.sample(rng)) as i32;
    let seed = rng.next_i64();
    canyon_walk(
        site, cfg, &shape, seed, x, y, z, thickness, yaw, pitch, 0, steps, y_scale,
    );
}

/// `CanyonWorldCarver.doCarve`.
#[allow(clippy::too_many_arguments)]
fn canyon_walk(
    site: &mut Site<'_, '_>,
    cfg: &Configured,
    shape: &CanyonShape,
    seed: i64,
    mut x: f64,
    mut y: f64,
    mut z: f64,
    thickness: f32,
    mut yaw: f32,
    mut pitch: f32,
    from: i32,
    steps: i32,
    y_scale: f64,
) {
    let mut rng = Legacy::from_seed(seed);
    let widths = canyon_widths(site.carvers.height, shape, &mut rng);
    let mut yaw_drift = 0.0f32;
    let mut pitch_drift = 0.0f32;
    for step in from..steps {
        let mut radius =
            1.5 + f64::from(mth_sin(step as f32 * HALF_TURN / steps as f32) * thickness);
        let mut vertical_radius = radius * y_scale;
        radius *= f64::from(shape.horizontal_radius_factor.sample(&mut rng));
        vertical_radius =
            canyon_vertical_radius(shape, &mut rng, vertical_radius, steps as f32, step as f32);
        let flat = mth_cos(pitch);
        let rise = mth_sin(pitch);
        x += f64::from(mth_cos(yaw) * flat);
        y += f64::from(rise);
        z += f64::from(mth_sin(yaw) * flat);
        // A canyon's pitch decays harder and drifts less than a cave's, which
        // is what keeps it a slot in the ground rather than a tube through it.
        pitch *= 0.7;
        pitch += pitch_drift * 0.05;
        yaw += yaw_drift * 0.05;
        pitch_drift *= 0.8;
        yaw_drift *= 0.5;
        pitch_drift += (rng.next_f32() - rng.next_f32()) * rng.next_f32() * 2.0;
        yaw_drift += (rng.next_f32() - rng.next_f32()) * rng.next_f32() * 4.0;
        if rng.next_i32_below(4) == 0 {
            continue;
        }
        if !can_reach(site.chunk_x, site.chunk_z, x, z, step, steps, thickness) {
            return;
        }
        let skip = Skip::Canyon { widths: &widths };
        carve_ellipsoid(site, cfg, x, y, z, radius, vertical_radius, &skip);
    }
}

/// `CanyonWorldCarver.initWidthFactors`: one width per world row, redrawn every
/// `width_smoothness` rows, and squared.
fn canyon_widths(height: i32, shape: &CanyonShape, rng: &mut Legacy) -> Vec<f32> {
    let rows = height.max(1) as usize;
    let mut widths = Vec::with_capacity(rows);
    let mut width = 1.0f32;
    for row in 0..rows {
        if row == 0 || rng.next_i32_below(shape.width_smoothness.max(1)) == 0 {
            width = 1.0 + rng.next_f32() * rng.next_f32();
        }
        widths.push(width * width);
    }
    widths
}

/// `CanyonWorldCarver.updateVerticalRadius`.
fn canyon_vertical_radius(
    shape: &CanyonShape,
    rng: &mut Legacy,
    radius: f64,
    steps: f32,
    step: f32,
) -> f64 {
    let along = 1.0 - (0.5 - step / steps).abs() * 2.0;
    let factor = shape.vertical_radius_default_factor + shape.vertical_radius_center_factor * along;
    f64::from(factor) * radius * f64::from(rng.next_f32() * 0.25 + 0.75)
}

/// `WorldCarver.canReach`: whether a step this far along can still land in the
/// chunk being carved, given every step left could go straight at it.
fn can_reach(
    chunk_x: i32,
    chunk_z: i32,
    x: f64,
    z: f64,
    step: i32,
    steps: i32,
    thickness: f32,
) -> bool {
    let dx = x - f64::from(chunk_x * 16 + 8);
    let dz = z - f64::from(chunk_z * 16 + 8);
    let left = f64::from(steps - step);
    let reach = f64::from(thickness + 2.0 + 16.0);
    dx * dx + dz * dz - left * left <= reach * reach
}

/// `WorldCarver.carveEllipsoid`.
#[allow(clippy::too_many_arguments)]
fn carve_ellipsoid(
    site: &mut Site<'_, '_>,
    cfg: &Configured,
    x: f64,
    y: f64,
    z: f64,
    horizontal: f64,
    vertical: f64,
    skip: &Skip<'_>,
) {
    let min_y = site.carvers.min_y;
    let height = site.carvers.height;
    let middle_x = f64::from(site.chunk_x * 16 + 8);
    let middle_z = f64::from(site.chunk_z * 16 + 8);
    let reach = 16.0 + horizontal * 2.0;
    if (x - middle_x).abs() > reach || (z - middle_z).abs() > reach {
        return;
    }
    let base_x = site.chunk_x * 16;
    let base_z = site.chunk_z * 16;
    let low_x = ((x - horizontal).floor() as i32 - base_x - 1).max(0);
    let high_x = ((x + horizontal).floor() as i32 - base_x).min(15);
    let low_y = ((y - vertical).floor() as i32 - 1).max(min_y + 1);
    let high_y = ((y + vertical).floor() as i32 + 1).min(min_y + height - 1 - TOP_MARGIN);
    let low_z = ((z - horizontal).floor() as i32 - base_z - 1).max(0);
    let high_z = ((z + horizontal).floor() as i32 - base_z).min(15);
    for rel_x in low_x..=high_x {
        let block_x = base_x + rel_x;
        let dx = (f64::from(block_x) + 0.5 - x) / horizontal;
        for rel_z in low_z..=high_z {
            let block_z = base_z + rel_z;
            let dz = (f64::from(block_z) + 0.5 - z) / horizontal;
            if dx * dx + dz * dz >= 1.0 {
                continue;
            }
            // One flag per column, and it is set on the way *down*: a cave that
            // opens under a meadow carries that fact to the floor it lands on.
            let mut saw_grass = false;
            let mut block_y = high_y;
            while block_y > low_y {
                let dy = (f64::from(block_y) - 0.5 - y) / vertical;
                if !skip.should_skip(min_y, dx, dy, dz, block_y) {
                    let bit =
                        ((block_y - min_y) as usize) * 256 + (rel_z as usize) * 16 + rel_x as usize;
                    let word = bit / 64;
                    let mask = 1u64 << (bit % 64);
                    if site.mask[word] & mask == 0 {
                        site.mask[word] |= mask;
                        site.counts.reached += 1;
                        carve_block(site, cfg, block_x, block_y, block_z, &mut saw_grass);
                    }
                }
                block_y -= 1;
            }
        }
    }
}

/// `WorldCarver.carveBlock` and `WorldCarver.getCarveState`, together.
fn carve_block(
    site: &mut Site<'_, '_>,
    cfg: &Configured,
    x: i32,
    y: i32,
    z: i32,
    saw_grass: &mut bool,
) {
    let min_y = site.carvers.min_y;
    let index =
        (y - min_y) as usize * 256 + (z.rem_euclid(16) as usize) * 16 + x.rem_euclid(16) as usize;
    let code = site.materials[index];
    if site.carvers.grassy[code as usize] {
        *saw_grass = true;
    }
    if !site.carvers.replaces[site.which][code as usize] {
        return;
    }
    let carved = if y <= cfg.lava_level.resolve(min_y, site.carvers.height) {
        // Under the lava level a carver does not ask the aquifer at all. This
        // is what stops a tunnel draining the bottom of the world into air.
        crate::terrain::Material::Lava
    } else {
        let substance = match site.flow.as_deref_mut() {
            Some(flow) => flow.substance(x, y, z, 0.0),
            // A dimension whose settings turn aquifers off still carves, and
            // vanilla hands the carver the same global fluid picker the noise
            // stage uses: the default fluid below sea level and air above.
            None => {
                if y < site.carvers.sea_level.min(-54) {
                    Substance::Fluid(Fluid::Lava)
                } else if y < site.carvers.sea_level {
                    Substance::Fluid(Fluid::Default)
                } else {
                    Substance::Air
                }
            }
        };
        match substance {
            // Vanilla returns null and leaves the block standing, which is the
            // aquifer's barrier holding a wall up inside a cave.
            Substance::Rock => {
                site.counts.barred += 1;
                return;
            }
            Substance::Air => crate::terrain::Material::Air,
            Substance::Fluid(Fluid::Default) => crate::terrain::Material::Fluid,
            Substance::Fluid(Fluid::Lava) => crate::terrain::Material::Lava,
        }
    };
    site.materials[index] = carved.code();
    site.counts.carved += 1;
    if *saw_grass && y > min_y {
        let below = index - 256;
        if site.carvers.is_dirt(site.materials[below]) {
            site.counts.grass_floors += 1;
        }
    }
}

impl Carvers {
    /// Whether a material code is plain dirt, which is the block vanilla
    /// re-runs the surface rules on under a carved-away grass block.
    fn is_dirt(&self, code: u8) -> bool {
        self.dirt == Some(code)
    }
}

fn biome_path(data_root: &Path, biome: &str) -> std::path::PathBuf {
    let (namespace, name) = split_id(biome);
    data_root.join(format!("{namespace}/worldgen/biome/{name}.json"))
}

/// Which configured carvers a biome names, in the order it names them.
fn carvers_of_biome(data_root: &Path, biome: &str) -> Result<Vec<String>, BuildError> {
    let path = biome_path(data_root, biome);
    let json = read_json(&path)?;
    let Some(carvers) = json.get("carvers") else {
        return Ok(Vec::new());
    };
    let object = carvers
        .as_object()
        .ok_or_else(|| malformed(&path, "`carvers` is an object keyed by carving step"))?;
    let mut named = Vec::new();
    for (step, value) in object {
        if step != "air" {
            // 1.18 removed the liquid step and 1.21.1 has only this one. A pack
            // that names another is naming something this generator has no
            // place to run, and saying so beats running it in the wrong place.
            return Err(malformed(
                &path,
                &format!("`{step}` is not a carving step this generator knows; only `air` is"),
            ));
        }
        match value {
            Value::String(one) => named.push(one.clone()),
            Value::Array(list) => {
                for entry in list {
                    named.push(
                        entry
                            .as_str()
                            .ok_or_else(|| malformed(&path, "a carver is named by a string"))?
                            .to_owned(),
                    );
                }
            }
            _ => {
                return Err(malformed(
                    &path,
                    "a carving step names one carver or a list",
                ))
            }
        }
    }
    Ok(named)
}

/// Read one `worldgen/configured_carver` entry.
fn configured(
    data_root: &Path,
    name: &str,
    tags: &mut BTreeMap<String, Vec<String>>,
) -> Result<Configured, BuildError> {
    let (namespace, id) = split_id(name);
    let path = data_root.join(format!("{namespace}/worldgen/configured_carver/{id}.json"));
    let json = read_json(&path)?;
    let kind = json
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(&path, "a configured carver names a `type`"))?
        .to_owned();
    let config = json
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed(&path, "a configured carver carries a `config`"))?;
    let field = |key: &str| -> Result<&Value, BuildError> {
        config
            .get(key)
            .ok_or_else(|| malformed(&path, &format!("`config` wants `{key}`")))
    };
    let probability = field("probability")?
        .as_f64()
        .ok_or_else(|| malformed(&path, "`probability` is a number"))? as f32;
    let y = Heights::parse(field("y")?, &path)?;
    let y_scale = Floats::parse(field("yScale")?, &path)?;
    let lava_level = Anchor::parse(field("lava_level")?, &path)?;
    let replaceable = replaceable_blocks(data_root, field("replaceable")?, &path, tags)?;
    let shape = match kind.as_str() {
        "minecraft:cave" | "minecraft:nether_cave" => Shape::Cave {
            horizontal_radius_multiplier: Floats::parse(
                field("horizontal_radius_multiplier")?,
                &path,
            )?,
            vertical_radius_multiplier: Floats::parse(field("vertical_radius_multiplier")?, &path)?,
            floor_level: Floats::parse(field("floor_level")?, &path)?,
        },
        "minecraft:canyon" => {
            let shape = field("shape")?
                .as_object()
                .ok_or_else(|| malformed(&path, "a canyon carries a `shape`"))?;
            let inner = |key: &str| -> Result<&Value, BuildError> {
                shape
                    .get(key)
                    .ok_or_else(|| malformed(&path, &format!("`shape` wants `{key}`")))
            };
            Shape::Canyon {
                vertical_rotation: Floats::parse(field("vertical_rotation")?, &path)?,
                shape: CanyonShape {
                    distance_factor: Floats::parse(inner("distance_factor")?, &path)?,
                    thickness: Floats::parse(inner("thickness")?, &path)?,
                    width_smoothness: inner("width_smoothness")?
                        .as_i64()
                        .ok_or_else(|| malformed(&path, "`width_smoothness` is an integer"))?
                        as i32,
                    horizontal_radius_factor: Floats::parse(
                        inner("horizontal_radius_factor")?,
                        &path,
                    )?,
                    vertical_radius_default_factor: inner("vertical_radius_default_factor")?
                        .as_f64()
                        .ok_or_else(|| {
                            malformed(&path, "`vertical_radius_default_factor` is a number")
                        })? as f32,
                    vertical_radius_center_factor: inner("vertical_radius_center_factor")?
                        .as_f64()
                        .ok_or_else(|| {
                            malformed(&path, "`vertical_radius_center_factor` is a number")
                        })? as f32,
                },
            }
        }
        other => {
            return Err(BuildError::UnknownType {
                name: name.to_owned(),
                kind: other.to_owned(),
            })
        }
    };
    Ok(Configured {
        name: name.to_owned(),
        probability,
        y,
        y_scale,
        lava_level,
        replaceable,
        shape,
    })
}

/// Expand a `replaceable`, which is a block tag, a list, or one block.
fn replaceable_blocks(
    data_root: &Path,
    value: &Value,
    path: &Path,
    tags: &mut BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>, BuildError> {
    let mut out = Vec::new();
    collect_entry(data_root, value, path, tags, &mut out, 0)?;
    out.sort();
    out.dedup();
    Ok(out)
}

fn collect_entry(
    data_root: &Path,
    value: &Value,
    path: &Path,
    tags: &mut BTreeMap<String, Vec<String>>,
    out: &mut Vec<String>,
    depth: usize,
) -> Result<(), BuildError> {
    if depth > 16 {
        return Err(malformed(path, "a block tag refers to itself"));
    }
    match value {
        Value::String(text) => {
            if let Some(tag) = text.strip_prefix('#') {
                let members = block_tag(data_root, tag, tags, depth)?;
                out.extend(members);
            } else {
                out.push(normalise(text));
            }
            Ok(())
        }
        Value::Array(list) => {
            for entry in list {
                collect_entry(data_root, entry, path, tags, out, depth)?;
            }
            Ok(())
        }
        Value::Object(object) => {
            let id = object
                .get("id")
                .ok_or_else(|| malformed(path, "a tag entry object carries an `id`"))?;
            collect_entry(data_root, id, path, tags, out, depth)
        }
        _ => Err(malformed(
            path,
            "a tag entry is a string, a list or an object",
        )),
    }
}

/// Read one block tag out of the pack, memoised: `#minecraft:dirt` is under
/// three of the six tags the carver tag names.
fn block_tag(
    data_root: &Path,
    tag: &str,
    tags: &mut BTreeMap<String, Vec<String>>,
    depth: usize,
) -> Result<Vec<String>, BuildError> {
    if let Some(known) = tags.get(tag) {
        return Ok(known.clone());
    }
    let (namespace, name) = split_id(tag);
    let path = data_root.join(format!("{namespace}/tags/block/{name}.json"));
    let json = read_json(&path)?;
    let values = json
        .get("values")
        .ok_or_else(|| malformed(&path, "a block tag carries `values`"))?;
    let mut members = Vec::new();
    collect_entry(data_root, values, &path, tags, &mut members, depth + 1)?;
    members.sort();
    members.dedup();
    tags.insert(tag.to_owned(), members.clone());
    Ok(members)
}

/// `minecraft:x` and `x` are the same id, and a pack writes both.
fn normalise(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn split_id(id: &str) -> (&str, &str) {
    match id.split_once(':') {
        Some((namespace, name)) => (namespace, name),
        None => ("minecraft", id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::rng::{mth_cos, mth_sin, Legacy};

    /// A data pack on disk, written by the test. Nothing of Mojang's is needed
    /// to check that the reader reads or that the tunnel walks.
    struct Pack {
        dir: std::path::PathBuf,
    }

    impl Pack {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("dust-gen-carver-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self { dir }
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        }

        fn biome(&self, name: &str, carvers: &str) {
            self.write(
                &format!("minecraft/worldgen/biome/{name}.json"),
                &format!(r#"{{"carvers": {carvers}}}"#),
            );
        }

        /// A cave carver that always starts, always cuts, and cuts well above
        /// the sea level so that what it leaves behind is air and not water.
        fn cave(&self, name: &str, probability: f64) {
            self.write(
                &format!("minecraft/worldgen/configured_carver/{name}.json"),
                &format!(
                    r##"{{"type": "minecraft:cave", "config": {{
                        "probability": {probability},
                        "y": {{"type": "minecraft:uniform",
                               "min_inclusive": {{"absolute": 100}},
                               "max_inclusive": {{"absolute": 180}}}},
                        "yScale": {{"type": "minecraft:uniform",
                                    "min_inclusive": 0.1, "max_exclusive": 0.9}},
                        "lava_level": {{"above_bottom": 8}},
                        "replaceable": "#minecraft:test_replaceables",
                        "horizontal_radius_multiplier": {{"type": "minecraft:uniform",
                            "min_inclusive": 0.7, "max_exclusive": 1.4}},
                        "vertical_radius_multiplier": {{"type": "minecraft:uniform",
                            "min_inclusive": 0.8, "max_exclusive": 1.3}},
                        "floor_level": {{"type": "minecraft:uniform",
                            "min_inclusive": -1.0, "max_exclusive": -0.4}}
                    }}}}"##
                ),
            );
        }

        /// A tag that reaches its second member only through another tag, so a
        /// reader that stopped at the first level would be caught.
        fn tags(&self) {
            self.write(
                "minecraft/tags/block/test_replaceables.json",
                r##"{"values": ["minecraft:stone", "#minecraft:test_nested"]}"##,
            );
            self.write(
                "minecraft/tags/block/test_nested.json",
                r#"{"values": ["minecraft:dirt"]}"#,
            );
        }
    }

    impl Drop for Pack {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn settings() -> NoiseSettings {
        NoiseSettings {
            min_y: -64,
            height: 384,
            cell_width: 4,
            cell_height: 8,
            sea_level: 63,
            default_block: spec("minecraft:stone"),
            default_fluid: spec("minecraft:water"),
            aquifers_enabled: false,
        }
    }

    fn spec(name: &str) -> BlockSpec {
        BlockSpec {
            name: name.to_owned(),
            properties: Vec::new(),
        }
    }

    /// A chunk of one material, `256 * height` codes.
    fn filled(code: u8) -> Vec<u8> {
        vec![code; 256 * 384]
    }

    fn changed(before: &[u8], after: &[u8]) -> usize {
        before
            .iter()
            .zip(after)
            .filter(|(was, now)| was != now)
            .count()
    }

    /// **The stream is `java.util.Random` and nothing near it.**
    ///
    /// Every value below came out of a JDK on this machine, not out of this
    /// file: `new Random(0L)` and friends, printed by a five-line Java program.
    /// A generator that is one draw out of step generates a different world
    /// that still looks like a world, and only a golden says so.
    #[test]
    fn legacy_is_java_util_random() {
        let mut rng = Legacy::from_seed(0);
        assert_eq!(rng.next_i64(), -4962768465676381896);
        assert_eq!(rng.next_i64(), 4437113781045784766);

        let mut rng = Legacy::from_seed(0);
        assert_eq!(
            [
                rng.next_f32(),
                rng.next_f32(),
                rng.next_f32(),
                rng.next_f32()
            ],
            [0.73096776, 0.831441, 0.24053639, 0.6063452]
        );

        // Fifteen is not a power of two and sixteen is, and they take different
        // paths through `nextInt`. Both are drawn by a cave carver on its first
        // two lines, so getting one of them wrong would be most of the world.
        let mut rng = Legacy::from_seed(0);
        let fifteen: Vec<i32> = (0..8).map(|_| rng.next_i32_below(15)).collect();
        assert_eq!(fifteen, vec![0, 13, 4, 2, 5, 8, 11, 6]);
        let mut rng = Legacy::from_seed(0);
        let sixteen: Vec<i32> = (0..8).map(|_| rng.next_i32_below(16)).collect();
        assert_eq!(sixteen, vec![11, 13, 3, 9, 10, 4, 8, 1]);

        let mut rng = Legacy::from_seed(-42);
        let tens: Vec<i32> = (0..5).map(|_| rng.next_i32_below(10)).collect();
        assert_eq!(tens, vec![5, 6, 2, 6, 6]);
        assert_eq!(rng.next_f32(), 0.78621805);
    }

    /// **A carver turns by a table, not by a sine.**
    ///
    /// `Mth.sin` is 65,536 samples indexed by a truncated `float`, and the gap
    /// between it and the real function is what a tunnel accumulates over a
    /// hundred steps. This check is a differential against `f32::sin`, and it
    /// requires them to *disagree*: a version of this file that called the real
    /// function would pass every other test here and generate a world whose
    /// caves are in the wrong place.
    #[test]
    fn sin_is_the_table_and_not_the_function() {
        // A quarter turn lands exactly on entry 16,384, which is why a room's
        // radius is its thickness and not something one part in ten thousand
        // off it.
        assert_eq!(mth_sin(QUARTER_TURN), 1.0);
        assert_eq!(mth_cos(0.0), 1.0);
        // And one that does not land on an entry.
        let angle = 1.234_f32;
        assert_ne!(mth_sin(angle), angle.sin());
        assert!((f64::from(mth_sin(angle)) - f64::from(angle.sin())).abs() > 1e-6);
        // The table is a sine of the *index*, so the entry either side of a
        // value brackets it.
        let index = (angle * 10430.378) as i32 & 65535;
        let expected = ((f64::from(index) * std::f64::consts::PI * 2.0) / 65536.0).sin() as f32;
        assert_eq!(mth_sin(angle), expected);
    }

    /// **A carver cuts what its tag names and nothing else, and the tag is
    /// followed through the tag it names.**
    ///
    /// Three chunks of one block each, carved by the same carver at the same
    /// coordinates. Stone is named directly, dirt only through a nested tag,
    /// and bedrock not at all. A reader that stopped at the first level of the
    /// tag would leave the dirt standing; a carver that ignored the tag would
    /// cut the bedrock.
    #[test]
    fn a_carver_cuts_what_its_tag_names_and_follows_the_tag_it_names() {
        let pack = Pack::new("tag");
        pack.tags();
        pack.cave("cave", 1.0);
        pack.biome("plains", r#"{"air": ["minecraft:cave"]}"#);
        // Palette code 4 is dirt, 5 is bedrock.
        let palette = [spec("minecraft:dirt"), spec("minecraft:bedrock")];
        let carvers = Carvers::over(
            &pack.dir,
            &settings(),
            7,
            &["minecraft:plains".into()],
            &palette,
        )
        .expect("the pack reads")
        .expect("a biome names a carver");

        let mut stone = filled(1);
        let mut dirt = filled(4);
        let mut bedrock = filled(5);
        let mut cutter = carvers.cutter();
        cutter.carve(0, 0, &mut stone, None);
        cutter.carve(0, 0, &mut dirt, None);
        cutter.carve(0, 0, &mut bedrock, None);

        let stone_cut = changed(&filled(1), &stone);
        let dirt_cut = changed(&filled(4), &dirt);
        let bedrock_cut = changed(&filled(5), &bedrock);
        assert!(stone_cut > 0, "the carver cut nothing at all");
        assert_eq!(
            stone_cut, dirt_cut,
            "stone is named directly and dirt through a nested tag; the same cells should go"
        );
        assert_eq!(bedrock_cut, 0, "bedrock is in no tag this carver names");
    }

    /// **Every chunk within eight is drawn, and a probability of zero draws
    /// none of them.**
    ///
    /// 289 is the whole of why a tunnel that starts nine chunks away still
    /// arrives. A narrower neighbourhood would leave holes at the seams that
    /// no single-chunk test could see.
    #[test]
    fn a_chunk_draws_the_carvers_of_every_chunk_within_eight() {
        let pack = Pack::new("reach");
        pack.tags();
        pack.cave("always", 1.0);
        pack.cave("never", 0.0);
        pack.biome(
            "plains",
            r#"{"air": ["minecraft:always", "minecraft:never"]}"#,
        );
        let carvers = Carvers::over(&pack.dir, &settings(), 3, &["minecraft:plains".into()], &[])
            .expect("the pack reads")
            .expect("a biome names carvers");
        assert_eq!(carvers.len(), 2);

        let mut materials = filled(1);
        let mut cutter = carvers.cutter();
        cutter.carve(0, 0, &mut materials, None);
        let counts = cutter.counts();
        assert_eq!(counts.neighbours, 289);
        // One of the two always starts and the other never does, so the count
        // separates the loop bounds from the probability test.
        assert_eq!(counts.starts, 289);
    }

    /// **A chunk is carved the same however it is visited.**
    ///
    /// The mask is scratch and not state. A `Cutter` that carried a mask
    /// between chunks would leave the second chunk it saw with holes the first
    /// one had already claimed, and the world would depend on which player
    /// walked where first.
    #[test]
    fn a_chunk_is_carved_the_same_however_it_is_visited() {
        let pack = Pack::new("order");
        pack.tags();
        pack.cave("cave", 0.4);
        pack.biome("plains", r#"{"air": "minecraft:cave"}"#);
        let carvers = Carvers::over(
            &pack.dir,
            &settings(),
            11,
            &["minecraft:plains".into()],
            &[],
        )
        .expect("the pack reads")
        .expect("a biome names a carver");

        let mut alone = filled(1);
        carvers.cutter().carve(3, -5, &mut alone, None);

        let mut after = filled(1);
        let mut cutter = carvers.cutter();
        let mut elsewhere = filled(1);
        cutter.carve(40, 40, &mut elsewhere, None);
        cutter.carve(-7, 2, &mut filled(1), None);
        cutter.carve(3, -5, &mut after, None);

        assert!(
            changed(&filled(1), &alone) > 0,
            "the fixture carved nothing, so this proves nothing"
        );
        assert_eq!(alone, after);
    }

    /// **Biomes that disagree about their carvers are refused by name.**
    ///
    /// Not merged, and not silently given the first one's list. See D39: the
    /// alternative is a biome lookup at each of 289 corners per chunk, and a
    /// world that quietly used one biome's caves everywhere would look right.
    #[test]
    fn biomes_that_disagree_about_their_carvers_are_refused() {
        let pack = Pack::new("disagree");
        pack.tags();
        pack.cave("cave", 0.4);
        pack.cave("other", 0.4);
        pack.biome("plains", r#"{"air": ["minecraft:cave"]}"#);
        pack.biome("desert", r#"{"air": ["minecraft:other"]}"#);
        let error = Carvers::over(
            &pack.dir,
            &settings(),
            0,
            &["minecraft:desert".into(), "minecraft:plains".into()],
            &[],
        )
        .expect_err("two lists is a refusal");
        let text = error.to_string();
        assert!(text.contains("minecraft:desert"), "{text}");
        assert!(text.contains("minecraft:plains"), "{text}");

        // And the same two biomes agreeing is not a refusal, so the check is
        // about the disagreement and not about having two biomes.
        pack.biome("desert", r#"{"air": ["minecraft:cave"]}"#);
        Carvers::over(
            &pack.dir,
            &settings(),
            0,
            &["minecraft:desert".into(), "minecraft:plains".into()],
            &[],
        )
        .expect("agreeing biomes read");
    }

    /// A dimension whose biomes name nothing has no carvers, rather than an
    /// empty list that costs 289 draws a chunk to produce nothing.
    #[test]
    fn a_dimension_whose_biomes_name_no_carver_has_none() {
        let pack = Pack::new("none");
        pack.biome("the_void", "{}");
        assert!(Carvers::over(
            &pack.dir,
            &settings(),
            0,
            &["minecraft:the_void".into()],
            &[]
        )
        .expect("the pack reads")
        .is_none());
    }
}
