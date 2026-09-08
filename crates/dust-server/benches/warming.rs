//! What a player waits for the world, and which of the two limits they hit.
//!
//! `benches/join.rs` says what a column costs. This says what a *player* gets,
//! by running the chunk stream's own discipline — `net::session::stream_inner`,
//! its 24-column window, its eight columns every twenty milliseconds — against
//! the real column store and its builder threads, and timing the columns as
//! they arrive.
//!
//! # The two limits, and the arithmetic that separates them
//!
//! A join at the default view distance is 289 columns. Two different things
//! can make a player wait for them and only one of them is the world:
//!
//! 1. **The pacing.** `STREAM_BATCH` is eight columns and
//!    `STREAM_BATCH_PERIOD` is 20 ms, so no session may be sent more than 400
//!    columns a second whatever the world costs. 289 of them is 37 passes and
//!    **740 ms**, and nothing below this line is reachable. The program prints
//!    the figure rather than trusting this comment.
//! 2. **The builders.** A generated column is milliseconds of noise, so a
//!    builder count is a supply rate: one builder is below the pacing and four
//!    are not far above it.
//!
//! What the ladder found is that a single join reaches the first limit and four
//! simultaneous ones do not. One joiner is **2,699 ms with one builder and
//! 1,069 ms with four**, against a 740 ms arithmetic floor and an all-resident
//! instrument that lands at 961 — and eight builders make it 1,076, which is
//! nothing. Four joiners are **17,202 ms and 5,584 ms**, and eight builders are
//! still 1.28x four there. Decision record 0036 has the table and what the cap
//! of four is worth.
//!
//! # The rungs, each one change
//!
//! - **resident** — every column built before the clock starts, so the
//!   builders have nothing to do. This is the instrument: the same driver
//!   threads, the same view arithmetic, the same residency lock, the same
//!   20 ms pacing, and no world at all. A generated row is only about the
//!   world to the extent that it is slower than this one.
//! - **1, 2, 4, 8 builders** — the same join with the pool sized differently.
//! - **one joiner and four** — four centres far enough apart to share no
//!   column, which is what the pool is for.
//!
//! Each row reports **counts, not rates**: the time the first column arrived,
//! the time the last one did, and how many of the stream's 20 ms passes had
//! nothing to send because no column was ready — the number that says whether
//! the player was waiting for the builders or for the pacing. A single join
//! starves for 273 passes on one builder and 19 on four, and that pair says
//! what the pool is for without reference to any wall clock.
//!
//! # It takes about twenty-five minutes, and most of that is not measurement
//!
//! A world is built **per row per round**. `GeneratedWorld` keeps a sky-floor
//! cache of 4,096 columns and a join touches 289, so three rounds over one
//! world would find the third round's floors already computed — and `report`
//! prints the *fastest* round, which would be the one an earlier round warmed.
//! The residency behaves the same way. A row that reused its world would report
//! a warm generator and call it a builder count.
//!
//! ```text
//! DUST_BENCH_CONSTANTS=.dust-extract/oracle-1.21.1/constants.tsv \
//!   DUST_BENCH_DATA=/path/to/data \
//!   cargo bench -p dust-server --bench warming
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use dust_server::net::edits::EditedWorld;
use dust_server::net::residency::ColumnClaim;
use dust_server::net::source::{GeneratedColumns, Source};
use dust_server::net::view::View;
use dust_server::net::world::{FlatWorld, Palette};
use dust_world::coords::ChunkPos;

/// The default view distance, and so the number of columns a join sends.
const VIEW: u32 = 8;
/// `net::session::STREAM_BATCH`.
const STREAM_BATCH: usize = 8;
/// `net::session::STREAM_BATCH_PERIOD`.
const STREAM_PERIOD: Duration = Duration::from_millis(20);
/// `net::session::STREAM_AHEAD`.
const STREAM_AHEAD: usize = 16;
/// How many times every row is run. A round is one pass over *all* the rows —
/// see the note beside the round loop in `main` — so three of them is three
/// interleaved passes and not three consecutive runs of each row.
const ROUNDS: usize = 3;

/// What one simulated session got.
struct Streamed {
    first: Duration,
    last: Duration,
    sent: usize,
    /// Passes where the view was incomplete and no column was ready. This is
    /// the builders being behind, and it is a count of 20 ms passes.
    starved: usize,
}

