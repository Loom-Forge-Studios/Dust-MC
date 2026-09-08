//! The columns the server keeps because somebody is standing near them.
//!
//! # The problem this exists for
//!
//! [D20](../../../../docs/decisions/0020-what-a-movement-check-really-costs-on-a-saved-world.md)
//! measured a movement check on a world read from region files at **8.8
//! microseconds a packet, 97% of which was rebuilding a chunk column out of a
//! region file inside the network path** — about 0.9 ms a build, nineteen
//! builds in a 432-block walk. That is two things at once and only one of them
//! is a number:
//!
//! - **A player feels it.** A file read, a decompress, an NBT parse and a light
//!   pass happen on the session's own task, between a movement packet arriving
//!   and the reply going out. Measured on a cold world: **2.25 ms a column**,
//!   and a single movement packet at the worst moment took **11.28 ms**.
//!   Walking into new terrain hitches, and a hundred players walking into new
//!   terrain at once is a server that stops answering.
//! - **It is paid per holder, not per column.** [`super::collide::Ground`]
//!   keeps four built columns *per session*, so two players standing in the
//!   same place hold two copies of the same column — and
//!   `net::items::ItemTicker` builds a fresh `Ground` inside **every tick**,
//!   whose cache dies with it: a single falling item over region files cost
//!   **555,197 ns a tick** against 38 on a flat world.
//!
//! The second of those is why this is not a bigger per-session cache. A cache
//! belongs to a caller; residency belongs to the server, and there are two
//! callers on two different kinds of thread.
//!
//! # What this is instead
//!
//! One copy of a column for the server, held while a player is near it and
//! dropped when none is. A column is an [`Arc<Chunk>`], so a second player
//! arriving costs a refcount and not a build.
//!
//! # Who owns a column, and when it is dropped
//!
//! Two kinds of holder, because there are two access patterns and one of them
//! is not a ring around anybody:
//!
//! - **A player is a moving window.** [`Residence`] holds the
//!   [`RESIDENT_RADIUS`] ring around the column a session is standing in and
//!   slides it as they walk.
//! - **Falling items are a static set.** [`ColumnClaim`] holds whatever columns
//!   `net::items::footprint_into` names — the columns the drops that are still
//!   in the air will read *next* tick — and gives them up as they land. A
//!   settled item never asks the world anything, so a heap lying on the floor
//!   holds nothing at all.
//!
//! Both are refcounts on the same column, which is the point: two players and a
//! bouncing pile of cobblestone over one column keep one copy of it between
//! them.
//!
//! A session **holds** the ring around the column it is
//! standing in — nine columns — and releases them when it moves or ends. A
//! column with at least one holder is never dropped. Nine and not the view's
//! several hundred, because this is what the *movement check* needs and it is
//! decided by arithmetic rather than by taste: a player box is 0.6 across, so
//! the cells one check reads span at most two columns on each axis and are
//! always inside the ring around the column the player is in.
//!
//! The ring is held from the player's **current** column, so crossing a
//! boundary is what asks for the five new columns ahead. A player who has just
//! crossed is standing at the edge of their new column and their box is still
//! over the two columns that were already resident; they have to walk the
//! width of a column — sixteen blocks, 1.6 seconds even at the speed limit
//! `dust_guard::SpeedLimit` allows and about 3.7 at a walk — before they can
//! touch one of the new ones. Warming a whole cold ring of nine is **20.3 ms,
//! measured**. **The margin is about eighty to one, and it is bounded by the
//! speed limit rather than by a lock**: the only way to reach a column that is
//! not yet built is to move faster than the server will believe, which is
//! separately refused.
//!
//! A column that loses its last holder is **retired rather than dropped**, and
//! the retired ones go wholesale when there are more than [`RETIRED_CAP`] of
//! them. That is one line of policy for one real player: somebody walking back
//! and forth across a chunk boundary — a corridor, a farm, a doorway — would
//! otherwise rebuild the same column every few seconds. The measurement is in
//! the decision record; a there-and-back walk builds 27 columns without the
//! retired tier and 9 with it.
//!
//! # What it costs
//!
//! **A column of a real world is 111 KB, measured** — `benches/movement.rs`
//! counts the heap under 256 of them — and not the megabyte three modules'
//! documentation has claimed for the life of the project. So:
//!
//! | | before, 4 a session | after, 9 shared |
//! | --- | --- | --- |
//! | 1 player | 0.4 MB | 1.0 MB |
//! | 10 players together | 4.4 MB | 1.0 MB |
//! | 10 players apart | 4.4 MB | 10 MB + 7 MB retired |
//! | 100 players apart | 44 MB | 100 MB + 7 MB retired |
//!
//! **This is more memory, not less, for players who are spread out**, and the
//! honest reading is that the efficiency case was nine times weaker than it
//! looked before anybody measured a column. What justifies it is the first
//! priority and not the second: 11.28 ms in one movement packet, and 555,197 ns
//! a tick for one falling item. The retired tier is a flat 7 MB for the whole
//! server, not per player.
//!
//! Zero of all of it on a flat world, which lends one template column to every
//! position and has nothing to be resident.
//!
//! # Thread safety, and what "serialised" means here
//!
//! One `RwLock` over one map, and **the lock is never held across a region
//! read**. What that buys, caller by caller:
//!
//! - **Movement checks**, on every session's task on every tokio worker
//!   thread, take the read lock, clone an `Arc` and drop it. They never build
//!   anything while holding it and never wait on a disk.
//! - **Holds and releases** take the write lock for the length of nine hash
//!   lookups. They do no I/O at all, which is why they are safe to do on the
//!   session's own task: a hold is a refcount, and only the *build* is
//!   expensive.
//! - **[`Residency::fill`]**, from whatever thread built the column, takes the
//!   write lock to insert one entry. The build happened outside it.
//!
//! It is deliberately **not** serialised against [`super::source::AnvilWorld`]'s
//! region-file mutex. Two sessions warming the same cold column will both build
//! it and the second `fill` will find it already there and drop its copy. That
//! is duplicated work, bounded to once per column per simultaneous arrival, and
//! it is the trade taken on purpose: holding this lock across the region mutex
//! would put disk latency inside every movement check on the server.
//!
//! An `Arc<Chunk>` that has already been handed out **outlives eviction**. The
//! last holder leaving drops the map's entry, not the chunk, so a movement
//! check reading a column that was retired mid-read sees the whole column it
//! started with rather than a freed or half-replaced one.
//!
//! # Why this needs no invalidation
//!
//! It holds the column **as generated** — a pure function of its position, from
//! a region file the running server never writes to — which is the same reason
//! [`super::collide::Ground`]'s per-session cache needs none. Everything that
//! changes under a player is an edit, and edits are read live out of
//! [`super::edits::EditedWorld`] ahead of any column, on every lookup.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use dust_world::chunk::Chunk;
use dust_world::coords::ChunkPos;

