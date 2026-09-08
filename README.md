# Dust

A Minecraft Java Edition server, written in Rust.

Dust is being built from nothing and is not finished — but you can play on it.

## Status

**Two people can connect, walk around a shared world, break and place blocks,
see each other doing it, and talk.** They place what they are holding — any of
the nine hundred and twenty-five blocks Minecraft has an item for, picked out of
the creative menu — where Minecraft would put it: into the tall grass they aimed
at, and not into the wall behind the face they clicked. **What goes down is the
state Minecraft would put there for 89.5% of the ways a block can be placed**,
measured against a real server rather than claimed: a stair faces the way you
stood, a furnace faces back at you, a lever takes the wall it is on, a piston
points where you looked and an observer looks back. They see each other
swing, crouch and break blocks,
particles and sound out of the block that broke, and hear each other put blocks
down, each block with its own sound. What they change is still there after a
restart, along with where they were standing. The world is lit the way Minecraft
lights it: sky light and block light both, from Minecraft's own numbers, read
out of the operator's own jar.

`dust server` binds `[server].bind`, answers the server-list ping with the MOTD,
player count and favicon from `dust.toml`, runs login in either offline or
online mode, syncs the eleven datapack registries a 1.21.1 client needs, streams
chunks as players move out to `[server].view_distance`, and keeps the connection
up. That distance is a ceiling: the client asks for one of its own during
configuration and is served the smaller of the two.

The loading screen ends once the near square has arrived rather than once the
whole view has, and the rest of the view is sent by the play loop a batch at a
time instead of in one burst. Measured A/B on one binary at the default
distance, timing the first keep-alive *after* the screen ends:

```text
           screen ends   first keep-alive after it   all 289 columns
  burst        648 ms       1,733 ms (1.1 s later)        1,731 ms
  batched      411 ms          428 ms (17 ms later)       1,768 ms
```

The same work, in the same order, finishing at the same moment — with the
session answering throughout instead of at the end. A player who joins and walks
immediately used to have their movement packets sit in the socket for a second.

The columns themselves are built by the world rather than by the session that
wants them, and a session sends only the ones that are ready — nearest first, a
prefix with no holes in it, over a window of 24 columns it claims in the shared
column store and gives back as they go out. That is what keeps a join off its
own tokio worker and out of everybody else's way. Four bots joining at once
were sent 273 to 279 of their 289 columns in the same three seconds where they
used to get 181 to 253, finishing within 20 ms of each other rather than
staggered across 400, because the store builds their columns once between them.
Decision record [0031](docs/decisions/0031-how-a-join-streams-its-chunks.md) has
the ladder that says which world each number is about, and
[0042](docs/decisions/0042-what-a-joining-crowd-costs-a-bystander.md) retracts
what it said about the *settled* player: that number was the test harness, which
parsed every joiner's chunks on the same node thread it timed the settler with.
Measured from a process of its own, a settled player's worst chat round trip
while four people join is **7 ms**, and while eight join it is 9.

A player who changes their render distance mid-game is served the new one, which
the pacing made cheap enough to bother with: the view forgets or sends the
difference on its next move, out of the same computation it already does.

**What a player waits for no longer depends on the view distance**, which is why
the default is Minecraft's own ten: 404/421 ms at a distance of 8, 396/415 at
10, 376/394 at 12. What the number still costs is the streaming behind them and
the memory of holding it — a bill paid while playing rather than while waiting.

**Tags go out, all thirteen registries of them** — 514 tags flattened to
6,362 registry ids, which is exactly what a real 1.21.1 server sends, compared
tag by tag and id by id against one. Nothing went out while five of the
thirteen were extracted, because a partial tag set is worse than none: a client
told `minecraft:mineable/pickaxe` holds eleven blocks believes the other nine
hundred are not mineable, where a client told nothing falls back to its own
copy.

**`mineflayer` joins it.** That matters more than it sounds: a client that does
not track data packs has no copy of the registry contents to fall back on, and
until now Dust had none to send it, so most of the bot and proxy ecosystem was
refused at configuration. Point `[data].path` at a copy of Minecraft's data —
the one the operator already has, since none of it is shipped here — and Dust
sends the two registries such a client cannot manage without. See decision
record [0007](docs/decisions/0007-registry-contents.md) for where the line
between a protocol fact and Mojang's content falls, and why it falls there.

Ten of the eleven synced registries go that way. The eleventh,
`minecraft:enchantment`, is **declined out loud**: a client sent nothing for a
registry falls back to its own correct copy, and one sent an enchantment
without its effects believes Protection does nothing. Decision record
[0009](docs/decisions/0009-enchantment-registry.md) measures what serving it
would take — 470 key paths, eleven levels deep — and
`cargo xtask harness registries --dump minecraft:enchantment` is the command
that measured it, against any registry and any version.

