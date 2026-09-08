//! The world's clock: two numbers that move, and who is told about them.
//!
//! # Two numbers, and they are not the same number
//!
//! Minecraft keeps a world's time as a pair, and conflating them is the first
//! mistake available here. **Game time** counts every tick the world has ever
//! run and never goes backwards or sideways; it is what redstone repeaters,
//! scoreboard triggers and `/time query gametime` read. **Day time** is the
//! sun's position, and it is the one a player sees: it advances with game time
//! while the daylight cycle runs and stands still when it does not, and
//! `/time set` moves it without touching the other.
//!
//! Day time is *not* reduced into `0..24_000` here, and that is deliberate
//! rather than an oversight. Vanilla keeps it as a total, and two things
//! depend on the total: `/time query day` is `day_time / 24_000`, which is the
//! number a player quotes when they say what day their world is on, and the
//! packet carries the raw value for the client to reduce itself. What wraps is
//! the *sun*, and [`WorldClock::time_of_day`] is where that reduction happens.
//!
//! # What a tick costs
//!
//! One relaxed `fetch_add`, on one atomic, whatever the daylight cycle is
//! doing — the sun is kept as an offset from the world's age rather than as a
//! counter of its own, so there is only ever one number to move. Measured by
//! `benches/daylight.rs`, median of five rounds of a million ticks:
//! **2.5 ns per tick**, which is 0.000005% of the 50 ms a tick has, and the
//! same 2.5 ns with the cycle stopped — which is the offset paying for itself,
//! since there is no second counter to skip. There is nothing here to make
//! cheaper, and `[server] daylight_cycle` exists because an operator may want
//! a world that does not get dark, not because a moving sun costs anything.
//!
//! # What a player costs
//!
//! One `set_time` packet per player per second, which is **18 bytes on the
//! wire** — a length prefix, a packet id and two longs — measured by the same
//! bench. Building and encoding one costs about 120 ns, so a hundred players
//! cost **1.8 kB/s of upload and 0.001% of a tick**; a thousand cost 18 kB/s
//! and 0.013%. Against the 111 kB one chunk column costs to send once, the entire
//! day-night cycle for a full server is a sixtieth of a chunk a second. This
//! is not a rate worth tuning, and it is vanilla's own:
//! `MinecraftServer.tickServer` broadcasts on `tickCount % 20 == 0`.
//!
//! The one place Dust is *quicker* than vanilla is `/time set`. Vanilla moves
//! the clock and lets the next 20-tick broadcast carry it, so a player can
//! watch up to a second go by before midnight arrives. Here every session
//! already wakes once a tick for item pickups, and comparing one atomic in
//! that arm — see [`WorldClock::epoch`] — makes the sun jump on the same tick
//! the command ran. One relaxed load per player per tick against a visibly
//! immediate command is a trade worth making.

use std::sync::atomic::{AtomicU64, Ordering};

use dust_protocol::packets::play;

/// Ticks in a Minecraft day.
pub const DAY_TICKS: u64 = 24_000;

/// Sunrise-and-then-some: the tick `/time set day` means, and the tick a
/// world that has never been played starts at.
pub const DAY: u64 = 1_000;

/// The sun at its highest. `/time set noon`.
pub const NOON: u64 = 6_000;

/// Sundown. `/time set night`.
pub const NIGHT: u64 = 13_000;

/// The moon at its highest. `/time set midnight`.
pub const MIDNIGHT: u64 = 18_000;

/// How often each player is told the time, in ticks.
///
/// Vanilla's `tickCount % 20 == 0`, which is once a second. See the module
/// header for what that costs; the short answer is 18 bytes a player a second.
pub const BROADCAST_TICKS: u64 = 20;

/// The same rate as a duration, for the session's own timer.
pub const BROADCAST_PERIOD: std::time::Duration =
    std::time::Duration::from_millis(BROADCAST_TICKS * 50);

