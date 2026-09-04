//! Density functions: the small language Minecraft's worldgen is written in.
//!
//! A density function is a pure function of a block position. Vanilla defines
//! them as JSON in its own data pack, references them by name, and wires six of
//! them together into the climate half of the noise router — temperature,
//! vegetation, continentalness, erosion, depth and weirdness. This module is
//! the evaluator for that language and holds none of the language's content:
//! the graph is built at run time from whatever data pack the operator has, so
//! a pack that reshapes the overworld reshapes Dust's biomes too.
//!
//! # Why an arena rather than a tree of boxes
//!
//! Vanilla's climate graph is a DAG, not a tree: `shift_x` appears in five
//! shifted noises and `overworld/continents` in both the offset spline and the
//! biome sample. Held as a tree it would be *evaluated* five times. Held as
//! indices into one `Vec`, each node is evaluated once per point and the memo
//! below is a flat array rather than a map.
//!
//! # The two caches, and why they are not an optimisation
//!
//! `flat_cache` and `cache_2d` are nodes in vanilla's own language, and what
//! they mean is "this value does not depend on y". Honouring them is what makes
//! a column of 24 biome cells cost one continentalness sample instead of 24.
//! The point memo below is the other half: within one point, a node reached
//! twice is computed once. Both are exact — every node here is a pure function
//! of the point — so neither can change an answer, only the time it takes.

use super::blended::BlendedNoise;
use super::perlin::NormalNoise;

/// One node of a compiled density-function graph.
///
/// Indices refer to earlier entries in the same arena, so evaluation never has
/// to guard against a cycle: the builder cannot produce one.
#[derive(Debug, Clone, Copy)]
pub enum Node {
    Constant(f64),
    Abs(usize),
    Add(usize, usize),
    Mul(usize, usize),
    Min(usize, usize),
    Max(usize, usize),
    /// Old-world blending, which a world with no old chunks in it answers
    /// with a constant. Kept as its own node rather than folded to 1.0 and 0.0
    /// so the graph still says what vanilla's said.
    BlendAlpha,
    BlendOffset,
    /// `cache_once`: a marker with no effect at a point.
    Passthrough(usize),
    /// `cache_2d`: a memo on the block column. Exact — the argument is a
    /// function of x and z alone.
    ColumnCache(usize),
    /// `flat_cache`: **not** the same node as `cache_2d`, and the difference
    /// is an answer and not a speed.
    ///
    /// Vanilla's `FlatCache` fills a table over the chunk's *quart* grid at
    /// y = 0 and every block in a 4x4 column reads the same entry. So the
    /// value at x = 5 is the value computed at x = 4. A memo keyed on the
    /// exact block column would be a different generator — smoother, and not
    /// Minecraft's.
    FlatCache(usize),
    /// `interpolated`: computed at the corners of a 4x8x4 cell and lerped
    /// inside it. The terrain's shape *is* this interpolation; a version that
    /// evaluated the argument at every block would be a different world.
    ///
    /// The payload is a slot in [`Graph::interpolated`], which holds the
    /// argument; the node itself carries no argument index, so nothing can
    /// evaluate one by accident at a position that is not a corner.
    Interpolated(usize),
    Clamp {
        argument: usize,
        min: f64,
        max: f64,
    },
    Square(usize),
    Cube(usize),
    HalfNegative(usize),
    QuarterNegative(usize),
    Squeeze(usize),
    /// Only one branch is evaluated, which is why a cave function can name a
    /// hundred nodes and cost none of them above the surface.
    RangeChoice {
        input: usize,
        min_inclusive: f64,
        max_exclusive: f64,
        in_range: usize,
        out_of_range: usize,
    },
    /// `weird_scaled_sampler`: a noise whose *scale* is chosen by another
    /// function, in four or five steps.
    WeirdScaledSampler {
        input: usize,
        noise: usize,
        rarity: Rarity,
    },
    /// `old_blended_noise`, indexed into the graph's blended noises.
    Blended(usize),
    /// `shift_a`: the offset noise sampled at (x, 0, z).
    ShiftA(usize),
    /// `shift_b`: the offset noise sampled at (z, x, 0). Not a typo — that
    /// rotation is how one noise yields two independent-looking offsets.
    ShiftB(usize),
    ShiftedNoise {
        noise: usize,
        shift_x: usize,
        shift_y: usize,
        shift_z: usize,
        xz_scale: f64,
        y_scale: f64,
    },
    Noise {
        noise: usize,
        xz_scale: f64,
        y_scale: f64,
    },
    Spline(usize),
    YClampedGradient {
        from_y: f64,
        to_y: f64,
        from_value: f64,
        to_value: f64,
    },
}