**It can serve a world Minecraft made, and hand one back.** Point
`[server].world_source` at a region directory and Dust reads the columns out of
it — blocks, their properties, biomes, heightmaps — and streams them. It also
writes Anvil: `cargo xtask harness rewrite` puts every chunk of a real world
through Dust's reader and writer and then boots a vanilla server on the result,
which reads back the world it started as and says nothing about it that it did
not say about its own. Without a world source Dust
generates the world its seed says is there: `dust-gen` samples the six climate
values Minecraft picks a biome from and evaluates its `final_density` over
Minecraft's own interpolation lattice, so the mountains, valleys, coastlines and
sea floors are where vanilla puts them and the biome of a cell is the biome
vanilla gives it — then paints the dimension's own **surface rules** over the
result, so a player lands on grass over dirt, sand on a beach, gravel on a shore
and deepslate below, and not on stone. Then it runs the dimension's own
**aquifers**, so a cave under the sea is somewhere a player walks rather than
somewhere they drown: 400,638 of seed 0's 588,215 missing cave cells were
pockets Dust had flooded, and 23 of them still are. Then it cuts the dimension's
own **carvers** through the result — the tunnels and the ravines — which takes
what is left of that count from 187,614 to 18,433, so **97.3% of the cells
Minecraft carved on seed 0's sample are open here too**, and turns eight cells
of air under a player at spawn into 1,738. **Not** the features that come after,
so there are still no trees and no ore veins. How far that is from the world
Minecraft generates for the same seed is measured rather than estimated —
`cargo xtask harness worldgen` scores it in six parts. Decision record
[0012](docs/decisions/0012-what-worldgen-is-worth-measured-first.md) is what
each stage of vanilla's pipeline is worth and the order to build them in,
[0021](docs/decisions/0021-which-biome-a-cell-gets.md) is the biome source,
[0026](docs/decisions/0026-the-terrain-dust-serves-and-what-it-does-at-the-seam.md)
is the terrain and what a served world does at the edge of a world file, and
[0032](docs/decisions/0032-what-the-ground-is-made-of.md) is the block underfoot
— and the finding that two thirds of what reads as a missing carver is a missing
aquifer — and
[0035](docs/decisions/0035-what-a-cave-holds.md) is the aquifers that finding
sent for and
[0039](docs/decisions/0039-what-a-carver-digs.md) the carvers after them, both
recovered from the operator's own server jar because they are the stages of
worldgen that are code rather than data. A world
is a disc in an infinite plane and a player can walk off the edge of it: with
that world's own seed, read from the `level.dat` beside it, the far side is the
terrain it would have had; without one, the superflat runs on as it always did,
because generating from the wrong seed would put a cliff exactly where the disc
ends.

What exists either way is the whole path from the socket to the block table —
framing, compression, encryption, the four connection states, the paletted
section codec, the chunk packet, the light engine.

A player only reaches as far as their arms: breaking and placing are refused
past `[server] interaction_range`, measured from the eye to the nearest point of
the block, and the check lives in `dust-guard` rather than in the session
because a rule that can only be run from inside a session can only be tested by
running one.

And a player is where they say they are, or they are put back. A movement
packet claiming a position further than `[server] movement_speed_limit` — ten
blocks a tick, which is what vanilla allows — is answered with a teleport to
the last position the server believed, not with a log line. The limit was
measured rather than chosen: `just movement` counts what a real client's
packets contain, and over 1,217 of them covering walking, sprinting,
sprint-jumping, creative flight, a 300-block free fall and a walk through a
700 ms network stall, the largest single tick was 3.58 blocks. Decision record
[0017](docs/decisions/0017-how-fast-a-player-may-say-they-moved.md) has the
table and says what it declined.

And a player cannot walk into a wall. A movement packet is checked against the
blocks the player would be standing in, not only against how far they claim to
have come: a player may not move from outside a solid block to inside one, and
a player already inside one may move anywhere, because every honest way to end
up inside a block — a block placed onto somebody standing still, a piston, a
boat — resolves by moving out of it. Which blocks count is Minecraft's own
`isCollisionShapeFullBlock`, read out of `dust-constants.tsv`, because every
proxy for it in the table is wrong in the direction that refuses honest play.
The world is asked as it is at that instant, so a block broken underneath a
player a tick ago costs them nothing, and an unloaded chunk is not solid, so a
player outrunning their own chunk loading is believed. `just collide` is the
third-party check: six cases, and the three refusals go red with the two lines
that refuse the move taken out. It costs 32 ns a packet on a flat world and 408
on a world read from region files, which at twenty packets a second is 0.08% of
one core across a hundred players. Decision record
[0015](docs/decisions/0015-what-a-movement-check-asks-the-world.md) has the
ladder and says what it declined.

And a player carries what they were carrying. All forty-six slots of the
player's own container — the twenty-seven main slots, the nine hotbar, four
armour, an offhand and the crafting grid — with counts bounded by each item's
own `max_stack_size` out of the generated component table, never by a 64
written here. A survival client's clicks are replayed over it: left, right,
shift, the number keys and F, creative clone, Q and control-Q, the three drags
and double-click-to-collect, with only the slots the client got wrong sent
back. **A stack carries its data components** — its name, its enchantments, how
worn it is, what is inside it — through the click, the wire and the save, and
two stacks merge only if their components are equal, so a named stack poured
onto a plain one swaps rather than taking the name off both. **And it survives a
relog and a restart**, written by name into the same file beside the world that
already held the block edits. `just bot` checks that
with a third-party client: set slots, leave, come back, look. Decision record
[0013](docs/decisions/0013-where-a-players-inventory-lives.md) says what the
record does and does not promise.

And a slot that has an opinion keeps it. An armour slot takes only what is worn
in that slot and holds one of it, the crafting output takes nothing, and the
offhand takes anything — so a shift-clicked chestplate goes on the chest, a
shield goes to the offhand, and cobblestone aimed at a helmet slot does nothing
at all. Which slot an item is worn in comes from Mojang's own item tags, with
the two items no tag places named in the source and guarded by a third tag that
fails the build if a version adds a wearable. `tools/bot/clicks.js` replays a
hundred clicks against Dust and against a real 1.21.1 server and diffs the two
recordings: **101 of 101 snapshots agree**, up from 60 of 83 when the armour
clicks were first asked. And a click aimed at a window that has since moved is answered with the whole
container rather than with corrections — the same repair Minecraft performs, and
the difference between a hiccuping connection costing a packet and it costing a
player an inventory that is not theirs until they touch the slot.
`tools/bot/clicks.js --stateid` scores that against a real 1.21.1 server at
**7 of 7**, up from 3 of 7. Decision records
[0016](docs/decisions/0016-which-slot-an-item-is-worn-in.md) has the table and
[0030](docs/decisions/0030-what-a-stale-container-click-gets-back.md) has the
sequence numbers.

**And everybody else can see it.** A helmet on a player's head, a shield in
their offhand and the sword in their hand are sent to every other player who
can see them, and the whole set is sent unprompted to anybody who has just come
into view — so a player who logs in finds the world dressed rather than finding
it naked until each of its inhabitants happens to change a slot. Only the slots
that actually moved go out, because the packet's entries are self-delimiting
and the difference costs 7 bytes where the same change spelled as all six slots
costs 17, and a container change that moved nothing visible costs nothing at
all. `tools/bot/equipment.js` runs three bots — one who dresses, one who
watches, and one who arrives after everything has already happened — against
Dust and against a real 1.21.1 server and diffs the two recordings: **14 of 15
snapshots agree**, and the fifteenth is Minecraft coalescing per tick where
Dust broadcasts per change, named in the script and declined in decision record
[0029](docs/decisions/0029-what-other-players-see-you-wearing.md), which also
says what this costs with ten players in view of each other and with a hundred.

