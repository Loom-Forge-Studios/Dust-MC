# D31 — How a join streams its chunks

**Status:** Built and measured, 2026-09-03. A join sends the same 289 columns in
the same order as before, and **the session's own task no longer builds any of
them**. On a generated world — which is what a server with no `world_source`
serves since [D26](0026-the-terrain-dust-serves-and-what-it-does-at-the-seam.md)
— that removed **2,450 ms of blocking work from a tokio worker**, and the
players already standing in the world felt it: with four bots joining at once, a
settled player's chat round trip went from a worst case of **469, 298 and 697 ms
over three runs to 20, 71 and 20 ms**, and from a third of its pings never
coming back inside the window to all of them.

**One number in this record has since been retracted** — the settled player's
round trip, here and everywhere below it, was a measurement of the test harness
rather than of this server. See
[D42](0042-what-a-joining-crowd-costs-a-bystander.md). What the join does to a
tokio worker stands; what it does to a bystander does not.

It is **not uniformly better**: on a world read from region files, where a
column was never expensive enough to be worth moving, four simultaneous joins
now have a fatter tail. The last section says what that is and what is known
about it.

[D25](0025-who-keeps-a-chunk-column.md) named this as the next measurement in
its own last section. It is the third caller of the store D25 built, and
deliberately not a third builder of columns.

## Which world a number is about

Since D26 there are three, and one number for "a column" hides which. Measured
by `benches/join.rs`, which runs a ladder over the same 289 columns on each —
each row the one above it plus a single named change, because a total cannot say
which input owns which part of it:

| | build only, cold | build and encode, warm | encode only, resident |
| --- | --- | --- | --- |
| **flat** | 0.0 ms | 8.0 ms | (keeps nothing) |
| **generated** | 2,450.0 ms | 1,658.7 ms | **14.0 ms** |
| **region files** | 452.5 ms | 249.5 ms | **14.9 ms** |

Two things fall out of the ladder and neither is visible in a single figure.

**Encoding a column is about 50 us and building one is 8.5 ms generated, 1.6 ms
out of a region file.** The build is 99.4% of the stream on a generated world
and 96.7% of it on a region one. So moving the *build* off the session task is
the whole of the change worth making, and moving the encode is not worth making:
15 ms spread over 289 columns is not something a player can feel, and a scheme
that also moved it would have to move the socket with it.

The region row is one saved world's columns — the one `tools/bot` runs against —
and a different save reads differently: the same ladder over a sparser world
answered 104.0 ms where this one answers 452.5. That is why the row is stated
with the world it is about rather than as "a region column".

**The world that needed D25's residency sixteen times more than the other was
the only one without it.** `Source::Generated` had no store at all; every
column, for a movement check as much as for the stream, was built on whatever
thread asked. That was correct when a generated world was a curiosity behind a
flag and wrong the day it became the default.

## What is built and by whom

**One `ColumnStore` per world, and both real worlds have one.** The residency,
the channel and the warming thread came out of `AnvilWorld` and now sit behind a
`Columns` trait with two implementors. That is the right seam: the rules for who
may build a column belong to the *threads*, not to where the blocks come from.
A session runs on a tokio worker and the item loop runs on the engine's own
`std` thread, and neither may block, whether the block came off a disk or out of
a noise function.

**One thread per store, not a pool.** What that thread serves is the ring ahead
of a walking player — nine columns, 76 ms of generated terrain against the
1,600 ms [D17](0017-how-fast-a-player-may-say-they-moved.md)'s speed limit gives
a player to cross the column they are standing in. A margin of 21 to one does
not need a second thread, and the one caller that wants 289 columns at once does
not come through here. The last section reopens this.

**A builder offers eight columns at a time, not one.** The residency's write
lock is what every movement check on every other session is waiting to read, and
a builder that takes it per column takes it about a thousand times a second on a
region-file world. The columns are still built with nothing held; only the
handover is batched. The cost is that the first column of a batch waits for the
eighth, which on a generated world is 46 ms against a streaming tick that was
never going to keep up with the builder anyway.

**The join's first twenty-five are built with `tokio::task::spawn_blocking`.**
This is the one place in this server where that door is the right one, and the
reason is the same reason D25 gave for the warming thread belonging to the
world: a session is on tokio and the item loop is not, so `spawn_blocking` is
available to one caller in two. A join is that caller. It has no movement packet
held up behind it, it wants the ground under the player's feet before the
loading screen ends, and blocking its worker for 95 ms of noise blocks every
other session that worker is running.

