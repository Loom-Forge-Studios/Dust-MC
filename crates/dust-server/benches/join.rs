//! What a join's chunk stream costs, and on which thread.
//!
//! The stopwatch pattern the other two benches use: no framework, one
//! workload, numbers printed. It exists because decision record 0031 turns on
//! a number — how long a session task is blocked building and encoding the
//! 289 columns a join sends at the default view distance — and a decision made
//! on a measurement should be re-checkable by rerunning it.
//!
//! # Which world a row is about
//!
//! Since the terrain landed, "a column" is two different costs and a single
//! number for the stream would hide which one it is. Three worlds are run, and
//! every row says which:
//!
//! 1. **flat** — one template column shared by every position. The floor:
//!    everything the stream costs when the world costs nothing.
//! 2. **generated** — every column built from noise, which is what a server
//!    with no `world_source` serves. Run it with `DUST_BENCH_DATA` pointing at
//!    the `[data] path` that has `dust-biomes.tsv` in it.
//! 3. **region files** — every column read from a world Minecraft wrote. Run
//!    it with `DUST_BENCH_REGION`.
//!
//! # The ladder
//!
//! Each row is the one above it plus a single named change, because one
//! percentage cannot say which input owns which part of a cost:
//!
//! - **build only** — `EditedWorld::template` for each of the 289, in the
//!   nearest-first order `View` hands the stream.
//! - **build and encode** — the same, plus `play::chunk_packet`. **This is
//!   what the session's own task is blocked for today**, minus the socket.
//! - **encode only, resident** — the same 289 with the column store already
//!   holding them, so the session task pays for encoding and nothing else.
//!   The difference between this row and the one above it is what moving the
//!   build off the session task is worth.
//!
//! Every row prints its **worst single column** as well as its total, because
//! a join is not an average: the column a player is standing in arriving late
//! is what a join feels like, and one 60 ms column in 289 is a stutter that a
//! mean over the batch cannot see.
//!
//! ```text
//! cargo xtask extract --version 1.21.1 --only constants,worldgen
//! DUST_BENCH_CONSTANTS=.dust-extract/oracle-1.21.1/constants.tsv \
//!   DUST_BENCH_DATA=/path/to/data DUST_BENCH_REGION=/path/to/region \
//!   cargo bench -p dust-server --bench join
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use dust_protocol::ProtocolVersion;
use dust_server::net::edits::EditedWorld;
use dust_server::net::source::{AnvilWorld, RegistryNames, Source};
use dust_server::net::view::View;
use dust_server::net::world::{FlatWorld, Palette};
use dust_world::coords::ChunkPos;

/// The default view distance, and so the number of columns a join sends.
const VIEW: u32 = 8;
const VERSION: ProtocolVersion = dust_protocol::version::V1_21_1;

fn main() {
    let Some(constants) = table() else { return };
    let Ok(palette) = Palette::resolve() else {
        eprintln!("the generated block table has no bedrock; nothing to bench");
        return;
    };
    let constants = Arc::new(constants);
    let opacity = dust_server::net::world::opacity_of(palette.air, Some(&constants));

    let order = order(ChunkPos::new(0, 0));
    println!(
        "a join at the default view distance sends {} columns, nearest first",
        order.len()
    );

    let flat = || FlatWorld::new(palette, 0, 64);
    ladder(
        "flat",
        &EditedWorld::new(Source::Flat(Box::new(flat()))),
        &order,
    );

    match std::env::var_os("DUST_BENCH_DATA").map(std::path::PathBuf::from) {
        Some(data) => match dust_server::net::generated::beside(
            &data,
            1,
            flat(),
            opacity.clone(),
            0,
            64,
            Some(Arc::clone(&constants)),
            &dust_config::ore::OresConfig::default(),
        ) {
            Ok(Some((world, _))) => ladder(
                "generated",
                &EditedWorld::new(Source::Generated(Box::new(
                    dust_server::net::source::GeneratedColumns::new(world),
                ))),
                &order,
            ),
            Ok(None) => println!("generated: no dust-biomes.tsv under DUST_BENCH_DATA"),
            Err(e) => println!("generated: {e}"),
        },
        None => println!(
            "generated: not run. Set DUST_BENCH_DATA to a [data] path with dust-biomes.tsv in it."
        ),
    }

    let Some(directory) = std::env::var_os("DUST_BENCH_REGION").map(std::path::PathBuf::from)
    else {
        println!("region files: not run. Set DUST_BENCH_REGION to a world's region directory.");
        return;
    };
    let Some(names) = RegistryNames::new() else {
        eprintln!("no synced biome registry; the region rows cannot be built");
        return;
    };
    let world = EditedWorld::new(Source::Anvil(Box::new(AnvilWorld::new(
        directory,
        names,
        flat(),
        opacity,
        Some(Arc::clone(&constants)),
    ))));
    ladder("region files", &world, &order);
}

