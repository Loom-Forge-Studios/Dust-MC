# D44 — How the world clock is kept and told

**Status:** Built and measured, 2026-09-07. **The sun moves.** It costs 2.5
nanoseconds a tick and eighteen bytes per player per second, and both numbers
are in `benches/daylight.rs` rather than in anybody's estimate.

Until this, `net/play.rs` held a function called `frozen_at_noon` whose own doc
comment admitted the problem: it sent `SetTime { world_age: 0, time_of_day:
-6000 }`, a negative day time is the protocol's way of saying the cycle is
stopped, and nothing in the server ticked a clock. Every player who has ever
joined Dust stood in a permanent, unmoving midday. This record is what replaced
it, and the four decisions inside it that could reasonably have gone the other
way.

## The sun is an offset, not a counter

Minecraft keeps two numbers. **Game time** is every tick the world has run;
**day time** is where the sun is. They are not the same number — the sun stops
when `doDaylightCycle` is off and game time does not — and vanilla stores them
as two independent longs, `Data.Time` and `Data.DayTime`.

The first version here did the same: two `AtomicU64`s, two `fetch_add`s a tick.
It was wrong in a way a test caught within the hour. `SetTime` carries both, a
session builds one from two separate atomic loads, and a tick landing between
those two loads produces a packet whose halves are one tick apart. The join
conversation test asserts that a fresh world opens at dawn, and it saw a world
that had opened at 999.

So day time is kept as a **difference from game time**:

```text
   day_time = game_time + offset      (cycle running)
   day_time = offset                  (cycle stopped)
```

all in wrapping `u64` arithmetic — the offset of a world a million ticks old
whose sun is at 6,000 is an enormous number that wraps back to 6,000 when the
age is added, which is modular arithmetic doing exactly its job. Two things
fall out, and both are the reason:

- **A tick is one atomic addition**, not two, whatever the cycle is doing. The
  bench measures 2.48 ns either way, and the two rows agreeing is the check
  that this is still true.
- **`WorldClock::reading` loads the age once and derives the sun from it**, so
  the pair in one packet cannot be a tick apart. It is not a rounding of the
  problem; it is a representation in which the problem does not exist.

What it costs is one branch on `cycle` per read, and a note that turning the
daylight cycle into a game rule someday means rebasing the offset at the moment
it changes, because the offset means a different thing on each side of it. That
note is on the field.

Day time is **not** reduced into `0..24_000`, which looks like the obvious
thing to do and is wrong for one concrete reason: `/time query day` is
`day_time / 24_000`, and a clock that wrapped internally could not answer it.
What wraps is the sun, at the moment it is read.

## Once a second, and once a tick when somebody asks

Vanilla broadcasts `SetTime` on `tickCount % 20 == 0` — once a second, not
once a tick — because **the client runs its own sky between packets** and is
only being corrected. Dust does the same, from a `tokio::time::interval` per
session, and the interval starts one full period out rather than immediately:
the join burst has already sent this client the clock, and an `interval`'s
first tick fires at once.

Measured, `benches/daylight.rs`:

| what | cost |
| --- | --- |
| one tick of the clock | 2.48 ns, cycle running or stopped |
| one `set_time`, framed | 18 bytes |
| building and encoding one | ~120 ns |
| 100 players, once a second each | 1.8 kB/s, 0.0012% of a tick |
| 1,000 players | 18 kB/s, 0.013% of a tick |

For scale, one chunk column costs 111 kB to send once. The entire day-night
cycle for a full server is a sixtieth of a chunk a second. There is nothing
here worth tuning, and the configuration setting below is not a performance
knob.

**One place Dust is deliberately quicker than vanilla.** Vanilla's `/time set`
moves the clock and lets the next 20-tick broadcast carry it, so a player can
watch up to a second pass before midnight arrives. Every Dust session already
wakes once a tick for item pickups, so that arm compares one atomic — a counter
bumped only by `/time set` and `/time add`, never by the passage of time — and
sends immediately when it has moved. One relaxed load per player per tick,
against a command that is on screen the tick it was typed. The bot check
sees the sun at 13000 within 300 ms of `/time set night`, where the once-a-second
broadcast alone could have left it up to a second behind.

