//! The world's own spawn point, which lives beside the region files rather
//! than in them.
//!
//! # Why this is a separate read
//!
//! `[server].world_source` names a directory of `.mca` files, and everything
//! else Dust serves comes out of those files. A world's spawn point does not:
//! it is in `level.dat`, one level up, alongside the region directory rather
//! than inside it.
//!
//! So a server that reads only the region files knows every block of a world
//! and not where its owner is meant to stand in it. Until this existed, Dust
//! spawned every player at x 0, z 0 — which on Minecraft's own seed 1 is 176
//! blocks from the spawn the world was generated with, in open ocean, and on
//! seed 0 is 32 blocks off. Both worlds look, to a player joining them, like a
//! server that lost the world and generated a different one.
//!
//! # What is read and what is refused
//!
//! Three integers and a float from `Data`: `SpawnX`, `SpawnZ`, `SpawnAngle`.
//! Two longs as well, and they are the one thing here that is also
//! **written**: `Time` and `DayTime`, the world's clock. See
//! [`time_beside`] and [`store_time_beside`].
//!
//! `SpawnY` is deliberately not among them. A stored y is a claim about what
//! the world was when it was saved, and the block at that column may have been
//! dug out since; the y Dust uses comes from the column's own heightmap, which
//! is a fact about the world being served right now. See
//! [`spawn_at`](super::world::spawn_at) for why that matters more than it
//! sounds.
//!
//! **A missing `level.dat` is not an error and a broken one is.** A region
//! directory with no world file beside it is a legitimate arrangement — it is
//! what `harness rewrite` produces, and what an operator pointing at a
//! directory of chunks they extracted has — and the answer there is the origin,
//! same as before. A `level.dat` that exists and cannot be read is different:
//! it is a world that has a spawn point which this server would then ignore,
//! and a player put at the origin of a world whose spawn is somewhere else is
//! the silent kind of wrong. It refuses to start, for the same reason the save
//! file beside it does.

use std::path::Path;

/// Where a world says its players belong.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldSpawn {
    /// The spawn column's x, in blocks.
    pub x: i32,
    /// The spawn column's z, in blocks.
    pub z: i32,
    /// The yaw a player faces on arriving, in degrees.
    pub angle: f32,
}

/// The file name Minecraft gives a world's own record of itself.
const LEVEL_DAT: &str = "level.dat";

/// The seed of the world whose region directory this is.
///
/// `None` when there is no `level.dat`, when it cannot be read, or when it
/// does not carry a seed — all three of which are the same answer to the
/// caller, and none of them an error.
///
/// **That is deliberately softer than [`spawn_beside`], and the difference is
/// what each one is for.** A spawn point that exists and is ignored puts every
/// player in the wrong place in a world that is otherwise right, so an
/// unreadable one stops the server. A seed is only ever used to generate the
/// columns *off the edge* of the world file; not having one costs a plain
/// there instead of terrain, which is what Dust served everywhere until this
/// existed. Refusing to start over it would be refusing to serve a world this
/// server can serve.
pub fn seed_beside(region_directory: &Path) -> Option<i64> {
    let world_directory = region_directory.parent()?;
    let bytes = std::fs::read(world_directory.join(LEVEL_DAT)).ok()?;
    read_seed(&bytes)
}

/// The seed inside a `level.dat`'s bytes, compressed or not.
fn read_seed(bytes: &[u8]) -> Option<i64> {
    let plain = dust_nbt::compression::decompress_detected(bytes, LEVEL_DAT_LIMIT).ok()?;
    let document = dust_nbt::read::from_bytes(&plain).ok()?;
    let dust_nbt::Tag::Compound(root) = &document.tag else {
        return None;
    };
    let dust_nbt::Tag::Compound(data) = root.get("Data")? else {
        return None;
    };
    // 1.16 moved it here from `Data.RandomSeed`, and both spellings are read:
    // an operator with an older save is serving an older world, not a broken
    // one.
    if let Some(dust_nbt::Tag::Compound(settings)) = data.get("WorldGenSettings") {
        if let Some(dust_nbt::Tag::Long(seed)) = settings.get("seed") {
            return Some(*seed);
        }
    }
    match data.get("RandomSeed") {
        Some(dust_nbt::Tag::Long(seed)) => Some(*seed),
        _ => None,
    }
}