**And a grid of items makes something.** The 2x2 a player carries crafts: put a
log in it and four planks appear in the output, take them and the log is spent,
shift-click and it crafts until the grid runs out. The recipes are the
operator's own data pack, read out of `[data] path` at boot the same way the
loot tables are — **887 of the 1,290 files vanilla ships are made in a grid,
all 887 compile and none is refused**, 389 want a furnace or a stonecutter and
14 are Java classes rather than described recipes. A lookup runs on every click
that moves a grid slot, so the recipes are indexed by ingredient item: 2,713
pairs, about 36 kB, 128 ms to build at boot and **35 ns a lookup**. A honey
bottle gives its glass bottle back. `tools/bot/crafting.js` records what the
container became after each of twenty-eight steps against Dust and against a
real 1.21.1 server and diffs them: **28 of 29 snapshots agree**, and the one
that does not is a shift-click whose result only half fits — where a real
server moves what it can, spends the ingredients and destroys the rest, and
Dust refuses the craft. `--refuse` is the other half, because a recording of
two servers that both stay silent agrees: it tells the lie a real client tells
about the output slot and requires the contradiction, at **6 of 6** on both.
Decision record
[0033](docs/decisions/0033-what-a-grid-of-items-makes.md) has the counts and
the three deliberate differences.

**And a crafting table opens.** Right-click one and the 3x3 comes up, which is
where a pickaxe, a bed, a chest and bread are made. A window is a *numbering*
and not a second container: the player's forty-six slots and the ten a table
adds are one array, and one implementation of the seven click modes serves
both — a table calls its result 0, its grid 1..=9 and the player's hotbar
37..=45, and cannot name the armour or the offhand at all. Shift-clicking with
one open lays the stack into the grid first, which is what a player does all
day. A player dropped mid-craft keeps what was in the grid, because the saved
form folds it back in. `tools/bot/crafting.js --table` places a table,
right-clicks it, builds a chest in it and closes it against Dust and against a
real 1.21.1 server: **17 of 17 snapshots agree**, and 1 of 17 with the
right-click branch removed. Decision record
[0034](docs/decisions/0034-how-a-crafting-table-opens.md) says why there is one
set of rules and not two.

**And a stonecutter cuts and a smithing table upgrades.** The 277 recipe files
nothing could reach are 259 of them: 250 `stonecutting` and the nine
`smithing_transform`. A stonecutter is the first screen here where **the client
decides what the buttons mean** — it filters the recipe list by what is in the
input slot, sorts the survivors, and sends back a *position in that list*, an
order neither end puts on the wire. Get it wrong and a player presses "stairs"
and is handed a wall, silently, with both sides believing they agree. So it was
measured before it was built: `tools/bot/benches.js --survey` pressed all 24
button ids on six inputs against a real 1.21.1 server, and the order is the
result item's **description id** — not the recipe id and not the order the
recipes arrive in. A smithing table's result is the **base stack transmuted**,
so a diamond chestplate that was enchanted, named and half-worn comes out
enchanted, named and half-worn in netherite; building it from the recipe's own
item would have stripped every enchantment and passed any check that compared
item ids. **17 of 17 rows agree with a real 1.21.1 server, with 2 declared
divergences** — both of them the eighteen armour trims Dust declines, seen from
two ends — and 0 of 17 when the wire layout is broken on purpose. Decision
record [0037](docs/decisions/0037-the-two-benches-that-are-not-a-grid.md) has
the button table and what it declined.

And a fence connects to what it touches, whichever way the wall was built. A
fence, a wall, a glass pane and a stair take their shape from the six cells
around them — when they are placed, and again whenever anything beside them is
placed or broken, because a fence that only connected in the direction it was
built would look worse than one that never connected. What a block asks of its
neighbour is whether that neighbour has a full square face on the side they
share, which is six columns off the operator's own jar and not one: the back of
a bottom stair is a full square and its front is not. Measured against a
vanilla server over 5,120 situations the old grid could not ask, an oak fence
now agrees with Minecraft about all 1,060 blocks it can stand beside, and the
sixty-one items placed wrongly for a neighbour reason are down to two. Decision
record [0014](docs/decisions/0014-what-a-block-reads-from-the-cell-next-door.md)
has the counts and says what it declined.

And a block put down in water comes out **wet**. A fence post in a river
waterlogs instead of leaving a dry hole in it, a second layer of snow stacks on
the first up to eight, a slab dropped into its own other half becomes a double
slab, and a torch or a sign put on the side of a block becomes the wall form of
it rather than the standing one that falls over. The first three read the cell
the block is going *into*, which is a third variable neither earlier survey
could vary — `tools/bot/placement.js --into` is the one that can, and it walls
its target with stone because an unwalled water source floods the arena within
two ticks. The fourth is data: an item carries two blocks, a torch and a wall
torch are related by nothing but the item that holds both, and the wall form
and its attachment direction are two more columns off the operator's own jar.
Against Minecraft's own answers the grid's 496 wrong states are down to 62 and
its 101 wrong items to 21, and 108 placements into a cell that already held
something agree exactly. Decision record
[0018](docs/decisions/0018-what-a-placed-block-reads-from-the-cell-it-lands-in.md)
has the counts and says what it declined.