/// How far around a player's own column the server keeps columns resident, in
/// columns. See the module documentation for why one is enough and what the
/// margin is.
pub const RESIDENT_RADIUS: i32 = 1;

/// How many columns nobody is holding are kept before all of them are dropped.
///
/// A bound, not a working set. The columns anybody is standing near are held
/// and are not in this number at all; this is only how much of a walk that
/// never comes back is remembered on the chance that it does. Sixty-four is
/// about seven players' worth of rings, and it is emptied wholesale rather
/// than a row at a time for the same reason the sky-floor cache is: a set that
/// has passed the cap is mostly columns nobody is near, and the cost of being
/// wrong is building the current ring once.
pub const RETIRED_CAP: usize = 64;

/// One resident column.
struct Resident {
    /// The column as generated, or `None` for a column that is spoken for and
    /// not yet built. The two are separate states because a hold is taken on
    /// the session's own task and the build is not: between the two, the
    /// column is claimed by somebody and there is nothing to read yet.
    chunk: Option<Arc<Chunk>>,
    /// How many players' rings cover this column. Zero is a retired column —
    /// kept, not dropped, until [`RETIRED_CAP`] of them have accumulated.
    holders: u32,
}

/// The columns the server is keeping, and how many of them nobody holds.
///
/// The count is carried rather than derived, and that is not a micro-optimism:
/// deriving it is a scan of the whole map, and [`Residency::release_columns`]
/// went from a caller that ran when a player crossed a chunk boundary to one
/// that runs fifty times a second per session the moment the chunk stream
/// started giving columns back. Counting it cost a session's settled
/// neighbours **41 ms of worst-case chat round trip against 758** on a world
/// read from region files, where a column is cheap enough that four joins
/// release two thousand of them a second.
#[derive(Default)]
struct Kept {
    columns: HashMap<(i32, i32), Resident>,
    /// How many entries in `columns` have `holders == 0`. Maintained by every
    /// path that moves a holder count across zero, and by nothing else.
    retired: usize,
}

