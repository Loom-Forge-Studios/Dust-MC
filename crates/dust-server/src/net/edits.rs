//! The blocks players have changed, and how everybody hears about it.
//!
//! # Why the edits are a layer over the world rather than inside it
//!
//! `FlatWorld` builds one column and every position shares it — which is
//! correct for a world where every position *is* the same, and stops being
//! correct the moment somebody breaks a block. The obvious repair is to give
//! the world a chunk per position and mutate those. That is also what a real
//! generator will want, and it is not what this should be yet: it would mean
//! keeping a megabyte per column for a world whose columns are still identical
//! except in the handful of cells a player has touched.
//!
//! So an edit is a `(position, state)` and the world is generated-plus-edits.
//! A chunk goes out as the template with its edits applied; a block query is
//! the edit if there is one and the template otherwise. That representation is
//! honest about what is actually different, and it is what a chunk cache would
//! be built *from* rather than something a cache would replace.
//!
//! # What is deliberately not here
//!
//! No physics, no block updates, no drops and no tool checks. Every one of
//! those is a rule about *the game* rather than about the world's storage, and
//! the place they go is between this and the session, not inside either. The
//! gap is worth stating because "you can place blocks" invites the assumption
//! that placing them follows any rules at all.
//!
//! **Reach is the first of them to be built, and it went where that sentence
//! said it would**: `dust-guard`, checked in the session before either verb
//! reaches this module. A player no longer breaks bedrock from across the map.
//! What still has no check at all is *movement* — the position a reach is
//! measured from is whatever the client last claimed — so the rule this
//! enforces is "you may not act far from where you say you are" and not "you
//! may not act far from where you are".

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use dust_protocol::types::Position;
use dust_world::chunk::Chunk;

use super::source::Column;
use dust_world::coords::ChunkPos;
use tokio::sync::broadcast;

use super::source::Source;

/// The six sides of a cell, in [`Face`] order — which is the order
/// `dust_sim::placement::Around` indexes by, so the two never need translating.
///
/// [`Face`]: dust_sim::placement::Face
const SIDES: [dust_sim::placement::Face; 6] = [
    dust_sim::placement::Face::Down,
    dust_sim::placement::Face::Up,
    dust_sim::placement::Face::North,
    dust_sim::placement::Face::South,
    dust_sim::placement::Face::West,
    dust_sim::placement::Face::East,
];

/// The cell one step from `position` in a given direction.
fn offset(position: Position, side: dust_sim::placement::Face) -> Position {
    use dust_sim::placement::Face;
    let (x, y, z) = match side {
        Face::Down => (0, -1, 0),
        Face::Up => (0, 1, 0),
        Face::North => (0, 0, -1),
        Face::South => (0, 0, 1),
        Face::West => (-1, 0, 0),
        Face::East => (1, 0, 0),
    };
    Position {
        x: position.x + x,
        y: position.y + y,
        z: position.z + z,
    }
}

/// A column, as the edit map keys it. Not [`ChunkPos`], because this is a hash
/// key and giving a domain type a `Hash` it does not otherwise need would put
/// the map's requirements into the world's vocabulary.
type ColumnKey = (i32, i32);

/// One column's changed cells, by `(x, y, z)` with x and z local to the column.
type ColumnEdits = HashMap<(i32, i32, i32), u32>;

/// One block changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    pub position: Position,
    pub state: u32,
    /// Who changed it and how, when a player did. `None` for a restore from
    /// the save file and for anything the server does on its own.
    pub by: Option<Player>,
}

/// A block change a player is responsible for, and what it looked like.
///
/// One field rather than two `Option`s, because at most one of them was ever
/// going to be set and a pair of them is a state this cannot be in. Every arm
/// carries the entity id for the same reason: the player who did it is the one
/// player who must **not** be sent the effect, since their own client played it
/// before the server heard about the click, and telling them again plays it
/// twice. Vanilla leaves them out for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Player {
    /// A block a player broke, carrying what was there.
    ///
    /// The state is what the client makes the particles and the sound out of —
    /// the *broken* block's, not the air left behind.
    Broke {
        /// The block state that was there.
        previous: u32,
        /// The player who broke it.
        by: i32,
    },
    /// A block a player put down, carrying what it now is.
    ///
    /// The state is the same one [`Edit::state`] holds, and it is repeated here
    /// rather than read off the edit because the two mean different things: the
    /// edit's state is what the world now holds, and this one is what a sound
    /// is chosen from. They agree today and the day a placement leaves
    /// something else behind they will not.
    Placed {
        /// The block state that went down.
        placed: u32,
        /// The player who placed it.
        by: i32,
        /// Which of the sound event's samples every listener hears.
        ///
        /// Decided here, once, rather than per session — that is the whole
        /// reason it is on the event. Two players watching one block go down
        /// are watching one event, and a seed drawn where the packet is built
        /// would give them different samples of it.
        seed: i64,
    },
}

impl Player {
    /// The entity id of the player responsible, whatever they did.
    pub fn entity_id(self) -> i32 {
        match self {
            Self::Broke { by, .. } | Self::Placed { by, .. } => by,
        }
    }
}

