#!/usr/bin/env node
// Panoptes System: control panel mock. Fleet and transcripts are simulated;
// the Cage captures are real desk screenshots from the e14 runs on rig.
//
//   header   name, unit counts, host numbers
//   default  MASTER | FLEET
//   open     FLEET + MASTER | UNIT loop | CAGE + its numbers
//   log      resd transitions
'use strict';
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');

const ASSETS = path.join(__dirname, 'assets');
const out = (s) => process.stdout.write(s);

// Luminance is urgency. Orange is the only accent; red only means failure.
const C = {
  ink: 0xf8f8f8, text: 0xe4e4e4, grey: 0xacacac, dim: 0x7c7c7c, rule: 0x404040,
  accent: 0xff9018, red: 0xff3b30, black: 0x000000,
};

// ------------------------------------------------------------- cell buffer

let W = 0, H = 0, cur = null, prev = null;

function alloc() {
  const n = W * H;
  return { ch: new Array(n).fill(' '), fg: new Int32Array(n).fill(C.text), bg: new Int32Array(n), bold: new Uint8Array(n) };
}

function put(x, y, s, fg = C.text, bg = null, bold = 0) {
  if (y < 0 || y >= H) return x;
  for (const c of s) {
    if (x >= 0 && x < W) {
      const i = y * W + x;
      cur.ch[i] = c; cur.fg[i] = fg; cur.bold[i] = bold;
      if (bg !== null) cur.bg[i] = bg;
    }
    x++;
  }
  return x;
}

function sgr(fg, bg, bold) {
  return `\x1b[${bold ? 1 : 22};38;2;${(fg >> 16) & 255};${(fg >> 8) & 255};${fg & 255};48;2;${(bg >> 16) & 255};${(bg >> 8) & 255};${bg & 255}m`;
}

function flush() {
  let o = '\x1b[?2026h';
  let cx = -1, cy = -1, style = '';
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      const i = y * W + x;
      if (prev && prev.ch[i] === cur.ch[i] && prev.fg[i] === cur.fg[i] && prev.bg[i] === cur.bg[i] && prev.bold[i] === cur.bold[i]) continue;
      if (cx !== x || cy !== y) o += `\x1b[${y + 1};${x + 1}H`;
      const k = sgr(cur.fg[i], cur.bg[i], cur.bold[i]);
      if (k !== style) { o += k; style = k; }
      o += cur.ch[i];
      cx = x + 1; cy = y;
    }
  }
  o += flushImages() + '\x1b[?2026l';
  out(o);
  prev = cur;
  cur = alloc();
}

// ----------------------------------------------------- Kitty graphics images

const IMG = {};
let nextImg = 1;
let slotsWant = new Map();
let slotsHave = new Map();

function transmit(id, format, b) {
  let o = '';
  for (let i = 0, first = true; i < b.length; i += 4096, first = false) {
    const chunk = b.slice(i, i + 4096);
    const more = i + 4096 < b.length ? 1 : 0;
    o += first ? `\x1b_Ga=t,${format},t=d,i=${id},q=2,m=${more};${chunk}\x1b\\` : `\x1b_Gm=${more},q=2;${chunk}\x1b\\`;
  }
  out(o);
}

function upload(name) {
  const id = nextImg++;
  IMG[name] = id;
  transmit(id, 'f=100', fs.readFileSync(path.join(ASSETS, name + '.png')).toString('base64'));
}

// RGBA pixels made at runtime.
function uploadRaw(name, rgba, w, h) {
  const id = nextImg++;
  IMG[name] = id;
  transmit(id, `f=32,o=z,s=${w},v=${h}`, zlib.deflateSync(rgba, { level: 1 }).toString('base64'));
}

// One image stretched into one screen rectangle, optionally nudged by pixels
// and optionally below the text layer. A slot is a stable number per screen
// position, so moving or swapping an image is one update.
function place(slot, name, x, y, c, r, X = 0, Y = 0, z = 0) {
  slotsWant.set(slot, { id: IMG[name], x, y, c, r, X, Y, z });
}

function flushImages() {
  let o = '';
  for (const [slot, have] of slotsHave) {
    const want = slotsWant.get(slot);
    if (!want || want.id !== have.id) o += `\x1b_Ga=d,d=i,i=${have.id},p=${slot},q=2\x1b\\`;
  }
  for (const [slot, w] of slotsWant) {
    const h = slotsHave.get(slot);
    if (h && h.id === w.id && h.x === w.x && h.y === w.y && h.c === w.c && h.r === w.r && h.X === w.X && h.Y === w.Y && h.z === w.z) continue;
    o += `\x1b[${w.y + 1};${w.x + 1}H\x1b_Ga=p,i=${w.id},p=${slot},c=${w.c},r=${w.r},X=${w.X},Y=${w.Y},z=${w.z},C=1,q=2\x1b\\`;
  }
  slotsHave = slotsWant;
  slotsWant = new Map();
  return o;
}

// ------------------------------------------------------------- drawing kit

const hrule = (x, y, w, c = C.rule) => put(x, y, '─'.repeat(Math.max(0, w)), c);

function vrule(x, y, h) {
  for (let j = y; j < y + h; j++) put(x, j, '│', C.rule);
}

// Heavy serif numerals, baked as images: the one place type gets large.
function bigNum(slot, x, y, s, color) {
  for (const ch of s) {
    const colon = ch === ':';
    const c = colon ? 2 : 4;
    place(slot++, `d-${color}-${colon ? 'colon' : ch}`, x, y, c, 3);
    x += c;
  }
  return x;
}

function gauge(x, y, w, f, c) {
  const n = Math.round(Math.max(0, Math.min(1, f)) * w);
  put(x, y, '━'.repeat(n), c);
  put(x + n, y, '─'.repeat(Math.max(0, w - n)), C.rule);
}

function title(x, y, w, s, focused, right) {
  put(x, y, s, focused ? C.ink : C.grey, null, 1);
  if (right) put(x + w - right.length, y, right, C.grey);
  hrule(x, y + 1, w, focused ? C.accent : C.rule);
}