/// The columns the server is keeping.
#[derive(Default)]
pub struct Residency {
    columns: RwLock<Kept>,
}

impl std::fmt::Debug for Residency {
    /// What a reader wants is how many columns are being kept and how many of
    /// them nobody is standing near, which are the two numbers the policy is
    /// about.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kept = self
            .columns
            .read()
            .expect("the residency is never poisoned");
        f.debug_struct("Residency")
            .field("columns", &kept.columns.len())
            .field("retired", &kept.retired)
            .finish()
    }
}

/// Every column within `RESIDENT_RADIUS` of `centre`.
fn ring(centre: ChunkPos) -> impl Iterator<Item = (i32, i32)> {
    (-RESIDENT_RADIUS..=RESIDENT_RADIUS).flat_map(move |dx| {
        (-RESIDENT_RADIUS..=RESIDENT_RADIUS).map(move |dz| (centre.x + dx, centre.z + dz))
    })
}

impl Residency {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The column at `pos`, if the server is keeping one and it has been built.
    ///
    /// A read lock and an `Arc` clone. `None` means "ask the source", never
    /// "there is nothing there": a caller that gets `None` builds the column
    /// itself, which is exactly what every caller did before this existed.
    #[must_use]
    pub fn resident(&self, pos: ChunkPos) -> Option<Arc<Chunk>> {
        self.columns
            .read()
            .expect("the residency is never poisoned")
            .columns
            .get(&(pos.x, pos.z))?
            .chunk
            .clone()
    }

    /// Claim the ring around `centre` for one player.
    ///
    /// Refcounts and nothing else — no file is opened here, which is what makes
    /// this safe to call from the session task that just read a movement
    /// packet. [`Residency::cold`] and [`Residency::fill`] are the half that
    /// costs something, and they belong on another thread.
    pub fn hold(&self, centre: ChunkPos) {
        let mut kept = self
            .columns
            .write()
            .expect("the residency is never poisoned");
        for key in ring(centre) {
            Self::take(&mut kept, key);
        }
    }