/// How many block changes a slow session may fall behind before it is told it
/// missed some.
///
/// A `broadcast` channel drops the oldest for a receiver that lags, and says
/// so rather than silently skipping — which matters, because a session that
/// missed an edit is showing a world that is wrong and the honest repair is to
/// resend the column rather than to carry on. Sixty-four is a couple of
/// seconds of one player mining at speed.
const EDIT_BACKLOG: usize = 64;

/// The world as it currently stands: what was generated, plus what was changed.
#[derive(Debug)]
pub struct EditedWorld {
    generated: Source,
    /// Keyed by column so applying edits to a chunk is one lookup rather than
    /// a scan of every edit in the world.
    edits: RwLock<HashMap<ColumnKey, ColumnEdits>>,
    announce: broadcast::Sender<Edit>,
    /// How many events have chosen a sound sample, which is what the next one
    /// is derived from. See [`EditedWorld::next_seed`].
    sounds: std::sync::atomic::AtomicU64,
    /// Minecraft's own per-state constants, when the operator put a table
    /// beside their data. The only thing this module reads out of it is which
    /// of a block state's faces are full, which is what a fence, a wall and a
    /// glass pane ask of the block beside them — see [`EditedWorld::reshape`].
    constants: Option<Arc<dust_registry::BlockConstants>>,
    /// Cells whose surroundings have changed and that nobody has looked at
    /// yet. Filled by every write here and drained by
    /// [`super::updates::WorldTicker`] — the world says *what changed* and
    /// the tick loop decides *what to do about it*, because deciding needs
    /// the loot tables and the entity worlds and this module has neither.
    pending: RwLock<super::updates::Pending>,
}

impl EditedWorld {
    pub fn new(generated: Source) -> Self {
        let (announce, _) = broadcast::channel(EDIT_BACKLOG);
        Self {
            generated,
            edits: RwLock::new(HashMap::new()),
            announce,
            sounds: std::sync::atomic::AtomicU64::new(0),
            constants: None,
            pending: RwLock::new(super::updates::Pending::default()),
        }
    }

    /// The table that says which of a block state's faces are full.
    ///
    /// A separate step rather than an argument to [`EditedWorld::new`] because
    /// a world without one is a world that works — it lights nothing and makes
    /// no sound either — and five callers that do not have one should not have
    /// to say so.
    #[must_use]
    pub fn with_constants(mut self, constants: Option<Arc<dust_registry::BlockConstants>>) -> Self {
        self.constants = constants;
        self
    }