function wrap(s, w) {
  const lines = [];
  let line = '';
  for (const word of s.split(' ')) {
    if (line && line.length + 1 + word.length > w) { lines.push(line); line = ''; }
    line += (line ? ' ' : '') + word;
  }
  lines.push(line);
  return lines;
}

const clip = (s, w) => (s.length > w ? s.slice(0, Math.max(0, w - 1)) + '…' : s);
const pad = (n, w) => String(n).padStart(w, '0');
const uid = (i) => pad(i, 4);
const rnd = (a, b) => a + Math.random() * (b - a);
const pick = (a) => a[Math.floor(Math.random() * a.length)];
const clock = (s) => `${pad(Math.floor(s / 60), 2)}:${pad(s % 60, 2)}`;

// ---------------------------------------------------------------- the world

const APPS = {
  kicad: {
    label: 'KiCad', window: 'demo — Schematic Editor', task: 'Add decoupling caps to U3 in demo.kicad_sch',
    steps: [
      { t: 'The old-format banner covers the sheet. Dismiss it, then read the symbol list.', c: ['click(933, 92)', 'screenshot()'], f: 0 },
      { t: 'Save in the new format before editing. File menu.', c: ['click(24, 42)'], f: 1 },
      { t: 'Save As is in the menu. Close it and use the shortcut instead.', c: ['key("Escape")', 'key("ctrl+s")'], f: 0 },
      { t: 'Add C1 beside U3 with the symbol chooser.', c: ['key("a")', 'type("C_Small")', 'key("Return")'], f: 0 },
      { t: 'Drop it on the +3V3 rail and wire it to ground.', c: ['click(612, 344)', 'key("w")', 'drag(612, 360, 612, 410)'], f: 0 },
    ],
  },
  blender: {
    label: 'Blender', window: 'Blender', task: 'Scale the default cube and set up a turntable render',
    steps: [
      { t: 'The splash screen is gone and the cube is selected in the outliner.', c: ['screenshot()'], f: 0 },
      { t: 'Scale it to 2 on all axes from the transform panel.', c: ['click(780, 200)', 'type("2")', 'key("Tab")', 'type("2")', 'key("Return")'], f: 0 },
      { t: 'Reopen the splash to check the version before scripting the render.', c: ['click(12, 12)', 'click(40, 240)'], f: 1 },
      { t: 'Close it. Add a keyframe on rotation at frame 1.', c: ['key("Escape")', 'click(770, 150)', 'key("i")'], f: 0 },
    ],
  },
  calc: {
    label: 'Calculator', short: 'Calc', window: 'Calculator', task: 'Check invoice line totals against the portal',
    steps: [
      { t: 'Enter the first line total.', c: ['type("1+2")'], f: 0 },
      { t: 'Evaluate it. The display should read 3.', c: ['key("Return")'], f: 1 },
      { t: 'It reads 3, which matches the portal. Clear and take the next line.', c: ['click(100, 372)'], f: 0 },
    ],
  },
  ltspice: {
    label: 'LTspice', window: 'LTspice', task: 'Run an AC sweep on the RC filter and read the -3 dB point',
    steps: [
      { t: 'A changelog window is over the schematic. Close it.', c: ['windows()', 'close(2)'], f: 0 },
      { t: 'It is still up: Wine ignored the close. Try Escape.', c: ['key("Escape")', 'screenshot()'], f: 1 },
      { t: 'Open the simulation dialog.', c: ['click(56, 10)', 'click(80, 40)'], f: 0 },
    ],
  },
};
const APP_NAMES = Object.keys(APPS);

const START = Date.now();
const PARK_AFTER = 30000;
const SCALES = [700, 127, 37, 12, 7];
// Cell shape, measured from the terminal at startup: height over width, and how
// many rows a 16:9 screen takes per column.
let cellW = 9, cellH = 19;
let cellRatio = 2.1;
let screenRows = (1 / cellRatio) * (9 / 16);
let scaleIdx = 0;
let agents = [];
let selected = 421;
let openUnit = null;
let focus = 'fleet';
let link = 'mcp';
let masterStep = 0;
let attention = -1;
let events = [];
const master = [];
const inputs = { master: '', loop: '' };
let steps = 0;
let stepTimes = [];
let fleetPos = [];

function newAgent(i, state) {
  const app = APP_NAMES[(i * 7 + (i >> 3)) % APP_NAMES.length];
  return {
    id: i, app, state, since: Date.now(), dur: rnd(800, 2800), long: false, step: Math.floor(rnd(1, 40)),
    mem: rnd(240, 420), memParked: rnd(70, 95), lines: [], shot: 0, ms: [],
  };
}

function spawn(n) {
  const now = Date.now();
  // At 700, the same split as frame 900 of the s700c run.
  const parkShare = n >= 500 ? 0.82 : n >= 100 ? 0.55 : 0.33;
  const actShare = n >= 500 ? 0.025 : n >= 100 ? 0.15 : 0.34;
  agents = [];
  for (let i = 0; i < n; i++) {
    const r = Math.random();
    let st = 'waiting';
    if (r < parkShare) st = 'parked';
    else if (r < parkShare + actShare) st = 'acting';
    else if (r > 0.97) st = 'queued';
    else if (r > 0.91) st = 'done';
    const a = newAgent(i, st);
    if (st === 'parked') a.since = now - rnd(0, 400000);
    if (st === 'waiting') { a.long = Math.random() < 0.5; a.since = now - rnd(0, 26000); }
    agents.push(a);
  }
  selected = n >= 500 ? 421 : 0;
  agents[selected].state = 'acting';
  events = [];
}

function ev(text, c = C.grey) {
  events.unshift({ text, c });
  if (events.length > 80) events.length = 80;
}

function setState(a, st) {
  a.state = st;
  a.since = Date.now();
}

// A big fleet spends most of its time waiting on the model; a small one is
// watched closely, so let it stay busy on screen.
const actFor = () => (agents.length < 100 ? rnd(1400, 3400) : rnd(400, 1200));
const waitFor = () => (agents.length < 100 ? rnd(600, 2000) : rnd(1500, 4500));

function thaw(a, verb) {
  ev(`${uid(a.id)} ${verb}, thaw ${rnd(0.2, a.state === 'parked' ? 5.5 : 1.5).toFixed(1)}ms`);
  setState(a, 'acting');
  a.long = false;
  a.dur = actFor();
}