    /// One holder more on `key`, and the retired count kept honest.
    ///
    /// A column that was not in the map at all was never retired, so it is not
    /// un-retired here. Writing this as "if it has no holders, one fewer
    /// retired" underflows the count on the first column any claim ever takes.
    fn take(kept: &mut Kept, key: (i32, i32)) {
        let Kept { columns, retired } = kept;
        match columns.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if entry.get().holders == 0 {
                    *retired -= 1;
                }
                entry.get_mut().holders += 1;
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Resident {
                    chunk: None,
                    holders: 1,
                });
            }
        }
    }

    /// One holder fewer on `key`, and the retired count kept honest. A column
    /// this is called for that is not there is not an error: a claim may
    /// outlive a wholesale retirement.
    fn give_back(kept: &mut Kept, key: (i32, i32)) {
        if let Some(entry) = kept.columns.get_mut(&key) {
            if entry.holders > 0 {
                entry.holders -= 1;
                if entry.holders == 0 {
                    kept.retired += 1;
                }
            }
        }
    }

    /// Give up one player's claim on the ring around `centre`.
    ///
    /// A column that loses its last holder stays in the map with `holders` at
    /// zero — retired, not dropped — until there are more than [`RETIRED_CAP`]
    /// such columns, at which point all of them go at once.
    pub fn release(&self, centre: ChunkPos) {
        let mut kept = self
            .columns
            .write()
            .expect("the residency is never poisoned");
        for key in ring(centre) {
            Self::give_back(&mut kept, key);
        }
        Self::retire(&mut kept);
    }

    /// Drop every column nobody holds, but only once there are more of them
    /// than the cap. See [`RETIRED_CAP`].
    ///
    /// **The test is a comparison, not a scan.** This is called on every
    /// release, and the chunk stream releases a column for every column it
    /// sends — so counting the retired ones here put a walk of the whole map
    /// inside the write lock fifty times a second per player. The `retain`
    /// below is still a walk, and it still happens: once every sixty-four
    /// retirements rather than on every one of them.
    fn retire(kept: &mut Kept) {
        if kept.retired > RETIRED_CAP {
            kept.columns.retain(|_, c| c.holders > 0);
            kept.retired = 0;
        }
    }

    /// Claim named columns for one holder, rather than a ring around a player.
    ///
    /// The second access pattern, and the reason `hold` is not the only way in.
    /// A player is a moving window and the ring is the right shape for it; a
    /// heap of item entities lying in a tunnel is a **static set** of whatever
    /// columns they happen to be in, which may be four chunks from anybody —
    /// `net::items::TICK_RADIUS` is 64 blocks — and is not a ring around
    /// anything. Both end up as the same refcount on the same column, which is
    /// the point: two players and a pile of cobblestone standing on one column
    /// keep one copy of it between them.
    pub fn hold_columns(&self, columns: &[ChunkPos]) {
        let mut kept = self
            .columns
            .write()
            .expect("the residency is never poisoned");
        for pos in columns {
            Self::take(&mut kept, (pos.x, pos.z));
        }
    }

    /// Give up a claim taken by [`Residency::hold_columns`].
    pub fn release_columns(&self, columns: &[ChunkPos]) {
        let mut kept = self
            .columns
            .write()
            .expect("the residency is never poisoned");
        for pos in columns {
            Self::give_back(&mut kept, (pos.x, pos.z));
        }
        Self::retire(&mut kept);
    }

    /// Take `added` and give up `dropped`, under **one** write lock.
    ///
    /// The chunk stream's window slides every twenty milliseconds per session
    /// and both halves happen together; two calls is two acquisitions of a
    /// lock that a movement check on every other session is waiting to read.
    /// Additions go first, so a column in both sets never falls to zero
    /// holders in between and is never retired and rebuilt.
    pub fn exchange(&self, added: &[ChunkPos], dropped: &[ChunkPos]) {
        if added.is_empty() && dropped.is_empty() {
            return;
        }
        let mut kept = self
            .columns
            .write()
            .expect("the residency is never poisoned");
        for pos in added {
            Self::take(&mut kept, (pos.x, pos.z));
        }
        for pos in dropped {
            Self::give_back(&mut kept, (pos.x, pos.z));
        }
        Self::retire(&mut kept);
    }

    /// The columns in the ring around `centre` that are held and not yet built.
    ///
    /// The list a builder works through. Taken as a snapshot under the
    /// read lock and then let go of, because building them is the part that
    /// takes a couple of milliseconds each and no lock may be held across it.
    #[must_use]
    pub fn cold(&self, centre: ChunkPos) -> Vec<ChunkPos> {
        self.cold_columns(
            &ring(centre)
                .map(|(x, z)| ChunkPos::new(x, z))
                .collect::<Vec<_>>(),
        )
    }

    /// The same question about a named set of columns.
    #[must_use]
    pub fn cold_columns(&self, columns: &[ChunkPos]) -> Vec<ChunkPos> {
        let kept = self
            .columns
            .read()
            .expect("the residency is never poisoned");
        columns
            .iter()
            .filter(|pos| {
                kept.columns
                    .get(&(pos.x, pos.z))
                    .is_some_and(|c| c.chunk.is_none())
            })
            .copied()
            .collect()
    }

    /// Put a built column in, if anybody is still keeping it.
    ///
    /// Returns whether it was kept. `false` is the ordinary outcome for a
    /// player who walked away while their column was being read, and for the
    /// loser of a race between two sessions warming the same column: the
    /// second one to arrive finds a chunk already there and its own copy is
    /// dropped on the spot. Duplicated work, never duplicated memory.
    pub fn fill(&self, pos: ChunkPos, chunk: Chunk) -> bool {
        let mut kept = self
            .columns
            .write()
            .expect("the residency is never poisoned");
        match kept.columns.get_mut(&(pos.x, pos.z)) {
            Some(entry) if entry.chunk.is_none() => {
                entry.chunk = Some(Arc::new(chunk));
                true
            }
            _ => false,
        }
    }

    /// Put a run of built columns in under **one** write lock.
    ///
    /// Same rule as [`Residency::fill`] for each of them; returns how many were
    /// kept. This exists because the write lock is the one every movement check
    /// on every other session is waiting to read, and a builder that takes it
    /// once per column takes it about a thousand times a second on a world read
    /// from region files — where a column is cheap enough that four joins can
    /// ask for two thousand of them. The columns are built with nothing held
    /// and offered together.
    pub fn fill_many(&self, built: Vec<(ChunkPos, Chunk)>) -> usize {
        if built.is_empty() {
            return 0;
        }
        let mut kept = self
            .columns
            .write()
            .expect("the residency is never poisoned");
        let mut filled = 0;
        for (pos, chunk) in built {
            if let Some(entry) = kept.columns.get_mut(&(pos.x, pos.z)) {
                if entry.chunk.is_none() {
                    entry.chunk = Some(Arc::new(chunk));
                    filled += 1;
                }
            }
        }
        filled
    }

    /// How many columns the server is keeping, held and retired together.
    #[must_use]
    pub fn len(&self) -> usize {
        self.columns
            .read()
            .expect("the residency is never poisoned")
            .columns
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One player's claim on the columns around them, given up however their
/// session ends.
///
/// A guard rather than a pair of calls, and for the same reason
/// `Counters::player_joined` is one: a session ends by disconnecting, by timing
/// out, by failing to decode a packet and by panicking, and a claim that is
/// only released on the tidy path is a claim that is sometimes never released.
/// A residency that leaks nine columns per crashed session is the memory leak
/// this whole module exists to not be.
pub struct Residence {
    /// The server's resident set, or `None` for a world that keeps nothing —
    /// a flat world lends one template column to every position, so there is
    /// nothing for a claim to be a claim on.
    residency: Option<Arc<Residency>>,
    /// The column this player was last known to be standing in, or `None` for
    /// a session that has not been placed yet.
    centre: Option<ChunkPos>,
}

impl std::fmt::Debug for Residence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Residence")
            .field("centre", &self.centre)
            .finish_non_exhaustive()
    }
}

