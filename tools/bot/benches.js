// The two benches that are not fires: a stonecutter and a smithing table.
//
//   node benches.js <port> --survey --out vanilla.json   what a server does
//   node benches.js --compare vanilla.json dust.json     whether Dust agrees
//   node benches.js <port> --check                       the gate
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
    return { name: name(item.itemId), count: item.itemCount }
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
  return s ? `${s.name} x${s.count}` : null
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
async function putInto (bot, windowSlot, hotbarWire, item, count) {
  const state = bot.tracked
  creativeSlot(bot, 36, item, count)
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

const SMITH_CASES = [
  {
    what: 'a netherite upgrade',
    template: 'netherite_upgrade_smithing_template',
    base: 'diamond_chestplate',
    addition: 'netherite_ingot'
  },
  {
    what: 'the same three in the wrong slots',
    template: 'netherite_ingot',
    base: 'diamond_chestplate',
    addition: 'netherite_upgrade_smithing_template'
  },
  {
    what: 'a base that is not upgradeable',
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
    await putInto(bot, SMITH_BASE, 31, one.base, 1)
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
    smithing: smith.rows,
    declared
  }
  if (out) require('fs').writeFileSync(out, JSON.stringify(record, null, 1))
  for (const row of record.cuts) {
    const made = row.made.filter(m => m.out).map(m => `${m.button}:${m.out}`)
    console.log(`${row.input}  ${made.length} button(s)  ${made.join(' ')}`)
  }
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
    console.log('usage: benches.js <port> [--survey --out file.json | --check]')
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

function compare (a, b) {
  console.log('not yet')
  process.exit(2)
}

main().catch(e => { console.error(e.message); process.exit(1) })