/// The world's clock, shared between the tick loop that moves it and every
/// session that reads it.
///
/// Every field is an atomic rather than the whole thing being a mutex,
/// because the access pattern is one writer and N readers at 20 Hz and a lock
/// would be a lock every session queued on once a second for two integers.
#[derive(Debug)]
pub struct WorldClock {
    /// Ticks this world has ever run. Vanilla's `Data.Time`, and **the only
    /// thing a tick touches**.
    game_time: AtomicU64,
    /// Where the sun is, as a *difference* from the world's age rather than a
    /// count of its own.
    ///
    /// With the cycle running, `day_time = game_time + offset`, all in
    /// wrapping `u64` arithmetic — the offset of a world at game time
    /// 1,000,000 and day time 6,000 is an enormous number that wraps back to
    /// 6,000 when the age is added, and that is fine and exact. With the cycle
    /// stopped the sun does not move at all and this simply *is* the day time.
    ///
    /// **Two things fall out of that and both are the reason for it.** A tick
    /// is one atomic addition rather than two, whatever the cycle is doing.
    /// And the pair of numbers in one packet is one consistent reading:
    /// [`Self::reading`] loads the age once and derives the sun from it, so a
    /// tick landing between the two loads cannot produce a packet whose two
    /// halves are a tick apart. Two independent counters cannot promise that —
    /// they were tried first, and the join test caught them a tick apart.
    offset: AtomicU64,
    /// Whether the sun moves at all. `[server] daylight_cycle`, and the
    /// equivalent of vanilla's `doDaylightCycle` game rule.
    ///
    /// Not an atomic and not settable: it comes from configuration, which is
    /// read once at boot. The day this becomes a game rule it becomes an
    /// `AtomicBool` and every reader below already reads it once per use —
    /// though it would also need [`Self::offset`] rebased at the moment it
    /// changed, because the offset means a different thing on each side of it.
    cycle: bool,
    /// Bumped whenever something other than the passage of time moved the
    /// clock — which today is exactly `/time set` and `/time add`.
    ///
    /// A session caches the last value it saw and compares; a difference means
    /// "tell this player now rather than at the next second boundary". It is a
    /// counter and not a flag because a flag would have to be consumed, and
    /// the first session to consume it would be the only one told.
    epoch: AtomicU64,
}

impl WorldClock {
    /// A clock starting at these two readings.
    ///
    /// Both are `u64` because neither can meaningfully be negative and the
    /// arithmetic below is all addition; the conversion to the packet's `i64`
    /// is where the sign appears, and it appears for a reason — see
    /// [`Self::packet`].
    #[must_use]
    pub fn new(game_time: u64, day_time: u64, cycle: bool) -> Self {
        Self {
            game_time: AtomicU64::new(game_time),
            offset: AtomicU64::new(Self::offset_for(cycle, game_time, day_time)),
            cycle,
            epoch: AtomicU64::new(0),
        }
    }

    /// The offset that puts the sun at `day_time` when the world is `age`
    /// ticks old. See [`Self::offset`].
    fn offset_for(cycle: bool, age: u64, day_time: u64) -> u64 {
        if cycle {
            day_time.wrapping_sub(age)
        } else {
            day_time
        }
    }

    /// A world that has never been played: dawn on day zero.
    ///
    /// 1,000 rather than 0 because that is where Minecraft starts a new world.
    /// Tick 0 is technically day, but it is the *first instant* of it, with the
    /// sun still on the horizon and the sky still holding the previous night's
    /// colour; 1,000 is the light a new world opens on.
    #[must_use]
    pub fn fresh(cycle: bool) -> Self {
        Self::new(0, DAY, cycle)
    }

    /// One tick. This is the whole of what the clock does per tick: one
    /// addition, whether or not the sun is moving.
    pub fn tick(&self) {
        self.game_time.fetch_add(1, Ordering::Relaxed);
    }

    /// Ticks this world has ever run.
    #[must_use]
    pub fn game_time(&self) -> u64 {
        self.game_time.load(Ordering::Relaxed)
    }

    /// Both numbers from one reading of the world's age.
    ///
    /// The only way to get a pair that agrees with itself; every other reader
    /// here is a projection of this one. See [`Self::offset`].
    #[must_use]
    pub fn reading(&self) -> (u64, u64) {
        let game_time = self.game_time();
        let offset = self.offset.load(Ordering::Acquire);
        let day_time = if self.cycle {
            game_time.wrapping_add(offset)
        } else {
            offset
        };
        (game_time, day_time)
    }

    /// The sun's position as a running total, which is what the wire and the
    /// save both carry. See the module header for why this is not reduced.
    #[must_use]
    pub fn day_time(&self) -> u64 {
        self.reading().1
    }

    /// The sun's position within its own day, `0..24_000`.
    #[must_use]
    pub fn time_of_day(&self) -> u64 {
        self.day_time() % DAY_TICKS
    }

    /// Which day this world is on. `/time query day`.
    #[must_use]
    pub fn day(&self) -> u64 {
        self.day_time() / DAY_TICKS
    }

    /// Whether the sun moves.
    #[must_use]
    pub fn cycle_runs(&self) -> bool {
        self.cycle
    }