impl Residence {
    #[must_use]
    pub fn new(residency: Option<Arc<Residency>>) -> Self {
        Self {
            residency,
            centre: None,
        }
    }

    /// Move this player's claim to the ring around `centre`, and say whether
    /// that was a change.
    ///
    /// `false` means the player is still in the column they were in, which is
    /// what all but about one movement packet in seventy says, and it costs a
    /// comparison. `true` means the caller should warm the new ring — **off
    /// this thread**, because warming reads region files and this does not.
    ///
    /// The new ring is claimed *before* the old one is given up, so a column
    /// in both — which is six of the nine, for a step across a boundary —
    /// never falls to zero holders in between and is never retired and rebuilt
    /// for a player who did not leave it.
    pub fn move_to(&mut self, centre: ChunkPos) -> bool {
        if self.centre == Some(centre) {
            return false;
        }
        let Some(residency) = &self.residency else {
            self.centre = Some(centre);
            return true;
        };
        residency.hold(centre);
        if let Some(previous) = self.centre.replace(centre) {
            residency.release(previous);
        }
        true
    }
}

impl Drop for Residence {
    fn drop(&mut self) {
        if let (Some(residency), Some(centre)) = (&self.residency, self.centre) {
            residency.release(centre);
        }
    }
}

/// A claim on a set of columns that is not a ring around anybody.
///
/// The second holder, and the reason [`Residency`] has two ways in. A player is
/// a **moving window** — nine columns that slide along as they walk, which
/// [`Residence`] handles by holding the new ring before letting the old one go.
/// The item entities are a **static set**: a heap of cobblestone lies where it
/// was dropped, is simulated from up to four chunks away
/// (`net::items::TICK_RADIUS`), and belongs to no player's ring. Left to itself
/// that set would rebuild its columns twenty times a second forever — W1's
/// measurement of a falling item on a region world, 558,308 ns a tick against
/// 38 on a flat one, is exactly that — and a claim that was never given up
/// would pin those columns after the items had despawned.
///
/// So this holds a set and is told a new one. It works out the difference,
/// takes the additions before it drops the removals so a column in both is
/// never retired and rebuilt, and gives the whole thing up when it is dropped.
pub struct ColumnClaim {
    residency: Option<Arc<Residency>>,
    /// Where a newly claimed column goes to be built, off this thread. See
    /// [`super::source::Source::want`].
    warm: Option<super::source::Warming>,
    held: Vec<ChunkPos>,
}

