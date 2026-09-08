# D45 — The ground the bot gate stands on

**Status:** Built and measured, 2026-09-08. `just bot` is **30 of 30 on a flat
world and 30 of 30 on `[worldgen] seed = 1`**, whose spawn is open ocean. It
was 29 of 29 and 22 of 29 on the same two worlds an hour earlier, and the seven
that were red had been red since generated terrain existed.

`just bot` is the strongest gate this project has: mineflayer implements the
client protocol independently and shares no code with Dust, so it finds what
the test suite — which agrees with itself — cannot. Its record is in
`tools/bot/README.md`.

**An amber gate is worse than a red one and much worse than a green one.**
Nobody can tell a new failure from the seven expected ones, so the next real
protocol break arrives as "23 of 29" and reads as noise. That, not the seven
rows, is what this record is about.

## What was actually wrong, and it was not the server

Every terrain-dependent check took the block at `stood.offset(dx, -1, 0)` to be
solid ground: the cell the actor digs out and fills back in, the three it
places against, and the one whose underside it clicks. On a superflat world it
is grass. On seed 1 it is **water, sixty-three deep**, and two separate things
happen.

**A right-click into water replaces the water.** `session::placement` asks
whether the clicked cell is replaceable before it asks about the face — which
is correct, and is what puts a block where the tall grass was rather than on
top of it. So a click on the top face of a water cell puts the block *in* that
cell, one below where the check then looks for it. Five rows failed on that
alone; the sound-position row said so in as many words, `placed at 2/61/0,
sound at 2.5/60.5/0.5`.

**And the actor has nothing to stand on.** A bot spawned over ocean sinks about
half a block a second and never stops: measured at `(0.5, 63, 0.5)` on arrival
and `(0.7, 48.7, 0.5)` twenty-five seconds later, still falling. The working
area runs six blocks out and `[server] interaction_range` is six, so by the
time the run reaches the far column the actor is not merely on the wrong floor,
it is out of reach of it.

**None of the seven was Dust doing the wrong thing.** Both mechanisms are the
server behaving exactly as vanilla does; the harness was asserting against a
world it had assumed rather than one it had built.

## The floor is built, not found

The resolution already exists in the same file, three hundred lines further
down, where the "a block is not placed into one that is already there" check
decayed for a related reason and was rewritten to put a block down and click
*its* underside. Its comment is the rule: **a check that needs a particular
arrangement of blocks builds it rather than looks for it.** That rule now
applies to the floor as well as to the one block.

`buildGround` does three things in an order that matters:

1. **A foothold first, on its own.** One cobblestone into the cell under the
   actor's feet, repeated until it is standing on something. Until the actor
   has landed there is no fixed position to measure the working area from, and
   every cell it could aim at is moving away from it while it falls. This is
   also the one placement whose reach cannot fail.
2. **`stood` is read after the fall, not during it.** A neighbour measured from
   a position the actor is still moving through is a neighbour of somewhere
   else.
3. **Then the floor**, one cell per column in `COLUMNS`, plus one of support
   under the column that gets dug out — the hole is filled back in by clicking
   the top face of the block below it, and over water there is no block below
   it. A cell that is already solid is left alone, so on a flat world the run
   places nothing at all and the world is untouched.

The placement primitive is the same one that caused the failures: a right-click
**on** a replaceable cell puts the block into it. It is the only placement that
needs no solid block to aim at, which is what makes a floor buildable over open
water in the first place.

The five columns are named constants gathered in one list, and the checks read
the same constants. That is not tidiness. A guard can only fail on a question
somebody thought to ask, and the question here — "does this column have ground
under it?" — is asked once, of exactly the list the checks use, so a check that
wants a sixth column gets ground with it rather than discovering on an ocean
spawn that it never had any.

## The setup is a row, not a silence

**A gate that cannot set itself up has to say so.** `the ground these checks
stand on was built` is check 30, it reads the cells back from the *other*
player rather than from the one that placed them, and it names the cells that
are missing and what is in them instead. Without it the seven checks that need
this floor go red with seven different-looking reasons and none of them the
real one — which is the state this record found them in.

Its bite is measured rather than assumed. Adding an unreachable column to
`COLUMNS` and running against seed 1 gives:

```text
FAIL  the ground these checks stand on was built — 20/59/0 is water
29/30 checks passed
```

## Numbers

| world | before | after |
| --- | --- | --- |
| flat (no `dust-biomes.tsv`) | 29 / 29 | 30 / 30 |
| `[worldgen] seed = 1`, ocean spawn | 22 / 29 | 30 / 30 |

Three consecutive runs against the same seed-1 world report the floor at the
same `y=59` and the actor standing at the same `y=60`. That is the other half
of the old lesson about this file — the actor used to sink a block per run
until a check was vacuous while still printing `ok` — and it now holds over
water too: the actor quits standing on the floor it built, so the next run
finds it and places nothing.

Release build throughout. Measured on this project before: against a real world
a debug binary fails checks a release binary passes, on the same commit.

## What this cost, and the standing lesson

**A control that only holds on a superflat is not a control.** `just bot` had
been green for a long time on a world with no `dust-biomes.tsv` beside its
`[data] path`, which is the configuration in which Dust silently serves flat
terrain. The gate was not measuring what anybody thought it was measuring, and
the seven-row amber that appeared when somebody finally pointed it at generated
terrain was the first news of it. **Run `just bot` against both**: a flat world
and a generated one, and preferably a generated one whose spawn is over water,
because that is the terrain that has nothing in it.

## One thing this found and did not fix

`block_dig` on a **water** cell removes the water. The run's dig-before-place
step turned three water cells into air, which is visible in the world
afterwards. A vanilla client never raycasts onto a fluid, so no player reaches
this by playing, and vanilla's own server honours a break packet it is sent —
whether it refuses one aimed at a fluid was not measured here and is not
something to guess at. It is written down rather than changed, and the harness
does not depend on the answer either way: after the dig the cell is air, and if
Dust ever stops honouring it the cell is water, which is replaceable, and the
placement lands in the same place.