    /// What a session compares against to know the clock was *moved* rather
    /// than merely advanced. See the field's own note.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Put the sun at an absolute day time, as `/time set` does.
    ///
    /// Vanilla's `/time set` is absolute even past a day boundary: setting 100
    /// on a world at day time 50,000 gives day time 100, which is day zero
    /// again. That is `ServerLevel.setDayTime` and it is what `/time query
    /// day` reporting a smaller number afterwards means.
    /// A tick may land between the age this reads and the offset it stores, in
    /// which case the sun ends up one tick past where it was asked for. That
    /// is a twentieth of a second of sun on a command a person typed, and the
    /// alternative is a lock on the tick path to serve a keystroke.
    pub fn set_day_time(&self, value: u64) {
        // The offset before the epoch, and both released: a session that sees
        // the new epoch must not then read the old offset and decide nothing
        // changed. This is the only ordering constraint in the type.
        self.offset.store(
            Self::offset_for(self.cycle, self.game_time(), value),
            Ordering::Release,
        );
        self.epoch.fetch_add(1, Ordering::Release);
    }

    /// Move the sun forward, as `/time add` does. Never backwards: vanilla's
    /// own argument parser refuses a negative, so there is no such command.
    ///
    /// Racing nothing, unlike [`set_day_time`](Self::set_day_time): a relative
    /// move is a relative move whatever the world's age is doing.
    pub fn add_day_time(&self, delta: u64) {
        self.offset.fetch_add(delta, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::Release);
    }

    /// The packet that tells one client where the sun is.
    ///
    /// **The sign of `time_of_day` is the protocol's way of saying the cycle
    /// is stopped**, and it is not a Dust invention: vanilla's
    /// `ClientboundSetTimePacket` negates the day time when `doDaylightCycle`
    /// is false, and turns a negated zero into `-1` because zero has no sign.
    /// A client given a negative value parks the sun at its absolute value and
    /// stops interpolating between packets; given a positive one it advances
    /// the sky itself between the twenty ticks it hears nothing.
    ///
    /// That interpolation is why this rate is affordable at all. The client is
    /// not waiting to be told each tick — it is running its own clock and
    /// being corrected once a second.
    #[must_use]
    pub fn packet(&self) -> play::clientbound::SetTime {
        let (game_time, day_time) = self.reading();
        // Saturating rather than wrapping: 2^63 ticks is fourteen thousand
        // million years of uptime, so this arm is unreachable, and if it ever
        // is reached a clamped clock is a better answer than one that jumps to
        // the far side of the sign.
        let day_time = i64::try_from(day_time).unwrap_or(i64::MAX);
        play::clientbound::SetTime {
            world_age: i64::try_from(game_time).unwrap_or(i64::MAX),
            time_of_day: if self.cycle {
                day_time
            } else if day_time == 0 {
                -1
            } else {
                -day_time
            },
        }
    }
}

/// The clock, as the tick loop sees it.
///
/// A participant of its own rather than a line inside another one, because it
/// is the one thing in the tick that has no other reason to exist: nothing
/// else in the loop needs the world's time to do its job, and a clock buried
/// in the world ticker would be a clock that stops when block updates do.
///
/// It runs before everything else in the tick that simulates anything, for the
/// ordinary reason: everything inside one tick should read one consistent
/// answer to "what time is it", and the way to get that is to move the clock
/// before anybody asks.
#[derive(Debug)]
pub struct DaylightTicker {
    clock: std::sync::Arc<WorldClock>,
}

impl DaylightTicker {
    #[must_use]
    pub fn new(clock: std::sync::Arc<WorldClock>) -> Self {
        Self { clock }
    }
}

impl crate::participant::TickParticipant for DaylightTicker {
    fn name(&self) -> &str {
        "daylight"
    }

    fn priority(&self) -> i32 {
        // Ahead of every other piece of world simulation — the block updates
        // share -1 and the item and furnace passes are at 0. Nothing reads the
        // clock inside a tick yet; when something does (crops, spawning, a bed)
        // it must not be able to read a tick-old one, and a number that says so
        // now costs nothing and cannot be got wrong later by registration
        // order.
        -2
    }

