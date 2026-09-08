//! Whether four callers building columns at once are waiting for a *thread* or
//! for a *lock*.
//!
//! Decision record 0031 shipped one regression and named this as the thing to
//! measure before fixing it: four simultaneous joins on a world read from
//! region files have a fatter tail than they did — median unchanged, worst
//! chat round trip 403 ms to 828 — and the two candidate causes want opposite
//! fixes. If the four are queued behind the single warming thread, a small
//! pool of warming threads is the answer. If they are queued behind
//! `AnvilCore`'s region mutex, **a pool makes it worse**: more threads holding
//! the same mutex for longer, and every session task that falls through to
//! building its own column waits behind all of them.
//!
//! A percentage cannot say which, and neither can the running server: with one
//! warming thread there is nothing much to contend *with*, so a lock-wait
//! counter on today's build would read low and would be answering the wrong
//! question. The question is the counterfactual — **would the lock be the wall
//! if the thread stopped being it** — and the way to answer that is to take
//! the thread out of the way and see what the work does.
//!
//! # The ladder
//!
//! Every row builds the **same set of columns** with the same total work, and
//! differs from the row above it in exactly one named thing:
//!
//! - **cpu control** — N threads doing arithmetic and touching nothing shared.
//!   The positive control, and it is not optional: three other builds run on
//!   this machine, and a row that does not scale proves nothing unless a row
//!   that should scale does. If this one is flat, stop and come back later.
//! - **region files, one store** — N threads through one `Source::Anvil`, which
//!   is one `Mutex<OpenRegions>`. This is the server as it is: a pool of
//!   warming threads would look exactly like this.
//! - **region files, a store each** — N threads through N `Source::Anvil`s over
//!   the same directory, so N independent mutexes over the same files. The one
//!   named change from the row above is *the sharing of the locks*. If this row
//!   scales and the one above it does not, the mutex is the wall and no number
//!   of threads behind it will help.
//!
//! **Every row gets a store built for it**, so that all of them start with an
//! empty sky-floor cache. That cache is shared between the threads of a shared
//! store and not between separate ones, and a row that inherited a warm one
//! from the row above would be two changes rather than one. The region files
//! themselves are read once before any row is timed, so that no row is
//! measuring this machine's disk.
//! - **generated, one store** — the other world, whose build path takes a short
//!   mutex around a cache lookup and nothing else. The second control: it says
//!   what column building looks like when the lock is not in the way.
//!
//! # The second question: what four joins cost after the column exists
//!
//! Decision record 0038 measured the first question and then overturned half
//! of it. A **flat** world — one template column, no file, no residency and no
//! lock — stalls a settled player as badly as a world read from region files,
//! so most of what a bystander waits through is not the column at all. The
//! ladder that followed, on the running server, says what it is: four joins
//! 600 ms apart cost a settled player nothing (worst round trip 92 ms over
//! eight runs) and the *same* 1,156 columns sent at the same moment cost up to
//! 1,291 ms. It is simultaneity, not volume, and dropping the view distance
//! from 8 to 2 — 25 columns a join instead of 289 — removes it entirely.
//!
//! So the last two rows are about the per-column work a session task does
//! **after** the column is in hand, which is the part four joins do four times
//! over in the same 700 ms:
//!
//! - **chunk packets, encode** — `play::chunk_packet` over the flat template.
//! - **chunk packets, encode and frame** — the same, plus what `Conn::send`
//!   does to it: the packet body encoded a second time and, above the 256-byte
//!   threshold every chunk packet clears, zlib at level 6. The single named
//!   change from the row above is *the framing*, so the gap between them is
//!   the compression.
//!
//! Both are run at 1, 2, 4 and 8 threads for the same reason the region rows
//! are: a path that scales is asking for cores, and a path that does not is
//! holding something. Which of the two it is decides whether the answer is to
//! do less work or to stop sharing something.
//!
//! Each row prints **throughput and the per-column latency distribution**,
//! because the number under investigation is a tail. Throughput can be flat
//! while p99 quadruples, and that is precisely the shape a queue has.
//!
//! The saturated throughput of a shared-lock row is itself a measurement: if N
//! threads cannot exceed T columns a second however many of them there are,
//! then `1/T` is the part of a column that is spent holding the lock, and no
//! arrangement of threads can beat it. That number is printed.
//!
//! ```text
//! cargo xtask extract --version 1.21.1 --only constants,worldgen
//! DUST_BENCH_CONSTANTS=.dust-extract/oracle-1.21.1/constants.tsv \
//!   DUST_BENCH_DATA=/path/to/data DUST_BENCH_REGION=/path/to/region \
//!   cargo bench -p dust-server --bench contention
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use dust_net::frame::{Compress, Frame, FrameEncoder, Limits};
use dust_protocol::packets::play;
use dust_protocol::wire::Writer;
use dust_protocol::ProtocolVersion;
use dust_server::net::source::{AnvilWorld, GeneratedColumns, RegistryNames, Source};
use dust_server::net::world::{FlatWorld, Palette};
use dust_world::coords::ChunkPos;

