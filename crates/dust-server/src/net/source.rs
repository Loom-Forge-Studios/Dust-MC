//! Where a column comes from.
//!
//! Two answers so far, and the shape exists because there will be a third.
//! [`Source::Flat`] builds one column and shares it. [`Source::Anvil`] reads
//! one out of a world Minecraft wrote. Phase 6's generator is the third, and it
//! will be a variant here rather than a rewrite of everything that asks for a
//! column.
//!
//! # What a world costs to serve, and who keeps what
//!
//! Region files are held open — a world is a few dozen of them and reopening
//! one per column would be a syscall storm — behind a mutex, because they are
//! read from every session's task and a `RegionFile` seeks as it reads.
//!
//! The parsed columns are kept by a [`ColumnStore`], and **only the ones
//! somebody is holding**: a player's ring, the columns falling items will read,
//! and the window a chunk stream is about to send. A column of a real world is
//! **111 KB** — measured by `benches/movement.rs` with a counting allocator,
//! against the "about a megabyte" three modules including this one claimed for
//! the life of the project — so what makes a cache of them a design decision is
//! how many are held and by whom, not their size. Decision records
//! [0025](../../../../docs/decisions/0025-who-keeps-a-chunk-column.md) and
//! [0031](../../../../docs/decisions/0031-how-a-join-streams-its-chunks.md) are
//! the two that answer it.
//!
//! # What *is* cached is 256 integers per column
//!
//! Light does not stop at a chunk boundary, which means lighting a column
//! needs to know where the sky reaches in the four columns around it. That is
//! a [`SkyFloor`] — sixteen by sixteen integers, a kilobyte — and it is
//! nothing like a chunk. So those are cached even though the chunks are not:
//! at the default view distance of eight the whole working set is under three
//! hundred kilobytes,
//! and without it every column would read its four neighbours off disk to ask
//! them one question.
//!
//! The cache is cleared wholesale when it grows past its cap rather than
//! evicted a row at a time. The working set is the columns around the players,
//! so a cache that has passed the cap is one whose contents are mostly
//! nowhere anybody is standing, and the cost of being wrong is re-reading the
//! current view once.
//!
//! # A missing column is not an error
//!
//! A real world is a disc of generated chunks in an infinite plane, and a
//! player can walk off the edge of it. Vanilla generates what is missing, and
//! so does Dust when it could read the world's own seed — see [`Fallback`],
//! which is where the two answers are chosen between and why the choice is not
//! a setting. Either way an error would disconnect a player for walking, and a
//! hole would look like a bug in the chunk packet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dust_world::anvil::{self, Ids, Names};
use dust_world::chunk::Chunk;
use dust_world::column_light::{Skirt, SkyFloor};
use dust_world::coords::{ChunkPos, RegionPos};
use dust_world::heightmap::WorldHeight;
use dust_world::region::{FileStore, RawChunk, RegionFile};

use super::generated::GeneratedWorld;
use super::residency::Residency;
use super::world::FlatWorld;

/// Region files that have been opened, or found not to exist.
///
/// `None` is a region that is not there, remembered rather than retried: a
/// world is a disc and the columns off its edge are asked for constantly by a
/// player walking outward.
type OpenRegions = HashMap<(i32, i32), Option<RegionFile<FileStore>>>;

/// A column, borrowed when it can be, shared when the server is keeping one,
/// and built when neither.
///
/// The variants differ in size by about a megabyte, and that is the point
/// rather than an oversight: the borrowed one is what a flat world hands out
/// once per column per join without allocating — 289 times at the default view
/// distance — and boxing it to even the
/// variants out would put an allocation on exactly the path the borrow exists
/// to keep free. The value is a temporary — a caller sends the column and drops
/// it — so the large variant never sits in a collection.
///
/// [`Column::Resident`] is the one that is *not* a temporary, and it is an
/// `Arc` for that reason: it is a handle on a column the whole server is
/// keeping, and a caller may hold it for as long as it likes without stopping
/// the residency from retiring its own entry. See [`Residency`].
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Column<'a> {
    /// Every position shares this one. See [`FlatWorld`].
    Shared(&'a Chunk),
    /// One the server is keeping because a player is near it. See
    /// [`Residency`].
    Resident(Arc<Chunk>),
    /// Read from disk for this position.
    Built(Chunk),
}

impl Column<'_> {
    pub fn as_chunk(&self) -> &Chunk {
        match self {
            Self::Shared(chunk) => chunk,
            Self::Resident(chunk) => chunk,
            Self::Built(chunk) => chunk,
        }
    }
}

/// Where the world comes from.
///
/// Both arms are boxed, so the enum is a pointer and a tag. Neither world is
/// small — the Anvil one carries the open region files and the sky floors, the
/// flat one a template column's worth of bookkeeping — and an unboxed enum is
/// the size of its largest arm wherever it is stored. There is one of these
/// per server, reached through an `Arc`, so the allocation happens once at
/// boot and the indirection costs a pointer hop on a path that is about to
/// read a region file. `Column` below is the opposite case and keeps its large
/// variant unboxed for a stated reason.
pub enum Source {
    Flat(Box<FlatWorld>),
    /// Built from noise, for a server with no world file: every column, out
    /// to the edge of the coordinate space.
    Generated(Box<GeneratedColumns>),
    Anvil(Box<AnvilWorld>),
}

/// Something that can build a column, for a [`ColumnStore`]'s thread to call.
///
/// Two implementors and both of them cost something a hot path may not pay: a
/// region read is a disk seek and a generated column is four milliseconds of
/// noise. That is the whole of what the store is about, and the reason it is
/// one type rather than one per world: **the rules for who may build a column
/// are a property of the threads, not of where the blocks come from.**
pub trait Columns: Send + Sync + 'static {
    fn column(&self, pos: ChunkPos) -> Chunk;
}

impl Columns for AnvilCore {
    fn column(&self, pos: ChunkPos) -> Chunk {
        AnvilCore::column(self, pos)
    }
}

impl Columns for GeneratedWorld {
    fn column(&self, pos: ChunkPos) -> Chunk {
        GeneratedWorld::column(self, pos)
    }
}

/// How many columns a builder offers to the residency at once.
///
/// Eight, which is `net::session::STREAM_BATCH`: the stream takes columns in
/// eights, so a builder that offers them in eights hands over exactly what the
/// next pass can use. The point of the batch is the write lock — offering one
/// column at a time takes it about a thousand times a second on a region-file
/// world, and it is the lock a movement check on every other session is waiting
/// to read. The cost is that the first column of a batch waits for the eighth,
/// which on a generated world is 46 ms against a 20 ms streaming tick that was
/// never going to keep up with the builder anyway.
const FILL_BATCH: usize = 8;

