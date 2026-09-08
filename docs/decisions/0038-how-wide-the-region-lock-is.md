# D38 — How wide the region lock is

**Status:** Measured and narrowed, 2026-09-03.
[D31](0031-how-a-join-streams-its-chunks.md) shipped one regression and named
this as the thing to measure before fixing it — four simultaneous joins on a
world read from region files had a fatter tail than they did, and the two
candidate causes wanted opposite fixes. It is **the lock**, and the lock was
six times wider than it needed to be: `AnvilCore::read` held the region mutex
across the seek, the decompress, the NBT parse *and* the chunk assembly, where
only the seek needs it.

Narrowing it takes four threads reading the world from **1,594 to 3,566 columns
a second** and their worst column from 6.96 ms to 2.85 ms. For the player it
takes the settled neighbour's worst chat round trip, across twelve interleaved
runs with four bots joining at once, from a **median of 390 ms to 194 ms**, and
the number of those runs in which somebody waited more than 300 ms for a chat
line from **seven of twelve to three of twelve**.

The measurement also overturned half of the question it was asked, and that is
the more useful half. See "The floor nobody had measured".

**Half of this record has since been retracted by
[D42](0042-what-a-joining-crowd-costs-a-bystander.md), 2026-09-03**, and it is
the player-facing half. Every bot in the harness lived in one node process, and
a joiner's 289 chunk packets are parsed by prismarine on the node thread that
receives them — so the settled player's chat round trip was timed by an event
loop four joins had just filled. Measured from a process of its own, the
settled player's worst round trip is **7 ms on this build and 7 ms on the build
before it**, and there is no floor. What survives is everything this record
measured about the server itself: the lock was six times wider than it needed
to be, and narrowing it is worth 1,594 to 3,566 columns a second. The chat
round trips below, and the flat world's "floor", are the harness.

## What was asked and why a running server could not answer it

Two candidates, opposite fixes. If four joins queue behind the **single warming
thread**, a small pool of warming threads is the answer. If they queue behind
the **region mutex**, a pool makes it worse: more threads holding the same lock
for longer, and every session task that falls through to building its own
column waits behind all of them.

A lock-wait counter on the running server would have answered the wrong
question. With one warming thread there is very little to contend *with*, so
the counter would have read low — and the question is not "is the lock
contended today", it is **"would the lock be the wall if the thread stopped
being it"**. That is a counterfactual, and the way to measure a counterfactual
is to take the thread out of the way and see what the work does.

## The ladder

`benches/contention.rs`. Every row builds the **same 512 columns** with the
same total work split over its threads, gets a store built for it so that no
row inherits a warm sky-floor cache from the row above, and differs from the
row above it in exactly one named thing. The region files are read once and
thrown away before any row is timed, so no row is measuring this machine's
disk. Each row prints throughput **and the per-column latency distribution**,
because the number under investigation is a tail and a total cannot see one.

```text
DUST_BENCH_CONSTANTS=… DUST_BENCH_DATA=… DUST_BENCH_REGION=… \
  cargo bench -p dust-server --bench contention
```

**Before**, on a ten-core machine with three other builds running:

| row | 1 thread | 2 | 4 | 8 |
| --- | --- | --- | --- | --- |
| cpu control (shares nothing) | 1.00x | 2.13x | 4.10x | **7.72x** |
| region files, one store | 1.00x | 1.41x | **1.49x** | 1.48x |
| region files, a store each | 1.00x | 1.43x | 1.99x | 2.78x |
| generated, one store | 1.00x | 1.82x | 2.83x | 4.13x |

The **cpu control is not optional**: three other agents were compiling
throughout, and a row that fails to scale proves nothing unless a row that
should scale does. It reached 7.72x, so the machine was not the answer.

Three things fall out and none of them is visible in a single figure.

**One store stops at 1.49x.** Two, four and eight threads all delivered about
1,590 columns a second. Throughput was flat while the per-column p99 climbed
from 2.1 ms to 12.0 ms, which is the shape of a queue and not the shape of a
machine running out of cores.

**A store per thread — the same work, the same files, the same page cache, N
locks instead of one — reached 1.99x at four threads and was still climbing at
eight.** That is the single named change that says it is the lock. It is not a
proposal: separate stores also stop sharing the sky-floor cache, so they do
strictly more work, which is why they land at 1.99x rather than anywhere near
the control.

**1,594 columns a second is itself a measurement.** If N threads cannot beat T
columns a second however many of them there are, `1/T` is the part of a column
spent holding the lock: 0.627 ms of the 1.041 ms a column costs one thread.
**Sixty per cent of a region column was serial**, which caps every arrangement
of threads at 1.66x by Amdahl. The observed 1.49x is that ceiling.

## What changed

`AnvilCore::read` is now two functions. `stored()` takes the region mutex, gets
or opens the `RegionFile`, calls `read_chunk_raw`, and returns owned compressed
bytes. `read()` decompresses them, parses the NBT and assembles the chunk with
nothing held.