/// A world's clock, as `level.dat` keeps it.
///
/// Both are longs and both count up. `Data.Time` is every tick the world has
/// ever run; `Data.DayTime` is the sun's position as a running total, which is
/// why it is not reduced here either — see [`crate::net::daylight`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldTime {
    /// `Data.Time`.
    pub game_time: u64,
    /// `Data.DayTime`.
    pub day_time: u64,
}

/// The clock of the world whose region directory this is.
///
/// `None` when there is no `level.dat`, when it cannot be read, or when it
/// does not carry both keys — the same softness [`seed_beside`] has, and for
/// a sharper version of the same reason. A world Dust has served before has
/// its clock in Dust's own save file, which is the record this server wrote
/// and the one that wins; this read is what gives a world **imported from
/// vanilla** the time of day it was left at, rather than dawn. Refusing to
/// start over it would refuse a world this server can serve.
///
/// A negative value in either key is read as zero rather than refused. Vanilla
/// stores signed longs and nothing it does produces a negative one, but a
/// third-party editor can; the sun cannot be at a negative position, and a
/// clamped clock is a smaller lie than a wrapped one.
pub fn time_beside(region_directory: &Path) -> Option<WorldTime> {
    let world_directory = region_directory.parent()?;
    let bytes = std::fs::read(world_directory.join(LEVEL_DAT)).ok()?;
    read_time(&bytes)
}

/// The clock inside a `level.dat`'s bytes, compressed or not.
fn read_time(bytes: &[u8]) -> Option<WorldTime> {
    let plain = dust_nbt::compression::decompress_detected(bytes, LEVEL_DAT_LIMIT).ok()?;
    let document = dust_nbt::read::from_bytes(&plain).ok()?;
    let dust_nbt::Tag::Compound(root) = &document.tag else {
        return None;
    };
    let dust_nbt::Tag::Compound(data) = root.get("Data")? else {
        return None;
    };
    let long = |name: &str| match data.get(name) {
        Some(dust_nbt::Tag::Long(value)) => Some(u64::try_from(*value).unwrap_or(0)),
        _ => None,
    };
    Some(WorldTime {
        game_time: long("Time")?,
        day_time: long("DayTime")?,
    })
}

/// Write the clock back into the `level.dat` beside this region directory.
///
/// **Read, modify, write, rename.** The file is parsed whole, two keys are
/// replaced, and everything else — the world's name, its game rules, its data
/// version, whatever a mod put there — is written back as it was read.
/// `dust-nbt` implements all thirteen tag types and round-trips them under a
/// property test, so what comes back out is what went in; a document it cannot
/// read is refused rather than partially rewritten.
///
/// `Ok(false)` means there was nothing to write to: no `level.dat` beside the
/// directory, which is the ordinary case for a generated world and for the
/// directories `harness rewrite` produces. **A world file is never created**,
/// because a `level.dat` holding two keys and nothing else is not a world and
/// vanilla would refuse it.
///
/// This is the only place in Dust that writes a file Minecraft also writes,
/// and the reason it does is that the alternative is a world served by Dust
/// for a week and then opened in vanilla at the time it was imported. Decision
/// record 0044 is the argument.
///
/// # Errors
///
/// The file exists and could not be read, parsed, re-encoded or replaced. The
/// caller logs it; nothing here is worth failing a shutdown over, because the
/// clock is also in Dust's own save.
pub fn store_time_beside(region_directory: &Path, time: WorldTime) -> Result<bool, String> {
    let Some(world_directory) = region_directory.parent() else {
        return Ok(false);
    };
    let path = world_directory.join(LEVEL_DAT);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    // The scheme it arrived in is the scheme it leaves in. Vanilla writes
    // gzip; a file some tool has decompressed is still a world, and writing
    // gzip back over it would be this server deciding how an operator's tools
    // should have left their file.
    let scheme = dust_nbt::Compression::detect(&bytes);
    let plain = dust_nbt::compression::decompress_detected(&bytes, LEVEL_DAT_LIMIT)
        .map_err(|e| format!("{} did not decompress: {e}", path.display()))?;
    let mut document = dust_nbt::read::from_bytes(&plain)
        .map_err(|e| format!("{} is not NBT: {e}", path.display()))?;
    let dust_nbt::Tag::Compound(root) = &mut document.tag else {
        return Err(format!("{}'s root is not a compound", path.display()));
    };
    let Some(dust_nbt::Tag::Compound(data)) = root.get_mut("Data") else {
        return Err(format!("{} has no `Data` compound", path.display()));
    };
    // Saturating, for the reason the packet's conversion is: a u64 that does
    // not fit in an i64 is 292 million years of uptime, and a clamped clock is
    // a better world file than one whose time went negative.
    data.insert(
        "Time",
        dust_nbt::Tag::Long(i64::try_from(time.game_time).unwrap_or(i64::MAX)),
    );
    data.insert(
        "DayTime",
        dust_nbt::Tag::Long(i64::try_from(time.day_time).unwrap_or(i64::MAX)),
    );

    let written = dust_nbt::write::to_vec(&document.name, &document.tag)
        .map_err(|e| format!("{} could not be written back: {e}", path.display()))?;
    let written = dust_nbt::compression::compress(&written, scheme)
        .map_err(|e| format!("{} could not be recompressed: {e}", path.display()))?;

    // The same temporary-then-rename the save file uses, and for the same
    // reason with more at stake: a half-written `level.dat` is a world
    // Minecraft will not open.
    let temporary = path.with_extension("dat.dust-tmp");
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&temporary)
            .map_err(|e| format!("{} could not be created: {e}", temporary.display()))?;
        file.write_all(&written)
            .map_err(|e| format!("{} could not be written: {e}", temporary.display()))?;
        file.sync_all()
            .map_err(|e| format!("{} could not be flushed: {e}", temporary.display()))?;
    }
    std::fs::rename(&temporary, &path)
        .map_err(|e| format!("{} could not be replaced: {e}", path.display()))?;
    Ok(true)
}