    fn tick(&mut self, _: &crate::participant::TickContext) {
        self.clock.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_reading_gives_a_pair_that_agrees_with_itself() {
        // The property the offset representation exists for. Two independent
        // counters were the first shape this had, and a tick landing between
        // their two loads produced a `set_time` whose age and sun were a tick
        // apart — which the join conversation test caught as a world that had
        // opened at 999 past midnight rather than 1,000.
        let clock = WorldClock::fresh(true);
        for _ in 0..1_234 {
            clock.tick();
        }
        let (game_time, day_time) = clock.reading();
        assert_eq!(game_time, 1_234);
        assert_eq!(day_time - game_time, DAY);
        let packet = clock.packet();
        assert_eq!(packet.time_of_day - packet.world_age, DAY as i64);
    }

    #[test]
    fn a_world_older_than_its_sun_reads_back_exactly() {
        // The wrapping case, spelled out because it looks alarming and is not:
        // a world a million ticks old whose sun is at 6,000 has an offset of
        // 6,000 - 1,000,000, which as a u64 is enormous. Adding the age back
        // wraps it to 6,000, which is modular arithmetic doing exactly its job.
        let clock = WorldClock::new(1_000_000, NOON, true);
        assert_eq!(clock.reading(), (1_000_000, NOON));
        clock.tick();
        assert_eq!(clock.reading(), (1_000_001, NOON + 1));
    }

    #[test]
    fn a_tick_moves_both_numbers_and_a_stopped_cycle_moves_only_one() {
        let running = WorldClock::new(0, 0, true);
        let stopped = WorldClock::new(0, 0, false);
        for _ in 0..50 {
            running.tick();
            stopped.tick();
        }
        assert_eq!((running.game_time(), running.day_time()), (50, 50));
        assert_eq!(
            (stopped.game_time(), stopped.day_time()),
            (50, 0),
            "game time is not the daylight cycle; redstone still runs at night"
        );
    }

    #[test]
    fn a_thousand_ticks_cost_no_real_time_and_land_exactly() {
        // The house rule the whole crate is built on, applied to the clock:
        // nothing here reads a wall clock, so a virtual day is a loop.
        let clock = WorldClock::fresh(true);
        for _ in 0..DAY_TICKS {
            clock.tick();
        }
        assert_eq!(clock.day_time(), DAY + DAY_TICKS);
        assert_eq!(clock.time_of_day(), DAY, "a whole day returns the same sun");
        assert_eq!(clock.day(), 1, "and it is tomorrow");
    }

    #[test]
    fn the_sun_wraps_and_the_day_counter_does_not() {
        let clock = WorldClock::new(0, 0, true);
        clock.set_day_time(3 * DAY_TICKS + NOON);
        assert_eq!(clock.time_of_day(), NOON);
        assert_eq!(clock.day(), 3);
        // The whole reason day time is stored as a total. A clock that wrapped
        // internally would answer 0 here and a player would have no way to say
        // what day their world is on.
        assert_eq!(clock.day_time(), 3 * DAY_TICKS + NOON);
    }

    #[test]
    fn setting_is_absolute_and_adding_is_relative() {
        let clock = WorldClock::new(0, 50_000, true);
        clock.add_day_time(100);
        assert_eq!(clock.day_time(), 50_100);
        // Vanilla's `/time set` is absolute even backwards across a day
        // boundary — this is what makes `/time set day` on day three answer
        // "day 0" to the next query, and it is not a bug here.
        clock.set_day_time(DAY);
        assert_eq!(clock.day_time(), DAY);
        assert_eq!(clock.day(), 0);
    }

    #[test]
    fn only_a_command_moves_the_epoch() {
        let clock = WorldClock::fresh(true);
        let before = clock.epoch();
        for _ in 0..100 {
            clock.tick();
        }
        assert_eq!(
            clock.epoch(),
            before,
            "the ordinary passage of time is not news; it is what every \
             session already assumes is happening"
        );
        clock.set_day_time(NIGHT);
        assert_ne!(clock.epoch(), before);
        let after_set = clock.epoch();
        clock.add_day_time(1);
        assert_ne!(clock.epoch(), after_set);
    }

    #[test]
    fn a_stopped_cycle_is_told_as_a_negative_and_zero_becomes_minus_one() {
        // Vanilla's `ClientboundSetTimePacket` exactly: negate, and if that
        // produced a zero — which carries no sign — send -1 instead. A client
        // sent a plain 0 would take it as midnight with the cycle *running*.
        let stopped = WorldClock::new(7, NOON, false);
        let packet = stopped.packet();
        assert_eq!(packet.world_age, 7);
        assert_eq!(packet.time_of_day, -6_000);

        let midnight_of_time = WorldClock::new(0, 0, false);
        assert_eq!(midnight_of_time.packet().time_of_day, -1);

        let running = WorldClock::new(7, NOON, true);
        assert_eq!(running.packet().time_of_day, 6_000);
    }

    #[test]
    fn the_ticker_moves_the_clock_it_was_given() {
        use crate::participant::TickParticipant;
        let clock = std::sync::Arc::new(WorldClock::fresh(true));
        let mut ticker = DaylightTicker::new(std::sync::Arc::clone(&clock));
        let logger = crate::logging::Logger::to_stdout(
            crate::logging::Level::Error,
            std::sync::Arc::new(crate::clock::ManualClock::new()),
        );
        for tick_index in 0..20 {
            ticker.tick(&crate::participant::TickContext {
                tick_index,
                tick_duration_ns: crate::engine::TICK_NS,
                logger: &logger,
            });
        }
        assert_eq!(clock.game_time(), 20);
        assert_eq!(clock.day_time(), DAY + 20);
    }
}
