# Dust — developer commands.
#
# `just verify` is CI's command list, in CI's order. Not a subset of it: a local
# gate that skips steps produces confidence in exact proportion to what it
# skipped, and the steps it skips are where the failures are.
#
# The CI workflow's step list is derived from these recipes for the same reason.

default:
    @just --list

# Everything CI runs, in CI's order. Run this before every push.
verify: fmt-check lint test docs-check licenses build

# Formatting, as a gate.
fmt-check:
    cargo fmt --all -- --check

# Formatting, applied.
fmt:
    cargo fmt --all

# Lints. Warnings are errors here; a warning nobody has to fix is a warning
# everybody stops reading.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# The test suite.
test:
    cargo test --workspace --all-features

# The generated configuration reference must match the configuration types.
docs-check:
    cargo xtask docs --check

# Regenerate the configuration reference.
docs:
    cargo xtask docs

# Every dependency's licence must be one a GPL-3.0 work may incorporate.
licenses:
    cargo xtask licenses

build:
    cargo build --workspace --all-features

# Point a third-party client at a running server.
#
# NOT part of `verify`, and that is the one recipe here that is deliberately
# outside it: this needs a server already running, an npm install, and a
# `[data] path` in its configuration. `verify` is CI's list in CI's order and
# a step CI cannot run has no business in it.
#
# It is still the first thing to run after any protocol change. mineflayer
# shares no code with this project, which is why it finds what the test suite
# agrees with itself about. See tools/bot/README.md.
bot port="25565":
    cd tools/bot && node check.js {{port}}

# Whether a block whose support is gone actually goes: a torch on a mined
# block, a control that must not move, a sand column that has to become an
# entity and land, and a leaf that has to learn its distance from the log put
# beside it.
#
# Outside `verify` for the same reason `bot` is: it needs a server already
# running and an npm install. Decision record 0040 is its account.
updates port="25565":
    cd tools/bot && node updates.js {{port}} --check

# The long one: a bot that stays, flies a square, digs and talks for a while,
# and says whether anything ended or went quiet. Phase 3's exit criterion asks
# for ten minutes, which is the default.
#
# Outside `verify` for the same reasons `bot` is, plus one of its own: ten
# minutes is not a pull-request gate.
soak port="25565" minutes="10":
    cd tools/bot && node soak.js {{port}} {{minutes}}

# What a real client's movement packets actually contain, as counts — and,
# with `check`, whether the server corrects one that lies.
#
# Outside `verify` for the same reason `bot` is: it needs a server already
# running and an npm install. It is what decision record 0017's table came
# from, and re-running it is how a change to `[server] movement_speed_limit`
# is argued about with numbers rather than opinions.
movement port="25565" check="":
    cd tools/bot && node movement.js {{port}} {{ if check == "check" { "--check" } else { "" } }}

# Whether a player who claims to be standing inside a block is put back —
# and, as the control that makes the answer mean anything, whether a move of
# the same length that ends in open air is left alone.
#
# Outside `verify` for the same reason `bot` is. Run it against both worlds:
# a flat one and a `world_source` of region files exercise different halves
# of the world lookup, and the second is the one with a column cache in it.
# What a player feels about a broken block, asked of a running server.
#
#   just drops 25565            the gate: drop, pickup, merge, wire cost
#   node drops.js <port> stone,dirt,oak_leaves     what came out, as TSV
#
# The survey half wants a real vanilla server and `--survival`; see the header
# of `tools/bot/drops.js`. Not in `verify`, for the same reason `bot` is not.
drops port="25565":
    cd tools/bot && node drops.js {{port}} --check

# How long a block takes to break, asked of a running server.
#
#   just break 25601
#
# **Needs a server whose `[server] game_mode` is survival.** On a creative
# server every answer is one tick, correctly, and a check that passed on both
# would be a check about nothing — the first row asserts against that. Needs a
# `[data] path` with a `destroy_speed` column too; without it the same five
# rows go red, which is the negative control decision record 0028 records.
#
# Outside `verify` for the same reason `bot` is: it needs a server already
# running, and this one needs a particular configuration of it.
break port="25565":
    cd tools/bot && node drops.js {{port}} --check-times

collide port="25565":
    cd tools/bot && node collide.js {{port}}

# Whether the sun actually moves, asked of a running server.
#
#   just daylight 25565
#   node daylight.js <port> --report     print the readings, assert nothing
#
# A bot watches `update_time` for four seconds and checks that the sun climbs
# at twenty ticks a second and arrives once a second rather than once a tick;
# runs every arm of `/time` and reads the answers back; and then joins a
# *second* bot into the world it has just moved to dusk, which is the check the
# whole thing exists for — a player joining mid-evening has to arrive
# mid-evening, and the first `update_time` they are sent is what decides it.
#
# What it cannot check is a restart, because it does not own the server: stop
# the server, look at `world/dust-edits.json` and the world's `level.dat`, and
# start it again. Decision record 0044 is the account.
#
# Outside `verify` for the same reason `bot` is.
daylight port="25565":
    cd tools/bot && node daylight.js {{port}}

# What four people joining at once does to somebody already standing there.
#
#   just join 25565 4          four joiners, one process each
#   just join 25565 4 same     every bot in one process, which is the trap
#
# A settler streams its whole view, then times a chat round trip twenty times a
# second while N others join. The mode is load-bearing and is not a tuning
# knob: with every bot in one node process the settler is timing an event loop
# that four joins have filled with chunk parsing, and that measurement — not
# this server — is what decision records 0031 and 0038 read as a stall. See
# decision record 0042. Outside `verify` for the same reason `bot` is.
join port="25565" joiners="4" where="each":
    cd tools/bot && node join.js {{port}} {{joiners}} {{where}}

# What one player can see of another player's armour and hand.
#
#   just equipment 25565                    record from a running server
#   node equipment.js <port> --out a.json   the same, naming the file
#   node equipment.js --compare vanilla.json dust.json
#
# Three bots: one dresses, one watches, and one joins after everything has
# already happened. The third is the point — a server that sends equipment only
# on change looks perfect to the watcher and leaves the latecomer looking at a
# naked player forever. Outside `verify` for the same reason `bot` is, and the
# comparison against a real 1.21.1 server is what decision record 0029 is.
equipment port="25565":
    cd tools/bot && node equipment.js {{port}}

# What a stonecutter and a smithing table do, recorded rather than asserted.
#
#   just benches 25603 dust.json                    record from a running server
#   node benches.js 25703 --survey --out v.json     a real 1.21.1 server
#   node benches.js --compare v.json dust.json      the gate
#
# **There is no single-server check here and that is deliberate.** A
# stonecutter's buttons are an index into a list the *client* sorted, and
# neither side puts that order on the wire — so there is no answer one server
# can give on its own that this script could tell was wrong. The comparison is
# the measurement; `--compare` exits 1 on any disagreement that is not named in
# the script, and on any agreement that is. Outside `verify` for the same
# reason `bot` is, and decision record 0037 is what it produced.
benches port="25565" out="benches.json":
    cd tools/bot && node benches.js {{port}} --survey --out {{out}}