/// The version everything on the wire is encoded for.
const VERSION: ProtocolVersion = dust_protocol::version::V1_21_1;

/// The compression threshold this server announces at login, in bytes.
///
/// Not a constant this bench chose: `login_flow::Config` defaults to 256 and
/// nothing in the server changes it, and every chunk packet is far above it.
/// It is here so that the row below compresses exactly what a session does.
const COMPRESSION_THRESHOLD: usize = 256;

/// How many columns a row builds, however many threads it splits them over.
///
/// The total work is held constant so that wall time is directly a speedup.
/// 512 columns is a 32-by-16 patch inside the four region files around the
/// origin of the world `tools/bot` runs against, and about eight joins' worth
/// of the 289 a join sends.
const COLUMNS: usize = 512;

/// The thread counts the ladder runs. Four is the case the regression is
/// about; eight is there to show whether a row is still climbing or has hit a
/// wall, which one point cannot say.
const THREADS: [usize; 4] = [1, 2, 4, 8];

fn main() {
    let Some(constants) = table() else { return };
    let Ok(palette) = Palette::resolve() else {
        eprintln!("the generated block table has no bedrock; nothing to bench");
        return;
    };
    let constants = Arc::new(constants);
    let opacity = dust_server::net::world::opacity_of(palette.air, Some(&constants));
    let flat = || FlatWorld::new(palette, 0, 64);

    let columns = patch();
    println!(
        "each row builds the same {} columns, split evenly over its threads",
        columns.len()
    );
    println!(
        "available parallelism: {:?}",
        std::thread::available_parallelism()
    );

    control();

    // The packet path, before either world: it needs no files and no data
    // directory, so it runs on every machine this bench is run on, and it is
    // the part a *flat* world still pays. See the module docs.
    packets("chunk packets, encode", flat(), &columns, false);
    packets("chunk packets, encode and frame", flat(), &columns, true);

    match std::env::var_os("DUST_BENCH_REGION").map(std::path::PathBuf::from) {
        Some(directory) => match RegistryNames::new() {
            Some(_) => {
                let anvil = |names: RegistryNames| {
                    Source::Anvil(Box::new(AnvilWorld::new(
                        directory.clone(),
                        names,
                        flat(),
                        opacity.clone(),
                        Some(Arc::clone(&constants)),
                    )))
                };
                let store =
                    || anvil(RegistryNames::new().expect("a biome registry that just built"));
                // Read once and thrown away, deliberately: 2 MB of region file
                // is in the page cache within seconds of a real server
                // starting, so a cold row would be measuring this machine's
                // disk rather than the thing under test.
                let warm = store();
                for pos in &columns {
                    std::hint::black_box(warm.column(*pos).as_chunk().sections().len());
                }
                drop(warm);

                shared("region files, one store", &columns, &store);
                separate("region files, a store each", &columns, &store);
            }
            None => println!("region files: no synced biome registry; not run"),
        },
        None => {
            println!("region files: not run. Set DUST_BENCH_REGION to a world's region directory.");
        }
    }

    let Some(data) = std::env::var_os("DUST_BENCH_DATA").map(std::path::PathBuf::from) else {
        println!(
            "generated: not run. Set DUST_BENCH_DATA to a [data] path with dust-biomes.tsv in it."
        );
        return;
    };
    // Rebuilt for every row for the same reason the Anvil store is: the
    // generated world caches sky floors too, and a row that inherited a warm
    // cache from the row above would differ from it in two things.
    let generated = || {
        dust_server::net::generated::beside(
            &data,
            1,
            flat(),
            opacity.clone(),
            0,
            64,
            Some(Arc::clone(&constants)),
            &dust_config::ore::OresConfig::default(),
        )
    };
    match generated() {
        Ok(Some(_)) => shared("generated, one store", &columns, &|| {
            Source::Generated(Box::new(GeneratedColumns::new(
                generated()
                    .expect("a world that built a moment ago")
                    .expect("a world that built a moment ago")
                    .0,
            )))
        }),
        Ok(None) => println!("generated: no dust-biomes.tsv under DUST_BENCH_DATA"),
        Err(e) => println!("generated: {e}"),
    }
}

