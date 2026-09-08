// The two benches that are not fires: a stonecutter and a smithing table.
//
//   node benches.js <port> --survey --out vanilla.json   what a server does
//   node benches.js --compare vanilla.json dust.json     whether Dust agrees
//
// **The comparison is the gate; there is no `--check`.** Nothing a single
// server says about itself can be wrong here in a way this file could notice —
// see the next paragraph — so the only assertion in this script is the one
// that has two servers on either side of it. `--compare` exits `1` on any
// disagreement that is not named in `DECLARED` below, and on any *agreement*
// that is.
//
// # Why this is a survey before it is a check
//
// A stonecutter is the first screen in Dust where **the client decides what
// the buttons mean**. The server sends the recipe list once, at join, and the
// client filters it by whatever is in the input slot and draws a button per
// survivor; the packet that comes back names a button by its *index in that
// filtered list*. So the server and the client have to agree about an order
// neither of them states, and the only way to know what the order is is to ask
// a real server what came out when button 3 was pressed.
//
// That is what `--survey` is. It is pointed at a vanilla 1.21.1 server, it
// presses every button of several inputs, and it writes down what each one
// made. `--compare` then asks Dust the same questions. A guess about the
// ordering rule would be invisible in a single-server test — Dust would agree
// with itself and hand the player the wrong block.
//
// # And a smithing table is the other shape
//
// Three inputs and one output, and the output is decided by the server. There
// is no button. The trap it walks into is the opposite one: the vanilla client
// *also* computes the result, out of the same recipe list, and clears its own
// result slot whenever an input changes. A server that never sent the smithing
// recipes would look right in a packet log and wrong on the screen.

const mineflayer = require('mineflayer')
const { Vec3 } = require('vec3')

const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 60000
const SETTLE_MS = 3000
const CLICK_MS = 400

const PLAYER_SLOTS = 46

// A stonecutter window: 0 input, 1 result, 2..28 inventory, 29..37 hotbar.
const CUT_IN = 0
const CUT_OUT = 1
const CUT_SLOTS = 38

// A smithing window: 0 template, 1 base, 2 addition, 3 result, then 4..30 and
// 31..39.
const SMITH_TEMPLATE = 0
const SMITH_BASE = 1
const SMITH_ADDITION = 2
const SMITH_OUT = 3
const SMITH_SLOTS = 40

const PICKUP = 0

const wait = ms => new Promise(r => setTimeout(r, ms))

function spawned (port, username) {
  return new Promise((resolve, reject) => {
    const b = mineflayer.createBot({
      host: '127.0.0.1', port, username, auth: 'offline', version: VERSION
    })
    b.tracked = tracker(b)
    const timer = setTimeout(
      () => reject(new Error(`${username} never reached the world in ${JOIN_TIMEOUT_MS / 1000}s`)),
      JOIN_TIMEOUT_MS
    )
    b.on('error', e => { clearTimeout(timer); reject(new Error(`${username}: ${e.message}`)) })
    b.on('kicked', r => { clearTimeout(timer); reject(new Error(`${username} was kicked: ${JSON.stringify(r)}`)) })
    b.once('spawn', () => { clearTimeout(timer); resolve(b) })
  })
}

// Attached at construction, not after `spawn`: the join burst arrives in one
// TCP read and `declare_recipes` is in it.
function tracker (b) {
  const state = {
    slots: new Array(SMITH_SLOTS).fill(null),
    property: null,
    window: 0,
    menu: null,
    opened: 0,
    recipes: null
  }
  const name = id => (b.registry.items[id] ? b.registry.items[id].name : `id:${id}`)
  const read = item => {
    if (!item || !item.itemCount || item.itemCount === 0) return null
    return { name: name(item.itemId), count: item.itemCount, components: componentsOf(item) }
  }
  b._client.on('open_window', p => {
    state.opened++
    state.window = p.windowId
    state.menu = p.inventoryType
    state.slots = new Array(SMITH_SLOTS).fill(null)
    state.property = null
  })
  b._client.on('set_slot', p => {
    if (p.windowId !== state.window) return
    if (p.slot >= 0 && p.slot < SMITH_SLOTS) state.slots[p.slot] = read(p.item)
  })
  b._client.on('window_items', p => {
    if (p.windowId !== state.window) return
    for (let i = 0; i < p.items.length && i < SMITH_SLOTS; i++) {
      state.slots[i] = read(p.items[i])
    }
  })
  b._client.on('craft_progress_bar', p => {
    if (p.windowId !== state.window) return
    if (p.property === 0) state.property = p.value
  })
  // The list the buttons are drawn from. Recorded as (type, id) pairs only:
  // what matters is which recipes arrived and in what order, and the layouts
  // are Mojang's data.
  b._client.on('declare_recipes', p => {
    state.recipes = p.recipes.map(r => ({
      id: r.name !== undefined ? r.name : r.recipeId,
      type: r.type
    }))
  })
  return state
}