function doStep(a) {
  const script = APPS[a.app].steps;
  const s = script[a.step % script.length];
  a.step++;
  a.shot = s.f;
  const m = { action: Math.round(rnd(20, 160)), settle: Math.round(rnd(180, 700)), observe: Math.round(rnd(120, 200)) };
  a.ms.push(m.action + m.settle + m.observe);
  if (a.ms.length > 60) a.ms.shift();
  steps++;
  stepTimes.push(Date.now());
  if (openUnit === a.id) {
    a.lines.push({ k: 'rule', s: `step ${a.step}   action ${m.action}   settle ${m.settle}   observe ${m.observe} ms` });
    a.lines.push({ k: 'think', s: s.t });
    for (const c of s.c) a.lines.push({ k: 'tool', s: c });
    a.lines.push({ k: 'res', s: `settled, ${pick(['no window changes', 'no window changes', '1 window opened', 'focus moved'])}` });
  }
}

function sim() {
  const now = Date.now();
  let active = 0;
  for (const a of agents) if (a.state === 'acting' || (a.state === 'waiting' && !a.long)) active++;
  const target = agents.length >= 500 ? 56 : agents.length >= 100 ? 36 : 5;
  for (const a of agents) {
    const t = now - a.since;
    if (a.state === 'queued') {
      if (a.queuedFor === undefined) a.queuedFor = rnd(6000, 120000);
      if (t > a.queuedFor) { setState(a, 'starting'); a.dur = rnd(500, 1500); }
    } else if (a.state === 'starting') {
      if (t > a.dur) { setState(a, 'acting'); a.dur = actFor(); }
    } else if (a.state === 'acting') {
      if (t > a.dur) {
        doStep(a);
        if (Math.random() < 0.004 && a.id !== openUnit) {
          setState(a, 'done');
          ev(`${uid(a.id)} done`, C.ink);
          continue;
        }
        a.long = active > target && Math.random() < 0.5;
        if (a.long) active--;
        ev(`${uid(a.id)} froze ${rnd(0.2, 1.4).toFixed(1)}ms`);
        setState(a, 'waiting');
        a.dur = waitFor();
      }
    } else if (a.state === 'waiting') {
      if (a.long ? t > PARK_AFTER : t > a.dur) {
        if (a.long) {
          ev(`${uid(a.id)} parked ${Math.round(a.mem)}→${Math.round(a.memParked)}M in ${Math.round(rnd(1400, 2800))}ms`);
          setState(a, 'parked');
        } else {
          thaw(a, 'woke');
        }
      }
    }
  }
  if (active < target) {
    const parked = agents.filter((a) => a.state === 'parked');
    for (let k = 0; k < Math.min(3, target - active) && parked.length; k++) {
      const a = pick(parked);
      if (a.state === 'parked') thaw(a, 'unparked');
    }
  }
  while (stepTimes.length && stepTimes[0] < now - 10000) stepTimes.shift();
}

function counts() {
  const n = { acting: 0, waiting: 0, parked: 0, starting: 0, done: 0, failed: 0, queued: 0 };
  for (const a of agents) n[a.state]++;
  return n;
}

// ----------------------------------------------------- master: MCP or a loop

const mline = (k, s) => { master.push({ k, s }); if (master.length > 200) master.shift(); };

function call(who, tool, args, result) {
  mline(who === 'you' ? 'youtool' : 'tool', `${tool}(${args})`);
  if (result) mline('res', result);
}

function openAgent(i) {
  const a = agents[i];
  if (!a) return;
  selected = i;
  openUnit = i;
  if (a.lines.length === 0) {
    a.lines.push({ k: 'sys', s: `task: ${APPS[a.app].task}` });
    a.step = Math.max(0, a.step - 3);
    for (let s = 0; s < 3; s++) doStep(a);
  }
  focus = 'loop';
}

function closeUnit() {
  openUnit = null;
  if (focus === 'loop') focus = 'fleet';
}

function runVerb(text, who) {
  const m = text.trim().match(/^(\w+)(?:\s+(\S+))?(?:\s+(.*))?$/);
  if (!m) return false;
  const [, verb, a1, rest] = m;
  const n = parseInt(a1, 10);
  const u = Number.isInteger(n) && agents[n] ? n : null;
  switch (verb) {
    case 'open':
      if (u === null) return false;
      call(who, 'panel.open', `unit: ${uid(u)}`, 'loop and cage on screen');
      openAgent(u);
      return true;
    case 'close':
      call(who, 'panel.close', '', 'ok');
      closeUnit();
      return true;
    case 'park':
      if (u === null) return false;
      call(who, 'agent.park', `unit: ${uid(u)}`, 'parked');
      setState(agents[u], 'waiting');
      agents[u].long = true;
      agents[u].since = Date.now() - PARK_AFTER;
      return true;
    case 'wake':
      if (u === null) return false;
      call(who, 'agent.wake', `unit: ${uid(u)}`, 'thawed');
      thaw(agents[u], 'woke');
      return true;
    case 'send':
      if (u === null) return false;
      call(who, 'agent.send', `unit: ${uid(u)}, text: "${rest || ''}"`, 'delivered to its loop');
      deliver(agents[u], rest || '', 'master');
      return true;
    case 'status': {
      const c = counts();
      call(who, 'fleet.status', '', `${c.acting} acting, ${c.waiting} waiting, ${c.parked} parked, ${c.failed} failed`);
      return true;
    }
    default:
      return false;
  }
}

function sendMaster(text) {
  if (!text.trim()) return;
  mline('you', text);
  if (runVerb(text, 'you')) return;
  if (link === 'loop') {
    setTimeout(() => {
      const c = counts();
      mline('rule', `master step ${++masterStep}`);
      mline('think', `${c.acting + c.waiting} units are live and ${c.failed} failed. Checking before I answer.`);
      call('master', 'fleet.status', '', `${c.acting} acting, ${c.waiting} waiting, ${c.parked} parked`);
      mline('say', `${c.parked} units are parked and cost little, so I'd leave them. I'll report back if something fails.`);
    }, 700);
  } else {
    mline('sys', 'Queued for claude-code. Panel verbs run here directly.');
  }
}

