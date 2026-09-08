# D37 — The two benches that are not a grid

**Status:** Decided, 2026-09-07. A stonecutter's buttons are sorted by the
**result item's description id**, because that is the order the client builds
and neither side puts it on the wire. A smithing table's result is the **base
stack transmuted**, not a fresh one. The eighteen `smithing_trim` recipes are
**declined out loud** rather than approximated.

## Context

Decision records 0033 and 0034 gave a player a 2x2 and a 3x3. 389 of the 1,290
recipe files an operator's data pack ships still wanted a block that does not
open, and the furnace family took 112 of them. The 277 left are the two benches
here: 250 `stonecutting`, nine `smithing_transform` and eighteen
`smithing_trim`.

Neither is a grid, and neither is a grid in a *different* way.

**A stonecutter answers with a list.** One block of andesite is six things, and
the player says which by pressing a button. The packet that comes back names a
recipe by **its index in a list the client built** — the client filters every
stonecutting recipe it was told about by what is in the input slot, sorts the
survivors, and draws a button per row. The order is on neither side of the
wire. A server that sorts differently hands a player a wall when they pressed
stairs, silently, with both ends believing they agree.

**A smithing table answers with one item and the client computes it too.** The
vanilla client runs the same recipe list over the three inputs and clears its
own result slot whenever one changes. A server that never sent the smithing
recipes at all would look correct in a packet log and be empty on the screen.

## Which order the buttons are in

Measured before anything was built, because there was nothing to guess from
that was not also the thing under test. `tools/bot/benches.js --survey` pressed
**all 24 button ids on six inputs** against a real 1.21.1 server and wrote down
what came out of each.

The order is **not** the recipe id and **not** the order the recipes arrive in.
A real server sends `diorite_wall_from_diorite_stonecutting` first in
`declare_recipes` and draws `andesite_slab` on button 0. It is the result
item's **description id** — `block.minecraft.<name>` for an item that places a
block, `item.minecraft.<name>` for one that does not — compared as bytes:

```text
  stone, on a real 1.21.1 server

  0 chiseled_stone_bricks   3 stone_brick_wall
  1 stone_brick_slab        4 stone_bricks
  2 stone_brick_stairs      5 stone_slab
```

**`stone_brick_wall` on 3 and `stone_bricks` on 4 is the row that says the
comparison is over bytes**, since `_` sorts before `s`. A comparison that split
on words, or one that sorted by item name in a locale, puts them the other way
round.

`Cut::sort_key` reproduces the description id rather than the item's name. The
two agree for all 167 distinct results in 1.21.1's own data, because every one
of them places a block — so this costs one registry lookup at boot and buys
nothing today. It buys a data pack whose stonecutter makes an *item*, where a
name comparison would order the client's list differently from the server's and
nothing would say so.

**Pressing a button that is not there keeps the last selection.** Vanilla's
`StonecutterMenu.clickMenuButton` guards on `isValidRecipeIndex` and does
nothing when it fails. Buttons 6 through 23 with andesite in the input leave
button 5's polished andesite stairs in the result slot on a real server rather
than emptying it, which is why the survey presses all 24 rather than as many as
it expects: the eighteen presses that do nothing are the measurement.

## What a smithing table hands back

**The base stack, transmuted.** `SmithingTransformRecipe.assemble` calls
`transmuteCopy`, which keeps the base stack's components and changes only which
item it is. Building the result out of the recipe's own item would look right in
every check that compared item ids and would silently strip every enchantment a
player had spent levels on. Priority 1, and it is measured rather than reasoned
about: a survey row upgrades a diamond chestplate named "Old Faithful" with 137
damage on it, and both servers hand back a netherite chestplate carrying
**both** components.

## What is declined, and said out loud

The eighteen `smithing_trim` recipes are not loaded. A trim's result is the
base item carrying a `minecraft:trim` component that the *server* has to
author, and Dust carries the components that arrive rather than writing new
ones — decision record 0024 is where that line is. The recipe loader counts
them by name so the boot line says so:

```text
  1290 recipe file(s) in minecraft, 887 craftable in a grid, 112 cooked at a
  fire; 250 cut at a stonecutter, 9 made at a smithing table; 18 not run here,
  14 are code rather than data, 0 refused
```

The consequence a player can see is one slot's worth: vanilla's base slot
accepts an iron chestplate because `#minecraft:trimmable_armor` names it there,
and Dust's bounces it back. Both of the differential's two divergences are this
one fact seen from two ends, and **both are named in the script**, so the day
somebody loads the trims and they stop diverging the check fails rather than
quietly agreeing with a record that has gone stale.

