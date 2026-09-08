# D42 — What a joining crowd costs a bystander

**Status:** Measured, 2026-09-03. **It costs them about seven milliseconds.**

[D31](0031-how-a-join-streams-its-chunks.md) reported that four simultaneous
joins on a world read from region files stall a settled player, and called it a
regression the chunk stream had introduced.
[D38](0038-how-wide-the-region-lock-is.md) narrowed the region lock, halved that
number, and then found the same stall on a **flat** world — one template column,
no file, no residency, no lock — and named the remainder a floor the join path
imposes on any world. This record was ordered to find what that floor is.

There is no floor. **Both records were measuring the harness.** All five bots
lived in one node process, and a joiner's 289 chunk packets are parsed by
prismarine on the node thread that receives them — so the settler's chat round
trip was timed by an event loop that four joins had just filled with work.
Give the settler a process of its own and the stall is gone: **48 runs, two
builds, two worlds, four and eight simultaneous joiners, and not one chat round
trip over 50 milliseconds.**

## What the running server said, and why it was convincing

The ladder that came first is on the real server, on a flat world, with the
settler timing a chat round trip twenty times a second across a fixed
three-second window. Eight interleaved passes a row, six busy threads held on
the machine throughout so that the rows are not scored against whatever the
other builds on this machine happened to be doing. The number is the **worst**
round trip of a run; the counts are round trips over 50, 100 and 300 ms summed
across the eight runs.

| row | median worst | worst | >50 | >100 | >300 |
| --- | --- | --- | --- | --- | --- |
| four joiners at once, view distance 8 | 193 ms | 1,291 ms | 12 | 11 | 4 |
| the same, view distance 2 (25 columns, not 289) | 37 ms | 89 ms | 3 | 0 | 0 |
| one joiner, view distance 8 | 48 ms | 61 ms | 4 | 0 | 0 |
| four joiners **600 ms apart**, view distance 8 | 61 ms | 92 ms | 14 | 0 | 0 |
| **nobody joins** | 1 ms | 1 ms | 0 | 0 | 0 |

Every rung pointed the same way. The load control says the busy machine alone
never touches a settled player. Dropping the view distance removes the stall,
so it is the chunk stream. One joiner does not do it, so it is not any single
join. The same 1,156 columns 600 ms apart do not do it either, so it is
simultaneity rather than volume. And it got monotonically worse with fewer
spare cores — worst round trip 595 ms with the machine idle, 1,291 ms with six
threads busy, 1,672 ms with twelve.

That is a coherent story about a server that wants four cores for seven hundred
milliseconds. It is also, word for word, a coherent story about a **node
process** that wants to parse 1,156 chunk packets in seven hundred
milliseconds, and nothing in the ladder distinguishes them.

## The arithmetic that said it could not be the server

`benches/contention.rs` grew two rows for the per-column work a session does
*after* the column is in hand — the part four joins do four times over. On the
flat template, where the column itself is free:

| row | per column | 1 thread | 2 | 4 | 8 |
| --- | --- | --- | --- | --- | --- |
| chunk packets, encode | 0.017 ms | 1.00x | 1.84x | 3.14x | 3.76x |
| chunk packets, encode and frame | 0.072 ms | 1.00x | 1.96x | 3.21x | 4.25x |

The second row is the first plus what `Conn::send` does: the body encoded again
and zlib at level 6, which every chunk packet gets because all of them clear
the 256-byte threshold announced at login. So a four-way join's **1,156 chunk
packets are 83 milliseconds of CPU in total**, spread over four tasks that have
seven hundred milliseconds to spend it, on a path that scales with threads and
therefore holds nothing. A quarter-second stall could not come from 21 ms of
work per session, and that is what sent the question back to the instrument.

The same row prints what a column costs on the wire, because a CPU number alone
cannot say whether a burst is bounded by cores or by bytes: **a flat column
frames to 264 bytes**, so a whole four-way join on a flat world is 305 KB. It
is not bandwidth either.

## The rung that settled it