**Breaking a block gives you the block.** What it yields is not a rule — it is
`loot_table/blocks/<block>.json`, in the data pack the operator already
produces for `[data] path`, compiled at boot and rolled per break: stone yields
cobblestone, wheat yields wheat at age seven and seeds otherwise, an ore yields
a variable count, leaves usually yield nothing. All 982 of vanilla 1.21.1's
block tables compile with one entry refused. Against a real 1.21.1 server
breaking fifty blocks in survival, `cargo xtask harness drops` scores 44 of 46
rows where what Minecraft dropped is something Dust drops; both misses are the
same missing thing, and it is named below. What comes out is an **item
entity** — the first entity Dust has — which pops out of the centre of the
broken block, falls, merges with its twin, can be walked over to collect, and
despawns after five minutes. A thousand of them beside a player cost 110
microseconds of a 50-millisecond tick, and a thousand with nobody near cost
616 nanoseconds. Decision records
[0022](docs/decisions/0022-what-a-broken-block-yields.md) and
[0023](docs/decisions/0023-what-shape-an-entity-has.md) are the accounts.

**The world reacts to being changed.** Every write to a loaded column queues
seven positions — the cell and its six neighbours — and the tick loop asks each
one whether it can still stay where it is. A torch whose wall is mined breaks
and drops, a rail whose ground is dug out breaks and drops, and one of the 32
falling states with nothing under it becomes an entity, falls with vanilla's own
gravity and drag, and lands as a block again. A **felled tree's canopy comes
down**: a leaf holds a `distance` counted up from the nearest log, the relabel
spreads outward one ring per pass, and a leaf that reaches seven waits about a
minute — the same distribution vanilla's random tick produces — and then breaks
and drops. All of it is Minecraft's own answer, asked of the operator's own jar:
`canSurvive` per state through a `java.lang.reflect.Proxy`, `FallingBlock` for
what falls, and `LeavesBlock.getOptionalDistanceAt` for the leaf, whose log half
comes out of the tag table because a jar's static initialisation has loaded no
data pack and that half comes back empty.

Against a real 1.21.1 server, over 313 blocks — every one with a state that
needs something — **977 of 1,017 scored rows agree**, 32 are blocks Minecraft
broke and Dust keeps, and 8 are the other way: candles, whose `updateShape`
never consults the `canSurvive` the support columns are extracted from.
Decision record 0040 has the bytecode and names the fix. `node updates.js
<port> --check` puts a torch on a block, mines the block and watches the torch
fall the way a player would: 10 of 10 on a release build, 6 of 10 with the rule
turned off. One break costs 1,271 nanoseconds of a 50-millisecond tick, a
64-block sand column 2,346, a thousand-block raft of sand 75,827, and felling an
oak 1.7 milliseconds spread over four minutes. Decision record
[0040](docs/decisions/0040-what-a-changed-cell-tells-its-neighbours.md) has the
counts, the two ceilings, and the one it found bounding the wrong thing.

The server **keeps the columns its players and its falling items are near**,
once, shared, and reads region files on a thread of the world's own rather than
on the network path or the tick loop. On a world Minecraft wrote, the worst
single movement packet of a walk into new terrain went from 19.2 milliseconds to
0.074, and a falling item's tick from 555 microseconds to 75. A column is 111
kilobytes, measured, not the megabyte three modules claimed. Decision record
[0025](docs/decisions/0025-who-keeps-a-chunk-column.md) is the account, and says
what it costs at 1, 10 and 100 players — which is *more* memory, for a reason
that is about the player and not about the megabytes.

**Not yet**, and each of these is stated where the code for it would go: **no
features and no structures**, so a generated world has no trees or ore veins,
18,433 of seed 0's 681,715 open cells below its own surface are still filled in
— mineshafts, dungeons and geodes open cells too and none of them is a carver —
and there are no icebergs; decision records
[0032](docs/decisions/0032-what-the-ground-is-made-of.md),
[0035](docs/decisions/0035-what-a-cave-holds.md) and
[0039](docs/decisions/0039-what-a-carver-digs.md)
are what each of those is worth on the same sample, in cells; **no fluids**,
so water and lava sit where they are put and a bucket does nothing at all,
which is the largest thing left and is decision record
[0040](docs/decisions/0040-what-a-changed-cell-tells-its-neighbours.md)'s
named next step; **no player physics**, so a player is stopped from
entering a block and never pushed out of one; **only one thread builds columns
for a world**, so a cold join into generated terrain is about 3.3 seconds of
world arriving around a player who is already walking — the loading screen ends
at 242 ms, and records
[0031](docs/decisions/0031-how-a-join-streams-its-chunks.md) and
[0038](docs/decisions/0038-how-wide-the-region-lock-is.md) say what a pool would
move and why it is declined —
[0042](docs/decisions/0042-what-a-joining-crowd-costs-a-bystander.md) declines
it a third time, because the stall it was for turned out to be the harness and
a bystander measured from her own process waits 7 ms while four people join;
**no water on the movement path**,
so a player who says they are sprinting and airborne is measured at their feet
rather than at their full height, because they might be swimming and no client
ever says so — decision record
[0019](docs/decisions/0019-how-tall-a-player-is.md) says what that costs and why
it is the direction to be wrong in; **break time is survival-only**, because
`[server] game_mode` defaults to creative and a creative break is instant on
both sides — set it to survival and every block takes Minecraft's own number,
scored 20/20 within a tick against a real 1.21.1 server by
`cargo xtask harness break`, with `just break` as the gate on a running one;
what a survival player has to do with that world is mine, and nothing else,
which is why the default has not moved. Decision record
[0028](docs/decisions/0028-how-long-a-block-takes-to-break.md) has the rule,
the two thresholds a client and a server disagree across, and the eight rows
that caught a bare hand being the right tool for dirt;
**no haste, no mining fatigue and no underwater mining penalty**, the last of
them left out on purpose rather than approximated because a five-fold error
would break the very agreement that keeps a predicted block from coming back;
**Q still destroys a stack rather than throwing it**, and item entities are not
saved, so a restart clears the floor; **a crafting table, a stonecutter and a
smithing table all stay open however far the player walks**, because the reach
check runs when the screen opens and never again — which costs nobody an item,
since everything in them comes back whenever they close it; no **armour trim**,
which is eighteen of the 1,290 recipe files an operator's data pack ships and
the only family left that a bench here opens and cannot make — a trim's result
is a `minecraft:trim` component the *server* has to author, and Dust carries
the components that arrive rather than writing new ones; a
stack carries its data components and a break now reads its
**enchantments** — silk touch, fortune and efficiency all work, scored 27/27
against a real 1.21.1 server and 15/27 with the component withheld — but
**nothing else in the world reads a component**, so a broken chest still drops
a chest without its contents; **armour protects from nothing**, since there is no
damage to protect from yet, and **hunger and health are the two things left
between this server and survival mode** now that crafting has landed; no
**redstone wire** and no **scaffolding distance**, which are the last two
neighbour rules of the sixty-one decision record
[0014](docs/decisions/0014-what-a-block-reads-from-the-cell-next-door.md)
found, and no **rail bend**; twenty-one of the eight hundred and
fifty-six items that place anything still go down in a state Minecraft would
not, which is `cargo xtask harness placement`'s number rather than this
paragraph's — a **hanging sign on a wall** keeps its standing form, because its
wall form faces across the wall rather than out of it and the grid was taken at
one yaw; a crafter's `orientation`, the age of three vines and a note block's
`instrument`, which is the block below it and needs a column of its own; light
that
crosses a chunk boundary — sky light from a neighbour it would have to travel
*through*, and any light at all from a torch on the far side of one — which is
an engine gap and not a data one, and is now the *only* thing between a served
world's light and Minecraft's own: 435 sky cells and 1,163 block cells of 2.4
million inland, costed and declined in decision record
[0010](docs/decisions/0010-how-wide-the-sky-light-volume.md); no plugins;
and the running server still saves its own edits in its own format beside a
world rather than back into it — writing Anvil works, but a chunk's block
entities and scheduled ticks survive a round trip by being *copied*, not because
Dust models them, and a server that edited a chest would be writing a record it
does not understand.