## The field that was not there

`SmithingTransformData` and `SmithingTrimData` carried a `group` string, like
every other recipe in `update_recipes`. **Neither smithing recipe has ever had
one.** `SmithingTransformRecipe`'s stream codec is four fields — template,
base, addition, result — and `SmithingTrimRecipe`'s is the first three.

It survived because **a round trip agreed with itself**. The corpus in
`crates/dust-protocol/tests/common` encodes a packet and decodes it with the
same code; so does the mutation loop. Both are silent about a field that is
consistently written and consistently read and that nobody else expects. The
first real client to be handed one read the recipe id that followed as a
stack's component patch and dropped the connection.

Two things changed, and the pair is the point:

- `play_bodies.rs` now pins **the bytes**, not a round trip: a transform body
  is 19 bytes opening with its template ingredient's count. Adding the field
  back does not fail an assertion, it fails to compile — the test names the
  four fields.
- `tools/bot/benches.js` puts an implementation nobody here wrote on the other
  side of the question. That is the check with a real second opinion in it; the
  byte test is the guard that catches the regression in CI without a server.

## What says this is right

`tools/bot/benches.js --survey` records what a server does — the whole button
sequence of six inputs, three multi-click stonecutter behaviours, six smithing
cases — and `--compare` diffs two recordings. It asserts nothing about one
server on its own, for the reason the file's header gives: there is no reading
of a single server that this script could tell was wrong.

```text
  node benches.js 25703 --survey --out vanilla.json   (a real 1.21.1 server)
  node benches.js 25603 --survey --out dust.json
  node benches.js --compare vanilla.json dust.json

  17/17 rows identical, 2 declared divergence(s)
```

The 144 button presses behind the six input rows are compared as **whole
sequences**, not as counts and not as sets. Both of the weaker forms pass a
server that sorted differently.

Watched failing, because a check nobody has seen fail is a check about nothing.
Two edits to this server, each run against the same vanilla recording:

| what was broken | `--compare` | `cargo test` |
| --- | --- | --- |
| nothing | **17/17**, 2 declared | green |
| the `group` field put back on `SmithingTransformData` | **0/17** | does not compile |
| the description-id tie-break deleted from `Cutting::index` | **16/17** | 3 red in `dust-sim` |

**The second row is the interesting one and it is not good news.** Deleting the
sort leaves the recipes in the order their files were read, and five of the six
inputs come out identical anyway — the file order happens to agree with
vanilla's for andesite, cobbled deepslate, copper, quartz and stone. Only
blackstone disagrees, where `polished_blackstone` falls from button 4 to button
8 behind its own four brick recipes. **One row of six carries the whole
ordering claim**, which makes the input list an instrument nobody would notice
weakening; `CUT_INPUTS` now says so at the list. The three unit tests in
`dust_sim::cutting` are the other half of that defence, and they are the half
that runs in CI.

**A declared divergence is satisfied by a broken server too.** In the 0/17 run
both `known` rows still read as known: a Dust that served no recipes at all
diverges from vanilla in exactly the place a Dust that declines trims does. The
mechanism catches an unexpected *agreement*, which is what it was built for, and
it cannot tell a deliberate difference from a dead one. The unqualified rows are
what say the server is alive.

## What this does not say

**Nothing here was measured on a generated world.** Both servers ran superflat
in creative, which is what lets the survey put a bench down beside the player
and press its buttons; the stonecutter's rules do not read the world, but the
statement "this agrees with vanilla" is about the situations the script
reaches, and 144 presses on six inputs is not 250 recipes.

**The three multi-click stonecutter rows are the shallow end.** Eight takes on
one press with an exact input count afterwards separates "the selection
survived" from "the server refilled from nothing" from "the server refilled
from the last press"; a single take after a single press looks the same in two
of those three. What is still untested is a shift-click that empties the input
in one go, which is how most players use the block.

**A smithing table is a nine-entry scan and stays one.** A per-item index over
1,333 items to save eight comparisons on a mouse click would cost about 5 kB to
buy nothing, and it is asked once per click on one of three slots rather than
hundreds of times a second like the grid. The stonecutter *is* indexed — one
`u32` and one `u16` per item plus six bytes a recipe, about 8 kB — because its
list is rebuilt every time the input slot changes, which is every click a
player makes in that screen.

**Nothing checks that the player is still near the bench.** The same gap
decision record 0034 leaves open for a crafting table, for the same reason: the
reach check runs when the screen opens and never again. It costs nobody an item
— closing gives the inputs back — and it wants the same `within_reach` call.