/// The columns every row builds: a patch inside the region files around the
/// origin, in a fixed order, disjoint between threads.
fn patch() -> Vec<ChunkPos> {
    let mut columns = Vec::with_capacity(COLUMNS);
    let mut x = -16;
    let mut z = -8;
    while columns.len() < COLUMNS {
        columns.push(ChunkPos::new(x, z));
        x += 1;
        if x == 16 {
            x = -16;
            z += 1;
        }
    }
    columns
}

/// N threads, one world between them. The server as it is built today, and
/// what a pool of warming threads would look like.
fn shared(name: &str, columns: &[ChunkPos], build: &dyn Fn() -> Source) {
    heading(name);
    let mut alone = None;
    for threads in THREADS {
        let world = build();
        let row = run(threads, columns, |_| &world);
        print_row(threads, &row, &mut alone);
    }
    println!();
}

/// N threads, one world *each* over the same files. The single named change
/// from `shared`: the same work, the same files, N locks instead of one.
fn separate(name: &str, columns: &[ChunkPos], build: &dyn Fn() -> Source) {
    heading(name);
    let mut alone = None;
    for threads in THREADS {
        let worlds: Vec<Source> = (0..threads).map(|_| build()).collect();
        let row = run(threads, columns, |thread| &worlds[thread]);
        print_row(threads, &row, &mut alone);
    }
    println!();
}

