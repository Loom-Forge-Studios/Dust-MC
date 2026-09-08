# D43 — What a chunk's ore is drawn against

**Status:** Built, measured, wired to the socket, and the knob D6 designed is
turning it. Stage five of decision record
[0012](0012-what-worldgen-is-worth-measured-first.md) has started: a player who
digs into Dust's terrain now finds coal, iron, copper, gold, redstone, lapis,
diamond and emerald where Minecraft put them. **Block disagreement falls from
1,838,763 cells to 304,789 on seed 0 and from 1,809,196 to 240,508 on seed 1**,
and a generated column costs 4.2 ms more than it did.

## Context

D39 cut the caves and left nothing in them. That is not a small gap dressed up
as a large one: a cave with nothing to mine is a corridor, and the whole reason
a player goes underground is missing. It is also, by D12's own count, the
*smallest* remaining stage — 360 cells inland and none over ocean.

D12 said in the same breath that the count understated it more than any other
row it printed, and it was right by a factor of four thousand. The reason is
that `minecraft:ore` does not only place ore. It places **tuff, andesite,
diorite and granite**, and at the carver rung those four were the top four
entries in "Minecraft has where Dust is wrong" on both seeds:

```text
                     seed 0     seed 1
  minecraft:tuff      323,799    336,928
  minecraft:andesite  315,029    331,027
  minecraft:diorite   307,063    301,014
  minecraft:granite   291,544    297,014
```

So the stage a player needs first and the stage that closes the largest single
block gap are the same stage, and it is one configured-feature type.

## What runs, and what is counted instead

One type: `minecraft:ore`. That is **30 of the 151 placed features the
overworld's biomes name**, and it is the whole of the underground-ores
decoration step. Every other type is read, indexed, ordered and then **skipped
by name with a count**, so the next stage starts from a list rather than a
survey:

```text
  121 placed feature(s) this generator does not run, by kind:
  minecraft:random_patch 35, minecraft:random_selector 19,
  minecraft:seagrass 8, minecraft:flower 7, minecraft:disk 5,
  minecraft:tree 5, and 26 more kind(s)
```

**Skipping is free of consequence for the features that do run**, and that is
not luck. `setFeatureSeed` re-seeds the stream *per feature* from the chunk's
decoration seed and the feature's own global index, so a feature that is not run
consumes nothing another feature would have drawn. What a skipped feature costs
is its blocks, which is what the count is for — and what is left in the
histogram after this record is leaves.

## The algorithm is code, and code is reachable

`worldgen/placed_feature/*.json` says where a feature lands,
`worldgen/configured_feature/*.json` says what it builds, each
`worldgen/biome/*.json` says which ones it runs, and the block tags say what an
ore may replace. All four are the operator's own data. Everything *done* with
them is `ChunkGenerator.applyBiomeDecoration`, `FeatureSorter`, four placement
modifiers, two height providers and `OreFeature` itself — read the way D8 and
D39 read theirs, `javap -p -c` on the inner server jar in the operator's own
`.dust-extract`, through the ProGuard mappings Mojang publishes beside it.
**Nothing Mojang's is committed.**

Four things a careful guess gets wrong, and each of them produces a world:

* **The stream is neither of the two generators this crate already had.**
  `applyBiomeDecoration` builds `WorldgenRandom` over an
  `XoroshiroRandomSource`, and `WorldgenRandom` overrides only `next(bits)` and
  `setSeed` — so every draw is `java.util.Random`'s own arithmetic, `nextInt`'s
  rejection loop and a **two-draw** `nextDouble`, reading xoroshiro's top bits.
  The carvers' stream is the legacy LCG and the density functions' is bare
  xoroshiro; this is a third thing made of both. Fifteen checks were written to
  fail before it existed, and the one that matters requires all three to
  *disagree*.
* **`OreFeature` uses a real sine and a table sine, in one feature.** The angle
  the vein is drawn along comes from `java.lang.Math.sin` on a double; the
  radius of each of its nodes comes from `Mth.sin`, the 65,536-entry lookup
  table D39 already found. Using either for both puts every vein somewhere
  else, and every test that only asks whether ore exists still passes.
* **The sort is a topological order, not a sort.** `FeatureSorter` numbers
  features by first appearance scanning biomes in the biome source's own order,
  then runs a reverse-post-order depth-first sort whose tie-break is that
  number. The position in the result is what `setFeatureSeed` takes, so a step
  whose order is wrong puts every feature in it on a different stream. Sorting
  by the pair instead is a total order, and any two topological orders disagree
  about some pair.