impl std::fmt::Debug for ColumnClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnClaim")
            .field("held", &self.held.len())
            .finish_non_exhaustive()
    }
}

impl ColumnClaim {
    #[must_use]
    pub fn new(residency: Option<Arc<Residency>>, warm: Option<super::source::Warming>) -> Self {
        Self {
            residency,
            warm,
            held: Vec::new(),
        }
    }

    /// Hold exactly `wanted` from now on, and ask for anything new in it to be
    /// built off this thread.
    ///
    /// Sorted-vector difference rather than a hash set. The sets here are a
    /// handful of columns — a mining player leaves items in one or two — and
    /// the caller runs twenty times a second, so what matters is that the
    /// common case, an unchanged set, costs one comparison of two short slices
    /// and no allocation at all.
    pub fn set(&mut self, wanted: &mut Vec<ChunkPos>) {
        wanted.sort_unstable_by_key(|pos| (pos.x, pos.z));
        wanted.dedup();
        let Some(residency) = &self.residency else {
            return;
        };
        if self.held == *wanted {
            return;
        }
        let added: Vec<ChunkPos> = wanted
            .iter()
            .filter(|pos| !self.held.contains(pos))
            .copied()
            .collect();
        let dropped: Vec<ChunkPos> = self
            .held
            .iter()
            .filter(|pos| !wanted.contains(pos))
            .copied()
            .collect();
        residency.exchange(&added, &dropped);
        self.held.clear();
        self.held.extend_from_slice(wanted);
        if !added.is_empty() {
            if let Some(warm) = &self.warm {
                // A closed queue takes nothing, and there is nothing to warm
                // for a world that is going away.
                warm.send(added);
            }
        }
    }
}

impl Drop for ColumnClaim {
    fn drop(&mut self) {
        if let Some(residency) = &self.residency {
            residency.release_columns(&self.held);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_world::heightmap::WorldHeight;

    fn a_column() -> Chunk {
        Chunk::uniform(ChunkPos::new(0, 0), WorldHeight::new(-64, 384), 2, 2, 0, 0)
    }

    fn at(x: i32, z: i32) -> ChunkPos {
        ChunkPos::new(x, z)
    }

    #[test]
    fn a_hold_claims_the_ring_and_nothing_further() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        assert_eq!(residency.len(), 9);
        assert_eq!(residency.cold(at(0, 0)).len(), 9);
        // Two columns away is outside the ring and was never claimed, so the
        // caller that asks about it gets nothing and builds its own.
        assert!(residency.resident(at(2, 0)).is_none());
    }

    #[test]
    fn a_filled_column_is_shared_rather_than_copied() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        assert!(residency.fill(at(0, 0), a_column()));
        let one = residency.resident(at(0, 0)).expect("just filled");
        let two = residency.resident(at(0, 0)).expect("still there");
        assert!(Arc::ptr_eq(&one, &two), "two readers, one column");
        // The loser of a race drops its own copy rather than replacing the
        // one that is already being read.
        assert!(!residency.fill(at(0, 0), a_column()));
    }

