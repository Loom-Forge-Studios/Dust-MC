//! What a moving sun costs: per tick on the server, and per player on the
//! wire.
//!
//! The claim `net/daylight.rs` makes is that a world clock is free, and "free"
//! is a word this project does not accept without a number beside it. There
//! are exactly two costs and they are charged to different things:
//!
//! ```text
//!   per tick      one or two atomic additions, whatever the player count
//!   per player    one `set_time` packet a second, encoded and written
//! ```
//!
//! No framework, for the reason the furnace bench gives: a fixed workload
//! timed by hand answers "how fast" without a dozen dependencies.
//!
//! The rows are a ladder — each one the row above it plus a single named
//! change — because the interesting question is not "how long does a tick
//! take" but "which of these two costs is the one that grows".

use std::hint::black_box;
use std::time::Instant;

use dust_protocol::packets::play;
use dust_server::net::daylight::{WorldClock, BROADCAST_TICKS};

/// Ticks per row. A thousand is fifty seconds of game time.
const TICKS: u32 = 1_000_000;
/// Rounds, of which the median is reported.
const ROUNDS: usize = 5;

fn version() -> dust_protocol::ProtocolVersion {
    dust_protocol::ProtocolVersion::from_name("1.21.1").expect("the target version")
}

/// The bytes one `set_time` costs on the wire, id included.
fn packet_bytes(clock: &WorldClock) -> usize {
    let packet: play::clientbound::Packet = clock.packet().into();
    let mut out = dust_protocol::wire::Writer::new();
    // `encode` writes the packet id as a VarInt and then the body, which is
    // what actually leaves the socket once the frame's own length prefix is
    // added — and that prefix is one byte at this size.
    packet.encode(&mut out, version()).expect("encodes");
    out.into_bytes().len() + 1
}

fn tick_row(label: &str, cycle: bool) {
    let mut nanos = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let clock = WorldClock::fresh(cycle);
        let started = Instant::now();
        for _ in 0..TICKS {
            clock.tick();
        }
        let elapsed = started.elapsed().as_nanos();
        black_box(clock.day_time());
        nanos.push(elapsed as f64 / f64::from(TICKS));
    }
    nanos.sort_by(f64::total_cmp);
    let median = nanos[ROUNDS / 2];
    let share = median / 50_000_000.0 * 100.0;
    println!("  {label:<44} {median:>10.2} ns/tick  {share:>10.7}% of a tick");
}

fn packet_row(label: &str, players: usize) {
    let clock = WorldClock::fresh(true);
    let bytes = packet_bytes(&clock);
    let mut nanos = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let started = Instant::now();
        for _ in 0..1_000 {
            for _ in 0..players {
                black_box(packet_bytes(&clock));
            }
        }
        nanos.push(started.elapsed().as_nanos() as f64 / 1_000.0);
    }
    nanos.sort_by(f64::total_cmp);
    let median = nanos[ROUNDS / 2];
    // One broadcast a second per player. A tick is 50 ms, so the share of a
    // *tick* is the cost spread over the twenty ticks between broadcasts.
    let per_tick = median / BROADCAST_TICKS as f64;
    let share = per_tick / 50_000_000.0 * 100.0;
    let per_second = bytes * players;
    println!(
        "  {label:<44} {median:>10.0} ns/second {share:>10.7}% of a tick  \
         ({per_second} B/s of upload)"
    );
}

fn main() {
    println!("median of {ROUNDS} rounds of {TICKS} ticks\n");
    println!("  the clock — what one tick of the world costs");
    // Both rows, and they should agree: the sun is an offset from the world's
    // age rather than a counter of its own, so a stopped cycle is not a
    // skipped addition — there was only ever one. A gap between these two rows
    // would mean that stopped being true.
    tick_row("cycle running: one atomic add", true);
    tick_row("cycle stopped: the same one", false);

    println!("\n  the packet — building and encoding one set_time per player");
    let bytes = packet_bytes(&WorldClock::fresh(true));
    println!("  one set_time, framed, is {bytes} bytes");
    for players in [1usize, 20, 100, 1_000] {
        packet_row(&format!("{players} player(s), once a second each"), players);
    }
}