/// The columns nobody has built yet, and the builders waiting on them.
///
/// **A queue of columns and not a queue of requests.** The channel this
/// replaced handed one caller's whole `Vec` to one thread, which is the same
/// thing as one thread for as long as there is one caller with work — a join
/// asks for 289 columns in a single send. Splitting at the column is what lets
/// a second builder help with the first builder's join.
///
/// The order is the order the columns arrived in, which is the nearest-first
/// order [`super::view::View`] hands the stream, because
/// [`Source::built_prefix`] counts a **prefix**: a column built out of turn is
/// a column nothing can send yet.
struct Pending {
    queue: std::collections::VecDeque<ChunkPos>,
    /// Every column some builder is going to produce: the ones still in
    /// `queue` **and** the ones a builder has taken and not yet offered back.
    ///
    /// Both halves matter and only the first one is obvious. A stream re-offers
    /// its window every pass, so without the queued half the queue would grow
    /// fifty times a second while a builder was behind. The in-flight half is
    /// what stops two builders from building the same column at once — with a
    /// single builder that could not happen, and [`Residency::fill_many`]
    /// merely threw the loser away; with four it is real duplicated work.
    claimed: std::collections::HashSet<(i32, i32)>,
    /// Set when the store is dropped. The builders end on it rather than on
    /// the last handle going away: sessions hold warming handles, so a channel
    /// that closed when its senders dropped would keep the builders alive
    /// until the last player disconnected.
    closed: bool,
}

/// The queue, the builders' condition variable, and what they build with.
struct Warmth {
    pending: std::sync::Mutex<Pending>,
    ready: std::sync::Condvar,
    residency: Arc<Residency>,
    core: Arc<dyn Columns>,
}

/// A handle on the queue that a caller can hold and clone.
///
/// Was `std::sync::mpsc::Sender<Vec<ChunkPos>>`, and the shape is deliberately
/// unchanged: callers still hand over a batch of columns and never wait.
#[derive(Clone)]
pub struct Warming(Arc<Warmth>);

impl std::fmt::Debug for Warming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Warming").finish_non_exhaustive()
    }
}

impl Warming {
    /// Ask for these columns to be built, in this order, off the caller's
    /// thread. Returns without waiting for any of them.
    ///
    /// Columns already resident, columns nobody is keeping any more, and
    /// columns already in the queue are dropped here rather than enqueued —
    /// the first two because there is nothing to build, the third because a
    /// stream re-offers its window every pass and an unfiltered queue would
    /// grow without bound while a builder was behind.
    pub fn send(&self, columns: Vec<ChunkPos>) {
        let cold = self.0.residency.cold_columns(&columns);
        if cold.is_empty() {
            return;
        }
        let mut pending = self
            .0
            .pending
            .lock()
            .expect("the warming queue is never poisoned");
        if pending.closed {
            return;
        }
        let mut added = 0;
        for pos in cold {
            if pending.claimed.insert((pos.x, pos.z)) {
                pending.queue.push_back(pos);
                added += 1;
            }
        }
        drop(pending);
        // One wake per column offered, capped by the builders that exist. A
        // `notify_all` here would wake every builder for a single column.
        for _ in 0..added {
            self.0.ready.notify_one();
        }
    }
}

/// The columns a world is keeping, and the builders that build them.
///
/// Lifted out of `AnvilWorld` when the generator landed, because a generated
/// column is **sixteen times more expensive than a region-file one** — 3.8 ms
/// against 0.24 ms, `benches/join.rs` — and the world that needed residency
/// least was the only one that had it. A server with no `world_source` now
/// serves generated terrain, so this is the default world and not a corner.
///
/// The threads are the answer to a question the server asks in two places and
/// cannot answer the same way in either. A session runs on a tokio worker and a
/// tick participant runs on the engine's own `std` thread; neither may block on
/// a column, and `tokio::task::spawn_blocking` exists only for the first. The
/// pool here serves both, and neither caller has to know it is there.
///
/// **A pool and not one thread**, and the sentence that chose one thread said
/// exactly why it would stop being true: what one thread served was the ring
/// ahead of a walking player, nine columns, 34 ms of generated terrain against
/// the 1,600 ms decision record 0017 gives a player to cross a column — "and a
/// join, the one caller that wants 289 columns at once, does not come through
/// here at all". `net::session::stream_inner` then moved the join onto the
/// store, so that a session's own task never builds a column, and nobody
/// re-sized the thread. A join is what this is sized for now: its last column
/// reaches the player at 2,699 ms with one builder and 1,069 with four, which
/// is 108 ms off a world with nothing left to build, and four people joining at
/// once at 17,202 and 5,584. `benches/warming.rs` is where those come from and
/// decision record 0036 is the account, including why the cap is four.
pub struct ColumnStore {
    residency: Arc<Residency>,
    /// Columns somebody has claimed and nobody has built yet. `None` where no
    /// builder could be started, which is a world that warms nothing and
    /// still works: every caller builds its own column, exactly as they did
    /// before any of this.
    wanted: Option<Warming>,
    warming: Vec<std::thread::JoinHandle<()>>,
}

impl ColumnStore {
    fn new(core: Arc<dyn Columns>) -> Self {
        Self::with_builders(core, default_builders())
    }

    /// The same store with a stated number of builder threads, for the bench
    /// that chose the number. `0` is a store with no builders at all.
    #[must_use]
    pub fn with_builders(core: Arc<dyn Columns>, builders: usize) -> Self {
        let residency = Arc::new(Residency::new());
        let warmth = Arc::new(Warmth {
            pending: std::sync::Mutex::new(Pending {
                queue: std::collections::VecDeque::new(),
                claimed: std::collections::HashSet::new(),
                closed: false,
            }),
            ready: std::sync::Condvar::new(),
            residency: Arc::clone(&residency),
            core,
        });
        let warming: Vec<_> = (0..builders)
            .filter_map(|n| {
                std::thread::Builder::new()
                    .name(format!("dust-warming-{n}"))
                    .spawn({
                        let warmth = Arc::clone(&warmth);
                        move || build_columns(&warmth)
                    })
                    .ok()
            })
            .collect();
        Self {
            residency,
            wanted: (!warming.is_empty()).then(|| Warming(warmth)),
            warming,
        }
    }

    /// How many of `columns`, counted from the front, are built. See
    /// [`Source::built_prefix`], which is where the rule is written down.
    fn built_prefix(&self, columns: &[ChunkPos]) -> usize {
        if self.wanted.is_none() {
            return columns.len();
        }
        columns
            .iter()
            .take_while(|pos| self.residency.resident(**pos).is_some())
            .count()
    }
}