    #[test]
    fn a_column_nobody_holds_is_only_dropped_once_the_retired_cap_is_passed() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        residency.release(at(0, 0));
        // Retired, not dropped: the player who walks back over the boundary
        // they just crossed finds their column still there.
        assert_eq!(residency.len(), 9);
        // Enough traffic to pass the cap, and every unheld column goes.
        for x in 0..40 {
            residency.hold(at(x * 4, 0));
            residency.release(at(x * 4, 0));
        }
        assert!(
            residency.len() <= RETIRED_CAP + 9,
            "the retired tier is a bound, not a working set: {}",
            residency.len()
        );
    }

    #[test]
    fn a_column_two_players_hold_survives_one_of_them_leaving() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        residency.hold(at(1, 0));
        residency.fill(at(1, 0), a_column());
        residency.release(at(0, 0));
        // (1,0) is in both rings, so the first player leaving does not retire
        // it — and the second player's movement check still reads it.
        assert!(residency.resident(at(1, 0)).is_some());
        let kept = residency.columns.read().expect("not poisoned");
        assert_eq!(kept.columns[&(1, 0)].holders, 1);
    }

    /// The order [`Residence::move_to`] takes its two steps in, checked by the
    /// one thing that can see the difference.
    ///
    /// Six of the nine columns are in both rings when a player steps across a
    /// boundary. Releasing the old ring first drops those six to zero holders
    /// for an instant — and `release` is where the retired tier is emptied, so
    /// a server already over [`RETIRED_CAP`] throws away the column the player
    /// is standing on and rebuilds it. Holding first means they never reach
    /// zero.
    ///
    /// This is why the check has to push the map over the cap first: below it
    /// a retired column is kept, and the wrong order is invisible.
    #[test]
    fn a_step_across_a_boundary_does_not_drop_the_columns_it_keeps() {
        let residency = Arc::new(Residency::new());
        for x in 0..(RETIRED_CAP as i32) {
            residency.hold(at(x * 4, 100));
            residency.release(at(x * 4, 100));
        }
        let mut player = Residence::new(Some(Arc::clone(&residency)));
        assert!(player.move_to(at(0, 0)));
        assert!(residency.fill(at(1, 0), a_column()));
        // A step from column 0 to column 1. (1, 0) is in both rings, and the
        // player is now standing on it.
        assert!(player.move_to(at(1, 0)));
        assert!(
            residency.resident(at(1, 0)).is_some(),
            "the column under the player was retired while they stepped onto it"
        );
    }

    /// A session that ends any way at all gives its ring back.
    #[test]
    fn a_session_that_ends_gives_its_ring_back() {
        let residency = Arc::new(Residency::new());
        {
            let mut player = Residence::new(Some(Arc::clone(&residency)));
            player.move_to(at(0, 0));
            let held = residency.columns.read().expect("not poisoned");
            assert_eq!(held.columns.values().filter(|c| c.holders > 0).count(), 9);
        }
        let held = residency.columns.read().expect("not poisoned");
        assert_eq!(
            held.columns.values().filter(|c| c.holders > 0).count(),
            0,
            "nine columns outlived the session that claimed them"
        );
    }

    /// The item world's claim follows its items and is given up with them.
    #[test]
    fn a_claim_follows_what_it_is_asked_for_and_ends_with_it() {
        let residency = Arc::new(Residency::new());
        {
            let mut claim = ColumnClaim::new(Some(Arc::clone(&residency)), None);
            claim.set(&mut vec![at(0, 0), at(1, 0), at(0, 0)]);
            assert_eq!(residency.len(), 2, "the duplicate was claimed twice");
            // The items moved on. What is still wanted keeps its holder; what
            // is not loses it.
            claim.set(&mut vec![at(1, 0), at(2, 0)]);
            let held = residency.columns.read().expect("not poisoned");
            assert_eq!(held.columns[&(0, 0)].holders, 0);
            assert_eq!(
                held.columns[&(1, 0)].holders,
                1,
                "a column wanted twice running"
            );
            assert_eq!(held.columns[&(2, 0)].holders, 1);
        }
        let held = residency.columns.read().expect("not poisoned");
        assert!(
            held.columns.values().all(|c| c.holders == 0),
            "the items despawned and their columns stayed claimed"
        );
    }

    /// The retired count is a second answer to a question the map already
    /// answers, and a second answer that drifts is worse than a scan.
    #[test]
    fn the_retired_count_agrees_with_the_map_through_every_path() {
        let residency = Residency::new();
        let agree = |what: &str| {
            let kept = residency.columns.read().expect("not poisoned");
            let scanned = kept.columns.values().filter(|c| c.holders == 0).count();
            assert_eq!(kept.retired, scanned, "{what}");
        };
        residency.hold(at(0, 0));
        agree("after a ring was taken");
        residency.hold_columns(&[at(5, 5), at(5, 6)]);
        agree("after named columns were taken");
        residency.exchange(&[at(5, 7)], &[at(5, 5)]);
        agree("after an exchange");
        residency.release(at(0, 0));
        agree("after the ring went back");
        residency.release_columns(&[at(5, 6), at(5, 7)]);
        agree("after the named columns went back");
        // And a column that was never held: `give_back` must not count one.
        residency.release_columns(&[at(90, 90)]);
        agree("after a column nobody ever held was released");
    }

    #[test]
    fn a_fill_for_a_column_nobody_kept_is_dropped() {
        let residency = Residency::new();
        assert!(!residency.fill(at(5, 5), a_column()));
        assert_eq!(residency.len(), 0);
    }
}