function itemStack (b, itemName, count) {
  return itemName
    ? {
        itemCount: count,
        itemId: b.registry.itemsByName[itemName].id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    : { itemCount: 0 }
}

function creativeSlot (b, slot, itemName, count) {
  b._client.write('set_creative_slot', { slot, item: itemStack(b, itemName, count) })
}

// The same, carrying components. The row that says a netherite upgrade keeps
// what the diamond one was: vanilla's `transmuteCopy` moves the base stack's
// name, enchantments and damage onto the new item, and a server that built the
// result out of the recipe's own item alone would look right in every check
// that compared item names.
function creativeSlotWith (b, slot, itemName, count, components) {
  b._client.write('set_creative_slot', {
    slot,
    item: {
      itemCount: count,
      itemId: b.registry.itemsByName[itemName].id,
      addedComponentCount: components.length,
      removedComponentCount: 0,
      components,
      removeComponents: []
    }
  })
}

function windowClick (b, windowId, slot, mouseButton, mode) {
  b._client.write('window_click', {
    windowId, stateId: 0, slot, mouseButton, mode, changedSlots: [], cursorItem: { itemCount: 0 }
  })
}

// The button press. `enchant_item` is the 1.21.1 wire name for what the
// vocabulary calls `container_button_click`; enchantment rows, lectern pages
// and stonecutter recipes all arrive down it.
function pressButton (b, windowId, button) {
  b._client.write('enchant_item', { windowId, enchantment: button })
}

function describe (s) {
  return s ? `${s.name} x${s.count}${s.components ? ' ' + s.components : ''}` : null
}

// A component patch as one comparable string — `clicks.js`'s renderer, and for
// its reason: the order a patch arrives in is not part of what it means.
function componentsOf (item) {
  const added = (item.components || [])
    .map(c => `${c.type}=${c.data === undefined ? 'present' : stable(c.data)}`)
    .sort()
  const removed = (item.removeComponents || []).map(c => c.type).sort()
  if (added.length === 0 && removed.length === 0) return ''
  return `[${added.join(' ')}${removed.length ? ' -' + removed.join(' -') : ''}]`
}

function stable (value) {
  if (value === undefined) return 'absent'
  if (value === null || typeof value !== 'object') return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(stable).join(',')}]`
  if (Buffer.isBuffer(value)) return value.toString('hex')
  const keys = Object.keys(value).sort()
  return `{${keys.map(k => `${k}:${stable(value[k])}`).join(',')}}`
}

function support (bot) {
  const feet = bot.entity.position
  for (const [dx, dz] of [[2, 0], [-2, 0], [0, 2], [0, -2], [3, 0], [0, 3], [2, 2], [-2, -2]]) {
    for (let dy = 0; dy >= -3; dy--) {
      const at = {
        x: Math.floor(feet.x) + dx,
        y: Math.floor(feet.y) + dy - 1,
        z: Math.floor(feet.z) + dz
      }
      const here = bot.blockAt(new Vec3(at.x, at.y, at.z))
      const above = bot.blockAt(new Vec3(at.x, at.y + 1, at.z))
      if (here && above && here.name !== 'air' && above.name === 'air') return at
    }
  }
  throw new Error('no solid block with air above it within reach')
}

function place (bot, at, sequence) {
  bot._client.write('block_place', {
    hand: 0, location: at, direction: 1, cursorX: 0.5, cursorY: 1.0, cursorZ: 0.5, insideBlock: false, sequence
  })
}

// Put a bench down and open it.
async function openBench (bot, block) {
  const state = bot.tracked
  for (let slot = 1; slot < PLAYER_SLOTS; slot++) creativeSlot(bot, slot, null, 0)
  creativeSlot(bot, 36, block, 1)
  bot._client.write('held_item_slot', { slotId: 0 })
  await wait(SETTLE_MS)

  const below = support(bot)
  const placed = { x: below.x, y: below.y + 1, z: below.z }
  place(bot, below, 1)
  await wait(SETTLE_MS)
  const there = bot.blockAt(new Vec3(placed.x, placed.y, placed.z))
  place(bot, placed, 2)
  await wait(SETTLE_MS)
  return { placed, block: there ? there.name : 'nothing', window: state.window, menu: state.menu }
}

// Put `item` into window slot `slot`, out of a hotbar slot, by picking it up
// and putting it down. Two clicks and not a creative write: a creative write
// names the *player's* numbering and cannot reach a slot that belongs to a
// block.
async function putInto (bot, windowSlot, hotbarWire, item, count, components) {
  const state = bot.tracked
  if (components) creativeSlotWith(bot, 36, item, count, components)
  else creativeSlot(bot, 36, item, count)
  await wait(CLICK_MS)
  windowClick(bot, state.window, hotbarWire, 0, PICKUP)
  await wait(CLICK_MS)
  windowClick(bot, state.window, windowSlot, 0, PICKUP)
  await wait(CLICK_MS)
}

// How many buttons a stonecutter shows for this input, asked of the server
// rather than counted here: press upwards until nothing comes out twice in a
// row, and stop. `LIMIT` is above the largest vanilla group (16, cobbled
// deepslate) with room to spare.
const BUTTON_LIMIT = 24

// **`blackstone` is the one input that carries the ordering claim. Do not
// remove it.** Measured: with the description-id tie-break deleted, so that a
// group comes out in the order the recipe files were read, this survey still
// scores 16 of 17 — five of these six inputs happen to be read in the order
// vanilla draws them, and only blackstone disagrees (`polished_blackstone`
// moves from button 4 to button 8, behind its own four brick recipes). Six
// inputs is a weak instrument for the ordering rule and this comment is what
// stops that from being an accident. `dust_sim::cutting`'s own unit tests are
// the other half: three of them go red on the same edit.
const CUT_INPUTS = [
  'andesite',
  'cobbled_deepslate',
  'blackstone',
  'copper_block',
  'quartz_block',
  'stone'
]

async function surveyCuts (bot) {
  const state = bot.tracked
  const rows = []
  const opened = await openBench(bot, 'stonecutter')
  for (const input of CUT_INPUTS) {
    // A fresh window per input. The selection is sticky in vanilla and a
    // survey that reused one screen would be measuring the previous answer.
    place(bot, opened.placed, 3)
    await wait(SETTLE_MS)
    await putInto(bot, CUT_IN, 29, input, 64)
    const before = describe(state.slots[CUT_OUT])
    const made = []
    for (let button = 0; button < BUTTON_LIMIT; button++) {
      pressButton(bot, state.window, button)
      await wait(CLICK_MS)
      made.push({ button, out: describe(state.slots[CUT_OUT]), selected: state.property })
    }
    rows.push({ input, before, made, menu: opened.menu })
  }
  return { opened, rows }
}

// What a stonecutter does over more than one click: whether the button stays
// pressed while the input is spent, whether a shift-click empties the stack,
// and whether closing the screen gives the input back.
//
// **The row that needs more than one reading.** Taking one slab and seeing a
// slab is not evidence the selection survived: the server could refill from
// nothing, or from the last press, or not at all, and a single snapshot after
// a single take looks the same in two of those three. Eight takes with an
// exact input count afterwards is the shape that can only be one of them.
async function surveyCutting (bot) {
  const state = bot.tracked
  const rows = []
  const opened = await openBench(bot, 'stonecutter')

  // Eight ordinary takes, one button press.
  place(bot, opened.placed, 5)
  await wait(SETTLE_MS)
  await putInto(bot, CUT_IN, 29, 'stone', 20)
  pressButton(bot, state.window, 5)
  await wait(CLICK_MS)
  const chose = describe(state.slots[CUT_OUT])
  let taken = 0
  for (let n = 0; n < 8; n++) {
    windowClick(bot, state.window, CUT_OUT, 0, PICKUP)
    await wait(CLICK_MS)
    // Put the cursor down in the player's half so the next take is not
    // refused for a full hand.
    windowClick(bot, state.window, 2 + n, 0, PICKUP)
    await wait(CLICK_MS)
    if (state.slots[2 + n]) taken++
  }
  rows.push({
    what: 'eight takes on one press',
    chose,
    taken,
    input: describe(state.slots[CUT_IN]),
    stillOffered: describe(state.slots[CUT_OUT])
  })

  // Closing the screen has to give the input back.
  const held = describe(state.slots[CUT_IN])
  bot._client.write('close_window', { windowId: state.window })
  await wait(SETTLE_MS)
  place(bot, opened.placed, 6)
  await wait(SETTLE_MS)
  rows.push({
    what: 'the input a close gave back',
    held,
    left: describe(state.slots[CUT_IN])
  })

  // An input nothing cuts.
  await putInto(bot, CUT_IN, 29, 'dirt', 4)
  pressButton(bot, state.window, 0)
  await wait(CLICK_MS)
  rows.push({
    what: 'an input nothing cuts',
    result: describe(state.slots[CUT_OUT]),
    selected: state.property
  })
  return { opened, rows }
}

const SMITH_CASES = [
  {
    what: 'a netherite upgrade',
    template: 'netherite_upgrade_smithing_template',
    base: 'diamond_chestplate',
    addition: 'netherite_ingot'
  },
  {
    // Vanilla's `transmuteCopy` keeps the base's components. A result built
    // from the recipe's own item would be a plain netherite chestplate and
    // would pass any check that read the item name.
    what: 'an upgrade of a named, damaged chestplate',
    template: 'netherite_upgrade_smithing_template',
    base: 'diamond_chestplate',
    baseComponents: [
      { type: 'damage', data: 137 },
      { type: 'custom_name', data: { type: 'string', name: '', value: 'Old Faithful' } }
    ],
    addition: 'netherite_ingot'
  },
  {
    what: 'a base nothing in this server upgrades or trims',
    template: 'netherite_upgrade_smithing_template',
    base: 'stone',
    addition: 'netherite_ingot'
  },
  {
    what: 'the same three in the wrong slots',
    template: 'netherite_ingot',
    base: 'diamond_chestplate',
    addition: 'netherite_upgrade_smithing_template'
  },
  {
    // **The one row the two servers are expected to differ on**, and it is
    // named so that the day they stop differing is a failure rather than a
    // silence. Vanilla's base slot accepts an iron chestplate because its
    // eighteen `smithing_trim` recipes name `#minecraft:trimmable_armor`
    // there; Dust does not load trims, so its base slot does not accept one.
    what: 'armour vanilla would take for a trim',
    template: 'netherite_upgrade_smithing_template',
    base: 'iron_chestplate',
    addition: 'netherite_ingot'
  },
  {
    what: 'no template at all',
    template: null,
    base: 'diamond_chestplate',
    addition: 'netherite_ingot'
  }
]