// What the master does by itself, so the panel shows it being driven.
function masterAuto() {
  const live = agents.filter((a) => a.state === 'acting' || a.state === 'waiting');
  const a = live.length ? pick(live) : null;
  const c = counts();
  if (link === 'loop') mline('rule', `master step ${++masterStep}`);
  const r = Math.random();
  if (r < 0.34 || !a) {
    if (link === 'loop') mline('think', 'Periodic fleet check.');
    call('master', 'fleet.status', '', `${c.acting} acting, ${c.waiting} waiting, ${c.parked} parked, ${c.done} done, ${c.failed} failed`);
  } else if (r < 0.7) {
    attention = a.id;
    if (link === 'loop') mline('think', `Unit ${uid(a.id)} has been on this for a while. Reading its last step.`);
    const script = APPS[a.app].steps;
    call('master', 'agent.read', `unit: ${uid(a.id)}, last: 1`, `step ${a.step}: ${script[a.step % script.length].t}`);
  } else {
    attention = a.id;
    if (link === 'loop') mline('think', `Unit ${uid(a.id)} needs a nudge.`);
    const msg = pick(['Save before you close anything.', 'Skip the changelog, it is not your task.', 'Report the value, do not round it.']);
    call('master', 'agent.send', `unit: ${uid(a.id)}, text: "${msg}"`, 'delivered to its loop');
    deliver(a, msg, 'master');
  }
}

function deliver(a, text, who) {
  if (a.lines.length === 0) a.lines.push({ k: 'sys', s: `task: ${APPS[a.app].task}` });
  a.lines.push({ k: who, s: text });
  if (a.state === 'parked' || a.state === 'waiting') thaw(a, a.state === 'parked' ? 'unparked' : 'woke');
  if (openUnit === a.id) setTimeout(() => a.lines.push({ k: 'think', s: `Noted. Carrying on with that in mind.` }), 900);
}

// ------------------------------------------------------------------- header

const HEADER = 6;

function header(n) {
  // The name: the only place the display serif appears.
  place(1, 'mark', 2, 1, Math.round(3 * cellRatio), 3);
  place(2, 'title', 11, 1, 66, 3);

  // Unit counts, large. Colors match the fleet below.
  const tally = (x, slot, label, v, color, labelColor) => {
    bigNum(slot, x, 1, String(v), color);
    put(x, 4, label, labelColor);
  };
  const tx = 80;
  if (W >= 122) {
    tally(tx, 200, 'ACTING', n.acting, 'accent', C.accent);
    tally(tx + 15, 210, 'WAITING', n.waiting, 'ink', C.text);
    tally(tx + 30, 220, 'PARKED', n.parked, 'grey', C.grey);
    if (W >= 137) tally(tx + 45, 230, 'DONE', n.done, 'grey', C.grey);
    if (W >= 152) tally(tx + 60, 240, 'FAILED', n.failed, n.failed ? 'red' : 'dim', n.failed ? C.red : C.dim);
  }

  // Host numbers: plain, two aligned columns.
  if (W >= 198) {
    const x = W - 46;
    const live = n.acting + n.waiting;
    const frac = agents.length / 700;
    const ram = 22 + live * 0.32 + n.parked * 0.05 + 8 * frac;
    const pool = n.parked * 0.026;
    const cpu = 4 + n.acting * 1.8;
    const t = Math.floor((Date.now() - START) / 1000) + 224;
    const rate = stepTimes.length / Math.min(10, Math.max(1, (Date.now() - START) / 1000));
    const row = (y, a, av, b, bv) => {
      put(x, y, a, C.grey); put(x + 7, y, av, C.ink);
      put(x + 24, y, b, C.grey); put(x + 31, y, bv, C.ink);
    };
    row(0, 'HOST', 'rig', 'RAM', `${ram.toFixed(0)} / 126 G`);
    row(1, 'UNITS', String(agents.length), 'ZSWAP', `${pool.toFixed(1)} G  5.9:1`);
    row(2, 'STEP', `${Math.round(410)} ms harness`, 'CPU', `${cpu.toFixed(0)} %`);
    row(3, 'RATE', `${rate.toFixed(1)} steps/s`, 'RUN', `${pad(Math.floor(t / 3600), 2)}:${clock(t % 3600)}`);
    put(x, 4, 'simulated fleet', C.dim);
  }
  hrule(0, HEADER - 1, W, n.failed ? C.red : C.rule);
}

// -------------------------------------------------------------------- fleet
//
// The honeycomb is generated from the unit count:
//
//   1. Each candidate hex size gives a lattice clipped to the panel.
//   2. The largest size whose lattice holds every unit wins.
//   3. Units take the lattice positions nearest the panel's centre, measured in
//      panel widths and heights, so the fleet is one solid block in the panel's
//      shape, with no holes.
//   4. What a hex shows follows from its size: nothing, then its ID and app,
//      then its real screen.
//
// Hexes are pixels, not glyphs. Neighbouring glyph hexes fight over shared
// cells and come out notched. Here each look and size is one anti-aliased
// sprite, and each unit is one Kitty placement of it below the text layer, so a
// state change costs one short escape however large the fleet is.

const STYLE = {
  acting: { c: C.accent, solid: true },
  starting: { c: C.accent, solid: false },
  waiting: { c: 0xa0a0a0, solid: true },
  parked: { c: 0x5c5c5c, solid: false },
  done: { c: 0x363636, solid: true },
  failed: { c: C.red, solid: true },
  queued: { c: 0x363636, solid: false },
};

const HEX_SLOT = 10000;

// `hh` rows tall, `stepx` columns between hex columns. Sizes that carry text
// stay on whole cells so the text centres; smaller ones can be fractional.
function shapes(ratio) {
  const list = [];
  for (let hh = 40; hh >= 4; hh -= 2) list.push({ hh, stepx: Math.round(0.866 * hh * ratio) });
  for (let k = 70; k >= 20; k--) list.push({ hh: k / 20, stepx: 0.866 * (k / 20) * ratio });
  return list;
}

