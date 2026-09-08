# D36 — How many threads build the world

**Status:** Built and measured, 2026-09-07. A generated join's last column
arrives at **1,069 ms against 2,699**, and four simultaneous joins finish at
**5,584 ms against 17,202** — the same 289 columns each, in one interleaved run
on an idle machine. The stream's own arithmetic floor is 740 ms and the
instrument — the same driver, the same pacing, a world with nothing left to
build — lands at 961, so **a single join is now within 108 ms of a world that
has no work in it at all**, and the 20 ms passes where the stream had a window
and nothing to send fell from 273 to 19.

It also found and fixed a **seed-determinism defect that predates the pool and
that the pool would have made routine**: a generated column's light depended on
the order its neighbours had been built in, on 15 of 900 columns.

[D31](0031-how-a-join-streams-its-chunks.md),
[D38](0038-how-wide-the-region-lock-is.md) and
[D42](0042-what-a-joining-crowd-costs-a-bystander.md) declined a pool three
times over. This is what reopened it, and the reason is the same in all three:
every one of them measured a player **standing still** while other people
joined. Nothing had ever timed the joining player's own wait for the ground.

## Why it was declined, and what changed

D31's own words, from "What was declined": a pool "would make a cold generated
join finish in about a quarter of the time. It also takes cores the tick loop
wants, and the number that matters — when the loading screen ends — is already
25 columns and not 289." Then, from its last section: "**A cold generated join
is still about 3.3 seconds of world arriving**, and almost all of it is one
thread evaluating noise."

Both of those are still true. What changed is that the second one stopped being
an aside. D31's last section also says "`STREAM_BATCH` is now a bound on the
region case only. On a generated world the store cannot build eight columns in
twenty milliseconds, so the stream is paced by the builder and the batch size
does nothing." That sentence is the whole case for this record: **the pacing the
stream was designed around was not the thing pacing it.** With four builders it
is again — the starved passes below say so directly.

The sentence in `net::source` that chose one thread said exactly what would make
it wrong, and then that thing happened. It reasoned about the ring ahead of a
walking player — nine columns, 34 ms, against the 1,600 ms
[D17](0017-how-fast-a-player-may-say-they-moved.md) gives a player to cross a
column — and ended "and a join, the one caller that wants 289 columns at once,
does not come through here at all." D31 then moved the join onto the store,
which is the right change and made the sentence false. Nobody re-sized
the thread. **A comment that names its own expiry condition is only as good as
whoever reads it next**, and the reader who invalidated it was the one change
that quoted it.

## What was measured

`benches/warming.rs`, new here. It runs `net::session::stream_inner`'s own
discipline — a 24-column window, `built_prefix` for back-pressure, eight columns
every 20 ms — against a real `ColumnStore`, and times the columns as they
arrive. Rows are **interleaved**: a round is one pass over every row, three
rounds, so a row that is slower than its neighbour is slower than it *was* and
not slower than the machine was an hour later. The number reported is the
fastest round, with the worst beside it.

Ten cores, idle, one process. `last column` is the slowest session of the round.

| | last column | first | starved passes |
| --- | --- | --- | --- |
| one joiner, resident (the instrument) | 961 ms | 0.1 ms | 0 |
| one joiner, **1 builder** | 2,699 ms | 189 ms | 273 |
| one joiner, 2 builders | 1,554 ms | 103 ms | 70 |
| one joiner, **4 builders** (the default) | **1,069 ms** | 80 ms | 19 |
| one joiner, 8 builders | 1,076 ms | 57 ms | 7 |
| four joiners, resident (the instrument) | 982 ms | 0.1 ms | 0 |
| four joiners, **1 builder** | 17,202 ms | 1,431 ms | 7,721 |
| four joiners, 2 builders | 9,408 ms | 835 ms | 3,553 |
| four joiners, **4 builders** | **5,584 ms** | 485 ms | 1,819 |
| four joiners, 8 builders | 4,351 ms | 470 ms | 1,260 |