async function surveySmithing (bot) {
  const state = bot.tracked
  const rows = []
  const opened = await openBench(bot, 'smithing_table')
  for (const one of SMITH_CASES) {
    place(bot, opened.placed, 4)
    await wait(SETTLE_MS)
    if (one.template) await putInto(bot, SMITH_TEMPLATE, 31, one.template, 1)
    await putInto(bot, SMITH_BASE, 31, one.base, 1, one.baseComponents)
    await putInto(bot, SMITH_ADDITION, 31, one.addition, 1)
    const result = describe(state.slots[SMITH_OUT])
    // Take it, and see what the three inputs are afterwards. A result that
    // cannot be taken is not a result.
    windowClick(bot, state.window, SMITH_OUT, 0, PICKUP)
    await wait(CLICK_MS)
    rows.push({
      what: one.what,
      result,
      afterTemplate: describe(state.slots[SMITH_TEMPLATE]),
      afterBase: describe(state.slots[SMITH_BASE]),
      afterAddition: describe(state.slots[SMITH_ADDITION])
    })
  }
  return { opened, rows }
}

async function survey (port, out) {
  const bot = await spawned(port, 'Bencher')
  const state = bot.tracked
  await wait(SETTLE_MS)
  const cuts = await surveyCuts(bot)
  const cutting = await surveyCutting(bot)
  const smith = await surveySmithing(bot)
  const declared = state.recipes
    ? {
        total: state.recipes.length,
        stonecutting: state.recipes.filter(r => /stonecutting/.test(String(r.type))).length,
        smithing: state.recipes.filter(r => /smithing/.test(String(r.type))).length,
        firstFew: state.recipes.filter(r => /stonecutting/.test(String(r.type))).slice(0, 5)
      }
    : null
  try { bot.quit() } catch (e) { /* already gone */ }
  const record = {
    cutMenu: cuts.opened.menu,
    cutBlock: cuts.opened.block,
    smithMenu: smith.opened.menu,
    smithBlock: smith.opened.block,
    cuts: cuts.rows,
    cutting: cutting.rows,
    smithing: smith.rows,
    declared
  }
  if (out) require('fs').writeFileSync(out, JSON.stringify(record, null, 1))
  for (const row of record.cuts) {
    const made = row.made.filter(m => m.out).map(m => `${m.button}:${m.out}`)
    console.log(`${row.input}  ${made.length} button(s)  ${made.join(' ')}`)
  }
  for (const row of record.cutting) console.log(JSON.stringify(row))
  for (const row of record.smithing) {
    console.log(`${row.what}  -> ${row.result}  (left ${row.afterTemplate}, ${row.afterBase}, ${row.afterAddition})`)
  }
  console.log(`menus: stonecutter ${record.cutMenu} on ${record.cutBlock}, smithing ${record.smithMenu} on ${record.smithBlock}`)
  console.log(`declared: ${JSON.stringify(record.declared)}`)
  return record
}