## Try it

```
cargo run -p dust-server -- server
```

Then add `localhost` to a 1.21.1 client's server list. Set `online_mode = false`
in `dust.toml` first unless you want Mojang consulted, and point
`world_source` at a `region` directory if you have a world to serve.

For light, block sounds, placing what you are holding and getting the right
thing out of what you break, put Minecraft's own answers beside your data:

```
cargo xtask extract --version 1.21.1 --only constants
cp .dust-extract/oracle-1.21.1/constants.tsv <[data] path>/dust-constants.tsv
cp .dust-extract/oracle-1.21.1/items.tsv     <[data] path>/dust-items.tsv
cp .dust-extract/oracle-1.21.1/blocks.tsv    <[data] path>/dust-blocks.tsv
```

How much light a block stops, how much it gives off, which of the six heightmaps
count it, whether a block placed there goes into it, what it sounds like going
down, whether it yields anything to the wrong tool, which block each item puts
down and which loot table each block draws from are Java code inside Minecraft
rather than data, so they are in no report, no data
pack and no copy here — the extractor asks your own server jar for them and
writes them to your own disk. Without the files, every block but air stops sky
light, the sky starts above the grass rather than through it, a placed block
makes no sound, a right-click puts the world's own surface block on the face
it clicked whatever the player is holding and whatever is already there, no
block asks for the right tool, and about sixty wall signs, wall banners, wall
heads and coral wall fans yield nothing at all because the file they draw from
is named after another block; the server says so at boot. Decision records
[0008](docs/decisions/0008-block-opacity-and-light-emission.md) and
[0027](docs/decisions/0027-which-tool-a-block-wants.md) are why they arrive this
way rather than in the binary — including why all three tables key on *names*
rather than registry ids, why sixteen items on 1.21.1 make the difference
between a table and a name match, and why fifty-eight blocks make the same
difference in the other direction.

The console takes `stop`, `list` and `say`, with or without a leading slash.

## How it is checked

Two ways, and the second is the one that matters.

The protocol tests **speak the wire by hand** — their own VarInts, their own
length prefixes, their own zlib — sharing no code with the server. A test client
built on Dust's own framing would agree with Dust by construction, under any
convention including a wrong one.

And the formats are **captured from a running Minecraft 1.21.1 server** rather
than read off a wiki: the configuration order, the eleven registries and their
entry counts, the NBT type of every field in a dimension type and a biome, the
offline-mode UUID derivation, and a chunk section decoded field by field until
its 18,779 bytes were consumed exactly.

Doing that found three defects in an afternoon, each with passing tests over it:

- **A player command was one VarInt short.** The jump boost reads as though it
  should be conditional — only the horse-jump actions mean anything by it — and
  the packet was modelled that way. Vanilla reads three VarInts whatever the
  action: sent two it disconnects naming the packet, sent three it carries on.
  Every sneak and every sprint a real client sends carries a zero there, so
  Dust refused all of them.

- **Login Start's shape was inverted.** The transport expected an optional
  profile id behind a presence flag — true in 1.20.2–1.20.4, wrong since
  1.20.5 — so it accepted exactly the two shapes vanilla refuses. *No real
  client could have logged in.* The protocol crate's definition had been right
  the whole time; nothing tied the two together, and now a test does.
- **The offline profile id was derived from a lowercased name.** Vanilla hashes
  the name as typed, so every offline player on Dust had a different identity
  from the one they have on every other server.
- **The status document carried two keys vanilla omits**, and both justifying
  comments had the reasoning backwards.

The lesson is the rule now: a test written from the same understanding as the
code agrees with the code, not with Minecraft.

Underneath: Stage 0's workspace, configuration system and gates; the vanilla
data extractor; and the crates the rest stands on — NBT, world storage with
paletted containers, heightmaps and a light engine, the 1.21.1 protocol codec,
the datapack loader, and the network transport.

## Vanilla data

Dust ships no Mojang data and no Mojang assets. What the repository holds is
the extractor, and the Rust that results from running it:

```
cargo xtask extract --version 1.21.1
```

That resolves the version through Mojang's manifest, downloads the server jar
to a gitignored cache, verifies its SHA-1 against the manifest **on every
run** — including when the jar is already cached — runs Minecraft's own data
generators and regenerates the tables in `dust-registry`, `dust-protocol` and
`dust-gen`. It needs a network and a JDK 21 or newer, runs by hand a few times
per Minecraft release, and is deliberately not part of `just verify` — what CI
checks is the generated code.