impl std::fmt::Debug for ColumnStore {
    /// What a reader wants is how much of the world is being kept, which is
    /// the number the policy is about. The channel and the join handle are
    /// bookkeeping.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnStore")
            .field("resident_columns", &self.residency.len())
            .field("builders", &self.warming.len())
            .finish()
    }
}

impl Drop for ColumnStore {
    /// The queue is closed first, which ends every builder's loop, and then
    /// each is waited for. Not detached: a builder still holding the
    /// region mutex while the process tears the world down is a shutdown that
    /// hangs on a lock nobody owns any more.
    ///
    /// Closing is a flag rather than the last handle dropping, because a
    /// session holds a handle for as long as its player is connected.
    fn drop(&mut self) {
        if let Some(warming) = self.wanted.take() {
            warming
                .0
                .pending
                .lock()
                .expect("the warming queue is never poisoned")
                .closed = true;
            warming.0.ready.notify_all();
        }
        for thread in std::mem::take(&mut self.warming) {
            let _ = thread.join();
        }
    }
}

/// How many builder threads a store runs when nobody says.
///
/// Decision record 0036 measured the scaling, and four is where the curve stops
/// for a single join: 2,699 ms with one builder, 1,554 with two, 1,069 with
/// four and 1,076 with eight, against a stream that cannot deliver 289 columns
/// in less than 740 ms whatever the world costs. Eight are steadier rather than
/// faster there — 1,081 ms worst round against 1,449 — and four *simultaneous*
/// joins do still want more, which is the case where the cores are least
/// available. Capped again at half the machine's parallelism so that a two-core
/// box keeps a core for the tokio workers and the tick thread, which are the
/// two things a builder starving would actually be felt as.
fn default_builders() -> usize {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    (cores / 2).clamp(1, 4)
}

/// One builder's whole life: take the column at the front of the queue, build
/// it with nothing locked, and offer it back in batches of [`FILL_BATCH`].
///
/// **The batch is given up before the builder parks**, which is the difference
/// between this and the queue of requests it replaced. That one flushed at the
/// end of each caller's `Vec`, which was the same thing while a `Vec` was a
/// whole request; with four builders splitting one request between them, three
/// of the four reach the end of the queue holding a partial batch, and a
/// builder that waited for an eighth column that is not coming is a player
/// looking at seven columns of hole for as long as they stand still.
fn build_columns(warmth: &Warmth) {
    let mut built: Vec<(ChunkPos, Chunk)> = Vec::with_capacity(FILL_BATCH);
    loop {
        let pos = loop {
            let mut pending = warmth
                .pending
                .lock()
                .expect("the warming queue is never poisoned");
            if pending.closed {
                drop(pending);
                offer(warmth, &mut built);
                return;
            }
            if let Some(pos) = pending.queue.pop_front() {
                // Stays in `claimed` until it has been offered back: it is
                // still a column this builder is going to produce.
                break pos;
            }
            if !built.is_empty() {
                drop(pending);
                offer(warmth, &mut built);
                continue;
            }
            let _parked = warmth
                .ready
                .wait(pending)
                .expect("the warming queue is never poisoned");
        };
        // Nothing is held across the build. `cold_columns` took a snapshot,
        // the column is built with no lock at all, and `fill_many` takes the
        // write lock once for the batch.
        built.push((pos, warmth.core.column(pos)));
        if built.len() == FILL_BATCH {
            offer(warmth, &mut built);
        }
    }
}

/// Hand a builder's batch to the residency and stop claiming those columns.
///
/// The order matters: resident first and claimed second, so that a caller who
/// asks in between finds the column built rather than finding it neither built
/// nor claimed and enqueueing it again.
fn offer(warmth: &Warmth, built: &mut Vec<(ChunkPos, Chunk)>) {
    if built.is_empty() {
        return;
    }
    let done: Vec<(i32, i32)> = built.iter().map(|(pos, _)| (pos.x, pos.z)).collect();
    warmth.residency.fill_many(std::mem::take(built));
    built.reserve(FILL_BATCH);
    let mut pending = warmth
        .pending
        .lock()
        .expect("the warming queue is never poisoned");
    for key in done {
        pending.claimed.remove(&key);
    }
}

/// A generated world and the columns the server is keeping of it.
pub struct GeneratedColumns {
    core: Arc<GeneratedWorld>,
    store: ColumnStore,
}

impl GeneratedColumns {
    #[must_use]
    pub fn new(world: GeneratedWorld) -> Self {
        let core = Arc::new(world);
        let store = ColumnStore::new(Arc::clone(&core) as Arc<dyn Columns>);
        Self { core, store }
    }

    /// The same, with a stated number of builder threads. `benches/warming.rs`
    /// is the caller, and it is the bench that chose [`default_builders`].
    #[must_use]
    pub fn with_builders(world: GeneratedWorld, builders: usize) -> Self {
        let core = Arc::new(world);
        let store = ColumnStore::with_builders(Arc::clone(&core) as Arc<dyn Columns>, builders);
        Self { core, store }
    }

    pub fn flat(&self) -> &FlatWorld {
        self.core.flat()
    }
}

impl std::fmt::Debug for GeneratedColumns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedColumns")
            .field("resident_columns", &self.store.residency.len())
            .finish_non_exhaustive()
    }
}

/// What a position a world file does not contain is served with.
///
/// A world file is a disc in an infinite plane and a player can walk off the
/// edge of it. The two answers are a plain that runs on, which is what Dust
/// served before there was a generator, and the terrain the world's own seed
/// says is there — which is the same terrain Minecraft would have generated,
/// so the edge is a seam in the *materials* and not in the shape.
///
/// Which one an operator gets is not a setting: it is whether the world's own
/// seed could be read. Generating the far side of the edge from a seed that is
/// not the world's own would put a cliff where the disc ends, and a wrong
/// answer that looks right is worse than an obviously artificial one.
#[derive(Debug)]
pub enum Fallback {
    Flat(Box<FlatWorld>),
    Generated(Box<GeneratedWorld>),
}

impl Fallback {
    /// The flat world underneath, which both arms carry: it owns the block
    /// palette and the world height everything else resolves against.
    pub fn flat(&self) -> &FlatWorld {
        match self {
            Self::Flat(flat) => flat,
            Self::Generated(world) => world.flat(),
        }
    }

    fn column(&self, pos: ChunkPos) -> Chunk {
        match self {
            Self::Flat(flat) => flat.column().clone(),
            Self::Generated(world) => world.column(pos),
        }
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flat(_) => f.write_str("Flat"),
            Self::Generated(_) => f.write_str("Generated"),
            Self::Anvil(world) => write!(f, "Anvil({})", world.core.directory.display()),
        }
    }
}