That is the whole change. A `RegionFile` seeks as it reads, so two threads
sharing one genuinely cannot be inside it at once — but decompressing a stream
and walking a tag tree are work on bytes the caller already owns, and they were
inside the lock only because they were written in the same function as the
seek.

**After**, same machine, same run, same rows:

| row | 1 thread | 2 | 4 | 8 |
| --- | --- | --- | --- | --- |
| region files, one store | 1.00x | 1.88x | **3.34x** | 4.62x |
| region files, a store each | 1.00x | 1.44x | 2.05x | 2.80x |

At four threads: 1,594 → **3,566 columns a second**, per-column p50 2.31 →
1.19 ms, p99 6.31 → 2.66 ms, worst column 6.96 → **2.85 ms**. The serial part
of a column is now about 10% rather than 60%.

The row that was the diagnosis is now the confirmation, backwards: **one store
beats a store per thread**, 3.34x against 2.05x. Sharing the sky-floor cache is
worth having once sharing the lock is not a tax, and that is the argument for
one `AnvilCore` per world rather than one per reader.

## The floor nobody had measured

D31 read the region-file tail as a regression the chunk stream had introduced.
Half of it is not about columns at all, and a **flat world says so**: a flat
world lends one template column to every position, keeps no residency, opens no
file and runs no warming thread, and four bots joining one at the same moment
still stall a settled player.

Twelve interleaved runs on each world, four joiners, a settled player timing a
chat round trip twenty times a second across a fixed three-second window. The
number is the **worst** round trip of a run, and the table is the distribution
of that number over the runs:

| | median worst | max | runs over 300 ms |
| --- | --- | --- | --- |
| **flat** (both builds, 12 runs) | 251 ms | 469 ms | 2 of 12 |
| **region files, before** | 390 ms | 696 ms | **7 of 12** |
| **region files, after** | **194 ms** | 522 ms | 3 of 12 |

A region-file world used to stall a bystander *worse than a world with no
columns in it*. It now stalls them no worse — 194 ms against the flat world's
251 — and the flat number is unchanged between the two builds, which is the
negative control: this change touches no flat path and moves no flat number.

The counts say something a median cannot. Round trips over 50 ms and over
100 ms are **identical between the two builds** — 21 and 14 across the twelve
region runs either way. What changed is how long the worst ones last: over
300 ms went from 8 to 3. **The same number of stalls happen, and each is about
half as long**, which is what narrowing a critical section does and is not what
adding threads would have done.

**A settled player with nobody joining sees a maximum round trip of 1 ms**, so
none of this is the harness, the loopback or the machine.

## What was declined

**A pool of warming threads**, which is what D31 was contemplating and what the
whole measurement was ordered to arbitrate. Declined again, for a new reason
and with the old one gone. The old reason was that it could not have worked: a
pool behind a lock that caps four threads at 1.49x buys almost nothing. That
cap is gone. The new reason is that the numbers above say build throughput is
no longer what the bystander is waiting for — **the count of stalls did not
change when the build got 2.2x faster, only their length** — so a pool would be
buying a thing that has already stopped being the constraint, at the price of
cores the tick loop wants. It should be reconsidered when something measures
what the residual stall is, and not before.

**Built by [D36](0036-how-many-threads-build-the-world.md), 2026-09-07.** The
condition was met from an unexpected direction: what got measured was not the
bystander's residual stall but the *joining* player's own wait, which nothing
had ever instrumented. `benches/warming.rs` runs the stream's own discipline
against the store and counts the 20 ms passes that had nothing ready to send —
273 of them on a single join with one builder, 19 with four. This record and
D42 are both right that the build was no longer what a *bystander* waited for;
neither had asked what the joiner waited for.

**A mutex per region file** instead of one over the map. It would help four
players spread across a world and not the case under test, where four joiners
at the same spawn want the same file. The measurement to justify it is four
joins at four distant coordinates, which nobody has taken.

**A timing test that asserts the lock is narrow.** A lock-width assertion is a
timing assertion, and a timing assertion on a machine running four agents is a
flake generator. The bench is the check, and it was watched failing: the 1.49x
table above is this same bench on the code as it was.

## What is still wrong

**Four simultaneous joins stall a settled player for a median of about 250 ms
on any world, including one where a column is free.** That is the floor this
record found and did not move, and it is now the whole of the region world's
tail. It is somewhere in the join path rather than in the world — four clients
handshaking, four rosters updating, and 4 × 289 chunk packets being encoded and
written on session tasks inside about seven hundred milliseconds. The flat
world is the instrument for it, because on a flat world every explanation
involving a column is already excluded.

**`sky_floor` reads a column out of the file to find where the sky reaches in
it**, and building one column asks for four neighbours' floors. On terrain
nobody has visited that is up to five region reads and five NBT parses to serve
one column, of which four will be repeated when those neighbours are built.
The floor cache makes it about two, and it is still the largest single thing a
region column pays for.