async function main () {
  const args = process.argv.slice(2)
  if (args[0] === '--compare') return compare(args[1], args[2])
  const port = Number(args[0])
  if (!port) {
    console.log('usage: benches.js <port> --survey [--out file.json]')
    console.log('       benches.js --compare vanilla.json dust.json')
    process.exit(2)
  }
  if (args.includes('--survey')) {
    await survey(port, args[args.indexOf('--out') + 1])
    process.exit(0)
  }
  console.log('nothing to do; pass --survey or --compare')
  process.exit(2)
}

// The rows the two servers are *expected* to disagree on, by their `what`, and
// why. A named divergence that stops diverging is a failure too: the day
// somebody loads the trim recipes this row flips, and the check says so
// instead of quietly agreeing with a record that has gone stale.
const DECLARED = new Map([
  [
    'the same number of smithing recipes',
    'A real 1.21.1 server declares 27 — nine `smithing_transform` and eighteen ' +
      '`smithing_trim`. Dust declares the nine. A trim\'s result is a ' +
      '`minecraft:trim` component this server would have to author, and Dust ' +
      'carries components rather than writing them. See decision record 0037.'
  ],
  [
    'armour vanilla would take for a trim',
    'The same eighteen recipes, seen from the other end. Vanilla\'s base slot ' +
      'takes an iron chestplate because `#minecraft:trimmable_armor` names it ' +
      'there; Dust has no recipe that accepts one, so the click bounces and the ' +
      'slot stays empty. See decision record 0037.'
  ]
])