impl Source {
    pub fn column(&self, pos: ChunkPos) -> Column<'_> {
        match self {
            Self::Flat(flat) => Column::Shared(flat.column()),
            Self::Generated(world) => match world.store.residency.resident(pos) {
                Some(chunk) => Column::Resident(chunk),
                None => Column::Built(world.core.column(pos)),
            },
            Self::Anvil(world) => match world.store.residency.resident(pos) {
                Some(chunk) => Column::Resident(chunk),
                // Nobody is keeping this one. Built here, on whatever thread
                // asked, which is what every caller did before residency
                // existed: this path can only ever be as slow as it was.
                None => Column::Built(world.core.column(pos)),
            },
        }
    }

    /// Claim the columns around `centre` for one player.
    ///
    /// Refcounts only — see [`Residency::hold`]. Nothing here reads a file, so
    /// a session may call it from the task that just judged a movement packet;
    /// [`Source::warm`] is the half that must not be.
    ///
    /// A flat world has nothing to keep: it lends one template column to every
    /// position, so residency would be a refcount on a borrow.
    pub fn hold(&self, centre: ChunkPos) {
        if let Some(store) = self.store() {
            store.residency.hold(centre);
        }
    }

    /// Give up one player's claim on the columns around `centre`.
    pub fn release(&self, centre: ChunkPos) {
        if let Some(store) = self.store() {
            store.residency.release(centre);
        }
    }

    /// The columns this world is keeping, or `None` for a flat one, which
    /// lends one template column to every position and has nothing to keep.
    fn store(&self) -> Option<&ColumnStore> {
        match self {
            Self::Flat(_) => None,
            Self::Generated(world) => Some(&world.store),
            Self::Anvil(world) => Some(&world.store),
        }
    }

    /// Claim named columns for one holder, for a caller whose working set is
    /// not a ring around a player. See [`Residency::hold_columns`].
    pub fn hold_columns(&self, columns: &[ChunkPos]) {
        if let Some(store) = self.store() {
            store.residency.hold_columns(columns);
        }
    }

    /// Give up a claim taken by [`Source::hold_columns`].
    pub fn release_columns(&self, columns: &[ChunkPos]) {
        if let Some(store) = self.store() {
            store.residency.release_columns(columns);
        }
    }

    /// Ask for these columns to be built, and carry on.
    ///
    /// **This is the call every caller on a hot path wants** and the only one
    /// that is safe from all of them: it hands a list to the world's own
    /// pool of builders and returns. A session task and the tick loop are
    /// different threads with different rules about blocking, and neither of
    /// them may read a region file; this is the one door both can use.
    ///
    /// Nothing waits on the result. A caller that reaches a column before the
    /// thread does builds it, which is what every caller did before residency
    /// existed — the floor is the old behaviour, never a hole in the world.
    pub fn want(&self, columns: Vec<ChunkPos>) {
        if let Some(wanted) = self.store().and_then(|store| store.wanted.as_ref()) {
            // Takes nothing once the queue is closed, which happens while
            // the world is being dropped. There is nothing to warm for a world
            // that is going away.
            wanted.send(columns);
        }
    }

    /// Ask for the ring around `centre` to be built, and carry on.
    pub fn want_ring(&self, centre: ChunkPos) {
        if let Some(store) = self.store() {
            self.want(store.residency.cold(centre));
        }
    }

    /// Build whatever around `centre` is claimed and not yet there, **on this
    /// thread**, and return how many columns that was.
    ///
    /// The blocking form, for the two callers that have a reason to wait: a
    /// join, which has no movement packet held up behind it and wants the
    /// ground under the player there before the loading screen ends, and a
    /// bench, which is measuring the cost itself. Everything else calls
    /// [`Source::want`].
    pub fn warm(&self, centre: ChunkPos) -> u32 {
        let Some(store) = self.store() else { return 0 };
        self.warm_columns(&store.residency.cold(centre))
    }

    /// The same, for a named set of columns rather than a ring.
    pub fn warm_columns(&self, columns: &[ChunkPos]) -> u32 {
        let (store, core): (&ColumnStore, &dyn Columns) = match self {
            Self::Flat(_) => return 0,
            Self::Generated(world) => (&world.store, world.core.as_ref()),
            Self::Anvil(world) => (&world.store, world.core.as_ref()),
        };
        let mut built = 0;
        let cold = store.residency.cold_columns(columns);
        // Built with no lock held, then offered in batches. A player who walked
        // away in the meantime, or another thread that got there first, means
        // the column is dropped rather than kept — see [`Residency::fill_many`].
        for run in cold.chunks(FILL_BATCH) {
            let batch: Vec<_> = run.iter().map(|pos| (*pos, core.column(*pos))).collect();
            built += batch.len();
            store.residency.fill_many(batch);
        }
        u32::try_from(built).unwrap_or(u32::MAX)
    }

    /// The server's resident set, for a caller that keeps a claim on it —
    /// `net::residency::Residence` for a session and
    /// `net::residency::ColumnClaim` for the item entities.
    ///
    /// `None` on a flat world, which lends one template column to every
    /// position and has nothing to keep. A claim on `None` does nothing and
    /// that is the right nothing: there is no column to hold.
    #[must_use]
    pub fn residency(&self) -> Option<Arc<Residency>> {
        self.store().map(|store| Arc::clone(&store.residency))
    }

    /// Where a claim sends the columns it has just taken, to be built off the
    /// caller's own thread. `None` where the world builds nothing or its
    /// builders would not start.
    #[must_use]
    pub fn warming(&self) -> Option<Warming> {
        self.store().and_then(|store| store.wanted.clone())
    }

    /// How many of `columns`, counted **from the front**, this world has
    /// already built.
    ///
    /// The chunk stream's back-pressure and its ordering rule in one number.
    /// A session sends this many and no more, so it never builds a column on
    /// its own task and never sends the far corner of a view before the ground
    /// under the player's feet: a prefix has no holes in it, and a client
    /// renders what it has.
    ///
    /// Two worlds answer `columns.len()` and neither of them is a shortcut.
    /// **A flat world** lends one template column to every position, so there
    /// is nothing to wait for. **A world whose builders would not start** has
    /// nobody to wait *on*, and a stream that paced itself against a thread
    /// that does not exist would be a player looking at a hole in the world
    /// forever; it builds its own columns instead, which is what every caller
    /// did before any of this existed.
    #[must_use]
    pub fn built_prefix(&self, columns: &[ChunkPos]) -> usize {
        self.store()
            .map_or(columns.len(), |store| store.built_prefix(columns))
    }

    /// How many columns the server is keeping. Zero on a flat world.
    #[must_use]
    pub fn resident_columns(&self) -> usize {
        self.store().map_or(0, |store| store.residency.len())
    }

    /// The flat world underneath, which every source has: it is the fallback
    /// for a column a real world does not contain, and it owns the block
    /// palette everything else resolves against.
    pub fn flat(&self) -> &FlatWorld {
        match self {
            Self::Flat(flat) => flat,
            Self::Generated(world) => world.flat(),
            Self::Anvil(world) => world.core.fallback.flat(),
        }
    }
}