/// Read the spawn point of the world whose region directory this is.
///
/// `Ok(None)` when there is no `level.dat` beside the directory. `Err` when
/// there is one and it does not answer the question — see the module note for
/// why those two are not the same answer.
///
/// # Errors
///
/// The file exists but cannot be read, is not NBT, has no `Data` compound, or
/// is missing one of the spawn keys.
pub fn spawn_beside(region_directory: &Path) -> Result<Option<WorldSpawn>, String> {
    // The region directory's parent, which is the world directory. A path with
    // no parent — a bare relative name — has no world beside it to read.
    let Some(world_directory) = region_directory.parent() else {
        return Ok(None);
    };
    let path = world_directory.join(LEVEL_DAT);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    read_spawn(&bytes).map(Some).map_err(|why| {
        format!(
            "{} is a world file this server cannot read its spawn point out of: {why}. \
             Starting anyway would put every player at x 0, z 0 in a world whose spawn \
             is somewhere else.",
            path.display()
        )
    })
}

/// The spawn point inside a `level.dat`'s bytes, compressed or not.
///
/// Split from [`spawn_beside`] so the parsing has tests that need no
/// directory: everything below the file system is a pure function of bytes.
fn read_spawn(bytes: &[u8]) -> Result<WorldSpawn, String> {
    let plain = dust_nbt::compression::decompress_detected(bytes, LEVEL_DAT_LIMIT)
        .map_err(|e| format!("it did not decompress: {e}"))?;
    let document = dust_nbt::read::from_bytes(&plain).map_err(|e| format!("it is not NBT: {e}"))?;
    let dust_nbt::Tag::Compound(root) = &document.tag else {
        return Err("its root is not a compound".to_owned());
    };
    let Some(dust_nbt::Tag::Compound(data)) = root.get("Data") else {
        return Err("it has no `Data` compound".to_owned());
    };

    let int = |name: &str| match data.get(name) {
        Some(dust_nbt::Tag::Int(value)) => Ok(*value),
        Some(other) => Err(format!(
            "`Data.{name}` is {:?} rather than a TAG_Int",
            other.tag_type()
        )),
        None => Err(format!("it has no `Data.{name}`")),
    };

    Ok(WorldSpawn {
        x: int("SpawnX")?,
        z: int("SpawnZ")?,
        // The one optional key. A world written before angles were stored has
        // no opinion about which way a player faces, and facing due south is
        // the answer vanilla gives when the field is absent — a default that
        // is a real behaviour rather than a stand-in for a missing number.
        angle: match data.get("SpawnAngle") {
            Some(dust_nbt::Tag::Float(value)) => *value,
            Some(other) => {
                return Err(format!(
                    "`Data.SpawnAngle` is {:?} rather than a TAG_Float",
                    other.tag_type()
                ))
            }
            None => 0.0,
        },
    })
}