/// What a session task pays per column **after** the column is in hand.
///
/// Every one of these is work the server does once per column per session, so
/// four players joining the same place at the same moment do it four times
/// over — and unlike the column itself, none of it is shared, cached or
/// reused. The flat template is deliberate: it is the world where the column
/// costs nothing, so whatever this row prints is the floor under every world.
///
/// With `frame`, the row adds exactly what `Conn::send` does — the body
/// encoded again into a frame and then zlib level 6, because a chunk packet is
/// far over the 256-byte threshold announced at login. The two rows differ in
/// that one named thing.
fn packets(name: &str, flat: FlatWorld, columns: &[ChunkPos], frame: bool) {
    if frame {
        // What one column costs on the wire, printed because the CPU number
        // alone cannot say whether a burst is bounded by cores or by bytes.
        let mut encoder = FrameEncoder::new(Limits::default());
        encoder.set_compression(Compress::At {
            threshold: COMPRESSION_THRESHOLD,
        });
        let packet: play::clientbound::Packet =
            dust_server::net::play::chunk_packet(flat.column(), ChunkPos::new(0, 0), VERSION)
                .expect("a flat column encodes")
                .into();
        let mut body = Writer::default();
        packet
            .encode_body(&mut body, VERSION)
            .expect("a flat column encodes");
        let wire = Frame::new(
            packet.protocol_id(VERSION).expect("a known packet") as i32,
            body.into_bytes(),
        );
        let mut out = Vec::new();
        encoder.encode(&wire, &mut out).expect("a frame encodes");
        println!(
            "\none flat column frames to {} bytes on the wire",
            out.len()
        );
    }
    heading(name);
    let mut alone = None;
    for threads in THREADS {
        let at = Instant::now();
        let mut latencies: Vec<Duration> = std::thread::scope(|scope| {
            let flat = &flat;
            let handles: Vec<_> = (0..threads)
                .map(|thread| {
                    scope.spawn(move || {
                        // One encoder per thread, as one per connection is what
                        // the server has. A shared one would be measuring a
                        // lock this server does not own.
                        let mut encoder = FrameEncoder::new(Limits::default());
                        encoder.set_compression(Compress::At {
                            threshold: COMPRESSION_THRESHOLD,
                        });
                        let mut out = Vec::with_capacity(1 << 16);
                        let mut mine = Vec::with_capacity(columns.len() / threads + 1);
                        for pos in columns.iter().skip(thread).step_by(threads) {
                            let started = Instant::now();
                            let packet: play::clientbound::Packet =
                                dust_server::net::play::chunk_packet(flat.column(), *pos, VERSION)
                                    .expect("a flat column encodes")
                                    .into();
                            if frame {
                                let mut body = Writer::default();
                                packet
                                    .encode_body(&mut body, VERSION)
                                    .expect("a flat column encodes");
                                let wire = Frame::new(
                                    packet.protocol_id(VERSION).expect("a known packet") as i32,
                                    body.into_bytes(),
                                );
                                out.clear();
                                encoder.encode(&wire, &mut out).expect("a frame encodes");
                                std::hint::black_box(out.len());
                            } else {
                                std::hint::black_box(&packet);
                            }
                            mine.push(started.elapsed());
                        }
                        mine
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        let wall = at.elapsed();
        latencies.sort_unstable();
        print_row(threads, &Row { wall, latencies }, &mut alone);
    }
    println!();
}

/// N threads doing arithmetic and sharing nothing. If this does not scale, the
/// machine is busy and no other row on this run means anything.
fn control() {
    heading("cpu control (shares nothing)");
    // Sized to about a region column, so the row's shape is comparable.
    const SPIN: u64 = 400_000;
    let mut alone = None;
    for threads in THREADS {
        let each = COLUMNS / threads;
        let at = Instant::now();
        let mut latencies: Vec<Duration> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    scope.spawn(move || {
                        let mut mine = Vec::with_capacity(each);
                        for _ in 0..each {
                            let started = Instant::now();
                            let mut acc = 1u64;
                            for i in 0..SPIN {
                                acc = acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i);
                            }
                            std::hint::black_box(acc);
                            mine.push(started.elapsed());
                        }
                        mine
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        let wall = at.elapsed();
        latencies.sort_unstable();
        print_row(threads, &Row { wall, latencies }, &mut alone);
    }
    println!();
}

struct Row {
    wall: Duration,
    /// Every column's own build time, sorted. The tail is the point of this
    /// bench, so the whole distribution is kept rather than a mean.
    latencies: Vec<Duration>,
}

/// Split `columns` over `threads` and build every one of them, timing each.
///
/// The split is by stride rather than by block so that every thread reads
/// across the same region files: a block split would give thread 0 one file
/// and thread 3 another, which is a different experiment.
fn run<'a>(
    threads: usize,
    columns: &[ChunkPos],
    world: impl Fn(usize) -> &'a Source + Sync,
) -> Row {
    let at = Instant::now();
    let mut latencies: Vec<Duration> = std::thread::scope(|scope| {
        let world = &world;
        let handles: Vec<_> = (0..threads)
            .map(|thread| {
                scope.spawn(move || {
                    let source = world(thread);
                    let mut mine = Vec::with_capacity(columns.len() / threads + 1);
                    for pos in columns.iter().skip(thread).step_by(threads) {
                        let started = Instant::now();
                        std::hint::black_box(source.column(*pos).as_chunk().sections().len());
                        mine.push(started.elapsed());
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let wall = at.elapsed();
    latencies.sort_unstable();
    Row { wall, latencies }
}

fn heading(name: &str) {
    println!("\n{name}:");
    println!(
        "  {:>7}  {:>9}  {:>9}  {:>8}  {:>8}  {:>8}  {:>8}",
        "threads", "wall ms", "col/s", "speedup", "p50 ms", "p99 ms", "max ms"
    );
}

/// `alone` is the one-thread wall time of this section, kept so that every
/// later row can state its speedup against the row it is trying to beat.
fn print_row(threads: usize, row: &Row, alone: &mut Option<Duration>) {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let n = row.latencies.len();
    let at = |p: f64| ms(row.latencies[((p * n as f64).ceil() as usize).clamp(1, n) - 1]);
    let per_second = n as f64 / row.wall.as_secs_f64();
    let base = *alone.get_or_insert(row.wall);
    println!(
        "  {threads:>7}  {:>9.1}  {:>9.0}  {:>7.2}x  {:>8.3}  {:>8.3}  {:>8.3}",
        ms(row.wall),
        per_second,
        base.as_secs_f64() / row.wall.as_secs_f64(),
        at(0.50),
        at(0.99),
        ms(*row
            .latencies
            .last()
            .expect("a row builds at least one column")),
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