/// A point on a cubic spline: where it is, what it is worth there, and how
/// steeply it leaves.
#[derive(Debug, Clone)]
pub struct SplinePoint {
    pub location: f32,
    pub value: SplineValue,
    pub derivative: f32,
}

/// A spline's value at a point is either a number or another spline.
#[derive(Debug, Clone)]
pub enum SplineValue {
    Constant(f32),
    Nested(usize),
}

/// A spline over one density function.
#[derive(Debug, Clone)]
pub struct Spline {
    /// The node whose value picks the interval.
    pub coordinate: usize,
    pub points: Vec<SplinePoint>,
}

/// The two step functions `weird_scaled_sampler` picks a scale with.
///
/// Named after the data pack's own spelling rather than after what they do,
/// because what they do is a table with no rule in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rarity {
    Type1,
    Type2,
}

impl Rarity {
    /// The scale this mapper gives a value. Steps, not a curve.
    pub fn scale(self, value: f64) -> f64 {
        match self {
            Self::Type1 => {
                if value < -0.5 {
                    0.75
                } else if value < 0.0 {
                    1.0
                } else if value < 0.5 {
                    1.5
                } else {
                    2.0
                }
            }
            Self::Type2 => {
                if value < -0.75 {
                    0.5
                } else if value < -0.5 {
                    0.75
                } else if value < 0.5 {
                    1.0
                } else if value < 0.75 {
                    2.0
                } else {
                    3.0
                }
            }
        }
    }
}

/// A compiled graph, its splines, and the noises it names.
///
/// `Default` is the empty graph — no nodes, no noises — which is what a caller
/// that has to hand over an evaluator it will never ask anything of uses.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub splines: Vec<Spline>,
    pub noises: Vec<NormalNoise>,
    /// One entry per `old_blended_noise` in the graph.
    pub blended: Vec<BlendedNoise>,
    /// One entry per `interpolated` node: the argument it samples at cell
    /// corners. The index into this is the slot the node carries.
    pub interpolated: Vec<usize>,
}

/// A graph plus the scratch space one thread needs to evaluate it.
///
/// Separate from [`Graph`] because the graph is shared and immutable while the
/// memo is per-thread: two threads generating two chunks share every noise
/// table and nothing else.
#[derive(Debug, Clone)]
pub struct Evaluator<'a> {
    graph: &'a Graph,
    point_memo: Vec<f64>,
    point_stamp: Vec<u64>,
    point_generation: u64,
    column_memo: Vec<f64>,
    column_stamp: Vec<u64>,
    column_generation: u64,
    column: (i32, i32),
    flat_memo: Vec<f64>,
    flat_stamp: Vec<u64>,
    flat_generation: u64,
    quart: (i32, i32),
    /// One stamp counter for all three memos, so a stamp is unique across the
    /// evaluator's life. That is what lets a nested evaluation at another
    /// position borrow the memos and hand them back untouched: the outer
    /// point's entries keep their own stamp and the inner point's can never
    /// collide with it.
    stamps: u64,
    /// The current value of each `interpolated` node, one slot each.
    lattice: Vec<f64>,
    /// The interval each `interpolated` node is confined to over the whole of
    /// the cell now being filled.
    lattice_bounds: Vec<(f64, f64)>,
    bounds_memo: Vec<(f64, f64)>,
    bounds_stamp: Vec<u64>,
    bounds_generation: u64,
}