* **A feature writes into the eight chunks around its own.** The FEATURES step
  declares a block-state write radius of one, so a chunk holds its own features
  *and* whatever the eight around it spilled in — a size-64 vein reaches
  thirteen blocks. This runs all nine origins and keeps the writes that land in
  the middle one. The alternative is veins sliced flat at every chunk boundary,
  which no test would fail and every player would see.

## What it scores

`cargo xtask harness worldgen --version 1.21.1 --seed <n> --radius 2` over
D21's twelve scattered 5x5 squares: 300 chunks, 76,800 columns, 29,491,200
cells. Every figure is a count of things **wrong**.

```text
seed 0 — 17 biomes in view

  surface  surface     biome    caves      false      blocks   KiB
    short    block     short  missing      caves       short  /col
    76800    76800    435459        0    9598921    10005374   2.2  the flat world Dust served
    74905    74931    435459   583625     795317    10475058  16.2  + the world's own sea level
    74905    74931       382   583625     795317    10475058  16.6  + Dust's biome source
    28796    49128       382   588215      75560     6840919  18.8  + Dust's terrain
    28796    32398       382   588215      75560     2529567  18.8  + Dust's surface rules
    28784    32393       382   187614      90892     2097950  18.8  + Dust's aquifers
    28349    32345       382    18433      92418     1838763  18.8  + Dust's carvers
    28349    32106       382    18433      92418      304789  19.1  + Dust's features        <- this record
        0    60037         0   681715          0    10405644  19.6  + Minecraft's surface height
        0    60037         0        0          0     9723929  19.6  + Minecraft's carvers
        0        0         0        0          0       12140  20.6  + its blocks at and below it
        0        0         0        0          0           0  20.7  + its blocks above it (control)

seed 1 — 20 biomes in view

    76800    76800    449472        0    9610683    10028458   2.2  the flat world Dust served
    72552    74345    449472   527846     746183    10434876  16.2  + the world's own sea level
    72552    74345       238   527846     746183    10434876  16.6  + Dust's biome source
    16678    54815       238   541571      33148     6807705  19.0  + Dust's terrain
    16678    23025       238   541571      33148     2376862  19.0  + Dust's surface rules
    16629    22969       238   221438      42614     2045421  19.0  + Dust's aquifers
    16064    22943       238    30431      46671     1809196  19.0  + Dust's carvers
    16064    22915       238    30431      46671      240508  19.4  + Dust's features        <- this record
        0    47083         0   662830          0    10402908  19.4  + Minecraft's surface height
        0    47083         0        0          0     9740078  19.4  + Minecraft's carvers
        0        0         0        0          0       10180  20.4  + its blocks at and below it
        0        0         0        0          0           0  20.5  + its blocks above it (control)
```

**Blocks: 1,838,763 -> 304,789 and 1,809,196 -> 240,508.** As a rate, 93.765%
of cells agreed and now 98.967% do on seed 0; 93.865% and now 99.184% on seed 1.
One stage closed 83.4% and 86.7% of what was left.

**And the ore is the smaller half of that, which is the finding.** Tuff,
andesite, diorite and granite were 1,237,435 cells of seed 0's 1,838,763 and are
gone from the top of the histogram; what is left there is leaves, packed ice and
sand. `[worldgen.ores]` calls the four of them ore groups too, and D6 already
said why: the group is derived from the blocks a placement puts down, not from
anybody's idea of what counts as an ore.

The two rows above and below this one are **unchanged to the cell**, which is
what a ladder is for: the carver row still reads 18,433 / 92,418 / 1,838,763 and
the control still reads zero everywhere.

**Two seeds, because one cannot see.** Seed 1 spawns in open ocean and disagrees
about nearly every other figure here, and it is the one that shows what is still
wrong: `minecraft:coal_ore where Minecraft has minecraft:stone 15,144` sits
directly beside `minecraft:stone where Minecraft has minecraft:coal_ore 13,707`.
Those are not missing veins. They are the same veins, one cell over. The next
heading is why.

## What is still wrong, and it is all at the chunk wall

A vein drawn from a neighbouring origin is drawn against **this** chunk's
blocks and nothing else, because this chunk's blocks are the only ones built.
Two consequences, and they are different:

* **`Feature.isAdjacentToAir` is answered "not air" for a cell outside the
  chunk.** Only the buried ores ask, and only at a chunk's four walls. The
  harness counts every time it happens.