The work is split into domains: blocks, items, entities, fluids, tags,
recipes, loot, commands, packets, worldgen and light. A full run regenerates
everything; each domain prints what it found and how long it took.

`light` is the odd one and is odd on purpose. How much light a block state
costs to enter and how much it gives off are Java code in Minecraft — in no
report, no data pack and nothing the generators emit — so that domain runs an
**oracle**: a small Java program on the jar's own classpath that boots
Minecraft's static initialisation and reads `getLightBlock` and `lightEmission`
off every state in the block-state registry. It writes a table to
`.dust-extract/` and, unlike every other domain, **generates no Rust**: those
are Mojang's numbers and they stay on the operator's disk. Decision record
[0008](docs/decisions/0008-block-opacity-and-light-emission.md) is why, and
`harness light` is what checks the result. Two things
make re-runs cheap:

- **The generator output is cached.** The `--reports` and `--server` trees are
  kept under `.dust-extract/`, keyed by version, and reused until deleted —
  running Minecraft's generators is the slow part, and nothing about reading
  them gets faster by repeating it.
- **`--only` extracts one domain at a time**: `cargo xtask extract --version
  1.21.1 --only tags` reads the cached trees and rewrites just that domain's
  table. A misspelled domain is refused rather than quietly extracting
  everything.

A full cold run — download plus both generators plus every table — takes a few
minutes, almost all of it inside Java. A warm run against the cache takes
seconds.

## Point a third-party client at it

```
cd tools/bot && npm install
just bot 25565
```

`mineflayer` implements the client protocol independently and shares no code
with this project, which is why it finds what a test suite agrees with itself
about. `tools/bot/check.js` joins, checks that the dimension it was told about
is the one it is in, that it has all sixty-four biomes, that it can read a
block, that a second bot's chat line arrives with the sender's name on it, and
that its swing, crouch, block-break and block-place all reach the first, that a
stair goes down facing the way it stood, and that a block fifty blocks away
reaches nothing —
twenty-one checks, exit 0 or 1. `tools/bot/README.md` has the list and what it has
caught.

`just soak <port> <minutes>` is the long version, and a different question:
`check` asks whether this works, `soak` asks whether it keeps working. A bot
stays for ten minutes, flies a forty-block square, digs at every corner and
talks, and what it watches for is *ending and stopping* — a connection dropped,
a keep-alive that stopped arriving, thirty seconds of silence. Ten minutes on a
real world: no failures, 15,634 packets, 7,633 columns streamed and 7,344
forgotten across 144 legs — about five and a half thousand blocks flown.

Both are deliberately outside `just verify`: they need a server already running,
an npm install and a `[data] path`, and `verify` is CI's list in CI's order. The
short one has already earned its keep — the break check caught the dig path
firing twice, the second one breaking air and sending a puff of particles made
of nothing.

## Differential testing

Testing against vanilla is the highest-value test this project will have: run
the real server and Dust over identical inputs and let Mojang's implementation
argue with ours. The harness provisions a vanilla server,
fingerprints a world it generates, compares fingerprints — and puts Dust's own
code in the loop:

```
cargo xtask harness provision --version 1.21.1 --seed 0 --yes
cargo xtask harness capture --version 1.21.1 --seed 0 --radius 2
cargo xtask harness compare captures/a captures/b
cargo xtask harness rewrite --version 1.21.1 --seed 0 --radius 2
cargo xtask harness registries --version 1.21.1
cargo xtask harness light --version 1.21.1 --seed 0 --radius 2
cargo xtask harness worldgen --version 1.21.1 --seed 0 --radius 2 --at 0,0 --at 300,300
```

`provision` resolves the server jar through the same manifest-and-SHA-1 path
the extractor uses (verified on every run, including cache hits), writes a
run directory tuned for headless determinism into the harness cache, and —
only with `--yes` — accepts Minecraft's EULA on your behalf by writing
`eula.txt`. Without that flag the file is left unwritten and vanilla refuses
to boot until you have read the EULA and chosen; agreeing to a licence is an
act, and the flag keeps it visible in your shell history where it belongs.

`registries` is the same idea one layer up, over the protocol rather than the
world. It boots Minecraft, boots Dust in the same process as the command, and
points one hand-written client — its own VarInts, its own zlib, sharing no code
with either server — at both, acknowledging no data packs so that both send the
registries' *contents* rather than their names. As of 2026-08-30 it reports no
differences: ten registries agree entry for entry and field for field, and all
thirteen tag registries agree over all 6,362 ids. The eleventh registry,
`minecraft:enchantment`, is listed as a stated omission rather than a
difference — Dust has no schema for it and says so in code, and the day one is
added and is wrong, this goes red. Watched to fail: changing one field's type
from `TAG_Double` to `TAG_Float` produced four findings naming the field.

`light` puts a number on how close Dust's light is, **both kinds, reported
apart** — a single figure covering both would read as "the lighting is 99.9%
right" while hiding which half of it was. A chunk Minecraft wrote carries the
light Minecraft computed, so the same chunks can be lit again with Dust's engine
and compared cell by cell.

It measures a **ladder**: five models over the same chunks in the same run,
each row the one above it plus a single named change, and it now covers **both
kinds of light**.

```text
seed 0, radius 2                                sky short   block short
  no table at all                                  14,276         7,185
  + Minecraft's own opacity and emission              611         1,163
  + Minecraft's own heightmap predicates              435         1,163   <- a server
  + a 3x3 volume of columns                             0             0
  + the heightmaps Minecraft wrote                      0             0
```

On seed 1 the second row's sky light is already zero, over 4,816,896 cells.

Block light's percentages need reading with care and the counts do not: most of
a world has no block light, so a server that computed none still "agrees" with
99.7% of cells, and the 7,185 in that first row is *every lit cell in view*.

**Every disagreement is accounted for, in both kinds of light.** Given
Minecraft's own answers to the questions Dust asks about a block, and a volume
wide enough for light to cross, the walks reproduce the light Minecraft computed
exactly — on both seeds and at every radius. Lighting has four inputs and only
one of them is the engine; that row is what says the walks are right and
everything above it is something Dust is *told* about the world.