impl<'a> Evaluator<'a> {
    pub fn new(graph: &'a Graph) -> Self {
        let size = graph.nodes.len();
        Self {
            graph,
            point_memo: vec![0.0; size],
            // Never zero: the stamp counter starts at zero and only ever
            // counts up, so an unwritten slot cannot match a live generation.
            // A zero-initialised stamp beside a zero-initialised generation is
            // a memo that answers before it has been filled.
            point_stamp: vec![u64::MAX; size],
            point_generation: 0,
            column_memo: vec![0.0; size],
            column_stamp: vec![u64::MAX; size],
            column_generation: 0,
            column: (i32::MIN, i32::MIN),
            flat_memo: vec![0.0; size],
            flat_stamp: vec![u64::MAX; size],
            flat_generation: 0,
            quart: (i32::MIN, i32::MIN),
            stamps: 0,
            lattice: vec![0.0; graph.interpolated.len()],
            lattice_bounds: vec![(f64::NEG_INFINITY, f64::INFINITY); graph.interpolated.len()],
            bounds_memo: vec![(0.0, 0.0); size],
            bounds_stamp: vec![u64::MAX; size],
            bounds_generation: 0,
        }
    }

    /// The value of one `interpolated` node's argument at a lattice corner.
    ///
    /// This is the only way to reach that argument, and it is deliberately not
    /// `compute`: everywhere else the node answers with whatever
    /// [`Self::set_interpolated`] last put in its slot, which is the lerp
    /// inside the cell.
    pub fn corner(&mut self, slot: usize, x: i32, y: i32, z: i32) -> f64 {
        let argument = self.graph.interpolated[slot];
        self.elsewhere(x, z, |evaluator| evaluator.eval(argument, x, y, z))
    }

    /// Put a slot's value for the block about to be asked for.
    pub fn set_interpolated(&mut self, slot: usize, value: f64) {
        self.lattice[slot] = value;
    }

    /// Say which cell is being filled, by handing over every interpolated
    /// node's eight corners.
    ///
    /// A trilinear interpolation is a convex combination of its corners, so
    /// the smallest and the largest of the eight bound the node over the whole
    /// cell — exactly, with nothing assumed.
    pub fn enter_cell(&mut self, corners: impl Fn(usize) -> [f64; 8]) {
        for slot in 0..self.lattice_bounds.len() {
            let eight = corners(slot);
            let mut low = eight[0];
            let mut high = eight[0];
            for value in &eight[1..] {
                low = low.min(*value);
                high = high.max(*value);
            }
            self.lattice_bounds[slot] = (low, high);
        }
        self.stamps += 1;
        self.bounds_generation = self.stamps;
    }

    /// The interval `root` is confined to anywhere in the cell
    /// [`Self::enter_cell`] named.
    ///
    /// **This can only ever be wider than the truth.** Every arm below is the
    /// interval arithmetic for its node, and every node whose value depends on
    /// the position inside the cell — a noise, a spline, the old blended noise
    /// — answers with the widest interval it could ever take anywhere, or with
    /// an infinite one where that is not known. A caller may therefore act on
    /// `high <= 0` or `low > 0` and cannot act on anything else.
    ///
    /// The walk stops at an `interpolated` node, which is why it is cheap: the
    /// noise under it is bounded by the eight corners that were sampled
    /// anyway, and the forty octaves below that are never reached.
    pub fn cell_bounds(&mut self, root: usize) -> (f64, f64) {
        if self.bounds_stamp[root] == self.bounds_generation {
            return self.bounds_memo[root];
        }
        let value = self.bounds_uncached(root);
        self.bounds_stamp[root] = self.bounds_generation;
        self.bounds_memo[root] = value;
        value
    }