fn main() {
    let Some(constants) = table() else { return };
    let Ok(palette) = Palette::resolve() else {
        eprintln!("the generated block table has no bedrock; nothing to bench");
        return;
    };
    let constants = Arc::new(constants);
    let opacity = dust_server::net::world::opacity_of(palette.air, Some(&constants));
    let flat = || FlatWorld::new(palette, 0, 64);

    let Some(data) = std::env::var_os("DUST_BENCH_DATA").map(std::path::PathBuf::from) else {
        println!("not run. Set DUST_BENCH_DATA to a [data] path with dust-biomes.tsv in it.");
        return;
    };

    let columns = View::with_radius(VIEW)
        .move_to(ChunkPos::new(0, 0))
        .send
        .len();
    let floor = STREAM_PERIOD * u32::try_from(columns.div_ceil(STREAM_BATCH)).unwrap_or(u32::MAX);
    println!(
        "a join sends {columns} columns, {STREAM_BATCH} every {} ms: nothing below {} ms is reachable",
        STREAM_PERIOD.as_millis(),
        floor.as_millis()
    );

    let world = |builders: usize| -> Option<EditedWorld> {
        match dust_server::net::generated::beside(
            &data,
            1,
            flat(),
            opacity.clone(),
            0,
            64,
            Some(Arc::clone(&constants)),
            &dust_config::ore::OresConfig::default(),
        ) {
            Ok(Some((world, _))) => Some(EditedWorld::new(Source::Generated(Box::new(
                GeneratedColumns::with_builders(world, builders),
            )))),
            Ok(None) => {
                println!("no dust-biomes.tsv under DUST_BENCH_DATA");
                None
            }
            Err(e) => {
                println!("{e}");
                None
            }
        }
    };

    // A world is built per row **per round**, and that is not tidiness.
    // `GeneratedWorld` keeps a sky-floor cache of 4,096 columns and a join
    // touches 289 of them, so three rounds over one world would find the third
    // round's floors already computed — and `report` takes the *fastest* round,
    // which is the one whose cache somebody else filled. The residency behaves
    // the same way. A row that reused its world would report a warm generator
    // and call it a builder count.
    let mut rows: Vec<Row> = Vec::new();
    // Whether the four centres cost the same. The four-joiner rows put four
    // *different* pieces of terrain on the builders at once, and a row that is
    // slower because its columns are dearer is not a row about contention.
    for centre in centres(4) {
        rows.push(Row {
            label: format!("one joiner at ({}, {}), 4 builders", centre.x, centre.z),
            builders: 4,
            at: vec![centre],
            resident: false,
            rounds: Vec::new(),
        });
    }
    for joiners in [1usize, 4] {
        let at = centres(joiners);
        // The instrument. Everything the rows below do, with the world already
        // built, so that what is left is the driver, the residency lock and
        // the pacing.
        rows.push(Row {
            label: format!("{joiners} joiner(s), resident (the instrument)"),
            builders: 4,
            at: at.clone(),
            resident: true,
            rounds: Vec::new(),
        });
        for builders in [1usize, 2, 4, 8] {
            rows.push(Row {
                label: format!("{joiners} joiner(s), {builders} builder(s)"),
                builders,
                at: at.clone(),
                resident: false,
                rounds: Vec::new(),
            });
        }
    }

    // **Interleaved, and this is not a detail.** The first cut of this bench
    // ran each row's three rounds consecutively and reported that four
    // builders finish a join in 950 ms; the identical row an hour later, when
    // the other builds on this machine were heavier, said 2,553 ms. Rows run
    // one after another are scored against different machines. A round here is
    // one pass over every row, so a row that is slower than its neighbour is
    // slower than it *was*, not slower than it would have been at a different
    // hour.
    for _ in 0..ROUNDS {
        for row in &mut rows {
            let Some(world) = world(row.builders) else {
                return;
            };
            row.round(&world);
        }
    }
    println!();
    for row in &rows {
        row.report();
    }
}

/// One row of the ladder: how many builders its world runs, where the joiners
/// stand, and whether the world is handed to them already built.
struct Row {
    label: String,
    builders: usize,
    at: Vec<ChunkPos>,
    /// Every column built and held before the clock starts. The instrument
    /// row, and the only one whose number is not about the world.
    resident: bool,
    rounds: Vec<Vec<Streamed>>,
}