The fifth row is a control and agrees with the fourth. It skips the recompute
and takes the heightmaps out of the chunk as Minecraft wrote them — not a mode
a server can run in, since an edited chunk has a heightmap its file does not.
Its agreeing is the statement that Dust's recompute, given Minecraft's
predicates, *is* Minecraft's heightmap and not merely close to it.

A server with a `dust-constants.tsv` stands on the third row. What separates it
from exact is the multi-column volume, costed and declined in decision record
[0010](docs/decisions/0010-how-wide-the-sky-light-volume.md); why the numbers
come from the operator's jar rather than from here is
[0008](docs/decisions/0008-block-opacity-and-light-emission.md).

**Getting there found a defect the stand-in had been hiding for the whole of
the light engine's life.** Minecraft's numbers on their own moved seed 0 by a
hundred and seven cells. The shortfall did not shrink; it got *shallower* —
6,128 cells short by fourteen became nineteen short by thirteen. Light was
reaching under the water and arriving at half the level it should, because the
engine charged `1 + opacity` for a step where Minecraft charges
`max(1, opacity)`. Nothing could see it while the only opacity model answered
0 or 15, because the two rules agree at both ends. A wrong constant hidden by
another wrong constant.

**A 5x5 volume buys exactly nothing** over a 3x3 — which is the argument for a
finite volume confirmed rather than assumed. Light loses a level
a block and a chunk is sixteen of them, so one ring of neighbours is not an
approximation of the infinite volume; it is the infinite volume.

**The report splits the shortfall by how far each cell sits from its column's
edge**, because light from a neighbouring column enters at a face and fades
inward while opacity does not care where in a column a cell is.

```text
distance from a face    0      1      2      3      4      5      6      7
seed 0, air only     0.660  0.595  0.561  0.548  0.530  0.510  0.530  0.581
seed 0, Minecraft's  0.072  0.021  0.008  0.007  0.005  0.005  0.006  0.018
```

Flat, then falling by an order of magnitude — two different causes, each
visible as a different shape. What it cannot do is separate a cause nobody
proposed: it read "flat, therefore opacity" and was right, and was equally
right about the step cost and about a heightmap predicate that put the sky
floor above a flower. The rate and not the count is the whole measurement: a column has `60 - 8d` columns at distance `d`, sixty at the face
against four in the middle, so a raw count reads as "it is all at the edges"
for a perfectly uniform cause.

Under the stand-in the percentage was a **property of the world rather than of
the engine**, which was worth knowing before quoting it: seed 0 read 99.4% and
seed 1 read 96.4% with the same server, because 168,428 of seed 1's 169,480
shortfalls were water. The world that was worst is the one that comes out exact
first.

It is a measurement and not a gate — a verb that failed for a known gap would be
red every time it ran — and **there are no over-lit cells, at any radius, either
seed or either model.** Getting to that took three corrections and every one was
the harness rather than the engine. It lit each column against *itself* on all
four sides: 805 over-lit cells. It compared chunks vanilla had not finished
lighting: 167,000 more, and the agreement fell to 98.1% with no change to the
engine. And it took sky floors from *neighbours* vanilla had not finished: the
last thirty-two, every one within a step of a chunk edge. Separating over-lighting
from under-lighting is what made all three visible instead of letting them hide
inside a number that already looked good.

`worldgen` asks the same question of the terrain, and asks it in five parts. It
counts how far Dust's world is from the one Minecraft generates for the same
seed, and which stage of vanilla's pipeline owns which part of the gap. Eleven
models over the same chunks in one run, each row the one above it plus a single
named change, and every figure a count of things **wrong**:

```text
seed 0, twelve 5x5 squares to sixteen thousand blocks out -- 17 biomes in view

  surface  surface     biome    caves     false      blocks  KiB/col
    short    block     short  missing     caves       short
    76800    76800    435459        0   9598921    10005374      2.2  the flat world Dust served
    74905    74931    435459   583625    795317    10475058     16.2  + the world's own sea level
    74905    74931       382   583625    795317    10475058     16.6  + Dust's biome source
    28796    49128       382   588215     75560     6840919     18.8  + Dust's terrain
    28796    32398       382   588215     75560     2529567     18.8  + Dust's surface rules
    28784    32393       382   187614     90892     2097950     18.8  + Dust's aquifers
    28349    32345       382    18433     92418     1838763     18.8  + Dust's carvers
        0    60037         0   681715         0    10405644     19.6  + Minecraft's surface height
        0    60037         0        0         0     9723929     19.6  + Minecraft's carvers
        0        0         0        0         0       12140     20.6  + its blocks at and below it
        0        0         0        0         0           0     20.7  + its blocks above it (control)
```

Out of 76,800 columns, 29,491,200 cells and 460,800 biome cells. The last row is
a control: it hands over every block and every biome, so a non-zero anywhere in
it is the harness's fault and not the generator's. It is exact on both seeds,
and it is checked in CI against chunks the harness builds itself — watched to
fail in both directions.

**Five scores and not one, because a percentage hides which half it is about.**
Putting the flat world's grass at sea level fixes 1,895 columns' surface height
and makes the block count **469,684 worse**; one number would have called that
a regression. The flat world scores 100% on caves — a world with no rock above
y -60 contains every cave Minecraft carved — which is why the summary prints
counts and a "false caves" column and no rate at all.

**Row two is Dust's own generator now.** The biome source samples the six
climate values out of the operator's own density functions and matches them
against their parameter list, and it takes 435,459 wrong cells to **382** — on a
sample that reaches seventeen biomes rather than the two a single square holds.
Every one of the 382 is an **exact tie** in climate space, broken the other way;
so are all 238 on seed 1. Decision record
[0021](docs/decisions/0021-which-biome-a-cell-gets.md) is why matching them is
declined.