/// How much `level.dat` is allowed to decompress to.
///
/// A world file is a few kilobytes; vanilla's own is under two. The library's
/// 32 MiB file default is sized for chunk data and is far more headroom than
/// anything here needs, and this path reads a file named by configuration.
const LEVEL_DAT_LIMIT: usize = 8 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use dust_nbt::{Compound, Tag};

    /// A directory of this test's own, named after it.
    ///
    /// The same shape as `save`'s: process id and a counter, so two tests in
    /// one binary and two binaries at once never share one. A dependency for
    /// this would be a dependency in the licence gate for eight lines.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dust-level-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join("region")).expect("temp dir");
        dir
    }

    /// A `level.dat` holding the keys this module reads, written the way
    /// Minecraft writes one: gzip around a file-form document whose root is
    /// unnamed and holds a single `Data`.
    fn level_dat(data: Compound) -> Vec<u8> {
        let mut root = Compound::new();
        root.insert("Data", Tag::Compound(data));
        let plain = dust_nbt::write::to_vec("", &Tag::Compound(root)).expect("writable");
        dust_nbt::compression::compress(&plain, dust_nbt::Compression::Gzip).expect("compressible")
    }

    fn spawn_of(x: i32, z: i32) -> Compound {
        let mut data = Compound::new();
        data.insert("SpawnX", Tag::Int(x));
        data.insert("SpawnY", Tag::Int(67));
        data.insert("SpawnZ", Tag::Int(z));
        data
    }

    #[test]
    fn the_seed_comes_out_of_either_place_a_world_has_kept_it() {
        // 1.16 moved the seed from `Data.RandomSeed` into
        // `Data.WorldGenSettings.seed`, and a server is asked to serve both.
        let mut modern = spawn_of(0, 0);
        let mut settings = Compound::new();
        settings.insert("seed", Tag::Long(-4172144997902289642));
        modern.insert("WorldGenSettings", Tag::Compound(settings));
        assert_eq!(read_seed(&level_dat(modern)), Some(-4172144997902289642));

        let mut ancient = spawn_of(0, 0);
        ancient.insert("RandomSeed", Tag::Long(7));
        assert_eq!(read_seed(&level_dat(ancient)), Some(7));
    }

    #[test]
    fn a_world_file_with_no_seed_in_it_is_not_an_error() {
        // Softer than the spawn read on purpose, and this is the test that
        // says so: a missing seed costs a plain off the edge of the disc,
        // which is what Dust served everywhere before there was a generator.
        // A missing spawn point puts every player in the wrong place in a
        // world that is otherwise right, and that one refuses to start.
        assert_eq!(read_seed(&level_dat(spawn_of(0, 0))), None);
        assert_eq!(read_seed(b"not a world file at all"), None);
        assert!(read_spawn(b"not a world file at all").is_err());
    }

    #[test]
    fn there_is_no_seed_beside_a_directory_with_no_world_file() {
        let dir = temp_dir("no-seed");
        assert_eq!(seed_beside(&dir.join("region")), None);
    }

    #[test]
    fn the_three_numbers_come_back_out() {
        let mut data = spawn_of(-32, 0);
        data.insert("SpawnAngle", Tag::Float(90.0));
        let spawn = read_spawn(&level_dat(data)).expect("readable");
        assert_eq!(
            spawn,
            WorldSpawn {
                x: -32,
                z: 0,
                angle: 90.0
            }
        );
    }

    #[test]
    fn an_uncompressed_world_file_reads_the_same() {
        // `Compression::detect` is what makes this work, and it is worth a
        // test because a world file that has been through a tool which
        // decompressed it is a real thing an operator will have.
        let mut root = Compound::new();
        root.insert("Data", Tag::Compound(spawn_of(7, 9)));
        let plain = dust_nbt::write::to_vec("", &Tag::Compound(root)).expect("writable");
        let spawn = read_spawn(&plain).expect("readable");
        assert_eq!((spawn.x, spawn.z), (7, 9));
    }

    #[test]
    fn an_absent_angle_faces_south_rather_than_failing() {
        let spawn = read_spawn(&level_dat(spawn_of(0, 0))).expect("readable");
        assert_eq!(spawn.angle, 0.0);
    }

    #[test]
    fn a_missing_spawn_key_is_named_rather_than_defaulted() {
        // Defaulting a missing `SpawnZ` to zero would put a player on the
        // right meridian of the wrong world and say nothing. The message names
        // the key so an operator can look at the file.
        let mut data = spawn_of(-32, 0);
        data.remove("SpawnZ");
        let why = read_spawn(&level_dat(data)).expect_err("refused");
        assert!(why.contains("SpawnZ"), "{why}");
    }

    #[test]
    fn a_spawn_key_of_the_wrong_type_is_refused_by_type() {
        // NBT has six number types and reading a `TAG_Long` as an int is a
        // choice about which half of it to keep. There is no right half.
        let mut data = spawn_of(-32, 0);
        data.insert("SpawnX", Tag::Long(-32));
        let why = read_spawn(&level_dat(data)).expect_err("refused");
        assert!(why.contains("SpawnX") && why.contains("TAG_Int"), "{why}");
    }

    #[test]
    fn bytes_that_are_not_nbt_are_refused() {
        let why = read_spawn(b"this is not a world").expect_err("refused");
        assert!(
            why.contains("not NBT") || why.contains("decompress"),
            "{why}"
        );
    }

    #[test]
    fn a_region_directory_with_no_world_file_beside_it_answers_none() {
        // The `harness rewrite` case, and the case of an operator who copied
        // out a directory of chunks. Not an error: there is no spawn point to
        // be wrong about.
        let dir = temp_dir("no-world-file");
        assert_eq!(spawn_beside(&dir.join("region")), Ok(None));
    }

    #[test]
    fn a_world_file_that_cannot_be_read_stops_the_server() {
        // The whole reason absent and broken are different answers. A player
        // put at the origin of a world whose spawn is elsewhere sees a server
        // that lost the world, and nothing in the log would say otherwise.
        let dir = temp_dir("broken-world-file");
        std::fs::write(dir.join(LEVEL_DAT), b"not a world").expect("writable");
        let why = spawn_beside(&dir.join("region")).expect_err("refused");
        assert!(why.contains("level.dat"), "{why}");
        assert!(
            why.contains("x 0, z 0"),
            "the message says what it prevents"
        );
    }

    #[test]
    fn the_clock_comes_back_out_of_a_world_file_written_the_way_vanilla_writes_one() {
        let mut data = spawn_of(0, 0);
        data.insert("Time", Tag::Long(1_234_567));
        data.insert("DayTime", Tag::Long(3 * 24_000 + 13_000));
        assert_eq!(
            read_time(&level_dat(data)),
            Some(WorldTime {
                game_time: 1_234_567,
                day_time: 3 * 24_000 + 13_000,
            })
        );
    }

    #[test]
    fn a_world_file_with_no_clock_in_it_is_not_an_error() {
        // The soft read, like the seed's and unlike the spawn point's. A world
        // with no `DayTime` costs a time of day, and the answer to that is
        // dawn, not a server that will not start.
        assert_eq!(read_time(&level_dat(spawn_of(0, 0))), None);
        assert_eq!(read_time(b"not a world file at all"), None);
    }

    #[test]
    fn a_negative_stored_time_is_clamped_rather_than_wrapped() {
        // Nothing vanilla does produces one; a third-party NBT editor can. The
        // sun cannot be at a negative position, and reading -1 as eighteen
        // quintillion would put the world on day 768,614,336,404,564.
        let mut data = spawn_of(0, 0);
        data.insert("Time", Tag::Long(-1));
        data.insert("DayTime", Tag::Long(-5));
        assert_eq!(
            read_time(&level_dat(data)),
            Some(WorldTime {
                game_time: 0,
                day_time: 0
            })
        );
    }

    #[test]
    fn writing_the_clock_back_keeps_every_other_key_the_world_had() {
        // The property that makes writing an operator's `level.dat` defensible
        // at all: this is a read-modify-write of a file Minecraft owns, and
        // anything it dropped would be a world that lost something. So the
        // fixture carries keys this module has never heard of — including a
        // nested compound and a long array, which are the two shapes a lazy
        // round trip loses — and they all have to survive.
        let dir = temp_dir("write-clock");
        let mut data = spawn_of(112, 176);
        data.insert("LevelName", Tag::String("A Dust world".to_owned()));
        data.insert("DataVersion", Tag::Int(3955));
        data.insert("Time", Tag::Long(1));
        data.insert("DayTime", Tag::Long(1));
        let mut rules = Compound::new();
        rules.insert("doDaylightCycle", Tag::String("true".to_owned()));
        data.insert("GameRules", Tag::Compound(rules));
        data.insert("DragonFight", Tag::LongArray(vec![7, 8, 9]));
        std::fs::write(dir.join(LEVEL_DAT), level_dat(data)).expect("writable");

        let region = dir.join("region");
        assert_eq!(
            store_time_beside(
                &region,
                WorldTime {
                    game_time: 500_000,
                    day_time: 18_000
                }
            ),
            Ok(true)
        );

        assert_eq!(
            time_beside(&region),
            Some(WorldTime {
                game_time: 500_000,
                day_time: 18_000
            })
        );
        // And the world is still the world it was.
        assert_eq!(
            spawn_beside(&region),
            Ok(Some(WorldSpawn {
                x: 112,
                z: 176,
                angle: 0.0
            }))
        );
        let bytes = std::fs::read(dir.join(LEVEL_DAT)).expect("readable");
        let plain =
            dust_nbt::compression::decompress_detected(&bytes, LEVEL_DAT_LIMIT).expect("gzip");
        let document = dust_nbt::read::from_bytes(&plain).expect("NBT");
        let Tag::Compound(root) = &document.tag else {
            panic!("root");
        };
        let Some(Tag::Compound(data)) = root.get("Data") else {
            panic!("Data");
        };
        assert_eq!(
            data.get("LevelName"),
            Some(&Tag::String("A Dust world".to_owned()))
        );
        assert_eq!(data.get("DataVersion"), Some(&Tag::Int(3955)));
        assert_eq!(
            data.get("DragonFight"),
            Some(&Tag::LongArray(vec![7, 8, 9]))
        );
        let Some(Tag::Compound(rules)) = data.get("GameRules") else {
            panic!("GameRules went missing");
        };
        assert_eq!(
            rules.get("doDaylightCycle"),
            Some(&Tag::String("true".to_owned()))
        );
    }

    #[test]
    fn a_world_file_that_is_not_there_is_not_written_into_existence() {
        // A `level.dat` holding two keys and nothing else is not a world, and
        // Minecraft would refuse to open the directory it appeared in. The
        // clock of a world with no world file lives in Dust's save.
        let dir = temp_dir("no-world-file-to-write");
        assert_eq!(
            store_time_beside(
                &dir.join("region"),
                WorldTime {
                    game_time: 1,
                    day_time: 2
                }
            ),
            Ok(false)
        );
        assert!(!dir.join(LEVEL_DAT).exists());
    }

    #[test]
    fn a_world_file_that_is_not_nbt_is_refused_rather_than_overwritten() {
        let dir = temp_dir("broken-world-file-to-write");
        std::fs::write(dir.join(LEVEL_DAT), b"not a world").expect("writable");
        let why = store_time_beside(
            &dir.join("region"),
            WorldTime {
                game_time: 1,
                day_time: 2,
            },
        )
        .expect_err("refused");
        assert!(why.contains("level.dat"), "{why}");
        assert_eq!(
            std::fs::read(dir.join(LEVEL_DAT)).expect("readable"),
            b"not a world",
            "the file an operator has is not this server's to replace with a guess"
        );
    }

    #[test]
    fn an_uncompressed_world_file_is_written_back_uncompressed() {
        // The scheme it arrived in is the scheme it leaves in. An operator
        // whose tools decompressed their world file has a world file, and
        // gzipping it on the way out would be this server deciding how their
        // tools should have left it.
        let dir = temp_dir("uncompressed-write");
        let mut root = Compound::new();
        let mut data = spawn_of(0, 0);
        data.insert("Time", Tag::Long(0));
        data.insert("DayTime", Tag::Long(0));
        root.insert("Data", Tag::Compound(data));
        let plain = dust_nbt::write::to_vec("", &Tag::Compound(root)).expect("writable");
        std::fs::write(dir.join(LEVEL_DAT), &plain).expect("writable");

        let region = dir.join("region");
        assert_eq!(
            store_time_beside(
                &region,
                WorldTime {
                    game_time: 9,
                    day_time: 6_000
                }
            ),
            Ok(true)
        );
        let written = std::fs::read(dir.join(LEVEL_DAT)).expect("readable");
        assert_eq!(
            dust_nbt::Compression::detect(&written),
            dust_nbt::Compression::None
        );
        assert_eq!(
            time_beside(&region),
            Some(WorldTime {
                game_time: 9,
                day_time: 6_000
            })
        );
    }

    #[test]
    fn a_world_file_beside_the_region_directory_is_found() {
        let dir = temp_dir("found");
        std::fs::write(dir.join(LEVEL_DAT), level_dat(spawn_of(112, 176))).expect("writable");
        assert_eq!(
            spawn_beside(&dir.join("region")),
            Ok(Some(WorldSpawn {
                x: 112,
                z: 176,
                angle: 0.0
            }))
        );
    }
}