// Hex centres inside a w by h panel, nearest the centre first.
function lattice(shape, w, h, shift) {
  const { hh, stepx } = shape;
  const half = stepx * 2 / 3;
  const cx0 = Math.floor(w / 2), cy0 = Math.floor(h / 2) + shift;
  const found = [];
  const qm = Math.ceil(w / stepx) + 1;
  for (let q = -qm; q <= qm; q++) {
    const cx = cx0 + q * stepx;
    if (cx - half < 0 || cx + half > w) continue;
    const rm = Math.ceil(h / hh) + Math.abs(q) + 1;
    for (let r = -rm; r <= rm; r++) {
      const cy = cy0 + q * hh / 2 + r * hh;
      if (cy - hh / 2 < 0 || cy + hh / 2 > h) continue;
      const dx = (cx - w / 2) / (w / 2), dy = (cy - h / 2) / (h / 2);
      // Fourth powers make a rounded rectangle, which fills the panel; squares
      // make an ellipse, which wastes its corners.
      found.push({ cx, cy, d: Math.round((dx ** 4 + dy ** 4) * 1e6), t: Math.atan2(cy - h / 2, cx - w / 2) });
    }
  }
  return found.sort((a, b) => a.d - b.d || a.t - b.t);
}

// The lattice can sit on a hex or on the edge between two; take the tighter.
function cellsFor(shape, n, w, h) {
  let best = null;
  for (const shift of [0, shape.hh / 2]) {
    const all = lattice(shape, w, h, shift);
    if (all.length >= n && (!best || all[n - 1].d < best[n - 1].d)) best = all;
  }
  return best && best.slice(0, n);
}

let layoutKey = '';
let layout = null;

function fit(n, w, h, maxHh) {
  const key = [n, w, h, maxHh, cellW, cellH].join();
  if (key === layoutKey) return layout;
  layoutKey = key;
  const list = shapes(cellRatio);
  for (const shape of list) {
    if (shape.hh > maxHh) continue;
    const cells = cellsFor(shape, n, w, h);
    if (cells) return (layout = { ...shape, cells, tall: h });
  }
  // More units than the panel holds: keep the smallest hex and scroll.
  const small = list[list.length - 1];
  // Too narrow for one column: no height helps, so show no hexes.
  if (!lattice(small, w, small.hh * 2, 0).length) return (layout = { ...small, cells: [], tall: h });
  let tall = h;
  let cells = null;
  while (!cells) {
    tall += 8;
    cells = cellsFor(small, n, w, tall);
  }
  return (layout = { ...small, cells, tall });
}

// How much of the pixel at (px, py) lies inside a flat-top hex centred on the
// origin, with half-width a and half-height b.
function hexCover(px, py, a, b) {
  const ay = Math.abs(py);
  const vert = Math.max(0, Math.min(1, b - ay + 0.5));
  if (!vert) return 0;
  const edge = a * (1 - Math.min(ay, b) / b / 2);
  return vert * Math.max(0, Math.min(1, edge - Math.abs(px) + 0.5));
}

function hexSprite(g, color, solid) {
  const name = `hex-${g.hh}-${g.stepx}-${cellW}x${cellH}-${color}-${solid ? 's' : 'o'}`;
  if (IMG[name]) return name;
  const tall = g.hh * cellH, wide = g.stepx * cellW * 4 / 3;
  const pw = Math.ceil(g.stepx * 4 / 3) * cellW, ph = Math.ceil(g.hh) * cellH;
  // Shrinking every hex by the same factor leaves an even gap between them.
  const shrink = (tall - Math.min(cellW * 0.5, tall * 0.14)) / tall;
  const a = wide / 2 * shrink, b = tall / 2 * shrink;
  const stroke = Math.max(1.5, cellW * 0.16);
  const px = Buffer.alloc(pw * ph * 4);
  for (let y = 0; y < ph; y++) {
    for (let x = 0; x < pw; x++) {
      const dx = x + 0.5 - pw / 2, dy = y + 0.5 - ph / 2;
      let cover = hexCover(dx, dy, a, b);
      if (cover && !solid) cover -= hexCover(dx, dy, a - stroke * 1.155, b - stroke);
      if (cover <= 0) continue;
      const i = (y * pw + x) * 4;
      px[i] = (color >> 16) & 255; px[i + 1] = (color >> 8) & 255; px[i + 2] = color & 255;
      px[i + 3] = Math.round(cover * 255);
    }
  }
  uploadRaw(name, px, pw, ph);
  return name;
}

function placeHex(slot, g, cx, cy, color, solid) {
  const name = hexSprite(g, color, solid);
  const cols = Math.ceil(g.stepx * 4 / 3), rows = Math.ceil(g.hh);
  const left = Math.round(cx * cellW - cols * cellW / 2);
  const top = Math.round(cy * cellH - rows * cellH / 2);
  const col = Math.floor(left / cellW), row = Math.floor(top / cellH);
  // Native size: a stretched placement loses its pixel offset from its height.
  place(slot, name, col, row, 0, 0, left - col * cellW, top - row * cellH, -1);
}

function drawHex(g, p, a, i, sel, watched) {
  const { hh } = g;
  const st = STYLE[a.state];
  const showScreen = hh >= 10;
  let c = st.c;
  let solid = st.solid && !showScreen;
  if (watched) { c = C.ink; solid = false; }
  if (sel) { c = C.ink; solid = !showScreen; }
  placeHex(HEX_SLOT + i, g, p.cx, p.cy, c, solid);

  if (hh < 4) return;
  const app = APPS[a.app].short || APPS[a.app].label;
  const label = (y, s, fg) => put(Math.round(p.cx - [...s].length / 2), y, s, fg);
  if (!showScreen) {
    const ink = !solid ? (sel ? C.ink : a.state === 'parked' ? C.grey : C.text) : c === STYLE.done.c ? 0x909090 : C.black;
    label(p.cy - 1, uid(a.id), ink);
    // Clipped to the hex's width one row from its centre, less a margin.
    label(p.cy, app.slice(0, Math.floor(g.stepx * 4 / 3 * (1 - 1 / hh)) - 2), ink);
    return;
  }

  // Big enough for its real screen, with a line above and below. The screen's
  // corners and those lines have to clear the slants.
  const half = g.stepx * 2 / 3;
  let rows = hh - 4, cols = 0;
  for (; rows > 1; rows--) {
    cols = Math.round(rows / screenRows);
    if (cols <= 2 * half * (1 - (rows / 2 + 1) / hh) - 2) break;
  }
  const frozen = a.state === 'parked';
  const ix = Math.round(p.cx - cols / 2);
  const iy = Math.round(p.cy - rows / 2);
  place(100 + i, `cap-${a.app}-${a.shot}${frozen ? '-dim' : ''}`, ix, iy, cols, rows);
  label(iy - 1, `${uid(a.id)}  ${app}`, sel ? C.ink : C.text);
  const state = a.state === 'waiting' && a.long
    ? `parks in ${Math.max(0, Math.ceil((PARK_AFTER - (Date.now() - a.since)) / 1000))}s`
    : a.state;
  label(iy + rows, state, a.state === 'acting' ? C.accent : a.state === 'failed' ? C.red : C.grey);
}