/// A world on disk, and the columns the server is keeping of it.
///
/// Two halves on purpose. [`AnvilCore`] is everything that answers a question
/// about the world, behind an `Arc` so that the builders can hold it; the
/// [`ColumnStore`] is the residency, the channel and the thread's own lifetime,
/// which belong to the world rather than to anything asking it for a column.
pub struct AnvilWorld {
    core: Arc<AnvilCore>,
    /// The columns the server is keeping because players or items are near
    /// them. See [`ColumnStore`], and [`Residency`] for what it is serialised
    /// against.
    store: ColumnStore,
}

/// Everything that answers a question about a world on disk.
struct AnvilCore {
    directory: PathBuf,
    /// Open region files, by the region they cover. Behind a mutex because a
    /// `RegionFile` seeks as it reads and every session's task asks it for
    /// columns.
    regions: Mutex<OpenRegions>,
    names: RegistryNames,
    height: WorldHeight,
    fallback: Fallback,
    /// How much light entering each block state costs. Built once at boot,
    /// because with a table in it this is 26,684 bytes and the alternative is
    /// building one per column served.
    opacity: dust_world::propagation::OpacityModel,
    /// What Minecraft says about a block state, or nothing. Held rather than
    /// folded into `opacity` because the heightmaps need it too and a second
    /// copy would be a second answer.
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    /// What every block state gives off. Built once at boot beside `opacity`
    /// and for the same reason: with a table in it this is 26,684 bytes.
    emission: dust_world::propagation::EmissionModel,
    /// Where the sky reaches in each column read so far, for lighting the
    /// columns beside it. See the module note for why this is cached when the
    /// chunks are not.
    sky_floors: Mutex<HashMap<(i32, i32), SkyFloor>>,
}

/// How many columns' sky floors are kept before the cache is emptied.
///
/// A kilobyte each, so this is four megabytes — several times a view distance
/// of ten in every direction, which is the point: the cap is a bound on a
/// walk that never comes back, not a limit on how much of one view fits.
const SKY_FLOOR_CACHE_CAP: usize = 4096;

impl std::fmt::Debug for AnvilWorld {
    /// The open region files are file handles and seek positions, and the name
    /// tables are three hundred strings. What a reader wants is which world
    /// this is, how much of it is open and how much of it is being kept.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnvilWorld")
            .field("directory", &self.core.directory)
            .field(
                "regions_open",
                &self
                    .core
                    .regions
                    .lock()
                    .map(|open| open.len())
                    .unwrap_or_default(),
            )
            .field("resident_columns", &self.store.residency.len())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RegistryNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryNames")
            .field("biomes", &self.biomes.len())
            .finish()
    }
}

impl AnvilCore {
    /// `opacity` is what the columns are lit with — see
    /// [`world::opacity_of`](super::world::opacity_of), which is the one place
    /// that decides between Minecraft's own numbers and the air-only stand-in.
    ///
    /// The flat `fallback` keeps its own model and that is not an oversight: it
    /// is made of bedrock, dirt and grass, every one of which both models agree
    /// is a wall, so the two answers are the same answer.
    fn new(
        directory: PathBuf,
        names: RegistryNames,
        fallback: Fallback,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        Self {
            directory,
            regions: Mutex::new(HashMap::new()),
            names,
            height: fallback.flat().height(),
            emission: super::world::emission_of(constants.as_deref()),
            fallback,
            opacity,
            constants,
            sky_floors: Mutex::new(HashMap::new()),
        }
    }
}

impl AnvilWorld {
    /// `opacity` is what the columns are lit with — see
    /// [`world::opacity_of`](super::world::opacity_of).
    pub fn new(
        directory: PathBuf,
        names: RegistryNames,
        fallback: FlatWorld,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        Self::with_fallback(
            directory,
            names,
            Fallback::Flat(Box::new(fallback)),
            opacity,
            constants,
        )
    }

    /// The same world with the generator underneath it, for a save whose own
    /// seed this server could read.
    pub fn generating(
        directory: PathBuf,
        names: RegistryNames,
        fallback: GeneratedWorld,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        Self::with_fallback(
            directory,
            names,
            Fallback::Generated(Box::new(fallback)),
            opacity,
            constants,
        )
    }

    fn with_fallback(
        directory: PathBuf,
        names: RegistryNames,
        fallback: Fallback,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        let core = Arc::new(AnvilCore::new(
            directory, names, fallback, opacity, constants,
        ));
        let store = ColumnStore::new(Arc::clone(&core) as Arc<dyn Columns>);
        Self { core, store }
    }

    /// Whether this looks like a world directory at all, checked at boot so an
    /// operator who mistyped a path is told then rather than by an empty world.
    pub fn is_region_directory(path: &Path) -> bool {
        std::fs::read_dir(path).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "mca")
            })
        })
    }
}

impl AnvilCore {
    /// Where the sky reaches in one column, read or remembered.
    ///
    /// A column the world does not contain answers with the flat fallback's
    /// floors, because that is what Dust serves there — the skirt has to
    /// describe the world a player will actually see, not the one on disk.
    fn sky_floor(&self, pos: ChunkPos) -> SkyFloor {
        if let Some(found) = self
            .sky_floors
            .lock()
            .expect("the sky-floor cache is never poisoned")
            .get(&(pos.x, pos.z))
        {
            return *found;
        }
        let floors = match self.read(pos) {
            Some(mut chunk) => {
                chunk.recompute_heightmaps(super::world::heightmap_predicate(
                    self.fallback.flat().palette().air,
                    self.constants.as_deref(),
                ));
                SkyFloor::of(&chunk)
            }
            None => SkyFloor::of(&self.fallback.column(pos)),
        };
        self.remember(pos, floors);
        floors
    }

    fn remember(&self, pos: ChunkPos, floors: SkyFloor) {
        let mut cache = self
            .sky_floors
            .lock()
            .expect("the sky-floor cache is never poisoned");
        if cache.len() >= SKY_FLOOR_CACHE_CAP {
            // Wholesale, not one row at a time. See the module note.
            cache.clear();
        }
        cache.insert((pos.x, pos.z), floors);
    }

    /// The four columns around `pos`, as a boundary condition for its light.
    fn skirt(&self, pos: ChunkPos) -> Skirt {
        Skirt {
            west: self.sky_floor(ChunkPos::new(pos.x - 1, pos.z)),
            east: self.sky_floor(ChunkPos::new(pos.x + 1, pos.z)),
            north: self.sky_floor(ChunkPos::new(pos.x, pos.z - 1)),
            south: self.sky_floor(ChunkPos::new(pos.x, pos.z + 1)),
        }
    }