## Where the time lives, and which record wins

Two files hold it and one of them is Minecraft's.

**Dust's own save** (`world/dust-edits.json`) grows a `time` key. It did **not**
bump `SAVE_VERSION`, and that is the interesting half: every previous bump
added a key whose absence had a correct meaning, and so does this one. A file
with no `time` is a world from before Dust had a clock — a world that stood at
noon forever and therefore has no time of day worth restoring. A bump would
have made every existing save unreadable in order to say the same thing.

**The world's `level.dat`** is read *and written*, `Data.Time` and
`Data.DayTime`, exactly as vanilla spells them. This is the only place in Dust
that writes a file Minecraft also writes, and it needed an argument:

- *For:* a world dropped into `world_source` opens at the hour its owner left
  it, rather than at dawn. And a world Dust has served for a week, opened in
  Minecraft, shows the time it was left at rather than the time it was
  imported. Without the write, the read alone is a one-way trip.
- *Against:* a read-modify-write of an operator's world file through a
  third-party NBT implementation is exactly the kind of thing that quietly
  loses a tag.

The against is answered rather than waved away. `dust-nbt` implements all
thirteen tag types and round-trips them under a property test; the write parses
the whole document, replaces two keys, and writes everything else back
untouched, to a temporary file that is renamed over the original. A document it
cannot read is **refused**, not partially rewritten, and the test that says so
also checks the original bytes are still there afterwards. A `level.dat` that
does not exist is never created — two keys and nothing else is not a world, and
Minecraft would refuse the directory it appeared in.

**Precedence on load is: Dust's save, then `level.dat`, then dawn.** The order
matters in one case and it is worth stating. Both are written from a single
reading at shutdown, so they agree — unless the `level.dat` write failed (a
read-only directory, a file open in an editor), in which case it holds an older
time and a server that preferred it would walk the world backwards a little on
every restart.

Ticks are stored, not a wall-clock deadline, which is the same decision the
furnace made one register down: **a world does not age while the server is
off.** Advancing the clock over the downtime would mean logging in after a
weekend to a random time of day, and on a server people take turns hosting, to
a different random time of day for each of them.

## `/time`, out of Minecraft's own command graph

A client does not discover commands; it is told them, once, as a brigadier node
graph, and everything it then does locally — tab completion, the red underline
while you type, whether `1d` is a legal argument — is that graph's doing.

`net/commands.rs` does not write those nodes. `cargo xtask extract` already
committed the whole vanilla graph as `dust_registry::commands`, so this takes a
**subgraph**: a list of declared command paths, and everything reachable from
them copied into the wire form with its shape intact. A client's parser agrees
with vanilla's by construction, and adding a command is adding a name to a
list. Fourteen nodes go out on join: the root, `/time`, and its whole subtree.

Two protocol bugs fell out of converting the graph, and both were found by a
test that converts **all 1,763 nodes** rather than the thirteen this server
needs:

1. **`minecraft:score_holder` was decoded under parser id 31.** It is id 30 —
   it sits immediately after `minecraft:scoreboard_slot` in vanilla's
   registration order, and 31 is `minecraft:swizzle`, which takes no properties
   at all. So the arm was unreachable and every real `/scoreboard` or `/trigger`
   node in a vanilla `commands` packet was refused as "known but not modelled".
   The same wrong number was in the encoder's `debug_assert`, which now asks
   `PARSERS` instead of carrying its own copy of the table it guards.
2. **The report omits `properties` for an unbounded numeric argument; the wire
   does not.** `brigadier:double` always writes a flags byte, and a client that
   read the next node's first byte as those flags would misparse the rest of the
   packet. An absent block becomes an empty one.

Neither could have been found by declaring only `/time`, which is the argument
for the wide test.

### `minecraft:chat_command` was on the blocked list and never belonged there