function fleetPanel(x, y, w, h, compact) {
  title(x, y, w, 'FLEET', focus === 'fleet', `${agents.length} units`);
  const n = agents.length;
  const iy = y + 2, ih = h - (compact ? 3 : 4);
  const g = fit(n, w, ih, compact ? 6 : 40);

  // Scroll only when the fleet outgrows the panel at the smallest hex.
  const sel = g.cells[selected];
  const maxScroll = Math.max(0, g.tall - ih);
  const scroll = Math.max(0, Math.min(maxScroll, Math.round((sel ? sel.cy : 0) - ih / 2)));

  const wide = g.stepx * 4 / 3;
  fleetPos = g.cells.map((p) => {
    const cx = x + p.cx, cy = iy + p.cy - scroll;
    const top = cy - g.hh / 2;
    return { cx, cy, x: cx - wide / 2, y: top, w: wide, h: g.hh, shown: top >= iy && top + g.hh <= iy + ih };
  });

  agents.forEach((a, i) => {
    const p = fleetPos[i];
    if (!p || !p.shown) return;
    drawHex(g, p, a, i, i === selected, a.id === attention);
  });

  // The selected unit, in one line.
  if (!compact && agents[selected]) {
    const a = agents[selected];
    const ry = y + h - 2;
    hrule(x, ry, w);
    let cx = put(x, ry + 1, uid(a.id), C.ink, null, 1);
    cx = put(cx + 2, ry + 1, APPS[a.app].label, C.text);
    cx = put(cx + 2, ry + 1, a.state, a.state === 'acting' ? C.accent : a.state === 'failed' ? C.red : C.text);
    cx = put(cx + 1, ry + 1, `${((Date.now() - a.since) / 1000).toFixed(0)}s`, C.grey);
    cx = put(cx + 2, ry + 1, `step ${a.step}`, C.grey);
    put(cx + 3, ry + 1, clip(APPS[a.app].task, Math.max(0, x + w - cx - 26)), C.text);
    put(x + w - 20, ry + 1, 'enter opens its loop', C.dim);
  }
}

// -------------------------------------------------------------- transcripts

const LINE = {
  you: { p: 'you', c: C.ink },
  master: { p: 'master', c: C.ink },
  say: { p: 'master', c: C.ink },
  think: { p: '', c: C.text },
  tool: { p: '', c: C.accent, indent: '→ ' },
  youtool: { p: '', c: C.ink, indent: '→ ' },
  res: { p: '', c: C.grey },
  sys: { p: '', c: C.dim },
};
const GUTTER = 7;

function transcript(x, y, w, h, lines) {
  const rows = [];
  for (const l of lines) {
    if (l.k === 'rule') {
      rows.push({ s: '', c: C.dim });
      rows.push({ s: clip(l.s, w), c: C.dim });
      continue;
    }
    const st = LINE[l.k] || LINE.res;
    const body = (st.indent || '') + l.s;
    wrap(body, w - GUTTER).forEach((text, i) => rows.push({ p: i ? '' : st.p, s: text, c: st.c, gutter: true }));
  }
  rows.slice(-h).forEach((r, j) => {
    if (!r.gutter) { put(x, y + j, r.s, r.c); return; }
    if (r.p) put(x, y + j, r.p, C.grey);
    put(x + GUTTER, y + j, r.s, r.c);
  });
}

function inputLine(x, y, w, text, focused, hint) {
  hrule(x, y, w, focused ? C.accent : C.rule);
  const px = put(x, y + 1, '› ', focused ? C.accent : C.dim);
  const room = x + w - px - 1;
  const end = put(px, y + 1, [...text].slice(-room).join(''), C.ink);
  if (focused) put(end, y + 1, '▏', C.accent);
  if (!text) put(end + 1, y + 1, clip(hint, Math.max(0, room - 2)), C.dim);
}

function masterPanel(x, y, w, h) {
  const focused = focus === 'master';
  const linkText = link === 'mcp' ? 'MCP · claude-code' : `panoptes-loop · step ${masterStep}`;
  title(x, y, w, 'MASTER', focused, linkText);
  transcript(x, y + 2, w, h - 4, master);
  inputLine(x, y + h - 2, w, inputs.master, focused,
    link === 'mcp' ? 'open 421 · send 421 … · wake 421' : 'talk to the master');
}

function loopPanel(x, y, w, h, a) {
  const focused = focus === 'loop';
  title(x, y, w, `UNIT ${uid(a.id)}`, focused, `${APPS[a.app].label} · panoptes-loop`);
  transcript(x, y + 2, w, h - 4, a.lines);
  inputLine(x, y + h - 2, w, inputs.loop, focused, 'message this unit; it thaws if frozen');
}

// --------------------------------------------------------------------- cage