impl Row {
    fn round(&mut self, world: &EditedWorld) {
        if self.resident {
            for centre in &self.at {
                let order = View::with_radius(VIEW).move_to(*centre).send;
                world.hold_columns(&order);
                world.warm_columns(&order);
            }
        }
        let at = &self.at;
        self.rounds.push(std::thread::scope(|scope| {
            let started = Instant::now();
            let handles: Vec<_> = at
                .iter()
                .map(|centre| {
                    let centre = *centre;
                    scope.spawn(move || stream(world, centre, started))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a stream thread never panics"))
                .collect::<Vec<_>>()
        }));
    }

    fn report(&self) {
        // The worst session of a round, because a join is not an average: the
        // row is about the player who waited longest. The fastest round is the
        // one least contended by whatever else this machine was doing, and the
        // worst is printed beside it so that a row whose two are far apart
        // says so rather than hiding behind one number.
        let worst = |r: &Vec<Streamed>| r.iter().map(|s| s.last).max().unwrap_or_default();
        let first = |r: &Vec<Streamed>| r.iter().map(|s| s.first).max().unwrap_or_default();
        let starved: usize = self
            .rounds
            .iter()
            .map(|r| r.iter().map(|s| s.starved).sum::<usize>())
            .sum();
        let sent: usize = self
            .rounds
            .iter()
            .map(|r| r.iter().map(|s| s.sent).sum::<usize>())
            .sum();
        let fastest = self.rounds.iter().map(worst).min().unwrap_or_default();
        let slowest = self.rounds.iter().map(worst).max().unwrap_or_default();
        let opening = self.rounds.iter().map(first).min().unwrap_or_default();
        println!(
            "  {:<38} last column {:>8.1} ms (worst round {:>8.1} ms), first {:>7.1} ms, \
             starved passes {starved:>5} over {ROUNDS} rounds, {sent} columns sent",
            self.label,
            fastest.as_secs_f64() * 1e3,
            slowest.as_secs_f64() * 1e3,
            opening.as_secs_f64() * 1e3,
        );
    }
}

/// Centres far enough apart that no two joiners share a column: a view is 17
/// columns across, so 64 apart is four views of clear air between them.
fn centres(joiners: usize) -> Vec<ChunkPos> {
    (0..joiners)
        .map(|n| ChunkPos::new(i32::try_from(n).unwrap_or(0) * 64, 0))
        .collect()
}

/// One session's chunk stream, as `net::session::stream_inner` runs it.
fn stream(world: &EditedWorld, centre: ChunkPos, started: Instant) -> Streamed {
    let mut view = View::with_radius(VIEW);
    let mut ahead = ColumnClaim::new(world.residency(), world.warming());
    let mut out = Streamed {
        first: Duration::ZERO,
        last: Duration::ZERO,
        sent: 0,
        starved: 0,
    };
    let deadline = started + Duration::from_secs(60);
    while !view.complete(centre) && Instant::now() < deadline {
        let window = view.peek(centre, STREAM_BATCH + STREAM_AHEAD);
        let mut wanted = window.clone();
        ahead.set(&mut wanted);
        let ready = world.built_prefix(&window).min(STREAM_BATCH);
        let change = view.move_to_limited(centre, Some(ready));
        if change.send.is_empty() {
            out.starved += 1;
        } else {
            if out.sent == 0 {
                out.first = started.elapsed();
            }
            out.sent += change.send.len();
            out.last = started.elapsed();
        }
        std::thread::sleep(STREAM_PERIOD);
    }
    out
}

/// Minecraft's own block table, from wherever the operator put it.
fn table() -> Option<dust_registry::BlockConstants> {
    let path = std::env::var_os("DUST_BENCH_CONSTANTS").map_or_else(
        || {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(".dust-extract/oracle-1.21.1/constants.tsv")
        },
        std::path::PathBuf::from,
    );
    match std::fs::read_to_string(&path) {
        Ok(text) => match dust_registry::BlockConstants::parse(&text) {
            Ok(table) => Some(table),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                None
            }
        },
        Err(_) => {
            println!(
                "no block table at {}. Run `cargo xtask extract --only constants` and point \
                 DUST_BENCH_CONSTANTS at the file it writes.",
                path.display()
            );
            None
        }
    }
}