## The three rules the stream keeps, which only work together

**Nearest first**, which `View` already did. A client renders what it has, and
the column underfoot arriving before the far corner is the difference between
walking forward and waiting.

**A claim on a bounded window ahead of the send point.** `View::peek` names the
batch and sixteen more *without recording them* — a peek that marked columns
loaded would credit a client with a world it was never sent — and one
`ColumnClaim` per session holds them in the store and asks its thread for them.
Held, so a column built for one session is kept: four bots joining at the same
place now arrive **within 20 ms of each other** where they used to be staggered
across 400, because the store builds their columns once between them.

**A prefix, never a hole.** `Source::built_prefix` counts how many of the window
are ready, *counted from the front*, and the pass sends exactly that many. That
is the ordering guarantee and the back-pressure at the same time: the window
advances only over columns that have gone out, so a client that cannot keep up,
or a player who outruns the store, asks for nothing new. There is no queue to
grow.

A flat world and a world whose warming thread would not start both answer
`columns.len()`, and neither is a shortcut: one has nothing to wait for and the
other has nobody to wait *on*. A stream pacing itself against a thread that does
not exist is a hole in the world forever.

## What it costs

**24 columns per streaming session, 2.7 MB** at the 111 KB a column of a real
world measures — the batch of eight plus sixteen of runway, each released as it
goes out. D25 declined a claim the size of the view for this exact reason: 289
columns is 32 MB a player, and a stream does not need to hold what it has
already sent.

A session whose view is full is charged nothing at all: `View::complete` answers
in two integers, where the old tick built the set of every column in range and
differenced it to say "none" — fifty times a second, for the life of every
session.

The total work is unchanged. The same columns are built once, in the same order,
and finish at about the same moment. What changed is which thread pays and who
waits behind it.

## The defect this uncovered, which is the transferable part

Giving a column back **scanned the whole resident map** to ask how many columns
nobody holds, under the write lock. That was affordable for its two original
callers: a player crossing a chunk boundary, and a heap of items despawning.
The chunk stream releases a column for every column it sends — fifty times a
second per session — and on a region-file world, where a column is cheap enough
that four joins move two thousand of them a second, it starved every reader. A
settled player's worst chat round trip was **758 ms**, worse than before the
change.

`Kept` now carries the count and every path that moves a holder across zero
maintains it, so the test is a comparison; the `retain` is still a walk and
still happens, once every sixty-four retirements rather than on every release.

**The lesson is not about locks.** An O(n) step is a policy decision about how
often it is allowed to run, and that decision lives in the *callers*, not in the
code that does it. This one was correct for the whole life of the module and
became wrong the moment a third caller arrived — with nothing in the module
changing, and nothing to see at the call site.

## What was measured, and how

Two instruments, because they answer different questions.

`benches/join.rs` is the ladder above: what a column costs and where the cost
is. `cargo bench -p dust-server --bench join` with `DUST_BENCH_CONSTANTS`,
`DUST_BENCH_DATA` and `DUST_BENCH_REGION` set.

The player-facing number is four bots joining at once while a settled player
times a chat round trip twenty times a second across a **fixed three-second
window**. The window has to be fixed: a first version measured until the last
joiner had its columns, which scored a faster build on fewer samples — a
measurement whose sample size is the variable it is measuring.

**The two builds were run interleaved, alternating on the same machine**, and
that is not fastidiousness. Three other builds were running throughout; a
sequential A then B would have attributed the machine's mood to the change.
Worst round trip, and how many of the ~150 pings completed:

|  | before | after |
| --- | --- | --- |
| **generated, four joining** | 469 / 298 / 697 ms (67, 93, 56 samples) | **20 / 71 / 20 ms** (155, 153, 153) |
| **region files, one joining** | 65 / 54 / 35 ms (140, 140, 142) | **9 / 11 / 11 ms** (144, 144, 143) |
| **region files, four joining** | 48–403 ms over 19 runs, median 146 | 11–828 ms over 19 runs, median 131 |