**The single-joiner ladder stands at the origin, which is the cheapest of the
four centres**, and that is stated rather than hidden: the four fairness rows —
one joiner at four builders, at (0, 0), (64, 0), (128, 0) and (192, 0) — are
**1,115 / 1,769 / 2,303 / 2,011 ms**. The same join two thousand blocks away is
twice the work, because the terrain there is. 1,069 ms is the best case and the
row that says so is in the same run.

**The starved-pass count is the column that answers the question**, and it is
why the bench reports counts rather than a rate. A starved pass is a 20 ms tick
where the stream had a window, had room, and had nothing built to send. A single
join spends **273 of them on one builder and 19 on four**. That is the
difference between a player waiting for the world and a player waiting for the
protocol, stated without reference to any wall clock.

**The instrument earns its row.** Every row above uses the same driver threads,
the same view arithmetic, the same residency lock and the same 20 ms pacing; the
resident row is all of that with nothing to build. It lands at 961 ms against a
740 ms arithmetic floor, so about 220 ms of every row is the harness and the
residency rather than the world. A row is about the world only to the extent
that it is slower than 961. It also moves between runs — an earlier run of the
same code put it at 815 — which is the other thing it is for: a row cannot be
read as a world number without the instrument from *its own* run beside it.

## Why four and not eight

**For a single join the curve stops at four: 1,069 ms with four builders and
1,076 with eight.** Four is where a joining player stops waiting on the world
and starts waiting on `STREAM_BATCH` — 108 ms above the instrument — and past
that point another thread has nowhere to put its work.

**Eight builders are steadier, though, and that is not nothing.** Where the two
tie on the fastest round they separate on the worst: **1,081 ms against 1,449**
over the same three rounds, and 7 starved passes against 19. What eight buys on
a single join is not a faster join, it is a narrower spread — and this bench has
no tick loop and no other sessions in it, which is exactly the load that would
decide whether that spread is worth two more cores.

The row that unambiguously wants more is **four simultaneous joins**, where
eight builders are 1.28x four. That is also the row where the cores are least
available, because four joins are four sessions the tokio workers are already
serving.

So four is where the curve stops for the case the default has to be right for,
and a resource decision everywhere else. `default_builders` is half the
machine's parallelism capped at four: a ten-core box gives four builders and
keeps six cores for the tokio workers, the tick thread and the operating system,
and a two-core box gives one builder. If a soak with a world's worth of entities
ticking says otherwise, the constant moves.

**An earlier run of this same ladder said eight was 21% better than four for a
single join** — 903 ms against 1,143 — and that run's instrument row was 815 ms
where this one's is 961. The instrument moved 18% between two runs of a row that
has no world in it at all. **So nothing here may be compared across runs**,
including that pair; what is comparable is one interleaved pass over every row
in one process, which is what the table above is and what every conclusion in
this record rests on. A builder ladder is a measurement of the machine until it
is proved otherwise.

## The defect this uncovered, which is the transferable part

**A generated column was not a function of its position.** `GeneratedWorld`
keeps a cache of sky floors — 256 integers per column, the boundary condition a
neighbour needs to light across a chunk seam — and that cache had two writers
putting two different things under one key:

* `GeneratedWorld::column` remembered `SkyFloor::of` the **carved** column it
  had just built.
* `GeneratedWorld::sky_floor` computed `SkyFloor::of` the **terrain-only** build
  of a neighbour nobody had asked for as a column yet.

Those are not the same floor. A ravine that breaks the surface lowers where the
sky reaches by tens of blocks, and carving is exactly what the cheap build
skips. So which value a column's skirt got depended on whether its neighbour had
already been built as a full column — **which is the build order**. With one
builder that order was the queue's and reproducible run to run. With four it is
a race, and with a `spawn_blocking` join warming the same columns beside the
pool it was already a race before this record.

