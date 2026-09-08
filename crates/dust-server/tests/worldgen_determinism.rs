//! Is a generated column a function of where it is, or of when it was built?
//!
//! # Why this is `#[ignore]` and how to run it
//!
//! Generating terrain needs the operator's own data pack and Minecraft's block
//! table, and nothing of Mojang's is committed — so this is skipped unless
//! `DUST_BENCH_DATA` points at a `[data]` path with `dust-biomes.tsv` in it and
//! `DUST_BENCH_CONSTANTS` at the constants table. CI does not set either, and
//! the test says out loud when it did not run rather than passing vacuously.
//!
//! ```text
//! cargo xtask extract --version 1.21.1 --only worldgen,constants
//! cp <cache>/oracle-1.21.1/dust-biomes.tsv <cache>/data-1.21.1/data/
//! DUST_BENCH_DATA=<cache>/data-1.21.1/data \
//!   DUST_BENCH_CONSTANTS=<cache>/oracle-1.21.1/constants.tsv \
//!   cargo test --release -p dust-server --test worldgen_determinism -- --ignored --nocapture
//! ```
//!
//! # What it is for
//!
//! `net::source::ColumnStore` builds the world on a pool of threads, and a pool
//! only preserves a seed if the thing it calls is a function of position alone.
//! The store's own half of that — a column built once, filed under its own key,
//! the same blocks whatever the builder count — is checked in `net::source`'s
//! unit tests, which need no world at all. **This is the other half**, and it
//! is the half that was broken: `GeneratedWorld`'s sky-floor cache held two
//! different functions' answers under one key, so a column's light depended on
//! whether a neighbour had been built as a full column yet. With one builder
//! that was the queue order and reproducible; with four it was a race.
//!
//! Fifteen of the nine hundred columns below disagreed before the fix.
//! **A first version of this test looked at sixteen columns around the origin
//! and found nothing** — seed 1's origin is ocean, where a carver changes
//! nothing a sky floor can see. The area a probe covers is part of what it
//! proves.

use std::sync::Arc;

use dust_server::net::generated::GeneratedWorld;
use dust_server::net::world::{FlatWorld, Palette};
use dust_world::coords::ChunkPos;

/// A generated world on seed 1, or a stated reason there is none.
fn world() -> GeneratedWorld {
    let data = std::env::var_os("DUST_BENCH_DATA")
        .map(std::path::PathBuf::from)
        .expect("DUST_BENCH_DATA is not set; see this file's own documentation");
    let table = std::env::var_os("DUST_BENCH_CONSTANTS")
        .map(std::path::PathBuf::from)
        .expect("DUST_BENCH_CONSTANTS is not set; see this file's own documentation");
    let text =
        std::fs::read_to_string(&table).unwrap_or_else(|e| panic!("{}: {e}", table.display()));
    let constants = Arc::new(
        dust_registry::BlockConstants::parse(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", table.display())),
    );
    let palette = Palette::resolve().expect("the generated block table has bedrock");
    let opacity = dust_server::net::world::opacity_of(palette.air, Some(&constants));
    match dust_server::net::generated::beside(
        &data,
        1,
        FlatWorld::new(palette, 0, 64),
        opacity,
        0,
        64,
        Some(constants),
    ) {
        Ok(Some((world, _))) => world,
        Ok(None) => panic!("no dust-biomes.tsv under {}", data.display()),
        Err(e) => panic!("{e}"),
    }
}

/// The same column, reached two ways, on two worlds of the same seed.
///
/// One world meets each position cold. The other has already built the four
/// columns around it, in full, before it asks for the position itself — which
/// is exactly what a second builder does to the first builder's neighbourhood.
/// A world that is a function of its seed answers the same thing both times.
///
/// Positions are **eight chunks apart** so that no two share a neighbour: a
/// cache one position leaves behind then says nothing about the next, and a
/// disagreement is about the column it is reported on.
///
/// Watched to fail: putting `self.remember(pos, SkyFloor::of(&chunk))` back at
/// the top of `GeneratedWorld::column` makes this red on 15 of the 900.
#[test]
#[ignore = "needs the operator's data pack; set DUST_BENCH_DATA and DUST_BENCH_CONSTANTS"]
fn a_column_is_the_same_however_its_neighbours_were_reached() {
    let cold = world();
    let warm = world();
    let mut differed: Vec<ChunkPos> = Vec::new();
    let mut checked = 0;
    for x in 0..30 {
        for z in 0..30 {
            let pos = ChunkPos::new(x * 8, z * 8);
            let a = cold.column(pos);
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let _ = warm.column(ChunkPos::new(pos.x + dx, pos.z + dz));
            }
            checked += 1;
            if warm.column(pos) != a {
                differed.push(pos);
            }
        }
    }
    assert_eq!(
        checked, 900,
        "the area the probe covers is part of the claim"
    );
    assert!(
        differed.is_empty(),
        "{} of {checked} columns depend on the order their world was built in: {:?}",
        differed.len(),
        &differed[..differed.len().min(8)]
    );
}