    /// The predicate the shape rules need, if this world has the table for it.
    fn solid(&self) -> Option<dust_sim::placement::Solid<'_>> {
        dust_sim::placement::Solid::from_constants(self.constants.as_deref()?)
    }

    /// What is in the six cells around one.
    ///
    /// Public because the block-update tick asks the same question of the same
    /// six cells, in the same [`Face`](dust_sim::placement::Face) order, and
    /// two spellings of "what is beside this" would be two chances to index
    /// the array the other way round.
    pub fn around(&self, position: Position) -> dust_sim::placement::Around {
        let mut around = dust_sim::placement::Around::empty();
        for side in SIDES {
            let state = self.block_at(offset(position, side));
            if let Some(state) = dust_registry::BlockState::from_id(state) {
                around = around.with(side, state);
            }
        }
        around
    }

    /// The state a block takes in this cell, given what is already around it.
    ///
    /// Applied **before** the write and not after, so that a fence goes into
    /// the world with its arms already on. Written the other way the cell would
    /// be announced twice for one click, and a player would watch the fence
    /// appear bare and then connect.
    fn shaped_at(&self, position: Position, state: u32) -> u32 {
        let Some(solid) = self.solid() else {
            return state;
        };
        let Some(placed) = dust_registry::BlockState::from_id(state) else {
            return state;
        };
        if !dust_sim::placement::reads_neighbours(placed) {
            return state;
        }
        dust_sim::placement::shaped(placed, self.around(position), solid).id()
    }

    /// Give every cell around `position` the shape its surroundings now imply.
    ///
    /// **This is the half that makes a fence a fence.** A rule applied only
    /// where the click landed connects a fence to what was already there and
    /// not to what arrives later, so a wall built west to east has arms and the
    /// same wall built east to west does not. The player sees a half-connected
    /// fence, which is worse to look at than one that never connects.
    ///
    /// One ring and not a search. A fence's connection reads whether its
    /// neighbour *is* a fence and never which way that fence is connected, so
    /// nothing here cascades — with one exception worth naming rather than
    /// hiding: a wall's post reads the post of the wall directly above it, so
    /// changing the top of a stack three walls high leaves the bottom one a
    /// tick behind. That is the whole of what one ring misses.
    ///
    /// Costs six block reads and, for each neighbour that is a fence, a wall, a
    /// pane or a stair, six more. A neighbour that is stone costs one property
    /// scan and nothing else, which is what almost every neighbour of almost
    /// every edit actually is.
    fn reshape(&self, position: Position) {
        let Some(solid) = self.solid() else { return };
        for side in SIDES {
            let at = offset(position, side);
            let Some(state) = dust_registry::BlockState::from_id(self.block_at(at)) else {
                continue;
            };
            if !dust_sim::placement::reads_neighbours(state) {
                continue;
            }
            let next = dust_sim::placement::shaped(state, self.around(at), solid);
            if next != state {
                // Announced with nobody's name on it, because nobody did it:
                // a fence growing an arm is not a placement and makes no sound.
                self.set_block(at, next.id());
            }
        }
    }

    /// Listen for every edit made from now on.
    ///
    /// Subscribing before the first chunk is sent is what keeps a session from
    /// missing an edit in the window between generating a column and starting
    /// to listen. A duplicate — an edit both applied to the chunk and
    /// announced — is harmless, because setting a block to the state it
    /// already holds is not observable; a missed one is a wrong world.
    pub fn subscribe(&self) -> broadcast::Receiver<Edit> {
        self.announce.subscribe()
    }

    /// The column at `pos`, with every edit in it applied.
    pub fn chunk(&self, pos: ChunkPos) -> Chunk {
        let mut chunk = self.generated.column(pos).as_chunk().clone();
        let edits = self.edits.read().expect("the edit map is never poisoned");
        if let Some(column) = edits.get(&(pos.x, pos.z)) {
            for ((x, y, z), state) in column {
                chunk.set_block(*x as u32, *y, *z as u32, *state);
            }
            // The heightmaps travel in the chunk packet and the client uses
            // them for lighting and for where rain lands, so a column with a
            // block missing from its surface has to say so. Recomputed rather
            // than adjusted because `set_block` above does not maintain them
            // and an adjustment that disagreed would be invisible.
            let air = self.generated.flat().palette().air;
            chunk.recompute_heightmaps(|_, state| state != air);
        }
        chunk
    }

    /// Whether this column has been edited at all.
    ///
    /// The common case by far is that it has not, and an untouched column can
    /// be sent from the template without a clone.
    pub fn is_edited(&self, pos: ChunkPos) -> bool {
        self.edits
            .read()
            .expect("the edit map is never poisoned")
            .contains_key(&(pos.x, pos.z))
    }

    /// The column as generated, for the untouched case.
    ///
    /// Borrowed where the source can lend one — a flat world shares a single
    /// column with every position — and built where it cannot. The caller
    /// sends it and does not keep it, so a borrow is worth having: it is the
    /// difference between a clone per column per viewer and none at all, on
    /// the path every join takes once per column in view — 289 times at the
    /// default view distance.
    pub fn template(&self, pos: ChunkPos) -> Column<'_> {
        self.generated.column(pos)
    }

    /// The state at a block position, if an edit has changed it.
    ///
    /// `None` means the world as generated still answers for that cell, not
    /// that the cell is empty. Separate from [`EditedWorld::block_at`] because
    /// a caller that has already resolved the column — which is the expensive
    /// half, and the half worth doing once for a box of cells rather than once
    /// per cell — still has to ask about the edits, and there should be one
    /// answer to what "has this been edited" means.
    pub fn edited_block_at(&self, position: Position) -> Option<u32> {
        self.edits_now().at(position)
    }

    /// Every edit in the world, borrowed, for a caller that is about to ask
    /// about a *box* of cells rather than one.
    ///
    /// [`EditedWorld::edited_block_at`] takes the map's read lock, and taking
    /// a lock is most of what it costs. That is the right shape for a caller
    /// asking one question and the wrong one for the movement check, which
    /// asks about up to twelve cells for one packet twenty times a second per
    /// player and was paying for twelve lock acquisitions to do it. This takes
    /// it once. [`Edits::is_empty`] is the other half: a world nobody has
    /// built in yet answers the whole box without a single hash of anything.
    ///
    /// The borrow is a read guard, so a placement lands the moment the box is
    /// done rather than in the middle of it — which also makes the box read
    /// against one state of the world rather than twelve.
    #[must_use]
    pub fn edits_now(&self) -> Edits<'_> {
        Edits(self.edits.read().expect("the edit map is never poisoned"))
    }

    /// Claim the columns around `centre` for one player, so that a movement
    /// check near them reads a column the server already has rather than one
    /// it has to build.
    ///
    /// Refcounts and no file reads: safe on a session's own task. See
    /// [`super::residency::Residency`] for who owns a column and when it goes.
    pub fn hold(&self, centre: ChunkPos) {
        self.generated.hold(centre);
    }

    /// Give up one player's claim on the columns around `centre`.
    pub fn release(&self, centre: ChunkPos) {
        self.generated.release(centre);
    }

    /// Claim named columns, for a caller whose working set is not a ring
    /// around a player — the item entities, whose columns are wherever they
    /// were dropped. See [`super::residency::Residency::hold_columns`].
    pub fn hold_columns(&self, columns: &[ChunkPos]) {
        self.generated.hold_columns(columns);
    }

    /// Give up a claim taken by [`EditedWorld::hold_columns`].
    pub fn release_columns(&self, columns: &[ChunkPos]) {
        self.generated.release_columns(columns);
    }

    /// Ask for claimed columns to be built and carry on, without waiting.
    ///
    /// The call every hot path wants: it hands the list to the world's own
    /// pool of builders. Safe from a session task and from the tick loop alike,
    /// which is the point of it — those are different threads with different
    /// rules and neither may read a region file.
    pub fn want(&self, columns: Vec<ChunkPos>) {
        self.generated.want(columns);
    }

    /// The same, for the ring around a player's column.
    pub fn want_ring(&self, centre: ChunkPos) {
        self.generated.want_ring(centre);
    }

    /// Build the claimed-and-missing columns around `centre`, off the network
    /// path. Returns how many were built.
    ///
    /// **Reads region files.** The caller is a blocking thread, never a
    /// session task — that is the whole point of it being a separate call from
    /// [`EditedWorld::hold`].
    pub fn warm(&self, centre: ChunkPos) -> u32 {
        self.generated.warm(centre)
    }

    /// The same, on this thread, for a named set of columns.
    pub fn warm_columns(&self, columns: &[ChunkPos]) -> u32 {
        self.generated.warm_columns(columns)
    }

    /// The server's resident set and the channel its builds go down, for a
    /// caller that keeps a claim on them. See
    /// [`super::source::Source::residency`].
    #[must_use]
    pub fn residency(&self) -> Option<Arc<super::residency::Residency>> {
        self.generated.residency()
    }

    /// How many of `columns`, counted from the front, the server has already
    /// built. See [`super::source::Source::built_prefix`].
    #[must_use]
    pub fn built_prefix(&self, columns: &[ChunkPos]) -> usize {
        self.generated.built_prefix(columns)
    }

    /// See [`super::source::Source::warming`].
    #[must_use]
    pub fn warming(&self) -> Option<super::source::Warming> {
        self.generated.warming()
    }

    /// How many columns the server is keeping resident, across all players.
    #[must_use]
    pub fn resident_columns(&self) -> usize {
        self.generated.resident_columns()
    }

    /// How far up and down this world goes.
    pub fn height(&self) -> dust_world::heightmap::WorldHeight {
        self.generated.flat().height()
    }

    /// The state at a block position.
    ///
    /// # Panics
    ///
    /// If `position.y` is outside the world's height and no edit has touched
    /// that cell. Every caller here reaches this from a position a client
    /// clicked, which the reach check has already bounded.
    pub fn block_at(&self, position: Position) -> u32 {
        if let Some(state) = self.edited_block_at(position) {
            return state;
        }
        let column = column_of(position);
        let local = local_of(position);
        self.generated
            .column(ChunkPos::new(column.0, column.1))
            .as_chunk()
            .get_block(local.0 as u32, local.1, local.2 as u32)
    }

    /// Change a block, and tell everyone listening.
    ///
    /// Returns `false` for a position outside the world's height, which is the
    /// one refusal here — a client is entitled to ask about y = 1000 and this
    /// is not the place to be surprised by it.
    pub fn set_block(&self, position: Position, state: u32) -> bool {
        if !self.set_block_quietly(position, state) {
            return false;
        }
        // Errors when nobody is listening, which is the ordinary state of a
        // server with no players and not a failure.
        let _ = self.announce.send(Edit {
            position,
            state,
            by: None,
        });
        true
    }

    /// The write half of [`EditedWorld::set_block`], without the announcement.
    ///
    /// Split out so the two announcing callers cannot drift on what "change a
    /// block" means — the height check and the map insert are one answer, and
    /// only the sentence sent about it differs.
    fn set_block_quietly(&self, position: Position, state: u32) -> bool {
        let world = self.generated.flat().height();
        if position.y < world.min_y() || position.y >= world.min_y() + world.height() as i32 {
            return false;
        }
        {
            let mut edits = self.edits.write().expect("the edit map is never poisoned");
            edits
                .entry(column_of(position))
                .or_default()
                .insert(local_of(position), state);
        }
        // **Every write, in one place.** All three of the announcing verbs go
        // through here, so a block that changes is a block whose neighbours
        // are told, whoever changed it and whether or not anybody was told
        // about the change itself. `restore` deliberately does not: a save
        // being read back is not a world reacting, and a boot that queued a
        // hundred thousand updates would spend its first ticks re-deciding
        // what the last shutdown already decided.
        self.pending
            .write()
            .expect("the pending queue is never poisoned")
            .touch(position);
        true
    }

    /// Take up to `limit` cells that need looking at.
    pub fn take_updates(&self, limit: usize, out: &mut Vec<Position>) {
        self.pending
            .write()
            .expect("the pending queue is never poisoned")
            .take(limit, out);
    }

    /// How many cells are waiting to be looked at.
    #[must_use]
    pub fn updates_pending(&self) -> usize {
        self.pending
            .read()
            .expect("the pending queue is never poisoned")
            .len()
    }

    /// How many updates were refused because the queue was full.
    #[must_use]
    pub fn updates_refused(&self) -> u64 {
        self.pending
            .read()
            .expect("the pending queue is never poisoned")
            .refused()
    }

    /// Break a block on a player's behalf, and tell everyone what broke.
    ///
    /// The same change as [`EditedWorld::set_block`] to air, plus the one
    /// thing that call cannot carry: what was there. A client makes the
    /// particles and the sound out of the *broken* block's state, so an
    /// announcement that only said "this is air now" leaves the other players
    /// watching blocks vanish in silence.
    ///
    /// **Breaking air is not breaking anything, and saying so is the point.**
    /// A creative client sends `start_digging` and a client that mines through
    /// sends `finish_digging` too, and this server honours both — so a single
    /// dig arrives twice. Setting air twice is idempotent and nothing noticed
    /// while the only announcement was the change; the moment the *effect* went
    /// out it became a second puff of particles made out of the air left
    /// behind, which is a silent one. Found by `tools/bot/check.js`, which saw
    /// effect 2001 carrying state 0.
    ///
    /// Reads the previous state under no lock the write also holds, which is a
    /// race worth naming: two players breaking the same block in the same
    /// instant can both read it unbroken and both announce it breaking. The
    /// world ends in the right state either way and the cost is one duplicate
    /// puff, which is a better trade than a lock every dig contends for.
    pub fn break_block(&self, position: Position, air: u32, by: i32) -> bool {
        let previous = self.block_at(position);
        if !self.set_block_quietly(position, air) {
            return false;
        }
        if previous == air {
            // Nothing changed, so nobody is told anything. Not even the block
            // update: a client that already holds air there would apply air to
            // air, and the packet is the same size whether it says something
            // or not.
            return true;
        }
        // The neighbours lose an arm. Breaking has to do this for the same
        // reason placing does, and forgetting it here would leave a fence
        // reaching towards a block that is not there any more.
        self.reshape(position);
        let _ = self.announce.send(Edit {
            position,
            state: air,
            by: Some(Player::Broke { previous, by }),
        });
        true
    }

    /// Put a block down on a player's behalf, and tell everyone who did it.
    ///
    /// The same change as [`EditedWorld::set_block`], plus the one thing that
    /// call cannot carry: whose it was. A placement makes a sound, and the
    /// sound goes to everybody except the player whose client already played
    /// it.
    ///
    /// **Placing what is already there announces nothing**, by the same
    /// argument [`EditedWorld::break_block`] makes about breaking air: setting
    /// a block to the state it already holds is not a change, and a sound for
    /// it is a noise with nothing behind it. A client that sends `use_item_on`
    /// twice for one click — which is a shape this server has already met once,
    /// from the other end — would otherwise be heard twice.
    /// The seed for the next sound this world makes.
    ///
    /// A counter through SplitMix64's finalizer, which is a well-distributed
    /// mixing of successive integers and is not a cryptographic anything. It
    /// does not need to be: the client uses this to pick one of a sound
    /// event's handful of samples, so the whole requirement is that
    /// consecutive placements do not all land on the same one — which a bare
    /// counter would fail, because the low bits of 0, 1, 2 are what a small
    /// modulus reads.
    ///
    /// Counting from zero, so a restarted server replays the same sequence of
    /// samples. Nobody can hear that, and a clock-seeded start would be a
    /// source of nondeterminism bought with nothing.
    fn next_seed(&self) -> i64 {
        let n = self
            .sounds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut z = n;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as i64
    }

    pub fn place_block(&self, position: Position, state: u32, by: i32) -> bool {
        let previous = self.block_at(position);
        let state = self.shaped_at(position, state);
        if !self.set_block_quietly(position, state) {
            return false;
        }
        self.reshape(position);
        if previous == state {
            return true;
        }
        let _ = self.announce.send(Edit {
            position,
            state,
            by: Some(Player::Placed {
                placed: state,
                by,
                seed: self.next_seed(),
            }),
        });
        true
    }

    /// Apply edits read from a save, without announcing any of them.
    ///
    /// Silent because nobody is connected yet: an announcement at boot would
    /// go to a channel with no receivers, and a receiver that appeared later
    /// would be told about a change already present in the first chunk it was
    /// sent.
    pub fn restore(&self, blocks: impl IntoIterator<Item = (Position, u32)>) -> usize {
        let mut edits = self.edits.write().expect("the edit map is never poisoned");
        let mut applied = 0;
        for (position, state) in blocks {
            let world = self.generated.flat().height();
            if position.y < world.min_y() || position.y >= world.min_y() + world.height() as i32 {
                continue;
            }
            edits
                .entry(column_of(position))
                .or_default()
                .insert(local_of(position), state);
            applied += 1;
        }
        applied
    }

    /// Every changed block, for writing down.
    ///
    /// Ordered, so two saves of the same world produce the same file and a
    /// diff between them is the changes rather than a reshuffle of a hash map.
    pub fn snapshot(&self) -> Vec<(Position, u32)> {
        let edits = self.edits.read().expect("the edit map is never poisoned");
        let mut out: Vec<(Position, u32)> = edits
            .iter()
            .flat_map(|((cx, cz), column)| {
                column.iter().map(move |((x, y, z), state)| {
                    (
                        Position {
                            x: (cx << 4) + x,
                            y: *y,
                            z: (cz << 4) + z,
                        },
                        *state,
                    )
                })
            })
            .collect();
        out.sort_by_key(|(p, _)| (p.x, p.y, p.z));
        out
    }

    /// How many blocks have been changed. For tests and for a status line.
    pub fn edit_count(&self) -> usize {
        self.edits
            .read()
            .expect("the edit map is never poisoned")
            .values()
            .map(HashMap::len)
            .sum()
    }
}