    fn column(&self, pos: ChunkPos) -> Chunk {
        match self.read(pos) {
            Some(mut chunk) => {
                // The file's heightmaps are replaced with this server's, for
                // serving. `anvil::read` loads what the file carried — see its
                // docs for why it does not throw them away — and this is the
                // caller that has a reason to overwrite them: the client is
                // sent WORLD_SURFACE and MOTION_BLOCKING, and they have to
                // agree with the blocks in the packet beside them rather than
                // with whatever produced the file.
                //
                // The predicate is "not air" for all six, which is **not**
                // vanilla's rule for three of the four it writes. That is a
                // known approximation and the reason a save must not go
                // through here: `harness rewrite` reads and writes without
                // this step, so a round trip keeps the file's own answers.
                chunk.recompute_heightmaps(super::world::heightmap_predicate(
                    self.fallback.flat().palette().air,
                    self.constants.as_deref(),
                ));
                // Light is computed, not read. A chunk's stored light is a
                // cache of what an engine would produce, and this server has
                // its own engine; trusting the file would mean serving light
                // that no code here can reproduce.
                // This column's own floors go in the cache before its
                // neighbours are asked for theirs: a column is almost always
                // asked for beside the ones around it, so the pass that lights
                // it pays for the four that follow.
                self.remember(pos, SkyFloor::of(&chunk));
                let skirt = self.skirt(pos);
                let _ =
                    super::world::light_column(&mut chunk, &self.opacity, &self.emission, skirt);
                chunk
            }
            // Off the edge of what was generated. See the module note: a plain
            // running on beats an error or a hole.
            None => self.fallback.column(pos),
        }
    }

    /// One column out of the file.
    ///
    /// **Only the seek holds the region mutex.** Decompressing the stream,
    /// parsing the NBT and assembling the chunk are pure work on bytes this
    /// thread now owns, and they used to happen with the map locked — which
    /// made about 60% of a column serial and put a hard ceiling of 1.5x on
    /// however many threads were reading the world. `benches/contention.rs`
    /// is the ladder that says so, and decision record 0038 is why it matters.
    fn read(&self, pos: ChunkPos) -> Option<Chunk> {
        let raw = self.stored(pos)?;
        let bytes = raw.compression.decompress(&raw.data).ok()?;
        let named = dust_nbt::read::from_bytes(&bytes).ok()?;
        let dust_nbt::Tag::Compound(root) = &named.tag else {
            return None;
        };
        anvil::chunk(root, self.height, &self.names).ok()
    }

    /// A column's bytes exactly as the file holds them, compressed.
    ///
    /// This is the whole of what needs the region mutex: a `RegionFile` seeks
    /// as it reads, so two threads sharing one cannot be inside it at once.
    /// What comes back is owned, so nothing the caller does with it needs the
    /// lock.
    fn stored(&self, pos: ChunkPos) -> Option<RawChunk> {
        let region = RegionPos::new(pos.x >> 5, pos.z >> 5);
        let mut open = self
            .regions
            .lock()
            .expect("the region map is never poisoned");
        let slot = open.entry((region.x, region.z)).or_insert_with(|| {
            // A region file that is not there is the ordinary case at the edge
            // of a world, so the absence is remembered rather than retried on
            // every column.
            RegionFile::open_in(&self.directory, region).ok()
        });
        slot.as_mut()?.read_chunk_raw(pos).ok()?
    }
}

/// [`Names`] backed by the generated registries.
///
/// This is where `dust-world`'s deliberate ignorance of the registry is repaid:
/// the parser takes a lookup, and the assembly crate is the one that has both
/// the block table and the biome ids the *client* was told about. Those biome
/// ids are positions in the synced registry, not an internal numbering, and
/// using the wrong ones would render a plains as whatever is at that index.
pub struct RegistryNames {
    biomes: HashMap<&'static str, u32>,
    /// The same table read the other way, for [`Ids`]. Built beside the
    /// forward one from the same iteration rather than searched on demand: a
    /// write walks every palette entry of every section, and a linear scan of
    /// sixty-four biomes per entry is a cost with no reason to exist.
    ///
    /// Two maps over one source is not two sources. What would be is building
    /// this from a second call to `synced::by_name`, which is why it is not.
    biome_names: Vec<&'static str>,
}

impl RegistryNames {
    /// Build the tables, from the same synced registry the configuration state
    /// sends. One source for both, so a client and a chunk cannot disagree
    /// about which number is which biome.
    pub fn new() -> Option<Self> {
        let synced = dust_registry::synced::by_name("minecraft:worldgen/biome")?;
        Some(Self {
            biomes: synced
                .entries
                .iter()
                .enumerate()
                .map(|(id, name)| (*name, id as u32))
                .collect(),
            biome_names: synced.entries.to_vec(),
        })
    }
}

impl Names for RegistryNames {
    /// Resolve a block *state*, not just a block.
    ///
    /// Start at the block's default state and apply each property the file
    /// named. `BlockState::with` returns `None` for a property this block does
    /// not have or a value it does not take, and that is **skipped rather than
    /// refused**: a world written by a newer Minecraft, or by a modded server,
    /// carries properties this build's table does not model, and refusing the
    /// whole chunk over one of them would make a world unopenable for a detail
    /// nobody can see. What is not done is returning the default and calling it
    /// the state — the properties that *are* understood are applied, so a
    /// staircase faces the way it was written even if some other field on it is
    /// unknown.
    fn block(&self, name: &str, properties: &[(&str, &str)]) -> Option<u32> {
        let mut state = dust_registry::Block::from_name(name)?.default_state();
        for (property, value) in properties {
            if let Some(next) = state.with(property, value) {
                state = next;
            }
        }
        Some(state.id())
    }

    fn biome(&self, name: &str) -> Option<u32> {
        self.biomes.get(name).copied()
    }

    fn block_registry_size(&self) -> u32 {
        dust_registry::STATE_COUNT
    }

    fn biome_registry_size(&self) -> u32 {
        self.biomes.len() as u32
    }
}

