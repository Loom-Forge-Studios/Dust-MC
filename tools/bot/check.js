// Points mineflayer at a running Dust server and reports what it can see.
//
// mineflayer implements the client protocol independently and shares no code
// with this project, which is the whole value: a check written against Dust's
// own framing would agree with Dust under any convention including a wrong
// one. See README.md for what it has found.
//
// Usage: node check.js [port]        (default 25565)
// Exits 0 if every check passed, 1 with a named failure otherwise.

const mineflayer = require('mineflayer')

const PORT = Number(process.argv[2] || 25565)
const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 30000
const SETTLE_MS = 3000

const results = []
function check (name, ok, detail) {
  results.push({ name, ok, detail })
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`)
}

function bot (username) {
  return mineflayer.createBot({
    host: '127.0.0.1', port: PORT, username, auth: 'offline', version: VERSION
  })
}

// A bot that has reached the world, or a rejection naming why it did not.
// Rejecting rather than throwing keeps a refused connection — which is what a
// server with no `[data] path` does, on purpose — a reported failure instead
// of an unhandled error.
function spawned (username) {
  return new Promise((resolve, reject) => {
    const b = bot(username)
    const timer = setTimeout(
      () => reject(new Error(`${username} never reached the world in ${JOIN_TIMEOUT_MS / 1000}s`)),
      JOIN_TIMEOUT_MS
    )
    b.on('error', e => { clearTimeout(timer); reject(new Error(`${username}: ${e.message}`)) })
    b.on('kicked', r => { clearTimeout(timer); reject(new Error(`${username} was kicked: ${JSON.stringify(r)}`)) })
    b.once('spawn', () => { clearTimeout(timer); resolve(b) })
  })
}

const wait = ms => new Promise(r => setTimeout(r, ms))

// Every column the terrain-dependent checks below work in, as an offset in x
// from where the actor stands. They are named and gathered here because
// `buildGround` floors exactly this list: a check that wants a new column adds
// it here and is given ground with it, rather than finding out on an ocean
// spawn that it never had any.
const STAIR = -2
const DUG = 1
const COBBLE = 2
const SEEDS = 4
const SOLID_FACE = 6
const COLUMNS = [STAIR, DUG, COBBLE, SEEDS, SOLID_FACE]

// How long to keep trying to get the actor onto something solid, and how long
// to let it fall between tries. Eight tries at 700 ms covers a bot that
// arrives falling; over open water it needs one.
const FOOTHOLD_TRIES = 8
const FOOTHOLD_MS = 700

// Every block-changing packet in a session carries a rising sequence number,
// so it is one counter for the whole run rather than one per section.
let sequence = 1

// Whether a cell holds something a player could stand on and place against.
// `boundingBox` is mineflayer's own reading of the block, so water and tall
// grass — the two things an unbuilt working area is made of — answer 'empty'
// exactly as air does.
const solid = block => Boolean(block) && block.boundingBox === 'block'

// Put a block *into* `at`, by right-clicking that cell itself.
//
// A right-click on a replaceable cell — air, water, tall grass — puts the
// block where the cell was rather than on the face that was clicked, which is
// the one placement that needs no solid block to aim at. It is what makes a
// floor buildable over open water at all. On a cell that is already solid it
// would land one higher, so every caller here checks first.
function fill (actor, at) {
  actor._client.write('block_place', {
    hand: 0,
    location: { x: at.x, y: at.y, z: at.z },
    direction: 1,
    cursorX: 0.5,
    cursorY: 1.0,
    cursorZ: 0.5,
    insideBlock: false,
    sequence: sequence++
  })
}

// The ground these checks stand on, built rather than looked for, and where
// the actor ends up standing on it.
//
// Seven of the twenty-nine checks below need a solid cell under the actor's
// feet: the one it digs out and fills back in, the three it places against,
// and the one whose underside it clicks. On a flat world the world provides
// them. On a generated world whose spawn is over ocean — `[worldgen] seed = 1`
// is one — every one of those cells is water, and two separate things go
// wrong. A right-click into water *replaces the water*, correctly, so the
// block lands a cell below where the check looks for it; and the actor, with
// nothing to stand on, sinks about half a block a second, so by the twentieth
// second the far end of the working area is outside its six-block reach.
// Twenty-two of twenty-nine, every run, for those two reasons and no other.
//
// This is the same resolution the "block this check needs is where it was put"
// check already reached, applied to the floor instead of to one block: a check
// that needs an arrangement of blocks builds it rather than hoping to find it.
//
// The foothold comes first and on its own. Until the actor has landed there is
// no fixed position to measure the rest of the working area from, and every
// cell it could aim at is moving away from it while it falls.
async function buildGround (actor, watcher) {
  const cobble = actor.registry.itemsByName.cobblestone
  // 36 is hotbar slot 0, in vanilla's own container numbering.
  actor._client.write('set_creative_slot', {
    slot: 36,
    item: {
      itemCount: 1,
      itemId: cobble.id,
      addedComponentCount: 0,
      removedComponentCount: 0,
      components: [],
      removeComponents: []
    }
  })
  actor._client.write('held_item_slot', { slotId: 0 })
  await wait(500)

  for (let tries = 0; tries < FOOTHOLD_TRIES; tries++) {
    const under = actor.entity.position.offset(0, -1, 0).floored()
    if (solid(actor.blockAt(under))) break
    fill(actor, under)
    await wait(FOOTHOLD_MS)
  }

  // Where it came to rest, which is `stood` for everything below. Read after
  // the fall rather than during it: a neighbour measured from a position the
  // actor is still moving through is a neighbour of somewhere else.
  let stood = actor.entity.position.clone()
  for (let tries = 0; tries < 10; tries++) {
    await wait(400)
    const now = actor.entity.position
    const settled = Math.abs(now.y - stood.y) < 0.01
    stood = now.clone()
    if (settled) break
  }

  // The floor, and one cell of support under the one the actor digs out — the
  // hole is filled back in by clicking the top face of the block below it, and
  // over water there is no block below it.
  const level = stood.offset(0, -1, 0).floored()
  const cells = COLUMNS.map(dx => level.offset(dx, 0, 0))
  cells.push(level.offset(DUG, -1, 0))
  for (const at of cells) {
    if (solid(actor.blockAt(at))) continue
    fill(actor, at)
    await wait(300)
  }
  await wait(SETTLE_MS)

  // Read back from the *other* player, and named rather than skipped. A gate
  // that cannot set itself up has to say so: without this row the seven checks
  // that need this floor would go red with seven different-looking reasons and
  // none of them the real one.
  const missing = cells.filter(at => !solid(watcher.blockAt(at)))
  check(
    'the ground these checks stand on was built',
    missing.length === 0,
    missing.length === 0
      ? `${cells.length} cells at y=${level.y}, standing at ${stood.y}`
      : missing
        .map(at => `${at.x}/${at.y}/${at.z} is ` +
          `${watcher.blockAt(at) ? watcher.blockAt(at).name : 'unknown'}`)
        .join(', ')
  )
  return stood
}

// The two packets a client uses to write its own container, written by hand
// because that is the point: prismarine builds them from its own reading of
// the protocol and Dust decodes them with its own.
function creativeSlot (b, slot, name, count) {
  const item = name
    ? {
        itemCount: count,
        itemId: b.registry.itemsByName[name].id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    : { itemCount: 0 }
  b._client.write('set_creative_slot', { slot, item })
}

// A click, with the client claiming nothing changed. That is legal — the
// changed-slot list is the client's prediction and an empty one predicts
// nothing — and it makes the server's push-back the only thing that can put
// the right answer in this bot's inventory.
function windowClick (b, slot, mouseButton, mode) {
  b._client.write('window_click', {
    windowId: 0,
    stateId: 0,
    slot,
    mouseButton,
    mode,
    changedSlots: [],
    cursorItem: { itemCount: 0 }
  })
}

// Not `named` — `main` declares a `named` of its own for sound events, and a
// const inside a function shadows a module-level one for the whole body. The
// first version of this was called `named`, resolved to the sound lookup, and
// reported every slot as `undefined` while the checks themselves passed.
const carrying = (b, slot) => {
  const item = b.inventory.slots[slot]
  return item ? `${item.name} x${item.count}` : 'nothing'
}

async function main () {
  const watcher = await spawned('Watcher')
  check('a third-party client joins', true)

  // The dimension came from the registry contents the server sent. A client
  // that had fallen back on defaults of its own would still have *a* height;
  // what says these arrived is that they are the overworld's and not a guess.
  const { minY, height, dimension } = watcher.game
  check(
    'it was told the dimension it is in',
    dimension === 'overworld' && minY === -64 && height === 384,
    `dimension=${dimension} minY=${minY} height=${height}`
  )

  const biomes = Object.keys(watcher.registry.biomes || {}).length
  check('it has the biome registry', biomes === 64, `${biomes} biomes`)

  // Reading a block means the chunk packet decoded and the palette resolved.
  const under = watcher.blockAt(watcher.entity.position.offset(0, -1, 0))
  check('it can read the block under its feet', Boolean(under && under.name), under && under.name)

  // Three things another player does that a server can drop without anything
  // else noticing: two that change no block and no position at all, and one
  // that changes a block but whose *effect* is a separate packet.
  let sawSwing = false
  let sawCrouch = false
  let sawBreakEffect = null
  watcher._client.on('animation', () => { sawSwing = true })
  watcher._client.on('entity_metadata', p => {
    const flags = (p.metadata || []).find(m => m.key === 0)
    const pose = (p.metadata || []).find(m => m.key === 6)
    if (flags && pose && (flags.value & 0x02) && pose.value === 5) sawCrouch = true
  })
  watcher._client.on('world_event', p => {
    if (p.effectId === 2001) sawBreakEffect = p
  })
  // A placement has no particles and no level event: the sound is the only
  // packet, and it has to name the sound itself. Kept as the raw packet
  // because the position on it is the thing being checked, and mineflayer's
  // own view of a sound would have applied its own idea of the units.
  let heardPlace = null
  watcher._client.on('sound_effect', p => { heardPlace = p })

  // Chat, which nothing else here covers and which is a whole packet path:
  // the message goes up as `chat`, is rendered server-side with the sender's
  // name kept apart from their words, and comes back down to everybody.
  let heard = null
  watcher.on('messagestr', message => {
    if (message.includes('soup')) heard = message
  })

  const actor = await spawned('Actor')
  await wait(500)
  actor.swingArm('right')
  await wait(500)
  actor.setControlState('sneak', true)
  await wait(500)

  // The floor every check below stands on, and where the actor stands on it.
  // Built first, because on an ocean spawn there is nothing there to measure
  // from and nothing to hold the actor still while it is measured.
  const stood = await buildGround(actor, watcher)

  // Breaking a block **beside** the actor rather than under it. `dig` waits
  // for the server to confirm; the server answers a start-digging as a
  // finished break, which is what a creative client sends and what this server
  // honours.
  //
  // Beside, because this server keeps its edits *and* remembers where a player
  // left. Digging underfoot drops the actor a block, that position is saved,
  // and the next run starts a block lower — after thirty runs against one
  // world the actor is at bedrock and half these checks are reading terrain
  // that is not there any more. The hole is filled back in below, which makes
  // the whole run leave the world as it found it.
  const target = actor.blockAt(stood.offset(DUG, -1, 0))
  if (target) {
    try { await actor.dig(target) } catch (e) { /* the effect is the check */ }
  }
  await wait(SETTLE_MS)

  // Placing. Everything below is written by hand rather than through
  // `bot.placeBlock`, which wants a held item mineflayer can only get from an
  // inventory this server does not keep. The packets are the same packets, from
  // a library that has never seen Dust's encoder.
  //
  // Two items, chosen for what each one can fail at. Cobblestone is the
  // ordinary case: a player holds a thing and that thing goes down. Wheat
  // seeds are the case a server that matched item names to block names gets
  // wrong — they place `minecraft:wheat`, and `minecraft:wheat` the *item* is
  // what bread is made of and places nothing at all.
  const held = [
    { item: 'cobblestone', block: 'cobblestone', sound: 'block.stone.place', at: COBBLE },
    { item: 'wheat_seeds', block: 'wheat', sound: 'item.crop.plant', at: SEEDS }
  ]
  for (const [slot, what] of held.entries()) {
    const id = actor.registry.itemsByName[what.item]
    // 36 is hotbar slot 0. The offset is vanilla's own container numbering.
    actor._client.write('set_creative_slot', {
      slot: 36 + slot,
      item: {
        itemCount: 1,
        itemId: id.id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    })
  }
  await wait(500)

  // Each on its own block, two apart, so neither placement is on top of the
  // other and neither is the block that was dug.
  //
  // Each cell is broken before it is placed into. Without that the check
  // depends on what the last run left there — this server keeps its edits
  // across a restart — and a placement into a cell that already holds that
  // block is correctly silent, so a stale world would fail the sound check for
  // the right reason at the wrong time.
  for (const [slot, what] of held.entries()) {
    const on = actor.blockAt(stood.offset(what.at, -1, 0))
    if (!on) continue
    what.on = on.position
    what.placedAt = on.position.offset(0, 1, 0)
    actor._client.write('held_item_slot', { slotId: slot })
    actor._client.write('block_dig', {
      status: 0,
      location: { x: what.placedAt.x, y: what.placedAt.y, z: what.placedAt.z },
      face: 1,
      sequence: sequence++
    })
    await wait(500)
    heardPlace = null
    // Face 1 is up, so the block lands above the one clicked — the server puts
    // it on the face that was clicked and not in the cell that was.
    actor._client.write('block_place', {
      hand: 0,
      location: { x: on.position.x, y: on.position.y, z: on.position.z },
      direction: 1,
      cursorX: 0.5,
      cursorY: 1.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: sequence++
    })
    await wait(SETTLE_MS)
    what.heard = heardPlace
  }
  const cobble = held[0]
  const seeds = held[1]

  actor.chat('there is soup')
  await wait(1000)

  check('one player hears another talk', Boolean(heard), heard || 'nothing arrived')
  // The sender's name is the server's to add, not the client's to send — a
  // server that relayed the raw line would let anybody speak as anybody.
  check(
    'and is told who said it',
    Boolean(heard) && heard.includes('Actor'),
    heard || ''
  )

  check('one player sees another swing', sawSwing)
  check('one player sees another crouch', sawCrouch, 'entity flag and pose together')
  // The data is the *broken* block's state, not the air left behind — the
  // client makes the particles and the sound out of it, and the air's id gives
  // a silent puff of nothing.
  check(
    'one player sees another break a block',
    Boolean(sawBreakEffect) && sawBreakEffect.data > 0,
    sawBreakEffect ? `effect 2001, state ${sawBreakEffect.data}` : 'no world_event arrived'
  )
  check(
    'one player hears another place a block',
    Boolean(cobble.heard),
    cobble.heard
      ? `sound at ${cobble.heard.x}/${cobble.heard.y}/${cobble.heard.z}, ` +
        `volume ${cobble.heard.volume} pitch ${cobble.heard.pitch}`
      : 'no sound_effect arrived — is there a dust-constants.tsv beside [data] path?'
  )
  // And it is the right sound, which is the half of this that the arithmetic
  // below cannot check: the id came out of Dust's own generated sound-event
  // table, and this resolves it through a table that has never seen it.
  //
  // minecraft-data numbers sound events from one, which is the wire's own
  // convention: the packet carries `id + 1` so that zero can mark an inline
  // sound, and prismarine subtracts it back off.
  const named = sound => watcher.registry.soundsByName[sound]
  const rang = (what) =>
    Boolean(what.heard) && Boolean(named(what.sound)) &&
    what.heard.sound.soundId + 1 === named(what.sound).id
  check(
    'and it is the sound that block makes',
    rang(cobble),
    cobble.heard && cobble.heard.sound
      ? `sent ${cobble.heard.sound.soundId}, ${cobble.sound} is ` +
        `${named(cobble.sound) && named(cobble.sound).id - 1}`
      : 'nothing to compare'
  )
  // The one field on that packet whose unit is not in its name: vanilla writes
  // eighths of a block, and a server that wrote the block coordinate would put
  // the sound an eighth of the way to the origin. Nothing on either side of the
  // wire can see that; a second implementation reading the number can.
  check(
    'and hears it where the block went',
    Boolean(cobble.heard) && Boolean(cobble.placedAt) &&
      cobble.heard.x === Math.trunc((cobble.placedAt.x + 0.5) * 8) &&
      cobble.heard.y === Math.trunc((cobble.placedAt.y + 0.5) * 8) &&
      cobble.heard.z === Math.trunc((cobble.placedAt.z + 0.5) * 8),
    cobble.placedAt && cobble.heard
      ? `placed at ${cobble.placedAt.x}/${cobble.placedAt.y}/${cobble.placedAt.z}, ` +
        `sound at ${cobble.heard.x / 8}/${cobble.heard.y / 8}/${cobble.heard.z / 8}`
      : 'nothing to compare'
  )

  // What actually went into the world, read by the *other* player: the block
  // update reached them and it is the block the actor was holding rather than
  // the one block a server with no item table can place.
  const landed = what => what.placedAt && watcher.blockAt(what.placedAt)
  check(
    'a player places what they are holding',
    Boolean(landed(cobble)) && landed(cobble).name === cobble.block,
    landed(cobble) ? landed(cobble).name : 'the watcher has no block there'
  )
  // The row that says this is a table and not a rule. `minecraft:wheat_seeds`
  // places `minecraft:wheat`, and a server that matched item names to block
  // names would put down `minecraft:wheat_seeds`, which is not a block — or,
  // reading the other way, would let `minecraft:wheat` the item place a crop
  // it does not place. Sixteen items on 1.21.1 are like this and every one of
  // them is a thing a player finds before a test does.
  check(
    'and an item whose block has another name still places the right block',
    Boolean(landed(seeds)) && landed(seeds).name === seeds.block,
    landed(seeds)
      ? `wheat_seeds placed ${landed(seeds).name}`
      : 'the watcher has no block there'
  )
  check(
    'and that block makes its own sound',
    rang(seeds),
    seeds.heard && seeds.heard.sound
      ? `sent ${seeds.heard.sound.soundId}, ${seeds.sound} is ` +
        `${named(seeds.sound) && named(seeds.sound).id - 1}`
      : 'no sound_effect arrived'
  )

  // A stair, and which way it ends up facing. The block that goes down is no
  // longer the block's default state: `dust_sim::placement` reads the click,
  // and this is the check that the click reaches it across the wire at all.
  //
  // **Looking west on purpose, because a stair's default state faces north.**
  // A check that expected north would pass against a server that had never
  // read the click at all, which is what this whole thing is about.
  //
  // `bot.look(PI/2, 0)` is a protocol yaw of 90 — mineflayer's convention and
  // the wire's differ by a sign and a half turn — which is looking west, and a
  // stair faces the way the player looks. Measured against a real server by
  // `placement.js`, not remembered: a furnace with the same four values faces
  // back at the player instead.
  let stair = null
  const stairId = actor.registry.itemsByName.oak_stairs
  if (stairId) {
    actor._client.write('set_creative_slot', {
      slot: 38,
      item: {
        itemCount: 1,
        itemId: stairId.id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    })
    actor._client.write('held_item_slot', { slotId: 2 })
    await actor.look(Math.PI / 2, 0, true)
    await wait(400)
    const on = actor.blockAt(stood.offset(STAIR, -1, 0))
    if (on) {
      const at = on.position.offset(0, 1, 0)
      actor._client.write('block_dig', {
        status: 0,
        location: { x: at.x, y: at.y, z: at.z },
        face: 1,
        sequence: sequence++
      })
      await wait(400)
      actor._client.write('block_place', {
        hand: 0,
        location: { x: on.position.x, y: on.position.y, z: on.position.z },
        direction: 1,
        cursorX: 0.5,
        cursorY: 1.0,
        cursorZ: 0.5,
        insideBlock: false,
        sequence: sequence++
      })
      await wait(SETTLE_MS)
      stair = watcher.blockAt(at)
    }
  }
  const stairProps = stair && stair.getProperties ? stair.getProperties() : {}
  check(
    'a stair faces the way the player was standing',
    Boolean(stair) && stair.name === 'oak_stairs' && stairProps.facing === 'west',
    stair ? `${stair.name} facing ${stairProps.facing}` : 'nothing was placed'
  )
  // And the half, which comes from the face rather than from the look: clicked
  // on the top of a block, a stair is the bottom half whatever the cursor said.
  check(
    'and is the bottom half, because the top of a block was clicked',
    Boolean(stair) && stairProps.half === 'bottom',
    stair ? `half ${stairProps.half}` : 'nothing was placed'
  )

  // Fill the hole back in, so the next run against this world starts where
  // this one did. The block below the hole is still there, so its top face is
  // what to click.
  if (target) {
    actor._client.write('held_item_slot', { slotId: 0 })
    await wait(200)
    const under = target.position.offset(0, -1, 0)
    actor._client.write('block_place', {
      hand: 0,
      location: { x: under.x, y: under.y, z: under.z },
      direction: 1,
      cursorX: 0.5,
      cursorY: 1.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: sequence++
    })
    await wait(SETTLE_MS)
  }
  check(
    'and the hole it dug is filled back in',
    Boolean(target) && Boolean(watcher.blockAt(target.position)) &&
      watcher.blockAt(target.position).name !== 'air',
    target && watcher.blockAt(target.position)
      ? watcher.blockAt(target.position).name
      : 'nothing to check'
  )

  // Clicking a face whose far side is solid. The block behind it must not
  // change, and nothing must be heard: this used to replace it, silently, for
  // every solid cell in the world — a player could hollow out a wall from the
  // outside without breaking anything.
  //
  // **The situation is built rather than found.** Reading it off the terrain
  // made the check depend on where the actor happened to be standing, and this
  // server keeps its edits: the actor digs under its own feet at the start of
  // every run and sinks a block each time, so a cell that was underground on
  // one run is open air on the tenth and the check passes without testing
  // anything. So: put a block on the floor, then click its underside, whose
  // far side is the floor.
  //
  // The floor itself is now built too, by `buildGround` — over water there was
  // none, and the block meant to go on top of it went into the water instead.
  const floor = actor.blockAt(stood.offset(SOLID_FACE, -1, 0))
  let refused = null
  if (floor) {
    actor._client.write('held_item_slot', { slotId: 0 })
    await wait(200)
    actor._client.write('block_place', {
      hand: 0,
      location: { x: floor.position.x, y: floor.position.y, z: floor.position.z },
      direction: 1,
      cursorX: 0.5,
      cursorY: 1.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: sequence++
    })
    await wait(SETTLE_MS)
    const perch = floor.position.offset(0, 1, 0)
    const standing = watcher.blockAt(perch)
    const before = watcher.blockAt(floor.position)
    heardPlace = null
    // The underside of the block just placed. Beyond it is the floor, which is
    // solid, so nothing may happen.
    actor._client.write('block_place', {
      hand: 0,
      location: { x: perch.x, y: perch.y, z: perch.z },
      direction: 0,
      cursorX: 0.5,
      cursorY: 0.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: sequence++
    })
    await wait(SETTLE_MS)
    refused = {
      standing,
      before,
      after: watcher.blockAt(floor.position),
      heard: heardPlace
    }
  }
  // The precondition is asserted rather than assumed: if the block the check
  // clicks was never placed, everything below it is vacuous.
  check(
    'the block this check needs is where it was put',
    Boolean(refused) && refused.standing && refused.standing.name === 'cobblestone',
    refused && refused.standing ? refused.standing.name : 'nothing was placed'
  )
  // Reaching across the map. Fifty blocks east of where the actor stands is a
  // column it has been streamed and can name, and one no arm reaches. Both
  // verbs are tried, because they are two packets down two paths and a check
  // that only covered one would pass while the other stayed open.
  //
  // The block is read from the *other* player, before and after, so what is
  // checked is what reached the world rather than what either client believes.
  const distant = actor.blockAt(stood.offset(50, -1, 0))
  let unreached = null
  if (distant) {
    const before = watcher.blockAt(distant.position)
    const aboveBefore = watcher.blockAt(distant.position.offset(0, 1, 0))
    actor._client.write('block_dig', {
      status: 0,
      location: {
        x: distant.position.x,
        y: distant.position.y,
        z: distant.position.z
      },
      face: 1,
      sequence: sequence++
    })
    actor._client.write('block_place', {
      hand: 0,
      location: {
        x: distant.position.x,
        y: distant.position.y,
        z: distant.position.z
      },
      direction: 1,
      cursorX: 0.5,
      cursorY: 1.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: sequence++
    })
    await wait(SETTLE_MS)
    unreached = {
      before,
      after: watcher.blockAt(distant.position),
      aboveBefore,
      aboveAfter: watcher.blockAt(distant.position.offset(0, 1, 0))
    }
  }
  // Both cells compared before and after, rather than asserting the one above
  // is air. It is not always air: this server keeps its edits, the actor digs
  // under its own feet at the start of every run and sinks a block each time,
  // so fifty blocks out `stood.y - 1` eventually lands under the surface. What
  // is being checked is that *nothing changed*, and that is true whatever the
  // terrain happens to be.
  check(
    'a player cannot break or place fifty blocks away',
    Boolean(unreached) && unreached.before && unreached.after &&
      unreached.aboveBefore && unreached.aboveAfter &&
      unreached.before.name === unreached.after.name &&
      unreached.aboveBefore.name === unreached.aboveAfter.name,
    unreached && unreached.before && unreached.after
      ? `${unreached.before.name} -> ${unreached.after.name}, ` +
        `above it ${unreached.aboveBefore && unreached.aboveBefore.name} -> ` +
        `${unreached.aboveAfter && unreached.aboveAfter.name}`
      : 'no distant block to aim at'
  )

  check(
    'a block is not placed into one that is already there',
    Boolean(refused) && refused.before && refused.after &&
      refused.before.name === refused.after.name && !refused.heard,
    refused && refused.before && refused.after
      ? `${refused.before.name} -> ${refused.after.name}` +
        (refused.heard ? ', and a sound was heard' : '')
      : 'no block underground to click into'
  )

  // ---------------------------------------------------------------------
  // What a player is carrying, and whether it is still there next time.
  //
  // A third bot rather than the actor, whose hotbar the placement checks own.
  // Its name is five characters because a mineflayer username under three
  // never spawns and never errors — the bot simply does not arrive, and there
  // is nothing in any log to say why.
  // ---------------------------------------------------------------------
  const carrier = await spawned('Carrier')

  // Cleared first, so nothing below can pass on what a previous run left in
  // this world. Slot 0 is the crafting output and no client may write it.
  for (let slot = 1; slot <= 45; slot++) creativeSlot(carrier, slot, null, 0)
  await wait(1000)

  // A count that differs between runs, for the same reason. A fixed count
  // would let a server that ignored every write below still pass, because the
  // number it already had would be the number expected.
  const many = 2 + (Date.now() % 40)

  creativeSlot(carrier, 9, 'cobblestone', many)   // main inventory, with a count
  creativeSlot(carrier, 5, 'iron_helmet', 1)      // armour: the head slot
  creativeSlot(carrier, 45, 'water_bucket', 1)    // the offhand
  // And one the server must refuse: a water bucket stacks to one, so
  // sixty-four of them in a slot is a count no client should be able to ask
  // for. An empty bucket stacks to sixteen, which is exactly why the number
  // has to come from the item and not from a constant.
  creativeSlot(carrier, 10, 'water_bucket', 64)
  carrier._client.write('held_item_slot', { slotId: 3 })
  await wait(SETTLE_MS)

  check(
    'a stack larger than that item allows is refused',
    carrier.inventory.slots[10] == null,
    carrying(carrier, 10)
  )

  // A click, replayed by the server and pushed back to a client that predicted
  // nothing. Pick the stack up out of slot 9 and put it down in slot 20.
  windowClick(carrier, 9, 0, 0)
  await wait(500)
  windowClick(carrier, 20, 0, 0)
  await wait(SETTLE_MS)

  const clicked = carrier.inventory.slots[20]
  check(
    'a click moves a stack to the slot it was dropped in',
    Boolean(clicked) && clicked.name === 'cobblestone' && clicked.count === many,
    `${carrying(carrier, 20)}, wanted cobblestone x${many}`
  )
  check(
    'and out of the one it came from',
    carrier.inventory.slots[9] == null,
    carrying(carrier, 9)
  )

  // The one this whole cycle is about. Leave, come back under the same name,
  // and look.
  try { carrier.quit() } catch (e) { /* already gone */ }
  await wait(2000)
  const returned = await spawned('Carrier')
  await wait(SETTLE_MS)

  const kept = returned.inventory.slots[20]
  check(
    'what a player was carrying is still there after a relog',
    Boolean(kept) && kept.name === 'cobblestone' && kept.count === many,
    `${carrying(returned, 20)}, wanted cobblestone x${many}`
  )
  check(
    'and so is their armour',
    Boolean(returned.inventory.slots[5]) &&
      returned.inventory.slots[5].name === 'iron_helmet',
    carrying(returned, 5)
  )
  check(
    'and their offhand',
    Boolean(returned.inventory.slots[45]) &&
      returned.inventory.slots[45].name === 'water_bucket',
    carrying(returned, 45)
  )
  check(
    'and the slot they had in hand',
    returned.quickBarSlot === 3,
    `hotbar slot ${returned.quickBarSlot}`
  )
  // The other side of the same coin: a slot emptied before the relog must
  // still be empty. A server that saved nothing and a server that saved
  // everything both fail this, in opposite directions.
  check(
    'and a slot they emptied is still empty',
    returned.inventory.slots[9] == null && returned.inventory.slots[10] == null,
    `${carrying(returned, 9)} / ${carrying(returned, 10)}`
  )

  try { returned.quit() } catch (e) { /* already gone */ }
  try { actor.quit() } catch (e) { /* already gone */ }
  try { watcher.quit() } catch (e) { /* already gone */ }
}

main().then(
  () => {
    const failed = results.filter(r => !r.ok)
    console.log(`\n${results.length - failed.length}/${results.length} checks passed`)
    process.exit(failed.length === 0 ? 0 : 1)
  },
  e => {
    console.log(`FAIL  ${e.message}`)
    console.log('\nIs a dust server running on this port, with online_mode = false')
    console.log('and [data] path set? A client that acknowledges no data packs')
    console.log('cannot be served without it, and is refused at configuration.')
    process.exit(1)
  }
)