function cagePanel(x, y, w, h, a) {
  const frozen = a.state === 'parked';
  title(x, y, w, `CAGE ${uid(a.id)}`, false, frozen ? 'frozen · last capture' : '1280×720');
  const ih = Math.round(w * screenRows);
  place(20, `cap-${a.app}-${a.shot}${frozen ? '-dim' : ''}`, x, y + 2, w, ih);

  // Under the screen: one large clock, then plain numbers.
  const ty = y + 2 + ih + 1;
  const t = Date.now() - a.since;
  let label, value, color, labelColor;
  if (a.state === 'waiting' && a.long) {
    label = 'UNTIL PARK'; value = Math.ceil(Math.max(0, PARK_AFTER - t) / 1000); color = 'ink'; labelColor = C.text;
  } else if (a.state === 'waiting') {
    label = 'MODEL WAIT'; value = Math.floor(t / 1000); color = 'ink'; labelColor = C.text;
  } else if (frozen) {
    label = 'PARKED FOR'; value = Math.floor(t / 1000); color = 'grey'; labelColor = C.grey;
  } else if (a.state === 'failed') {
    label = 'FAILED'; value = Math.floor(t / 1000); color = 'red'; labelColor = C.red;
  } else {
    label = a.state.toUpperCase(); value = Math.floor(t / 1000); color = 'accent'; labelColor = C.accent;
  }
  put(x, ty, label, labelColor);
  bigNum(300, x, ty + 1, clock(value), color);

  const rx = x + 26;
  const rw = x + w - rx;
  const row = (j, k, v) => { put(rx, ty + j, k, C.grey); put(rx + rw - v.length, ty + j, v, C.ink); };
  const mem = frozen ? a.memParked : a.mem;
  row(0, 'memory.current', `${Math.round(mem)} M`);
  gauge(rx, ty + 1, rw, mem / 500, frozen ? C.grey : C.text);
  const last = a.ms[a.ms.length - 1] || 0;
  row(2, 'harness ms/step', String(last));
  const spark = '▁▂▃▄▅▆▇█';
  const vals = a.ms.slice(-rw);
  const max = Math.max(1, ...vals);
  put(rx, ty + 3, vals.map((v) => spark[Math.min(7, Math.floor((v / max) * 8))]).join(''), C.grey);

  const wy = ty + 5;
  if (wy + 2 < y + h) {
    hrule(x, wy, w);
    put(x, wy + 1, 'WINDOWS', C.grey);
    put(x + 12, wy + 1, clip(APPS[a.app].window, w - 22), C.ink);
    put(x + w - 7, wy + 1, 'focused', C.dim);
    put(x, wy + 2, 'TASK', C.grey);
    put(x + 12, wy + 2, clip(APPS[a.app].task, w - 12), C.text);
  }
}

// ---------------------------------------------------------------------- log

function logPanel(y, h) {
  hrule(0, y, W);
  let row = y + 1, cx = 2;
  for (const e of events) {
    const n = e.text.length + 4;
    if (cx + n >= W - 2) { row++; cx = 2; }
    if (row >= y + h - 1) break;
    put(cx, row, e.text, e.c);
    cx += n;
  }
  const keys = {
    fleet: openUnit !== null
      ? 'arrows select   enter open   esc close   tab panels   L master link   + - add or remove   z fleet size   ! fail units   q quit'
      : 'arrows select   enter open loop and cage   tab panels   L master link   + - add or remove   z fleet size   ! fail units   q quit',
    master: 'type a panel verb or a message   enter send   tab panels   esc back',
    loop: 'type to this unit\'s loop   enter send   tab panels   esc close',
  }[focus];
  put(2, H - 1, clip(keys, W - 4), C.dim);
}

// --------------------------------------------------------------------- draw

function draw() {
  const n = counts();
  header(n);
  const logH = 5;
  const by = HEADER, bh = H - HEADER - logH;
  if (openUnit !== null && agents[openUnit]) {
    const a = agents[openUnit];
    const lw = Math.max(36, Math.min(50, Math.floor(W * 0.21)));
    const cw = Math.max(64, Math.min(104, Math.floor(W * 0.44)));
    const cx = W - cw - 2;
    const mx = lw + 4;
    const fh = Math.floor(bh * 0.5);
    fleetPanel(2, by, lw, fh, true);
    masterPanel(2, by + fh, lw, bh - fh);
    vrule(lw + 3, by, bh);
    loopPanel(mx + 1, by, cx - mx - 4, bh, a);
    vrule(cx - 2, by, bh);
    cagePanel(cx, by, cw, bh, a);
  } else {
    const mw = Math.max(44, Math.min(72, Math.floor(W * 0.3)));
    masterPanel(2, by, mw, bh);
    vrule(mw + 3, by, bh);
    fleetPanel(mw + 5, by, W - mw - 7, bh, false);
  }
  logPanel(H - logH, logH);
  flush();
}

// -------------------------------------------------------------------- input

function failUnits() {
  if (counts().failed) {
    for (const a of agents) if (a.state === 'failed') { setState(a, 'starting'); a.dur = rnd(500, 1500); }
    mline('sys', 'Failed units restarted.');
    return;
  }
  const live = agents.filter((a) => a.state === 'acting' || a.state === 'waiting');
  for (let k = 0; k < 3 && live.length; k++) {
    const a = pick(live);
    setState(a, 'failed');
    ev(`${uid(a.id)} failed: kiosk exited`, C.red);
  }
  call('master', 'fleet.status', 'state: "failed"', `${counts().failed} units failed`);
}

// Spawning or stopping units live: the honeycomb re-fits on the next draw.
function addUnits(k) {
  const n = Math.max(1, agents.length + k);
  while (agents.length < n) {
    const a = newAgent(agents.length, 'starting');
    a.dur = rnd(500, 1500);
    agents.push(a);
    ev(`${uid(a.id)} spawned`, C.ink);
  }
  while (agents.length > n) {
    const a = agents.pop();
    ev(`${uid(a.id)} stopped`);
    if (openUnit === a.id) closeUnit();
  }
  selected = Math.min(selected, agents.length - 1);
}

function cycleFocus() {
  const order = openUnit !== null ? ['fleet', 'master', 'loop'] : ['fleet', 'master'];
  focus = order[(order.indexOf(focus) + 1) % order.length];
}