Two worlds on seed 1, one reaching each position cold and one reaching it with
its four neighbours already built as columns: **15 of 900 columns disagreed.**
`crates/dust-server/tests/worldgen_determinism.rs` is that comparison.

The fix is one deletion and it is a rule rather than a patch: **a memo may hold
one function's answers.** `sky_floor` is now the cache's only writer, so the key
has one meaning and no order can change it.

**The first version of that test found nothing.** It looked at sixteen columns
around the origin, where seed 1 puts ocean and a carver changes nothing a sky
floor can see. The area a probe covers is part of what it proves, and a probe
that returns zero has said something about its own range before it has said
anything about the code. This is the third time on this project that a check
answered a narrower question than it appeared to.

## What the fix costs, and the accuracy it does not buy

Not letting the carved floor into the cache means a neighbour's skirt is always
the uncarved one, so light crossing a seam next to a ravine is the light of a
neighbour without the ravine. **That is an approximation, and it is now the same
approximation everywhere instead of on whichever columns a build order reached
first.** It is also the approximation `GeneratedWorld::build`'s own
documentation already claimed to be making; the line that seeded the carved
floor contradicted the module it was in.

The accurate alternative — hand the skirt a fully carved neighbour — was costed
and declined here. 289 columns on one thread, at four centres, **all three
variants in one process** with a round being one pass over all twelve rows, so
the three are scored against the same machine and not against three different
minutes of it. Fastest of three rounds:

| | (0, 0) | (64, 0) | (128, 0) | (192, 0) |
| --- | --- | --- | --- | --- |
| the order-dependent code this replaces | 2,787 ms | 4,186 ms | 6,286 ms | 5,736 ms |
| **one writer, uncarved skirt** | 3,018 ms | 4,512 ms | 6,495 ms | 5,393 ms |
| carved skirt | 4,650 ms | 9,585 ms | 11,993 ms | 8,957 ms |

Determinism costs between **-6% and +8%** — it is not free in either direction,
because dropping the carved floor removes a memo *and* removes the deeper sky
reach that made lighting the next column dearer. Accuracy costs **1.5x to
2.1x**, on the one thread that is the whole of what a joining player waits for.
The second is a light question and not a threading one, and it is handed to
whoever points the light oracle at generated terrain — `harness light` has only
ever been run against worlds Minecraft wrote, so nobody yet knows whether the
carved skirt is what Minecraft does either.

**A single-threaded ladder is the right instrument for this one**, and that is
not laziness: the question is what a column costs to build, and putting four
builders on it would let the pool's speedup hide the change under test. An
earlier cut ran the three variants as three separate processes and reported that
determinism was *free and slightly faster*; run in one process against one
machine it is up to 8% dearer. A control run is only a control if the machine
was in the same state.

## What is in the store, and the three rules that make it safe

**The queue holds columns, not requests.** The channel this replaced handed one
caller's whole `Vec` to whichever thread called `recv` first, which is one
thread for as long as there is one caller with work — and a join asks for 289
columns in a single send. Splitting at the column is the whole of what lets a
second builder help with the first builder's join. With the old shape, four
builders finished a join in exactly the time one did.

**A column stays claimed from the moment it is enqueued until it has been
offered back.** Not just while it is queued: the in-flight half is what stops
two builders building the same column, and it is a case that could not arise
with one builder — `Residency::fill_many` merely threw the loser away. Two
players standing beside each other produce it on every stream pass.

**The batch is given up before a builder parks.** `FILL_BATCH` is eight, which
is `STREAM_BATCH`, and a builder offers what it has whenever it reaches the end
of the queue. Three of four builders reach that end holding a partial batch, and
a builder waiting for an eighth column that is not coming is a player looking at
seven columns of hole for as long as they stand still.

## What was measured, and how