`dust-protocol` listed it under the chat-signing wall, on the reasoning that
"unsigned commands still ride the signed-chat envelope". True of 1.20.4. In
1.20.5 Mojang split the packet: `chat_command_signed` kept the timestamp, the
salt, the per-argument signatures and the acknowledgement chain, and
`chat_command` was left holding **a single string**. A client sends the signed
half only for a command with an argument the server declared as
`minecraft:message` — `/say`, `/msg`, `/me`. Everything else arrives unsigned.
Confirmed against `minecraft-data`, a separate implementation, which describes
the same one-field container. The signed half stays blocked; that wall is real.

The string is bounded at the protocol default rather than at chat's 256. The
vanilla chat box will not let anyone type more than 256, but the packet does not
say so, and a decoder that refuses what the format permits disconnects a client
instead of answering it.

### What is not decided here

**Anyone may run it.** Dust has no operator list — the console can `say`, `list`
and `stop`, and no player has a permission level — so a declared command is a
command every player has. On a server where any player can already break any
block that is consistent, and it is also the thing that has to change first
when the second declared command is one that should not be. Said out loud here
rather than left to be discovered.

**Feedback is English text, not a translation key.** Vanilla sends
`commands.time.set`, whose English is `"Set the time to %s"`, and a translated
component's arguments live in a `with` list that `dust_protocol::text::Component`
does not model. A key with no arguments renders the `%s` literally, which is
worse than English for everybody. The day `Component` grows `with`, this is
three lines.

## The setting

`[server] daylight_cycle`, default **on**, which is vanilla's `doDaylightCycle`
as a setting because Dust has no game rules yet. Off, the clock stands where it
was left and the client is told so as a negative day time, so the sun stops
rather than jittering; the world's *age* still counts up either way, so anything
measuring elapsed ticks keeps measuring them.

It is not a performance setting and turning it off saves nothing: the sun is an
offset, so a stopped cycle is the same single addition.

## What a real client says

`tools/bot/daylight.js`, and `just daylight`. mineflayer shares no code with
this project and its `chat_command` writer had never been pointed at this
server before, because the packet was on the blocked list. Against a release
build, **14 of 14**:

```text
ok    the sun moves — day time went 37492 -> 37552 in 3000 ms
ok    and at twenty ticks a second, within 10% — 60 ticks where 60 were owed
ok    the world age moves with it — age went 2202 -> 2262
ok    a positive day time, which is what says the cycle is running
ok    about one packet a second, not one a tick — median gap 1000 ms
ok    /time set night is accepted and answered — "Set the time to 13000"
ok    and the client is told inside a tick or two — day time is now 13000
ok    /time query daytime reads back what was set — "The time is 13006"
ok    /time query day answers — "The time is 0"
ok    /time query gametime answers — "The time is 2273"
ok    /time add takes a unit — "Set the time to 13006"
ok    and nonsense is refused rather than run — "Expected float, got \"\""
ok    a player joining at dusk arrives at dusk — joined at 13030, world at 13015
ok    and not at the frozen noon this server used to serve
```

The last two are the ones it exists for. A client draws whatever it is told
first, so a join that says noon is a sky that visibly snaps a second later —
which is exactly the server every player met before this.

The restart half the bot cannot do, because it does not own the server, was
done by hand and is the reason this record can claim it:

- A flat world served for 1,030 ticks stopped at `day 1, tick 13743`, wrote
  `{"game_time": 1030, "day_time": 37743}`, and started again at **day 1, tick
  13743, from this server's own save**.
- A world with a `level.dat` written the way vanilla writes one — `Time` 555,
  `DayTime` 18000, plus `SpawnX`, `SpawnAngle`, `LevelName` and `DataVersion` —
  opened at **midnight, from the world's own level.dat**, ran for 882 ticks, and
  on shutdown its `level.dat` held `Time` 882 and `DayTime` 18327 **with every
  other key intact and in its original order**. Deleting the Dust save and
  starting again resumed at tick 18327 out of `level.dat` alone.
- With `daylight_cycle = false`, four consecutive readings over 3.5 seconds:
  `[1881,-37370] [1901,-37370] [1921,-37370] [1941,-37370]`. Negative, still,
  and the age counting.

## What this is not

Sleeping, beds, phantoms, weather and mob spawning are all things a day-night
cycle eventually implies and none of them is here. This is the clock, its
persistence, its packet and its command.
