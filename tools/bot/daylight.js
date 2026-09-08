// Whether the sun actually moves, asked of a running server by a client that
// shares no code with it.
//
// The test suite can prove the clock counts and that the packet encodes; what
// it cannot prove is that a *client* reads it as a moving sun, that `/time`
// parses on the far side of a real command graph, or that a player who joins
// at dusk arrives at dusk. mineflayer maintains its own `bot.time` from the
// `update_time` packet and its own command parsing is not involved at all,
// which is exactly why its opinion is worth something.
//
// Usage: node daylight.js [port]        (default 25565)
//        node daylight.js [port] --report   print the readings, assert nothing
//
// Exits 0 if every check passed, 1 with a named failure otherwise.

const mineflayer = require('mineflayer')

const PORT = Number(process.argv[2] || 25565)
const REPORT = process.argv.includes('--report')
const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 30000
const DAY_TICKS = 24000

const results = []
function check (name, ok, detail) {
  results.push({ name, ok, detail })
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`)
}

const wait = ms => new Promise(r => setTimeout(r, ms))

function spawned (username) {
  return new Promise((resolve, reject) => {
    const b = mineflayer.createBot({
      host: '127.0.0.1', port: PORT, username, auth: 'offline', version: VERSION
    })
    const timer = setTimeout(
      () => reject(new Error(`${username} never reached the world in ${JOIN_TIMEOUT_MS / 1000}s`)),
      JOIN_TIMEOUT_MS
    )
    b.on('error', e => { clearTimeout(timer); reject(new Error(`${username}: ${e.message}`)) })
    b.on('kicked', r => { clearTimeout(timer); reject(new Error(`${username} was kicked: ${JSON.stringify(r)}`)) })
    b.once('spawn', () => { clearTimeout(timer); resolve(b) })
  })
}

// Every `update_time` this bot is sent, with the moment it arrived. The raw
// packet rather than mineflayer's derived `bot.time`, because the cadence is
// half of what is being checked and a derived value has no arrival time.
function recordTime (b) {
  const seen = []
  b._client.on('update_time', packet => {
    seen.push({
      at: Date.now(),
      age: Number(packet.age),
      dayTime: Number(packet.time)
    })
  })
  return seen
}

// Say something and wait for the server's next chat line back.
function ask (b, command, ms = 2000) {
  return new Promise(resolve => {
    const timer = setTimeout(() => { b.removeListener('messagestr', once); resolve(null) }, ms)
    const once = text => { clearTimeout(timer); b.removeListener('messagestr', once); resolve(text) }
    b.on('messagestr', once)
    b._client.write('chat_command', { command })
  })
}

async function main () {
  const watcher = await spawned('SunWatcher')
  const seen = recordTime(watcher)

  // --- the sun moves -----------------------------------------------------
  //
  // Four seconds is four broadcasts at vanilla's rate, which is enough to see
  // both that the number climbs and that it climbs at the right speed. A
  // server whose clock ticked at the wrong rate would pass "it moved" and fail
  // this.
  await wait(4200)
  if (seen.length < 3) {
    check('the server tells the client the time at all', false, `${seen.length} update_time packets in 4.2s`)
    finish(watcher)
    return
  }
  const first = seen[0]
  const last = seen[seen.length - 1]
  const elapsedMs = last.at - first.at
  const ticks = last.dayTime - first.dayTime
  const expected = elapsedMs / 50
  check(
    'the sun moves',
    ticks > 0,
    `day time went ${first.dayTime} -> ${last.dayTime} in ${elapsedMs} ms`
  )
  check(
    'and at twenty ticks a second, within 10%',
    Math.abs(ticks - expected) <= expected * 0.1 + 2,
    `${ticks} ticks where ${expected.toFixed(0)} were owed`
  )
  check(
    'the world age moves with it',
    last.age > first.age,
    `age went ${first.age} -> ${last.age}`
  )
  check(
    'a positive day time, which is what says the cycle is running',
    seen.every(s => s.dayTime >= 0),
    `lowest ${Math.min(...seen.map(s => s.dayTime))}`
  )

  // --- the rate ----------------------------------------------------------
  //
  // Vanilla broadcasts on `tickCount % 20 == 0`, once a second. A server
  // sending one a tick would work and would cost twenty times the bandwidth
  // for a sky the client already interpolates.
  const gaps = seen.slice(1).map((s, i) => s.at - seen[i].at)
  const median = gaps.slice().sort((a, b) => a - b)[Math.floor(gaps.length / 2)]
  check(
    'about one packet a second, not one a tick',
    median >= 800 && median <= 1300,
    `median gap ${median} ms over ${gaps.length} intervals`
  )

  // --- /time -------------------------------------------------------------
  const setNight = await ask(watcher, 'time set night')
  check(
    '/time set night is accepted and answered',
    setNight === 'Set the time to 13000',
    JSON.stringify(setNight)
  )
  await wait(300)
  const afterSet = seen[seen.length - 1]
  check(
    'and the client is told inside a tick or two, not at the next second',
    afterSet.dayTime % DAY_TICKS >= 13000 && afterSet.dayTime % DAY_TICKS < 13000 + 40,
    `day time is now ${afterSet.dayTime % DAY_TICKS}`
  )

  const queried = await ask(watcher, 'time query daytime')
  const queriedTicks = Number((queried || '').replace(/\D+/g, ''))
  check(
    '/time query daytime reads back what was set',
    queriedTicks >= 13000 && queriedTicks < 13000 + 200,
    JSON.stringify(queried)
  )

  const day = await ask(watcher, 'time query day')
  check('/time query day answers', /^The time is \d+$/.test(day || ''), JSON.stringify(day))
  const gametime = await ask(watcher, 'time query gametime')
  check('/time query gametime answers', /^The time is \d+$/.test(gametime || ''), JSON.stringify(gametime))

  const added = await ask(watcher, 'time add 1d')
  check(
    '/time add takes a unit',
    /^Set the time to 130\d\d$/.test(added || ''),
    `a whole day later is the same time of day: ${JSON.stringify(added)}`
  )

  // Refused, and refused *specifically*. Vanilla answers a bad argument with
  // the parser's own complaint rather than "unknown command" — the command was
  // found, its argument was not readable — so what matters is that the server
  // said something and did not set anything.
  const before = seen[seen.length - 1].dayTime
  const nonsense = await ask(watcher, 'time set banana')
  await wait(200)
  const after = seen[seen.length - 1].dayTime
  check(
    'and nonsense is refused rather than run',
    !!nonsense && !nonsense.startsWith('Set the time to') && after >= before,
    `${JSON.stringify(nonsense)}, and the sun kept going: ${before} -> ${after}`
  )

  // --- joining mid-evening ------------------------------------------------
  //
  // The check the whole thing exists for. A second bot joins a world that is
  // now at dusk and must arrive at dusk: the very first `update_time` it is
  // sent has to be the world's time, not a constant, because the client draws
  // whatever it is told first and a wrong one is a sky that visibly snaps a
  // second later.
  const latecomer = await spawned('LateJoiner')
  const lateSeen = recordTime(latecomer)
  const arrival = await new Promise(resolve => {
    if (lateSeen.length) return resolve(lateSeen[0])
    latecomer._client.once('update_time', packet =>
      resolve({ at: Date.now(), age: Number(packet.age), dayTime: Number(packet.time) })
    )
  })
  const worldNow = seen[seen.length - 1].dayTime
  check(
    'a player joining at dusk arrives at dusk',
    Math.abs(arrival.dayTime - worldNow) < 100,
    `joined at ${arrival.dayTime % DAY_TICKS}, world is at ${worldNow % DAY_TICKS}`
  )
  check(
    'and not at the frozen noon this server used to serve',
    arrival.dayTime % DAY_TICKS !== 6000 && arrival.dayTime > 0,
    `${arrival.dayTime}`
  )

  latecomer.quit()
  finish(watcher)
}

function finish (b) {
  b.quit()
  const failed = results.filter(r => !r.ok)
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`)
  if (REPORT) {
    console.log('(--report: nothing here is an assertion)')
    process.exit(0)
  }
  process.exit(failed.length === 0 ? 0 : 1)
}

main().catch(e => {
  console.error(`FAIL  ${e.message}`)
  process.exit(1)
})