* **The air-exposure draw is not taken for a cell outside the chunk**, because
  the cell is skipped before the block under it is read. Five of the thirty
  placements have a discard chance strictly between 0 and 1 — `ore_coal_buried`
  and `ore_gold_buried` at 0.5, `ore_diamond_small` and `ore_diamond_medium` at
  0.5, `ore_diamond_large` at 0.7 — and for those a vein that straddles a
  boundary leaves the stream one draw short for every later attempt of the same
  feature in the same origin chunk. That is the 15,144 against the 13,707: coal,
  which is the only buried ore common enough to show at the top of a histogram.

**Fixing it means building the blocks of all nine chunks and throwing eight of
them away, which is nine times the terrain for the stage that is already the
most expensive thing a chunk does.** Declined, with the number attached, exactly
as D10 declined a 5x5 light volume. What it would buy is bounded above by
29,000 cells on seed 1 out of 240,508 — a twelfth of what is left, for 9x the
cost of the stage. If it is ever worth doing it is worth doing as a chunk cache
shared between neighbours, which is a different design and a later record.

## What it costs, which for worldgen is not a footnote

This runs for every chunk a player walks toward, forever.

**A cost measured under varying load is not a cost** — D39's finding, and the
ladder's own `cols/s` column tripped over it again while this was being built.
So the cost here is the join bench, which is the number a player feels: 289
columns at the default view distance, built nearest-first in the order `View`
hands the stream, five paired runs, and the fastest round of each taken.

```text
  289 columns, generated world, build only        per column   worst column
  Dust's carvers, no features            2,148 ms      7.4 ms        10.3 ms
  + Dust's features                      3,364 ms     11.6 ms        15.7 ms
```

**The feature stage costs 4.2 ms a column, +57%.** A join's build goes from
2.1 s to 3.4 s, and D31 already moved that off the session task — with the
columns resident the session pays 16 ms to encode all 289.

### The first number was 128 ms a column, and the cache is why

A vein whose origin is two chunks away still reaches in, so before the stage can
run at all it needs `OCEAN_FLOOR_WG` over the **5x5** window around the chunk —
not 3x3, because `OreFeature` asks about a box that reaches thirteen blocks past
its own origin, and an origin in the far corner of the ring asks twenty-six
blocks beyond that. The only way to answer for a chunk is to build its terrain.

The generator's own scratch keeps five rows of 64 chunks, direct-mapped on
`(z mod 5, x mod 64)`. That is exactly right for a scan — and **a join is not a
scan**. It is a nearest-first spiral out from the player, which thrashes a
direct-mapped cache, so every column served paid twenty-five terrain fills
instead of one:

```text
  289 columns, generated world, build only        per column   worst column
  window heights from the scratch's own cache   36,921 ms    127.8 ms   164.9 ms
  window heights from a map the world keeps      3,364 ms     11.6 ms    15.7 ms
```

**Eleven times.** 512 bytes a chunk, kept beside the sky floors the same world
already keeps for the same reason, and capped and cleared the same way. The
lesson is not "add a cache": it is that **a cache is right for an access order,
and the order the harness uses is not the order a player creates.** The ladder
ran the whole time at 4.2 carves a chunk because it scans; the server ran at 25
because it spirals; and only the join bench asks the question the player asks.

### The write loop skips a vein that cannot reach this chunk, and that is exact

Eight of every nine origins are neighbours, and most of their veins land nowhere
near the chunk being built. The write loop is now skipped outright when a vein's
bounding box misses this chunk's sixteen columns.

**This is not an approximation, and the reason is worth stating**: the only draw
inside that loop is the air-exposure chance, and it is reached only through the
cell index — which already answers `None` for every cell outside this chunk. A
vein that cannot reach draws nothing whether the loop runs or not. The proof is
the ladder: 304,789 and 240,508, and every other figure on both seeds, are
unchanged to the cell with the skip in place.

## D6's knob is turning something now

D6 designed `[worldgen.ores]` in August and said plainly what it could not yet
do: *"the generator that consumes it does not exist"*. It does. What changed
here:

* **The group an operator names is derived from the blocks a placement puts
  down, by one implementation.** `dust_gen::ore_density::group` is the rule, and
  both `cargo xtask extract` and the generator call it. Two implementations of a
  naming rule are two chances for `[worldgen.ores.overrides.diamond]` to name
  nothing, and the failure mode is a server that started and a setting that did
  nothing. Vanilla's overworld yields sixteen groups — `andesite, clay, coal,
  copper, diamond, diorite, dirt, emerald, gold, granite, gravel, infested,
  iron, lapis, redstone, tuff` — which is the vocabulary D6 promised, plus the
  eleven D6 said the data would add.
* **An unknown name is refused at boot**, against the groups this world has,
  naming the nearest match. That is the check D6 said could not happen until a
  world was loaded.
* **The identity property is a fact about the chain, not about arithmetic.** A
  group whose settings change nothing keeps the `Vec<Modifier>` the pack was
  read into — the code that ran before this setting existed, not a rewrite that
  ought to come out the same. `enabled = false` is proved identity separately
  from the defaults, because "the defaults are identity" and "the master switch
  is identity" are two claims and the parity run uses the second one.
* **Both of vanilla's ways of writing a frequency collapse into one modifier.**
  `Modifier::Attempts { per_chunk, extra }` is "n attempts and perhaps one
  more", which is one modifier rather than two nested ones because "three and
  perhaps a fourth" is not "three, each of which perhaps happens". A chance of
  zero must not *draw* zero: `next_f32() < 0.0` is always false and still moves
  the stream, so an integer multiplier of an integer count draws exactly what
  the pack's own `count` drew.
* **A `count` written as a uniform provider over anything but `0..=1` is left
  alone and said out loud.** `0..=1` crosses over exactly — it is what
  `RarityFilter { one_in: 2 }` already means, and vanilla's `ore_gold_lower` is
  written that way — and any other range has nowhere to land without throwing
  away either its mean or its spread. `cargo xtask extract` refuses the same
  shape for the same reason. Obeying it by guessing and ignoring it in silence
  are both worse than reporting it.
* **A switched-off ore draws nothing at all** — it is not run with a count of
  zero. Free of consequence for its neighbours, for the `setFeatureSeed` reason
  above.

Nothing in D6 is re-litigated here. The multiplier is still a multiplier, the
knob is still keyed by ore group, the baseline is still whatever the loaded
world has, and the vanilla numbers still arrive from the operator's own jar.

## What was costed and declined

* **Building the eight neighbours' blocks**, so that a straddling vein reads
  real blocks and takes its air-exposure draw. 9x the terrain for a bound of one
  twelfth of what is left. Above.
* **Any feature type but `minecraft:ore`.** `random_patch` (35), `tree` (5) and
  `random_selector` (19) are the next three by count and are three different
  algorithms; the machinery under them — the sorter, the seeds, the modifier
  chain, the nine origins, the palette — is written once and is what this record
  is mostly about. What is left in the histogram after this stage says which to
  do next: leaves.
* **Widening the height window past 5x5.** `OreFeature`'s reach is bounded by
  the largest vein a pack contains and the pack is read, so the window is
  derived rather than chosen. A pack with a bigger vein would need a wider one
  and would say so.
* **Applying `[worldgen.ores]` inside `harness worldgen`.** The verb is scored
  against Minecraft, and Minecraft is the identity setting. The groups are
  printed so an operator can see the names; the numbers are not scaled.

## Consequences

- `cargo xtask harness worldgen` has a twelfth rung and it is the last one a
  server could run in. Everything below it reads the region file.
- **The generated world's block palette is one list**, the surface rules' and
  the features' together. Two lists would map an ore's material code onto
  whichever surface block sat at that index, which is a bug that would look like
  a worldgen bug.
- The harness prints **both sides** of a block the two worlds disagree about —
  "Minecraft has where Dust is wrong" and "Dust has, where they disagree". One
  list cannot say whether a block is missing because it was never placed or
  because it was placed one cell over, and those are different stages. It is
  what found the 15,144 against the 13,707 above.
- A generated column now costs 11.6 ms and holds 19.1 KiB of blocks. Light is
  still 96 KiB more, unconditional, and still the largest single cost of holding
  a column.
- **The ladder's `cols/s` column is not a cost measurement and should stop being
  read as one.** It is wall time on whatever machine ran it, and this record was
  built on a machine running four other things. The join bench is where the cost
  of a stage goes.

## Related

* D6 — the ore knob this record wires up, and the reason its default path is an
  early return rather than arithmetic.
* D12 — the ladder, the five scores, and the order of the work this is stage
  five of.
* D39 — the carvers this fills, the `javap` route to an algorithm that is code
  rather than data, and the rule that a cost measured under varying load is not
  a cost.
* D10 — the shape of every finding here: a stand-in can only expose the defects
  its own range reaches, and a differential cannot catch a rule that is wrong on
  both sides.
* D31 — why 3.4 s of build on a join is a number about a background task rather
  than about a stalled session.