`tools/bot/join.js` grew a `where` argument, and it is the only difference
between the rows below: `same` puts every bot in one node process, as this
harness always did; `each` gives every joiner a process of its own and leaves
the settler alone in hers. Same server, same commit, same run script, eight
interleaved passes a row, six busy threads.

| row | median worst | worst | >50 | >100 | >300 |
| --- | --- | --- | --- | --- | --- |
| region files, 4 joiners, **same** process | 1,486 ms | 1,680 ms | 8 | 8 | 8 |
| region files, 4 joiners, **a process each** | **7 ms** | 11 ms | **0** | 0 | 0 |
| region files, **8** joiners, a process each | 9 ms | 20 ms | 0 | 0 | 0 |
| region files, 4 joiners, same, **before D38** | 372 ms | 1,744 ms | 10 | 9 | 7 |
| region files, 4 joiners, each, **before D38** | 7 ms | 10 ms | 0 | 0 | 0 |
| flat world, 4 joiners, a process each | 4 ms | 5 ms | 0 | 0 | 0 |

Two things in that table are worth saying out loud.

**The joiners really joined, and joined faster.** Each of them reports its own
column count, and in `each` mode all four had all 289 columns within about 870
milliseconds — sooner than in any other arrangement, because a joiner that owns
a thread receives as fast as the server sends. The most isolated arrangement is
also the most simultaneous one, so this is not a stall hidden by a slower test.

**`same` scores the current server worse than the one before D38** — 1,486 ms
against 372 — which is not a statement any server-side story can make. The
same-process number is not a noisy measurement of the server. It is a
measurement of something else.

## What this retracts

- **D31's region-file regression.** The tail it reported is the harness's. The
  chunk stream may still have changed something; this measurement says nothing
  about it either way, and nothing has been observed that a player would feel.
- **D38's 250 ms floor**, and its "four simultaneous joins stall a settled
  player on any world". They do not stall anyone.
- **D38's player-facing case for narrowing the region lock** — worst round trip
  390 ms to 194. Both builds are at 7 ms when measured properly. The change
  keeps its other justification, which is a measurement of the server rather
  than of a bot: four threads through one `AnvilCore` went from 1,594 to 3,566
  columns a second and their worst column from 6.96 ms to 2.85. That is priority
  2, fewer resources for the same work, and it stands.

## What was declined

**A pool of warming threads**, for the third time and now for a reason that
ends the question rather than deferring it: the stall it was meant to fix does
not exist. It should be reconsidered only if something a player can feel is
measured first.

**Built by [D36](0036-how-many-threads-build-the-world.md), 2026-09-07**, and
this paragraph's condition is what built it. The stall this record retracted was
a bystander's and it did not exist. The thing a player can feel is the *joiner's
own* wait for the ground, which no instrument in D31, D38 or here ever timed —
three records argued about a pool while measuring somebody standing still.
Measured, a cold generated join is 2,699 ms of world arriving with one builder
and 1,069 with four, against a pacing floor of 740.

**Deleting the `same` mode.** It is the only way to reproduce what D31 and D38
published, and a record that says "the old number was the instrument" is worth
much less if the old number can no longer be produced.

**A server-side stall counter.** It would have read zero all along and been
right, and nobody would have believed it over a bot that was visibly stalling.
The way to check an instrument is to change the instrument, not to add a second
one inside the thing under test.

## What is still wrong

**Nothing a joining crowd does has been shown to cost a bystander anything**,
which means the honest state of this question is that there is no measurement
of a join cost worth optimising and there has not been one since D31. The next
thing worth doing is not in the join path.

**Nothing measures a bystander from another machine.** Every number here is on
loopback, which has no bandwidth limit and no latency worth the name. A flat
column is 264 bytes and a column of a world Minecraft wrote is a great deal
more — that size has not been measured on the wire, and it is what would decide
whether a bystander on a real uplink waits for bytes where one on loopback
waits for nothing. That is the measurement this record cannot make, and the one
to make first if anybody asks this question again.