```text
cargo xtask extract --version 1.21.1 --only worldgen,constants
cp <cache>/oracle-1.21.1/dust-biomes.tsv <cache>/data-1.21.1/data/
DUST_BENCH_DATA=<cache>/data-1.21.1/data \
  DUST_BENCH_CONSTANTS=<cache>/oracle-1.21.1/constants.tsv \
  cargo bench -p dust-server --bench warming
```

It takes about twenty-five minutes, and most of that is not measurement: **a
world is built per row per round**, because `GeneratedWorld`'s floor cache holds
4,096 columns and a join touches 289, so three rounds over one world would find
the third round's floors already computed — and the reported number is the
*fastest* round, which would be the one somebody else's round had warmed. The
residency behaves the same way. A row that reused its world would report a warm
generator and call it a builder count.

The determinism test is `#[ignore]`d and needs the same two variables:

```text
cargo test --release -p dust-server --test worldgen_determinism -- --ignored
```

Its store-side half needs no world at all and runs in CI:
`net::source::tests::the_same_columns_come_out_however_many_builders_went_in`
puts a join's 289 columns through the pool at one, two, four and eight builders
and compares what came out against arithmetic rather than against another run —
a run-to-run comparison would agree with itself if every builder count were
wrong the same way. Four mutations were run against it and each is caught by a
different assertion; the test says which.

## What was declined

**Raising the cap above four.** See "Why four and not eight": what is missing is
a measurement under tick load, not an argument.

**Letting a session build a column the pool has not reached yet.** D31 declined
this and the reason survives the pool: with builders idle the session would win
that race and the store would build everything twice. The floor that is kept is
narrower and exact — a world with no builders at all builds on the caller, as
every caller did before D25.

**Handing the skirt a carved neighbour.** Costed above at 1.5x to 1.9x a join.
It is a light-accuracy change and belongs with the light oracle.

**Making the builder count a setting.** Nobody operating a server can answer
"how many threads should build your world", and a setting is a question dressed
as a knob.

## What this does not change

Nothing about what any check decides, and nothing about which blocks a seed
produces — that is the point of the determinism half. `built_prefix` still
counts a prefix, so a client still gets its columns nearest-first with no holes.
A flat world still builds nothing and still answers `columns.len()`.

## What is still wrong

**A single join is 1,069 ms against a 961 ms instrument, and that instrument is
961 against an arithmetic floor of 740.** The 108 ms is the world; the 221 ms
between the instrument and the arithmetic is the harness, the residency lock and
the scheduler, and nothing here says which. That is the next measurement, and it
is a measurement about the *stream* rather than about the builders.

**Four simultaneous joins are 5,584 ms against a 982 ms instrument.** Four
joiners want 1,600 columns a second and four builders on this machine supply
about 250. This is the row the pool helps most — 3.1x — and the row still
furthest from its floor.

**The single-joiner ladder is measured at the cheapest of four centres.** The
same join at (128, 0) is 2,303 ms with four builders. Nothing here is wrong
about it, but "a join is 1,069 ms" is a sentence about the origin on seed 1.

**A neighbour's ravine is invisible to the light across the seam**, uniformly
and by decision. Whether Minecraft agrees is unmeasured: `harness light` has
never been pointed at generated terrain.

**Each builder fills its own batch of eight, so the prefix advances in lumps.**
`built_prefix` counts from the front of the view and a builder offers after
eight columns of its own — which are columns 0, N, 2N... of the queue with N
builders — so the front column reaches the residency only when its builder's
eighth does, or when the queue runs dry and the partial batch is given up. The
first column arrives at 189 ms with one builder, 80 with four and 57 with eight,
all far under the floor, so this costs nothing a player waits on today. Whether
offering the moment the queue's *front* is complete would close any of the
remaining 108 ms is unmeasured.

**The bench has no tick loop in it.** Every number here is a world with nobody
living in it, which is the wrong world for the one question the cap of four
exists to answer.