**Row four is what a server now serves.** `final_density` over the 4x8x4 cell
Minecraft interpolates across takes 74,905 wrong columns to **28,796**, and 3.6M
cells of block disagreement with it. Nearly every column left has a tree on it:
`MOTION_BLOCKING` counts leaves, so a column whose ground is exactly right reads
five short with an oak on it, and the summary names Minecraft's own surface
block in the columns whose *height* disagrees — 25,938 of the 28,796 are leaves
and 2,279 more are packed ice. Every delta is negative; Dust is never too high.
Seed 1 goes 72,552 to **16,678** the same way.

**Two seeds, because one cannot see.** Seed 1 spawns in open ocean, where every
column has water underfoot and is short by *exactly one* under the sea-level
rung, because `sea_level: 63` names the level water reaches *to* and the topmost
water block is at 62. It sees twenty biomes to seed 0's seventeen and agrees
about almost no other number here.

**The shape of the sample matters more than its size.** `--at` is repeatable:
the twelve scattered 5x5 squares above hold not quite four times the chunks of a
single 9x9 and reach eight to ten times as many biomes. A biome source scored on
one square is not being scored, it is being asked whether one of two answers
came out.

Cost is scored beside accuracy, because this code runs for every chunk a player
walks toward: a real column's blocks are 20.7 KiB against a flat one's 2.2, and
**light is 96 KiB more per column whatever the terrain** — at the default view
distance a join is 5.8 MiB of blocks once terrain is real and 27 MiB of light
either way. Decision records
[0012](docs/decisions/0012-what-worldgen-is-worth-measured-first.md) and
[0021](docs/decisions/0021-which-biome-a-cell-gets.md) are what the ladder was
built to write: what each stage is worth, what it costs, and the order to build
them in.

`capture` boots the provisioned server headless, watches its own log for the
readiness line, force-generates the square of chunks within `--radius` chunks
of each `--at` centre with `forceload`, asks it to `save-all` every ten seconds
until every chunk has reached disk, flushes,
stops the server over RCON, and then reads the region files directly — anvil
layout, chunk decompression, a minimal NBT walk — to produce one digest per
chunk: a block-state multiset hash (order-independent), a biome hash, and
per-heightmap hashes. Output lands as `chunks.bin` plus a human-readable
`chunks.tsv`. `harness rcon` stands alone for talking to a running server.

`compare` diffs two capture sets and prints one row per chunk that is missing,
extra or divergent, with both digests side by side:

```
$ cargo xtask harness compare 1.21.1-seed-0-radius-2 1.21.1-seed-0-radius-2-rerun
comparing seed 0 data version 3953: 25 chunks vs 25 chunks
identical
```

Its exit codes are for scripts: **0** when identical, **1** when they differ
(a finding, not a failure), **2** when the comparison could not run at all.

`rewrite` is Phase 2's exit criterion made runnable. It copies the provisioned
world, rewrites every chunk through Dust's Anvil reader and writer, boots vanilla
on the copy, and compares what vanilla read back against the capture of the world
it started as. It found a defect on its first run that nothing in the test suite
could have: the reader had never read `Heightmaps` at all, which is invisible
in-process because the one caller that serves chunks recomputes them first.

**The digest is not the whole check.** Vanilla does not fail on a chunk it
cannot read — it logs, discards it and regenerates it from the seed, so the
server boots, the capture completes, and nothing in the digests says anything
went wrong. Measured, by scrambling 200 bytes of one chunk and leaving its
header intact: vanilla logged four errors about it and then printed
`Done (4.392s)!` and ran.

So the criterion's other words are checked separately: everything vanilla says
is kept, and the transcript of the run over Dust's world is diffed against the
transcript of the run over vanilla's own. Anything new is a finding — a diff
rather than a list of known-bad strings, because a list can only fail on what
whoever wrote it already thought of.

The two checks overlap more than expected in that experiment: the regenerated
chunk digested differently as well, because regenerating one chunk into a world
whose neighbours are already finished loses the decoration those neighbours
would have contributed. Where they do *not* overlap is the failure this writer
can actually cause. A digest covers blocks, biomes and heightmaps and nothing
else, so a carried block entity whose block Dust has since broken — a record
vanilla drops and logs about, with every block still exactly where it was —
is visible only in the transcript.

Two honesty notes. First, what is seed-stable: terrain, biomes, ore and
structure placement are stable for a fixed seed and version, and that is
exactly what the digest covers; everything clock-shaped (mob cycles, weather,
container loot) is excluded by construction rather than filtered afterwards.
Second, where things live: nothing Mojang ships and nothing vanilla generates
is ever committed. Jars, worlds and digests stay under the harness cache —
outside the repository, shared by all worktrees, movable with
`DUST_HARNESS_CACHE`. Each verb's own usage (`cargo xtask harness`) carries
the operational details.

## Building

```
just verify
```

That is CI's command list in CI's order — formatting, lints, tests, the
generated configuration reference, the dependency licence audit and the build.
It is deliberately not a subset of what CI runs, because a local gate that skips
steps produces confidence in exact proportion to what it skipped.

## Configuration

One `dust.toml`. See [`dust.toml.example`](dust.toml.example) to start, and
[`docs/configuration.md`](docs/configuration.md) for every setting Dust has.

That reference is generated from the server's own types by `cargo xtask docs`,
and a setting with no documentation does not compile. There is no third place
for a setting to hide.

## Decisions

The reasoning behind the things that are hard to change later is in
[`docs/decisions/`](docs/decisions/): why Dust is written from scratch, why it is
GPL-3.0, why it targets 1.21.1 first, why it is Rust throughout, why ore
density is configured the way it is, which of Minecraft's numbers may live here
rather than on the operator's disk, how wide a volume the sky light is computed
over — a record of a thing measured and deliberately *not* built — and how a
placed block gets its state, which is the first value the oracle route cannot
carry and the first one to be answered with rules and a check instead of a
table.

## Licence

GPL-3.0-only, copyright Ledgeworth Studios. See [LICENSE](LICENSE) and
[NOTICE](NOTICE).

Dust ships no Mojang data and no Mojang assets. Minecraft is a trademark of
Mojang Synergies AB; Dust is not affiliated with or endorsed by Mojang or
Microsoft.