    fn bounds_uncached(&mut self, index: usize) -> (f64, f64) {
        let unknown = (f64::NEG_INFINITY, f64::INFINITY);
        match self.graph.nodes[index] {
            Node::Constant(value) => (value, value),
            Node::Interpolated(slot) => self.lattice_bounds[slot],
            Node::Passthrough(a) | Node::ColumnCache(a) | Node::FlatCache(a) => self.cell_bounds(a),
            Node::Abs(a) => {
                let (low, high) = self.cell_bounds(a);
                if low >= 0.0 {
                    (low, high)
                } else if high <= 0.0 {
                    (-high, -low)
                } else {
                    (0.0, (-low).max(high))
                }
            }
            Node::Add(a, b) => {
                let (al, ah) = self.cell_bounds(a);
                let (bl, bh) = self.cell_bounds(b);
                (al + bl, ah + bh)
            }
            Node::Mul(a, b) => {
                let (al, ah) = self.cell_bounds(a);
                let (bl, bh) = self.cell_bounds(b);
                let corners = [al * bl, al * bh, ah * bl, ah * bh];
                if corners.iter().any(|value| value.is_nan()) {
                    unknown
                } else {
                    corners
                        .iter()
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| {
                            (l.min(*v), h.max(*v))
                        })
                }
            }
            Node::Min(a, b) => {
                let (al, ah) = self.cell_bounds(a);
                let (bl, bh) = self.cell_bounds(b);
                (al.min(bl), ah.min(bh))
            }
            Node::Max(a, b) => {
                let (al, ah) = self.cell_bounds(a);
                let (bl, bh) = self.cell_bounds(b);
                (al.max(bl), ah.max(bh))
            }
            Node::BlendAlpha => (1.0, 1.0),
            Node::BlendOffset => (0.0, 0.0),
            Node::Clamp { argument, min, max } => {
                let (low, high) = self.cell_bounds(argument);
                (low.clamp(min, max), high.clamp(min, max))
            }
            Node::Square(a) => {
                let (low, high) = self.cell_bounds(a);
                let ends = [low * low, high * high];
                if low <= 0.0 && high >= 0.0 {
                    (0.0, ends[0].max(ends[1]))
                } else {
                    (ends[0].min(ends[1]), ends[0].max(ends[1]))
                }
            }
            // Cubing and both negative-halving rules are increasing, so the
            // ends stay the ends.
            Node::Cube(a) => {
                let (low, high) = self.cell_bounds(a);
                (low * low * low, high * high * high)
            }
            Node::HalfNegative(a) => {
                let half = |value: f64| if value > 0.0 { value } else { value * 0.5 };
                let (low, high) = self.cell_bounds(a);
                (half(low), half(high))
            }
            Node::QuarterNegative(a) => {
                let quarter = |value: f64| if value > 0.0 { value } else { value * 0.25 };
                let (low, high) = self.cell_bounds(a);
                (quarter(low), quarter(high))
            }
            Node::Squeeze(a) => {
                let squeeze = |value: f64| {
                    let clamped = value.clamp(-1.0, 1.0);
                    clamped / 2.0 - clamped * clamped * clamped / 24.0
                };
                let (low, high) = self.cell_bounds(a);
                (squeeze(low), squeeze(high))
            }
            // Either branch may be taken somewhere in the cell, so the answer
            // is both of them. The input is not consulted: narrowing on it
            // would be a claim about where in the cell the branch flips.
            Node::RangeChoice {
                in_range,
                out_of_range,
                ..
            } => {
                let (al, ah) = self.cell_bounds(in_range);
                let (bl, bh) = self.cell_bounds(out_of_range);
                (al.min(bl), ah.max(bh))
            }
            Node::YClampedGradient {
                from_value,
                to_value,
                ..
            } => (from_value.min(to_value), from_value.max(to_value)),
            Node::Noise { noise, .. } => {
                let max = self.graph.noises[noise].max_value();
                (-max, max)
            }
            Node::ShiftedNoise { noise, .. } => {
                let max = self.graph.noises[noise].max_value();
                (-max, max)
            }
            Node::ShiftA(noise) | Node::ShiftB(noise) => {
                let max = self.graph.noises[noise].max_value() * 4.0;
                (-max, max)
            }
            Node::WeirdScaledSampler { noise, rarity, .. } => {
                let scale = match rarity {
                    Rarity::Type1 => 2.0,
                    Rarity::Type2 => 3.0,
                };
                (0.0, scale * self.graph.noises[noise].max_value())
            }
            // A spline extends linearly past its ends and the old blended
            // noise's range is not derived here. Both sit under an
            // `interpolated` in every dimension vanilla ships, so an infinite
            // interval here costs nothing and claims nothing.
            Node::Spline(_) | Node::Blended(_) => unknown,
        }
    }

    /// Run `body` as if it were a fresh point, then give the caller's point
    /// back.
    ///
    /// A flat cache reads its argument at the quart corner and y = 0, and a
    /// lattice corner reads it at the cell corner. Both are *other points*, and
    /// letting them write the current point's memo would answer a later node
    /// with a value sampled somewhere else — the shape of defect this project
    /// keeps finding, in a place a test would have to be looking for it.
    fn elsewhere<R>(&mut self, x: i32, z: i32, body: impl FnOnce(&mut Self) -> R) -> R {
        let point = self.point_generation;
        let column = self.column_generation;
        let at = self.column;
        let flat = self.flat_generation;
        let quart = self.quart;
        self.start_point(x, z);
        let value = body(self);
        self.point_generation = point;
        self.column_generation = column;
        self.column = at;
        self.flat_generation = flat;
        self.quart = quart;
        value
    }

    /// Evaluate `root` at a block position.
    pub fn compute(&mut self, root: usize, x: i32, y: i32, z: i32) -> f64 {
        self.start_point(x, z);
        self.eval(root, x, y, z)
    }

    /// Evaluate several roots at one point, sharing everything they share.
    ///
    /// The six climate functions are not six graphs; they are six views of one,
    /// and `shift_x` alone is under five of them. Evaluating them one call at a
    /// time would compute it five times.
    pub fn compute_all(&mut self, roots: &[usize], x: i32, y: i32, z: i32, out: &mut [f64]) {
        debug_assert_eq!(roots.len(), out.len());
        self.start_point(x, z);
        for (slot, &root) in out.iter_mut().zip(roots) {
            *slot = self.eval(root, x, y, z);
        }
    }

    fn start_point(&mut self, x: i32, z: i32) {
        self.stamps += 1;
        self.point_generation = self.stamps;
        if self.column != (x, z) {
            self.column = (x, z);
            self.stamps += 1;
            self.column_generation = self.stamps;
        }
        let quart = (x >> 2, z >> 2);
        if self.quart != quart {
            self.quart = quart;
            self.stamps += 1;
            self.flat_generation = self.stamps;
        }
    }

    fn eval(&mut self, index: usize, x: i32, y: i32, z: i32) -> f64 {
        if self.point_stamp[index] == self.point_generation {
            return self.point_memo[index];
        }
        let value = self.eval_uncached(index, x, y, z);
        self.point_stamp[index] = self.point_generation;
        self.point_memo[index] = value;
        value
    }

    fn eval_uncached(&mut self, index: usize, x: i32, y: i32, z: i32) -> f64 {
        match self.graph.nodes[index] {
            Node::Constant(value) => value,
            Node::Abs(argument) => self.eval(argument, x, y, z).abs(),
            Node::Add(a, b) => self.eval(a, x, y, z) + self.eval(b, x, y, z),
            Node::Mul(a, b) => {
                // Minecraft short-circuits a zero first argument rather than
                // multiplying. That is not the same as multiplying when the
                // second argument is infinite or NaN, so it is reproduced.
                let first = self.eval(a, x, y, z);
                if first == 0.0 {
                    0.0
                } else {
                    first * self.eval(b, x, y, z)
                }
            }
            Node::Min(a, b) => self.eval(a, x, y, z).min(self.eval(b, x, y, z)),
            Node::Max(a, b) => self.eval(a, x, y, z).max(self.eval(b, x, y, z)),
            Node::BlendAlpha => 1.0,
            Node::BlendOffset => 0.0,
            Node::Passthrough(argument) => self.eval(argument, x, y, z),
            Node::Interpolated(slot) => self.lattice[slot],
            Node::Clamp { argument, min, max } => self.eval(argument, x, y, z).clamp(min, max),
            Node::Square(argument) => {
                let value = self.eval(argument, x, y, z);
                value * value
            }
            Node::Cube(argument) => {
                let value = self.eval(argument, x, y, z);
                value * value * value
            }
            Node::HalfNegative(argument) => {
                let value = self.eval(argument, x, y, z);
                if value > 0.0 {
                    value
                } else {
                    value * 0.5
                }
            }
            Node::QuarterNegative(argument) => {
                let value = self.eval(argument, x, y, z);
                if value > 0.0 {
                    value
                } else {
                    value * 0.25
                }
            }
            Node::Squeeze(argument) => {
                let value = self.eval(argument, x, y, z).clamp(-1.0, 1.0);
                value / 2.0 - value * value * value / 24.0
            }
            Node::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                in_range,
                out_of_range,
            } => {
                let value = self.eval(input, x, y, z);
                // One branch, never both. The caves under a mountain are a
                // hundred nodes that a column above ground never touches.
                if value >= min_inclusive && value < max_exclusive {
                    self.eval(in_range, x, y, z)
                } else {
                    self.eval(out_of_range, x, y, z)
                }
            }
            Node::WeirdScaledSampler {
                input,
                noise,
                rarity,
            } => {
                let scale = rarity.scale(self.eval(input, x, y, z));
                scale
                    * self.graph.noises[noise]
                        .value(
                            f64::from(x) / scale,
                            f64::from(y) / scale,
                            f64::from(z) / scale,
                        )
                        .abs()
            }
            Node::Blended(noise) => self.graph.blended[noise].value(x, y, z),
            Node::FlatCache(argument) => {
                if self.flat_stamp[index] == self.flat_generation {
                    return self.flat_memo[index];
                }
                // The quart corner, and y = 0. Vanilla fills this table before
                // the chunk is walked and every block in the 4x4 column reads
                // the entry; a memo on the exact column would be a smoother
                // world than Minecraft's.
                let qx = self.quart.0 << 2;
                let qz = self.quart.1 << 2;
                let value = self.elsewhere(qx, qz, |e| e.eval(argument, qx, 0, qz));
                self.flat_stamp[index] = self.flat_generation;
                self.flat_memo[index] = value;
                value
            }
            Node::ColumnCache(argument) => {
                if self.column_stamp[index] == self.column_generation {
                    return self.column_memo[index];
                }
                let value = self.eval(argument, x, y, z);
                self.column_stamp[index] = self.column_generation;
                self.column_memo[index] = value;
                value
            }
            Node::ShiftA(noise) => self.shift(noise, f64::from(x), 0.0, f64::from(z)),
            Node::ShiftB(noise) => self.shift(noise, f64::from(z), f64::from(x), 0.0),
            Node::ShiftedNoise {
                noise,
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
            } => {
                let sx = f64::from(x) * xz_scale + self.eval(shift_x, x, y, z);
                let sy = f64::from(y) * y_scale + self.eval(shift_y, x, y, z);
                let sz = f64::from(z) * xz_scale + self.eval(shift_z, x, y, z);
                self.graph.noises[noise].value(sx, sy, sz)
            }
            Node::Noise {
                noise,
                xz_scale,
                y_scale,
            } => self.graph.noises[noise].value(
                f64::from(x) * xz_scale,
                f64::from(y) * y_scale,
                f64::from(z) * xz_scale,
            ),
            Node::Spline(spline) => f64::from(self.spline(spline, x, y, z)),
            Node::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => clamped_map(f64::from(y), from_y, to_y, from_value, to_value),
        }
    }

    /// `shift_a` and `shift_b` share one body; only the coordinates they feed
    /// it differ.
    fn shift(&self, noise: usize, x: f64, y: f64, z: f64) -> f64 {
        self.graph.noises[noise].value(x * 0.25, y * 0.25, z * 0.25) * 4.0
    }

    /// Evaluate a cubic spline. Deliberately `f32`, because Minecraft's is.
    ///
    /// Widening this to `f64` would be more accurate and would be wrong: the
    /// value lands in a biome parameter that is quantised by ten thousand, and
    /// a cell near a boundary would fall the other side of it.
    fn spline(&mut self, index: usize, x: i32, y: i32, z: i32) -> f32 {
        let spline = &self.graph.splines[index];
        let coordinate = spline.coordinate;
        let count = spline.points.len();
        let point = self.eval(coordinate, x, y, z) as f32;

        // Read through the arena rather than gathering the locations into a
        // `Vec` first. This runs once per spline per climate sample and there
        // are 124,416 biome cells in a 9x9: a heap allocation here is one per
        // sample, forever, for a slice that is only ever indexed.
        let start = self.interval_start(index, point);
        let last = count - 1;
        if start < 0 {
            let value = self.spline_value(index, 0, x, y, z);
            let at = self.location(index, 0);
            return linear_extend(point, at, value, self.derivative(index, 0));
        }
        let start = start as usize;
        if start == last {
            let value = self.spline_value(index, last, x, y, z);
            let at = self.location(index, last);
            return linear_extend(point, at, value, self.derivative(index, last));
        }
        let low = self.location(index, start);
        let high = self.location(index, start + 1);
        let t = (point - low) / (high - low);
        let value_low = self.spline_value(index, start, x, y, z);
        let value_high = self.spline_value(index, start + 1, x, y, z);
        let derivative_low = self.derivative(index, start);
        let derivative_high = self.derivative(index, start + 1);
        let n = derivative_low * (high - low) - (value_high - value_low);
        let o = -derivative_high * (high - low) + (value_high - value_low);
        lerp_f32(t, value_low, value_high) + t * (1.0 - t) * lerp_f32(t, n, o)
    }

    fn derivative(&self, spline: usize, point: usize) -> f32 {
        self.graph.splines[spline].points[point].derivative
    }

    fn location(&self, spline: usize, point: usize) -> f32 {
        self.graph.splines[spline].points[point].location
    }

    /// The index of the last point whose location is not greater than `value`,
    /// or -1 when every location is.
    ///
    /// Minecraft's `Mth.binarySearch` over the same predicate, walked against
    /// the arena so nothing is copied out of it first.
    fn interval_start(&self, spline: usize, value: f32) -> i32 {
        let points = &self.graph.splines[spline].points;
        let mut low = 0usize;
        let mut span = points.len();
        while span > 0 {
            let half = span / 2;
            let mid = low + half;
            if value < points[mid].location {
                span = half;
            } else {
                low = mid + 1;
                span -= half + 1;
            }
        }
        low as i32 - 1
    }

    fn spline_value(&mut self, spline: usize, point: usize, x: i32, y: i32, z: i32) -> f32 {
        match self.graph.splines[spline].points[point].value {
            SplineValue::Constant(value) => value,
            SplineValue::Nested(nested) => self.spline(nested, x, y, z),
        }
    }
}