/// The order `View` hands the stream: nearest to the player first.
fn order(centre: ChunkPos) -> Vec<ChunkPos> {
    View::with_radius(VIEW).move_to(centre).send
}

fn ladder(world_name: &str, world: &EditedWorld, order: &[ChunkPos]) {
    println!("\n{world_name}:");
    row("  build only", || {
        let mut worst = Duration::ZERO;
        for pos in order {
            let at = Instant::now();
            std::hint::black_box(world.template(*pos).as_chunk().sections().len());
            worst = worst.max(at.elapsed());
        }
        worst
    });
    row("  build and encode", || {
        let mut worst = Duration::ZERO;
        for pos in order {
            let at = Instant::now();
            let packet = if world.is_edited(*pos) {
                dust_server::net::play::chunk_packet(&world.chunk(*pos), *pos, VERSION)
            } else {
                dust_server::net::play::chunk_packet(world.template(*pos).as_chunk(), *pos, VERSION)
            };
            std::hint::black_box(packet.is_ok());
            worst = worst.max(at.elapsed());
        }
        worst
    });
    // Everything resident before the clock starts, which is what a stream that
    // builds ahead of itself on another thread hands the session task.
    if world.residency().is_some() {
        world.hold_columns(order);
        world.warm_columns(order);
        row("  encode only, resident", || {
            let mut worst = Duration::ZERO;
            for pos in order {
                let at = Instant::now();
                let packet = dust_server::net::play::chunk_packet(
                    world.template(*pos).as_chunk(),
                    *pos,
                    VERSION,
                );
                std::hint::black_box(packet.is_ok());
                worst = worst.max(at.elapsed());
            }
            worst
        });
        println!("    (resident set: {} columns)", world.resident_columns());
        world.release_columns(order);
    } else {
        println!("  encode only, resident            this world keeps no columns");
    }
}

/// Three rounds, and the first is printed on its own: it is the only cold one,
/// and a join into terrain nobody has been in is the round a real player
/// generates.
const ROUNDS: usize = 3;

fn row<F: FnMut() -> Duration>(name: &str, mut work: F) {
    let mut totals = Vec::with_capacity(ROUNDS);
    let mut worsts = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let at = Instant::now();
        let worst = work();
        totals.push(at.elapsed());
        worsts.push(worst);
    }
    let name = format!("{name:<34}");
    println!(
        "{name} {:>8.1} ms for the stream, worst column {:>8.3} ms   \
         (first round {:.1} ms / {:.3} ms, fastest round {:.1} ms)",
        totals[ROUNDS / 2].as_secs_f64() * 1000.0,
        worsts[ROUNDS / 2].as_secs_f64() * 1000.0,
        totals[0].as_secs_f64() * 1000.0,
        worsts[0].as_secs_f64() * 1000.0,
        totals.iter().min().unwrap().as_secs_f64() * 1000.0,
    );
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