**How much of the world arrived in those three seconds** is the other half of
the generated row: the four joiners were sent **181, 253 and 189 of their 289
columns before, and 273, 279 and 275 after** — and they used to finish
staggered across 400 ms of each other and now finish within 20, because the
store builds their columns once between them instead of four times.

## What was declined

**A pool of warming threads.** It would make a cold generated join finish in
about a quarter of the time. It also takes cores the tick loop wants, and the
number that matters — when the loading screen ends — is already 25 columns and
not 289. Declined on priority 2 with priority 1 satisfied, and the last section
says what would reopen it.

**Built by [D36](0036-how-many-threads-build-the-world.md), 2026-09-07**, and
what reopened it is in this record's own last section: "`STREAM_BATCH` is now a
bound on the region case only. On a generated world the store cannot build eight
columns in twenty milliseconds, so the stream is paced by the builder and the
batch size does nothing." With four builders it is again. A single generated
join's last column arrives at 1,069 ms against 2,699 — 108 ms off a world with
nothing left to build — and four simultaneous joins at 5,584 against 17,202. The
estimate above was optimistic by a little and right in shape: four builders take
a single join to 2.5x and to the end of its curve, and it is *four simultaneous*
joins that would still take more threads.

**Moving the encode off the session task too.** 5.8 ms across 289 columns, and
it would put the socket write behind another hop.

**Holding the whole view.** D25's number stands: 32 MB a player.

**Letting the session build a column the store has not got to yet.** It is the
obvious floor and it is the wrong one here: with a single builder the session
would win that race on nearly every column and the store would build everything
twice. The floor that is kept is narrower and exact — *a world with no warming
thread* builds on the caller, as every caller did before D25.

## What this does not change

Nothing about what any check decides. `tools/bot/collide.js` is **10 of 10 on a
superflat, 10 of 10 on generated terrain and 10 of 10 on a world Minecraft
wrote**, five runs each on both builds.

## What is still wrong

**Four simultaneous joins on a region-file world have a fatter tail than they
did.** Nineteen interleaved runs either side: the median worst round trip is
unchanged at 131 ms against 146, and the maximum went from 403 ms to 828. It is
**not** the join path — with *one* joiner the same measurement is 9–11 ms after
against 35–65 before, five times better and never worse. It is four callers
meeting at one store: one warming thread, one region-file mutex and one
residency write lock, where before the four built their own columns on four
tokio workers and contended only on the file.

That is the case a small pool would answer, and it is the reason the pool above
is declined rather than rejected: it is not wanted for the generated world,
where the loading screen already ends at 242 ms, but it is the shape of the one
regression this record ships. **What must be measured before building it is
whether the tail is the thread or the lock**, because a pool makes the second
worse.

**Answered by [D38](0038-how-wide-the-region-lock-is.md), 2026-09-03.** It was
the lock, and the lock was six times wider than it needed to be: sixty per cent
of a region column was spent holding the region mutex, which capped four
threads at 1.49x however many of them there were. Narrowing it to the seek
alone brings the settled player's worst chat round trip on a region-file world
to a median of 194 ms over twelve interleaved runs, against 390 before — and
against **251 ms on a flat world, which has no columns to build, no lock and no
warming thread**. Most of what this section called a regression is a floor that
four simultaneous joins impose on any world at all, and the pool is declined a
second time because the count of stalls did not change when the build got 2.2x
faster; only their length did.

**Retracted by [D42](0042-what-a-joining-crowd-costs-a-bystander.md),
2026-09-03.** There is no regression and no floor. Every bot in that harness
lived in one node process, and a joiner's 289 chunk packets are parsed by
prismarine on the node thread that receives them — so the settled player's
round trip was timed by an event loop four joins had just filled. Given a
process of its own the settler's worst round trip is **7 ms** on this world, and
7 ms on the build before D38 as well. The numbers this section reports are the
harness's, and so are the ones above about a settled player.

**A cold generated join is still about 3.3 seconds of world arriving** at the
terrain [D32](0032-what-the-ground-is-made-of.md) now generates, and almost all
of it is one thread evaluating noise. The loading screen ends at 242 ms and the
player can walk, so this is terrain filling in around somebody who is already
playing rather than a wait.

**`STREAM_BATCH` is now a bound on the region case only.** On a generated world
the store cannot build eight columns in twenty milliseconds, so the stream is
paced by the builder and the batch size does nothing.