fn linear_extend(point: f32, location: f32, value: f32, derivative: f32) -> f32 {
    if derivative == 0.0 {
        value
    } else {
        value + derivative * (point - location)
    }
}

fn lerp_f32(delta: f32, start: f32, end: f32) -> f32 {
    start + delta * (end - start)
}

fn clamped_map(value: f64, from: f64, to: f64, from_value: f64, to_value: f64) -> f64 {
    let delta = (value - from) / (to - from);
    if delta < 0.0 {
        from_value
    } else if delta > 1.0 {
        to_value
    } else {
        from_value + delta * (to_value - from_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(nodes: Vec<Node>, splines: Vec<Spline>) -> Graph {
        Graph {
            nodes,
            splines,
            noises: Vec::new(),
            blended: Vec::new(),
            interpolated: Vec::new(),
        }
    }

    #[test]
    fn the_arithmetic_nodes_do_the_arithmetic() {
        let g = graph(
            vec![
                Node::Constant(3.0),
                Node::Constant(-4.0),
                Node::Add(0, 1),
                Node::Mul(0, 1),
                Node::Abs(1),
                Node::Min(0, 1),
                Node::Max(0, 1),
            ],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(2, 0, 0, 0), -1.0);
        assert_eq!(e.compute(3, 0, 0, 0), -12.0);
        assert_eq!(e.compute(4, 0, 0, 0), 4.0);
        assert_eq!(e.compute(5, 0, 0, 0), -4.0);
        assert_eq!(e.compute(6, 0, 0, 0), 3.0);
    }

    #[test]
    fn a_zero_first_argument_short_circuits_a_multiply() {
        let g = graph(
            vec![
                Node::Constant(0.0),
                Node::Constant(f64::INFINITY),
                Node::Mul(0, 1),
                Node::Mul(1, 0),
            ],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(2, 0, 0, 0), 0.0, "0 * inf is 0 here, not NaN");
        assert!(e.compute(3, 0, 0, 0).is_nan(), "the other order is not");
    }

    #[test]
    fn the_y_gradient_clamps_at_both_ends() {
        let g = graph(
            vec![Node::YClampedGradient {
                from_y: -64.0,
                to_y: 320.0,
                from_value: 1.5,
                to_value: -1.5,
            }],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(0, 0, -1000, 0), 1.5);
        assert_eq!(e.compute(0, 0, 1000, 0), -1.5);
        assert_eq!(e.compute(0, 0, -64, 0), 1.5);
        assert_eq!(e.compute(0, 0, 320, 0), -1.5);
        assert!((e.compute(0, 0, 128, 0) - 0.0).abs() < 0.01);
    }

    #[test]
    fn a_column_cache_holds_across_y_and_is_dropped_across_x() {
        // Watched to fail: with `ColumnCache` replaced by `Passthrough` the
        // first assertion below still passes and the third still passes, so
        // this test earns its place only because of the second.
        let g = graph(
            vec![
                Node::YClampedGradient {
                    from_y: 0.0,
                    to_y: 100.0,
                    from_value: 0.0,
                    to_value: 100.0,
                },
                Node::ColumnCache(0),
            ],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(1, 5, 10, 5), 10.0);
        assert_eq!(
            e.compute(1, 5, 40, 5),
            10.0,
            "a column cache must not notice y changing"
        );
        assert_eq!(
            e.compute(1, 6, 40, 6),
            40.0,
            "and must notice the column changing"
        );
    }

    #[test]
    fn a_spline_interpolates_between_its_points_and_extends_past_them() {
        let g = graph(
            vec![
                // An identity over the range this test asks about, so the
                // spline's coordinate is simply y.
                Node::YClampedGradient {
                    from_y: -1000.0,
                    to_y: 1000.0,
                    from_value: -1000.0,
                    to_value: 1000.0,
                },
                Node::Spline(0),
            ],
            vec![Spline {
                coordinate: 0,
                points: vec![
                    SplinePoint {
                        location: 0.0,
                        value: SplineValue::Constant(0.0),
                        derivative: 0.0,
                    },
                    SplinePoint {
                        location: 10.0,
                        value: SplineValue::Constant(10.0),
                        derivative: 1.0,
                    },
                ],
            }],
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(1, 0, 0, 0), 0.0);
        assert_eq!(e.compute(1, 0, 10, 0), 10.0);
        assert_eq!(e.compute(1, 0, -5, 0), 0.0, "a zero derivative holds flat");
        assert_eq!(e.compute(1, 0, 20, 0), 20.0, "a unit derivative extends");
        let middle = e.compute(1, 0, 5, 0);
        assert!((0.0..=10.0).contains(&middle), "{middle} left the interval");
    }

    #[test]
    fn the_interval_search_answers_the_ends_the_way_minecraft_does() {
        let g = graph(
            vec![Node::Constant(0.0)],
            vec![Spline {
                coordinate: 0,
                points: [-1.0f32, 0.0, 1.0]
                    .into_iter()
                    .map(|location| SplinePoint {
                        location,
                        value: SplineValue::Constant(0.0),
                        derivative: 0.0,
                    })
                    .collect(),
            }],
        );
        let e = Evaluator::new(&g);
        assert_eq!(e.interval_start(0, -2.0), -1);
        assert_eq!(e.interval_start(0, -1.0), 0);
        assert_eq!(e.interval_start(0, 0.5), 1);
        assert_eq!(e.interval_start(0, 1.0), 2);
        assert_eq!(e.interval_start(0, 9.0), 2);
    }
}