function compare (vanillaFile, dustFile) {
  const v = JSON.parse(require('fs').readFileSync(vanillaFile, 'utf8'))
  const d = JSON.parse(require('fs').readFileSync(dustFile, 'utf8'))
  const rows = []
  const say = (what, a, b) => rows.push({ what, a: stable(a), b: stable(b) })

  say('the stonecutter opens the same menu', v.cutMenu, d.cutMenu)
  say('the smithing table opens the same menu', v.smithMenu, d.smithMenu)
  say(
    'both servers declared the same number of stonecutting recipes',
    v.declared && v.declared.stonecutting,
    d.declared && d.declared.stonecutting
  )
  say(
    'both servers declared the same number of smithing recipes',
    v.declared && v.declared.smithing,
    d.declared && d.declared.smithing
  )

  // One row per input, comparing the *whole* button sequence. Not "the same
  // number of buttons" and not "the same set": the order is the thing under
  // test, and both of the weaker forms pass a server that sorted differently.
  for (let i = 0; i < Math.max(v.cuts.length, d.cuts.length); i++) {
    const a = v.cuts[i]
    const b = d.cuts[i]
    say(
      `every button of ${a ? a.input : '?'} makes what vanilla makes`,
      a && a.made.map(m => `${m.button}:${m.out}`),
      b && b.made.map(m => `${m.button}:${m.out}`)
    )
  }
  for (let i = 0; i < Math.max(v.cutting.length, d.cutting.length); i++) {
    const a = v.cutting[i]
    const b = d.cutting[i]
    say(`stonecutter: ${a ? a.what : '?'}`, a, b)
  }
  for (let i = 0; i < Math.max(v.smithing.length, d.smithing.length); i++) {
    const a = v.smithing[i]
    const b = d.smithing[i]
    say(`smithing: ${a ? a.what : '?'}`, a, b)
  }

  let same = 0
  let diverged = 0
  let unexpected = 0
  for (const row of rows) {
    const declaredFor = [...DECLARED.keys()].find(key => row.what.includes(key))
    const agrees = row.a === row.b
    if (agrees && declaredFor) {
      unexpected++
      console.log(`  FAIL  ${row.what}`)
      console.log('        the two servers now agree, and this row is recorded as a divergence')
      continue
    }
    if (agrees) {
      same++
      console.log(`  ok    ${row.what}`)
      continue
    }
    if (declaredFor) {
      diverged++
      console.log(`  known ${row.what}`)
      console.log(`        ${DECLARED.get(declaredFor)}`)
      continue
    }
    unexpected++
    console.log(`  FAIL  ${row.what}`)
    console.log(`        vanilla ${row.a}`)
    console.log(`        dust    ${row.b}`)
  }
  console.log(`\n${same}/${rows.length - diverged} rows identical, ${diverged} declared divergence(s)`)
  process.exit(unexpected === 0 ? 0 : 1)
}

main().catch(e => { console.error(e.message); process.exit(1) })