// Units sit in a spiral, so arrows go to the nearest hex in that direction on
// screen rather than to the next number.
function moveSel(dx, dy) {
  const p = fleetPos[selected];
  if (!p) return;
  let best = -1, bestScore = Infinity;
  fleetPos.forEach((q, i) => {
    if (i === selected) return;
    const vx = q.x - p.x, vy = (q.y - p.y) * cellRatio;
    const along = vx * dx + vy * dy;
    if (along <= 0) return;
    const across = Math.abs(vx * dy) + Math.abs(vy * dx);
    const score = along + across * 2;
    if (score < bestScore) { bestScore = score; best = i; }
  });
  if (best < 0) return;
  selected = best;
  if (openUnit !== null) {
    const f = focus;
    openAgent(selected);
    focus = f;
  }
}

function onKey(k, ch) {
  if (focus === 'master' || focus === 'loop') {
    if (k === 'esc') {
      if (focus === 'loop') closeUnit();
      focus = 'fleet';
    } else if (k === 'tab') cycleFocus();
    else if (k === 'enter') {
      const text = inputs[focus];
      inputs[focus] = '';
      if (focus === 'master') sendMaster(text);
      else if (text.trim() && openUnit !== null) deliver(agents[openUnit], text, 'you');
    } else if (k === 'bs') inputs[focus] = [...inputs[focus]].slice(0, -1).join('');
    else if (ch) inputs[focus] += ch;
    return;
  }
  switch (k) {
    case 'left': moveSel(-1, 0); break;
    case 'right': moveSel(1, 0); break;
    case 'up': moveSel(0, -1); break;
    case 'down': moveSel(0, 1); break;
    case 'tab': cycleFocus(); break;
    case 'enter': openAgent(selected); break;
    case 'esc': closeUnit(); break;
  }
  switch (ch) {
    case 'h': moveSel(-1, 0); break;
    case 'l': moveSel(1, 0); break;
    case 'k': moveSel(0, -1); break;
    case 'j': moveSel(0, 1); break;
    case 'q': quit(); break;
    case 'm': focus = 'master'; break;
    case 'L':
      link = link === 'mcp' ? 'loop' : 'mcp';
      mline('sys', link === 'loop' ? 'Master now runs in its own panoptes-loop.' : 'Master now connects over MCP.');
      break;
    case 'z': case 'Z': scaleIdx = (scaleIdx + 1) % SCALES.length; openUnit = null; spawn(SCALES[scaleIdx]); break;
    case '!': failUnits(); break;
    case '+': case '=': addUnits(1); break;
    case '-': addUnits(-1); break;
  }
}

function onMouse(b, x, y, press) {
  if (!press || b !== 0) return;
  // Hex bounding boxes overlap, so the click goes to the nearest centre.
  let hit = -1, near = Infinity;
  fleetPos.forEach((p, i) => {
    if (!p.shown || x < p.x || x >= p.x + p.w || y < p.y || y >= p.y + p.h) return;
    const d = (x + 0.5 - p.cx) ** 2 + ((y + 0.5 - p.cy) * cellRatio) ** 2;
    if (d < near) { near = d; hit = i; }
  });
  if (hit < 0) return;
  if (selected === hit || openUnit !== null) openAgent(hit);
  else { selected = hit; focus = 'fleet'; }
}

function onInput(s) {
  while (s.length) {
    let m;
    if ((m = s.match(/^\x1b\[6;(\d+);(\d+)t/))) {
      cellH = +m[1]; cellW = +m[2];
      cellRatio = cellH / cellW;
      screenRows = (1 / cellRatio) * (9 / 16);
      layoutKey = '';
      s = s.slice(m[0].length);
    } else if ((m = s.match(/^\x1b\[<(\d+);(\d+);(\d+)([Mm])/))) {
      onMouse(+m[1], +m[2] - 1, +m[3] - 1, m[4] === 'M');
      s = s.slice(m[0].length);
    } else if ((m = s.match(/^\x1b\[([ABCD])/))) {
      onKey({ A: 'up', B: 'down', C: 'right', D: 'left' }[m[1]]);
      s = s.slice(m[0].length);
    } else if ((m = s.match(/^\x1b\[Z/))) {
      cycleFocus();
      s = s.slice(m[0].length);
    } else if ((m = s.match(/^\x1b[\[O][0-9;]*[A-Za-z~]/))) {
      s = s.slice(m[0].length);
    } else if (s[0] === '\x1b') {
      onKey('esc');
      s = s.slice(1);
    } else {
      const c = String.fromCodePoint(s.codePointAt(0));
      s = s.slice(c.length);
      if (c === '\r' || c === '\n') onKey('enter');
      else if (c === '\t') onKey('tab');
      else if (c === '\x7f' || c === '\b') onKey('bs');
      else if (c === '\x03') quit();
      else if (c >= ' ') onKey(null, c);
    }
  }
  draw();
}

// ---------------------------------------------------------------- lifecycle

function resize() {
  W = process.stdout.columns || 200;
  H = process.stdout.rows || 60;
  cur = alloc();
  prev = null;
  slotsHave = new Map();
  // Placements only. Never ESC[2J: a terminal clear also frees every uploaded
  // image without a placement. prev = null repaints every cell anyway.
  out('\x1b[0m\x1b_Ga=d,d=a,q=2\x1b\\');
}

function quit() {
  out('\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l\x1b[0m');
  process.exit(0);
}

function main() {
  out('\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h');
  out('\x1b[16t');
  resize();
  for (const name of fs.readdirSync(ASSETS)) if (name.endsWith('.png')) upload(name.slice(0, -4));
  spawn(SCALES[scaleIdx]);
  mline('sys', 'Panel online. Master connects over MCP by default.');
  call('master', 'fleet.status', '', 'reading');

  process.stdin.setRawMode(true);
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', onInput);
  process.stdout.on('resize', () => { resize(); draw(); });
  process.on('SIGTERM', quit);

  setInterval(sim, 250);
  setInterval(draw, 200);
  setInterval(masterAuto, 5200);

  // Test hook: keys appended to this file are read as input.
  const ctl = process.env.MOCK_CTL;
  if (ctl) {
    setInterval(() => {
      let s = '';
      try { s = fs.readFileSync(ctl, 'utf8'); } catch { return; }
      if (s) { fs.writeFileSync(ctl, ''); onInput(s); }
    }, 100);
  }
  draw();
}

main();