/// The column a block position falls in.
///
/// Shifting rather than dividing, for the reason spelled out in
/// [`super::view::column_of`]: `-1 / 16` is zero and the block west of the
/// origin is in column -1.
fn column_of(position: Position) -> (i32, i32) {
    (position.x >> 4, position.z >> 4)
}

/// Where in its column a block sits. The x and z are masked rather than
/// remaindered, because `-1 % 16` is `-1` and a chunk has no cell -1.
fn local_of(position: Position) -> (i32, i32, i32) {
    (position.x & 15, position.y, position.z & 15)
}

/// Every edit in the world, held open. See [`EditedWorld::edits_now`].
#[derive(Debug)]
pub struct Edits<'a>(std::sync::RwLockReadGuard<'a, HashMap<ColumnKey, ColumnEdits>>);

impl Edits<'_> {
    /// Whether nothing anywhere has been edited. True for a world nobody has
    /// built in, which is the common case for a server that has just started
    /// and the case worth not hashing a key for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The state at a block position, if an edit has changed it.
    #[must_use]
    pub fn at(&self, position: Position) -> Option<u32> {
        self.0
            .get(&column_of(position))
            .and_then(|c| c.get(&local_of(position)))
            .copied()
    }
}

/// A handle sessions share.
pub type SharedWorld = Arc<EditedWorld>;

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> EditedWorld {
        fresh_world()
    }

    /// A second name for the same builder, so a test that already has a
    /// `world` binding can still make another one.
    fn fresh_world() -> EditedWorld {
        let palette = super::super::world::Palette::resolve().expect("the block table");
        EditedWorld::new(Source::Flat(Box::new(super::super::world::FlatWorld::new(
            palette, 0, 64,
        ))))
    }

    /// A world that knows which of a block state's faces are full.
    ///
    /// The table is written here and says exactly the named blocks are full on
    /// every side. It is a stand-in and reaches only what its own range
    /// reaches; what says the rules are right against Minecraft is
    /// `cargo xtask harness placement`.
    fn world_knowing(names: &[&str]) -> EditedWorld {
        let states: std::collections::HashSet<u32> = names
            .iter()
            .flat_map(|name| {
                dust_registry::Block::from_name(name)
                    .expect("this build has that block")
                    .states()
                    .map(dust_registry::BlockState::id)
            })
            .collect();
        let mut text = String::from("# state_id\topacity\temission");
        for column in dust_sim::placement::STURDY {
            text.push('\t');
            text.push_str(column);
        }
        text.push('\n');
        for state in 0..dust_registry::STATE_COUNT {
            let full = u32::from(states.contains(&state));
            text.push_str(&format!("{state}\t0\t0"));
            for _ in dust_sim::placement::STURDY {
                text.push_str(&format!("\t{full}"));
            }
            text.push('\n');
        }
        let table = dust_registry::BlockConstants::parse(&text).expect("a complete table");
        fresh_world().with_constants(Some(Arc::new(table)))
    }

    fn state_of(name: &str) -> u32 {
        dust_registry::Block::from_name(name)
            .expect("this build has that block")
            .default_state()
            .id()
    }

    fn property_at(world: &EditedWorld, position: Position, property: &str) -> String {
        dust_registry::BlockState::from_id(world.block_at(position))
            .expect("a state this build has")
            .property(property)
            .expect("the block has that property")
            .to_owned()
    }

    #[test]
    fn a_fence_connects_whichever_way_the_wall_is_built() {
        // The half a placement-time rule alone cannot do. Two fences, built
        // west to east; the *first* one has to grow an arm when the second
        // arrives, and a server that only shaped the cell being written would
        // leave it reaching at nothing while its neighbour reached back.
        let world = world_knowing(&[]);
        let west = Position { x: 4, y: 70, z: 4 };
        let east = Position { x: 5, y: 70, z: 4 };
        world.place_block(west, state_of("minecraft:oak_fence"), 1);
        assert_eq!(
            property_at(&world, west, "east"),
            "false",
            "nothing there yet"
        );
        world.place_block(east, state_of("minecraft:oak_fence"), 1);
        assert_eq!(property_at(&world, west, "east"), "true", "the older fence");
        assert_eq!(property_at(&world, east, "west"), "true", "the newer fence");
    }

    #[test]
    fn a_fence_lets_go_when_what_it_held_is_broken() {
        // And the other direction, which is the same rule and a different path
        // through this module: an arm reaching at a block that is not there any
        // more is the same defect seen from the other end.
        let world = world_knowing(&["minecraft:stone"]);
        let fence = Position { x: 4, y: 70, z: 4 };
        let post = Position { x: 5, y: 70, z: 4 };
        world.place_block(post, state_of("minecraft:stone"), 1);
        world.place_block(fence, state_of("minecraft:oak_fence"), 1);
        assert_eq!(property_at(&world, fence, "east"), "true");
        let air = state_of("minecraft:air");
        world.break_block(post, air, 1);
        assert_eq!(property_at(&world, fence, "east"), "false");
    }

    #[test]
    fn a_world_with_no_table_places_a_bare_fence_rather_than_half_a_connected_one() {
        // The deliberate answer to "no constants table". Half-connected looks
        // worse than never connected, so a world that cannot answer the
        // full-face question does not guess at it.
        let world = fresh_world();
        let west = Position { x: 4, y: 70, z: 4 };
        let east = Position { x: 5, y: 70, z: 4 };
        world.place_block(west, state_of("minecraft:oak_fence"), 1);
        world.place_block(east, state_of("minecraft:oak_fence"), 1);
        assert_eq!(property_at(&world, west, "east"), "false");
    }

    #[test]
    fn a_snapshot_round_trips_through_restore_and_is_ordered() {
        let world = world();
        for (x, z) in [(5, 5), (-1, -1), (0, 0), (20, 3)] {
            world.set_block(
                Position {
                    x,
                    y: super::super::world::SURFACE_Y,
                    z,
                },
                0,
            );
        }
        let snapshot = world.snapshot();
        assert_eq!(snapshot.len(), 4);
        // Ordered, so two saves of one world are the same file and a diff
        // between them is the changes rather than a reshuffled hash map.
        let mut sorted = snapshot.clone();
        sorted.sort_by_key(|(p, _)| (p.x, p.y, p.z));
        assert_eq!(snapshot, sorted);

        // And the positions come back as world coordinates, not local ones —
        // the case that catches a column key folded in the wrong direction.
        assert!(snapshot.iter().any(|(p, _)| p.x == -1 && p.z == -1));
        assert!(snapshot.iter().any(|(p, _)| p.x == 20 && p.z == 3));

        let fresh = fresh_world();
        assert_eq!(fresh.restore(snapshot.clone()), 4);
        assert_eq!(fresh.snapshot(), snapshot);
    }

    #[test]
    fn an_unedited_column_reads_from_the_template() {
        let world = world();
        assert!(!world.is_edited(ChunkPos::new(0, 0)));
        assert_eq!(world.edit_count(), 0);
        let surface = Position {
            x: 0,
            y: super::super::world::SURFACE_Y,
            z: 0,
        };
        assert_ne!(world.block_at(surface), 0, "the surface is not air");
    }

    #[test]
    fn a_broken_block_reads_back_as_air_and_only_in_its_own_column() {
        let world = world();
        let here = Position {
            x: 3,
            y: super::super::world::SURFACE_Y,
            z: 5,
        };
        let elsewhere = Position { x: 4, ..here };
        let before = world.block_at(elsewhere);

        assert!(world.set_block(here, 0));
        assert_eq!(world.block_at(here), 0);
        assert_eq!(world.block_at(elsewhere), before, "one block, not two");
        assert!(world.is_edited(ChunkPos::new(0, 0)));
        assert!(!world.is_edited(ChunkPos::new(1, 0)));
        assert_eq!(world.edit_count(), 1);
    }

    #[test]
    fn negative_coordinates_land_in_the_column_and_cell_they_belong_to() {
        // The case masking and shifting exist for. Block x = -1 is cell 15 of
        // column -1; under division and remainder it is cell -1 of column 0,
        // which is not a cell at all.
        let world = world();
        let west = Position {
            x: -1,
            y: super::super::world::SURFACE_Y,
            z: -1,
        };
        assert!(world.set_block(west, 0));
        assert!(world.is_edited(ChunkPos::new(-1, -1)));
        assert_eq!(world.block_at(west), 0);

        // And the cell with the same local coordinates in the column to the
        // east is untouched, which is what says the column key is right.
        let east = Position {
            x: 15,
            y: super::super::world::SURFACE_Y,
            z: 15,
        };
        assert_ne!(world.block_at(east), 0);
    }

    #[test]
    fn a_block_outside_the_world_is_refused_rather_than_stored() {
        let world = world();
        assert!(!world.set_block(
            Position {
                x: 0,
                y: 5000,
                z: 0
            },
            0
        ));
        assert!(!world.set_block(
            Position {
                x: 0,
                y: -5000,
                z: 0
            },
            0
        ));
        assert_eq!(world.edit_count(), 0);
    }

    #[test]
    fn an_edit_reaches_a_listener_that_subscribed_first() {
        let world = world();
        let mut listener = world.subscribe();
        let here = Position {
            x: 1,
            y: super::super::world::SURFACE_Y,
            z: 1,
        };
        world.set_block(here, 0);
        let edit = listener.try_recv().expect("the edit was announced");
        assert_eq!(
            edit,
            Edit {
                position: here,
                state: 0,
                // `set_block` is what the server itself does; only a player
                // breaking or placing something carries who did it.
                by: None,
            }
        );
    }

    #[test]
    fn a_break_carries_what_broke_and_who_broke_it() {
        let world = world();
        let mut listener = world.subscribe();
        let here = Position {
            x: 2,
            y: super::super::world::SURFACE_Y,
            z: 2,
        };
        let before = world.block_at(here);
        assert!(before != 0, "the surface is not air to begin with");

        assert!(world.break_block(here, 0, 7));
        let edit = listener.try_recv().expect("the break was announced");
        assert_eq!(edit.position, here);
        assert_eq!(edit.state, 0, "broken to air");
        assert_eq!(
            edit.by,
            Some(Player::Broke {
                previous: before,
                by: 7
            }),
            "the block that broke, not the air left behind"
        );
    }

    #[test]
    fn breaking_air_announces_nothing() {
        // A creative client sends `start_digging` and a mining one sends
        // `finish_digging` too, so one dig arrives twice. The second has air
        // to break, and an effect made out of air is a silent puff of nothing.
        let world = world();
        let here = Position {
            x: 3,
            y: super::super::world::SURFACE_Y,
            z: 3,
        };
        assert!(world.break_block(here, 0, 7));
        let mut listener = world.subscribe();
        assert!(world.break_block(here, 0, 7), "still a legal position");
        assert!(
            listener.try_recv().is_err(),
            "nothing changed, so nobody is told anything"
        );
    }

    #[test]
    fn a_placement_carries_what_went_down_and_who_put_it_there() {
        let world = world();
        let here = Position {
            x: 4,
            y: super::super::world::SURFACE_Y + 1,
            z: 4,
        };
        assert_eq!(world.block_at(here), 0, "the air above the surface");
        let mut listener = world.subscribe();

        let stone = world.block_at(Position {
            y: super::super::world::SURFACE_Y,
            ..here
        });
        assert!(world.place_block(here, stone, 9));
        let edit = listener.try_recv().expect("the placement was announced");
        assert_eq!(edit.position, here);
        assert_eq!(edit.state, stone);
        assert!(
            matches!(edit.by, Some(Player::Placed { placed, by, .. }) if placed == stone && by == 9),
            "{:?}",
            edit.by
        );
        assert_eq!(
            edit.by.map(Player::entity_id),
            Some(9),
            "whatever a player did, the id of the one who did it is reachable"
        );
    }

    #[test]
    fn placing_what_is_already_there_announces_nothing() {
        // The same argument `break_block` makes about breaking air. A client
        // that sends one click twice — a shape this server has already met
        // from the other end — would otherwise be heard twice.
        let world = world();
        let here = Position {
            x: 5,
            y: super::super::world::SURFACE_Y,
            z: 5,
        };
        let already = world.block_at(here);
        let mut listener = world.subscribe();
        assert!(
            world.place_block(here, already, 9),
            "still a legal position"
        );
        assert!(listener.try_recv().is_err(), "nothing changed");
    }

    #[test]
    fn two_placements_do_not_choose_the_same_sound_sample() {
        // What the seed is for. A counter would pass an "are they different"
        // test and fail the thing the seed exists for, because the client reads
        // it through a small modulus — so this asserts on the low bits, which
        // is where a bare counter is visible.
        let world = world();
        let stone = world.block_at(Position {
            x: 0,
            y: super::super::world::SURFACE_Y,
            z: 0,
        });
        let mut listener = world.subscribe();
        let mut low = Vec::new();
        for x in 0..8 {
            let here = Position {
                x,
                y: super::super::world::SURFACE_Y + 1,
                z: 7,
            };
            assert!(world.place_block(here, stone, 9));
            let edit = listener.try_recv().expect("announced");
            let Some(Player::Placed { seed, .. }) = edit.by else {
                panic!("a placement carries a seed: {:?}", edit.by);
            };
            low.push(seed & 0b11);
        }
        low.sort_unstable();
        low.dedup();
        assert!(
            low.len() > 1,
            "eight placements landed on one of four samples every time: {low:?}"
        );
    }

    #[test]
    fn a_chunk_carries_the_edits_made_in_it() {
        let world = world();
        let here = Position {
            x: 2,
            y: super::super::world::SURFACE_Y,
            z: 2,
        };
        world.set_block(here, 0);
        let chunk = world.chunk(ChunkPos::new(0, 0));
        assert_eq!(chunk.get_block(2, super::super::world::SURFACE_Y, 2), 0);
        // And the heightmap followed the block down, which is what the client
        // uses to decide where the sky starts. `first_available` is one above
        // the highest solid block, so removing the block at SURFACE_Y takes it
        // from SURFACE_Y + 1 to SURFACE_Y — asserted exactly, because "lower
        // than before" would also pass if it had collapsed to the bedrock.
        let motion = dust_world::heightmap::HeightmapKind::MotionBlocking;
        assert_eq!(
            chunk.heightmaps().get(motion).first_available(2, 2),
            super::super::world::SURFACE_Y
        );
        assert_eq!(
            world
                .chunk(ChunkPos::new(0, 0))
                .heightmaps()
                .get(motion)
                .first_available(3, 3),
            super::super::world::SURFACE_Y + 1,
            "the column next to it is untouched"
        );
    }
}