impl Ids for RegistryNames {
    /// Name a block *state*, with every property that distinguishes it.
    ///
    /// The inverse of [`Names::block`], and deliberately not its exact mirror.
    /// The reader *skips* a property it does not understand, because a world
    /// written by a newer Minecraft carries fields this build cannot model and
    /// refusing a chunk over one would make a world unopenable for a detail
    /// nobody can see. The writer has no such case to be lenient about: every
    /// id it is handed came from this same table, so an id with no name is not
    /// a version gap, it is a caller holding a number that means nothing. That
    /// stops the write.
    ///
    /// A block with no properties returns an empty vector and the file omits
    /// the `Properties` compound, which is what vanilla writes and what the
    /// reader expects to find missing.
    fn block_name(&self, id: u32) -> Option<(&str, Vec<(&str, &str)>)> {
        let state = dust_registry::BlockState::from_id(id)?;
        Some((state.block().name(), state.properties()))
    }

    fn biome_name(&self, id: u32) -> Option<&str> {
        self.biome_names.get(id as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A world that costs nothing and counts what it was asked for.
    ///
    /// Neither real world can be built in a test — one needs a directory of
    /// `.mca` files and the other needs Minecraft's own worldgen tables, and
    /// nothing Mojang's is ever committed. What is under test here is not
    /// where the blocks come from: it is **which thread builds them and who
    /// keeps the result**, which is the whole of [`ColumnStore`] and is the
    /// same code for both worlds.
    struct Counted {
        built: AtomicUsize,
    }

    impl Columns for Counted {
        fn column(&self, pos: ChunkPos) -> Chunk {
            self.built.fetch_add(1, Ordering::SeqCst);
            Chunk::uniform(
                pos,
                dust_world::heightmap::WorldHeight::new(-64, 384),
                2,
                2,
                0,
                0,
            )
        }
    }

    /// Wait for `f`, up to a second, so a slow machine does not decide the
    /// answer. A failure here is the thread never doing the work, not the
    /// thread being late.
    fn within_a_second(mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        f()
    }

    fn store() -> (Arc<Counted>, ColumnStore) {
        let core = Arc::new(Counted {
            built: AtomicUsize::new(0),
        });
        let store = ColumnStore::new(Arc::clone(&core) as Arc<dyn Columns>);
        (core, store)
    }

    /// A world whose every column is a stated function of where it is, so that
    /// a test can say whether the pool changed the answer.
    ///
    /// The point is the pair: `column` is pure, and [`Seeded::at`] is the same
    /// arithmetic written a second time for the assertion to compare against.
    /// A chunk that came back holding another position's blocks — the failure a
    /// pool can have and a single thread cannot — is then a mismatch rather
    /// than something a `Chunk::uniform` stand-in would have swallowed, because
    /// every uniform chunk of the same size is equal to every other.
    struct Seeded {
        built: AtomicUsize,
    }

    impl Seeded {
        /// The block state a column puts at `y`. A hash rather than a counter:
        /// a counter is the build *order*, which is exactly the thing under
        /// test and would make the assertion agree with whatever happened.
        fn at(pos: ChunkPos, y: i32) -> u32 {
            let mixed = (pos.x as i64)
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add((pos.z as i64).wrapping_mul(0x85EB_CA6B))
                .wrapping_add(y as i64);
            u32::try_from(mixed.rem_euclid(4)).unwrap_or(0)
        }
    }

    impl Columns for Seeded {
        fn column(&self, pos: ChunkPos) -> Chunk {
            self.built.fetch_add(1, Ordering::SeqCst);
            let world = dust_world::heightmap::WorldHeight::new(-64, 384);
            let mut chunk = Chunk::uniform(pos, world, 4, 2, 0, 0);
            for y in [-64, 0, 100, 319] {
                chunk.set_block(1, y, 2, Self::at(pos, y));
            }
            chunk
        }
    }

    /// **The same seed builds the same world however many builders ran.**
    ///
    /// A join's 289 columns through the pool at one, two, four and eight
    /// builders, and the world that came out compared against the arithmetic
    /// rather than against another run: a comparison of run to run would agree
    /// with itself if every builder count were wrong the same way.
    ///
    /// What this can catch is the pool's own half of determinism — a column
    /// filed under a neighbour's key, a column built twice, a column dropped
    /// when two builders raced for it. **What it cannot catch is the other
    /// half**, which is that [`Columns::column`] must itself be a function of
    /// position alone; that is a property of the world and not of the threads,
    /// and `GeneratedWorld` is where it is checked. Decision record 0036 says
    /// which defect lived on which side of that line.
    ///
    /// Watched to fail, four mutations run and each one caught by a different
    /// assertion here: letting [`Warming::send`] enqueue a column it has
    /// already claimed builds **578 of the 289**; shifting `offer`'s keys one
    /// column north leaves a column unbuilt; rotating them within the batch is
    /// caught by [`Chunk::pos`]; and filing a blank chunk under the right key
    /// passes both of those and is caught by the block comparison, which is
    /// what that comparison is for.
    #[test]
    fn the_same_columns_come_out_however_many_builders_went_in() {
        let order: Vec<ChunkPos> = (-8..=8)
            .flat_map(|x| (-8..=8).map(move |z| ChunkPos::new(x, z)))
            .collect();
        assert_eq!(order.len(), 289, "a join at the default view distance");
        for builders in [1usize, 2, 4, 8] {
            let core = Arc::new(Seeded {
                built: AtomicUsize::new(0),
            });
            let store = ColumnStore::with_builders(Arc::clone(&core) as Arc<dyn Columns>, builders);
            store.residency.hold_columns(&order);
            let wanted = store.wanted.as_ref().expect("the builders started");
            // In eights, which is the shape a stream offers its window in and
            // the shape that gives more than one builder something to take —
            // and **each batch twice**, because a stream re-offers its window
            // every pass and the second offer is what makes the claim on an
            // in-flight column load-bearing rather than decorative. Sent once,
            // the duplicate-suppression mutation below stays green.
            for batch in order.chunks(8) {
                wanted.send(batch.to_vec());
                wanted.send(batch.to_vec());
            }
            assert!(
                within_a_second(|| order
                    .iter()
                    .all(|pos| store.residency.resident(*pos).is_some())),
                "{builders} builder(s) left a column unbuilt"
            );
            for pos in &order {
                let chunk = store.residency.resident(*pos).expect("built");
                assert_eq!(chunk.pos(), *pos, "{builders} builder(s) misfiled a column");
                for y in [-64, 0, 100, 319] {
                    assert_eq!(
                        chunk.get_block(1, y, 2),
                        Seeded::at(*pos, y),
                        "{builders} builder(s): ({}, {}) at y {y}",
                        pos.x,
                        pos.z
                    );
                }
            }
            assert_eq!(
                core.built.load(Ordering::SeqCst),
                order.len(),
                "{builders} builder(s) built a column more than once"
            );
        }
    }

    /// A column that takes a stated 40 ms to build, so that a test can say
    /// whether two builders worked on one caller's request at the same time.
    /// Wall clock rather than a counter, because the thing under test is
    /// concurrency and a counter cannot see it.
    struct Slow {
        built: AtomicUsize,
        each: Duration,
    }

    impl Columns for Slow {
        fn column(&self, pos: ChunkPos) -> Chunk {
            std::thread::sleep(self.each);
            self.built.fetch_add(1, Ordering::SeqCst);
            Chunk::uniform(
                pos,
                dust_world::heightmap::WorldHeight::new(-64, 384),
                2,
                2,
                0,
                0,
            )
        }
    }

    fn slow_store(builders: usize, each: Duration) -> (Arc<Slow>, ColumnStore) {
        let core = Arc::new(Slow {
            built: AtomicUsize::new(0),
            each,
        });
        let store = ColumnStore::with_builders(Arc::clone(&core) as Arc<dyn Columns>, builders);
        (core, store)
    }

    /// Eight columns in one `send`, which is the shape a join has: 289 of them
    /// in a single call from one session. The channel this replaced handed the
    /// whole `Vec` to whichever thread called `recv` first, so four builders
    /// finished it in exactly the time one would.
    ///
    /// Watched to fail: with `builders` at 1 the elapsed time is eight times
    /// `each` and the assertion goes red.
    #[test]
    fn four_builders_share_one_caller_s_request() {
        let each = Duration::from_millis(40);
        let (core, store) = slow_store(4, each);
        let columns: Vec<ChunkPos> = (0..8).map(|x| ChunkPos::new(x, 0)).collect();
        store.residency.hold_columns(&columns);
        let at = Instant::now();
        store
            .wanted
            .as_ref()
            .expect("four builders started")
            .send(columns.clone());
        assert!(within_a_second(|| store.residency.len() == 8
            && columns
                .iter()
                .all(|p| store.residency.resident(*p).is_some())));
        let elapsed = at.elapsed();
        assert_eq!(core.built.load(Ordering::SeqCst), 8);
        // Four builders over eight columns is two rounds of 40 ms. Half of the
        // serial 320 ms is the loosest bound that still contradicts one
        // builder, and it is the bound a scheduler under load can still meet.
        assert!(
            elapsed < each * 4,
            "eight columns took {elapsed:?}; four builders should not need {:?}",
            each * 4
        );
    }

    /// Two `send` calls naming the same not-yet-built column, which is what two
    /// players standing beside each other produce. With one builder this was
    /// impossible by construction; with four it is a race, and the answer is
    /// that a column stays claimed until it has been offered back.
    #[test]
    fn a_column_already_in_flight_is_not_taken_by_a_second_builder() {
        let each = Duration::from_millis(60);
        let (core, store) = slow_store(4, each);
        let pos = ChunkPos::new(11, -3);
        store.residency.hold_columns(&[pos]);
        let wanted = store.wanted.as_ref().expect("four builders started");
        wanted.send(vec![pos]);
        // Inside the first builder's 60 ms, so the column is neither queued
        // nor resident — the window the `claimed` set exists to cover.
        std::thread::sleep(each / 3);
        wanted.send(vec![pos]);
        wanted.send(vec![pos]);
        assert!(within_a_second(|| store.residency.resident(pos).is_some()));
        assert!(!within_a_second(|| core.built.load(Ordering::SeqCst) > 1));
        assert_eq!(core.built.load(Ordering::SeqCst), 1);
    }

    /// The one answer that must not be "wait": a flat world builds nothing, so
    /// a stream that paced itself against its store would never send a column
    /// at all.
    #[test]
    fn a_flat_world_has_every_column_ready_because_it_builds_none() {
        let palette = super::super::world::Palette::resolve().expect("the block table");
        let source = Source::Flat(Box::new(FlatWorld::new(palette, 0, 64)));
        let columns: Vec<ChunkPos> = (0..9).map(|x| ChunkPos::new(x, 0)).collect();
        assert_eq!(source.built_prefix(&columns), 9);
        assert!(source.residency().is_none());
    }

    #[test]
    fn a_store_answers_with_the_built_prefix_and_never_past_a_gap() {
        let (_core, store) = store();
        let columns: Vec<ChunkPos> = (0..4).map(|x| ChunkPos::new(x, 0)).collect();
        store.residency.hold_columns(&columns);
        // The first two and the fourth. A stream that sent what was *ready*
        // rather than what was ready *in order* would put the fourth column on
        // the wire before the third and leave a hole in front of the player.
        for pos in [columns[0], columns[1], columns[3]] {
            store.residency.fill(
                pos,
                Chunk::uniform(
                    pos,
                    dust_world::heightmap::WorldHeight::new(-64, 384),
                    2,
                    2,
                    0,
                    0,
                ),
            );
        }
        assert_eq!(store.built_prefix(&columns), 2);
        assert_eq!(
            store.built_prefix(&columns[3..]),
            1,
            "and it counts from the front"
        );
    }

    #[test]
    fn a_wanted_column_is_built_by_the_store_and_not_by_the_caller() {
        let (core, store) = store();
        let pos = ChunkPos::new(3, 4);
        store.residency.hold_columns(&[pos]);
        // Nothing is built by holding: a hold is a refcount and runs on a
        // session's own task.
        assert_eq!(core.built.load(Ordering::SeqCst), 0);
        assert!(store.residency.resident(pos).is_none());

        store
            .wanted
            .as_ref()
            .expect("the builders started")
            .send(vec![pos]);
        assert!(within_a_second(|| store.residency.resident(pos).is_some()));
        assert_eq!(core.built.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_column_nobody_holds_is_not_kept_when_it_is_built() {
        let (_core, store) = store();
        let pos = ChunkPos::new(-2, 7);
        store
            .wanted
            .as_ref()
            .expect("the builders started")
            .send(vec![pos]);
        // `cold_columns` only names columns somebody is keeping, so an
        // unclaimed one is never even built. The store is a cache of what is
        // *claimed*, which is what bounds it without a cap.
        assert!(!within_a_second(|| store.residency.resident(pos).is_some()));
        assert_eq!(store.residency.len(), 0);
    }

    #[test]
    fn a_column_the_store_already_holds_is_not_built_twice() {
        let (core, store) = store();
        let pos = ChunkPos::new(0, 0);
        store.residency.hold_columns(&[pos]);
        let wanted = store.wanted.as_ref().expect("the builders started");
        wanted.send(vec![pos]);
        assert!(within_a_second(|| store.residency.resident(pos).is_some()));
        wanted.send(vec![pos]);
        wanted.send(vec![pos]);
        // Three requests, one build. This is the guarantee a third caller for
        // the chunk stream depends on: asking for a column the store has costs
        // a hash lookup, not a world.
        assert!(!within_a_second(|| core.built.load(Ordering::SeqCst) > 1));
        assert_eq!(core.built.load(Ordering::SeqCst), 1);
    }
}
