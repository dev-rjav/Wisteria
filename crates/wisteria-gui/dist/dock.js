// Wisteria floating dock window — always-on-top, frameless, transparent. Reflects engine state
// and resizes the OS window to hug its pill (so it barely blocks clicks behind it). Idle is a
// rice-sized ovalish pill; hovering expands it; listening/processing bloom to the full wave.
'use strict';

const TAURI = window.__TAURI__;
const invoke = TAURI ? TAURI.core.invoke : async () => null;
const listen = TAURI ? TAURI.event.listen : async () => () => {};
const win = TAURI ? TAURI.window.getCurrentWindow() : null;
const { LogicalSize, LogicalPosition } = (TAURI && TAURI.window) || {};

const $ = (id) => document.getElementById(id);

let phase = 'idle';
let enabled = true;
let words = 0;
let seconds = 0;
let timer = null;
let hovering = false;
let lastW = 0, lastH = 0;   // last applied window size — skip redundant resizes (avoids flicker)
let laying = false;         // re-entrancy guard for the async resize
let collapseTimer = null;   // debounce so transient leave events during a resize don't collapse

// Logical window sizes per state. Idle is tiny (rice); the transparent margin around the pill
// (from .wrap padding) keeps the soft shadow from clipping into a rectangle.
const SIZES = {
  idle:       [96, 34],
  hover:      [300, 74],
  listening:  [480, 116],
  processing: [460, 108],
};

function stateName() {
  if (enabled && phase === 'listening') return 'active';
  if (enabled && phase === 'processing') return 'active';
  if (hovering) return 'hover';
  return 'idle';
}

function currentSize() {
  if (enabled && phase === 'listening') return SIZES.listening;
  if (enabled && phase === 'processing') return SIZES.processing;
  if (hovering) return SIZES.hover;
  return SIZES.idle;
}

async function layout() {
  if (!win || !LogicalSize) return;
  const [w, h] = currentSize();
  // Skip if the target size is already applied — resizing to the same size still perturbs the
  // cursor/hit-testing and is the main source of the hover flicker.
  if (w === lastW && h === lastH) return;
  if (laying) return;
  laying = true;
  lastW = w; lastH = h;
  try {
    // Bottom-anchored: keep the bottom edge fixed so the pill grows upward, never jumping under
    // the cursor (which would bounce hover state).
    // Sit well clear of the taskbar. screen.availHeight already excludes it, but a small gap
    // still overlapped, so keep a comfortable margin above the work-area bottom.
    const x = Math.round((screen.availWidth - w) / 2);
    const y = Math.round(screen.availHeight - h - 52);
    await win.setSize(new LogicalSize(w, h));
    await win.setPosition(new LogicalPosition(x, y));
  } catch (e) { lastW = 0; lastH = 0; /* force retry next time */ }
  finally { laying = false; }
}

function render() {
  const listening = enabled && phase === 'listening';
  const proc = enabled && phase === 'processing';
  const st = stateName();

  document.body.classList.toggle('s-idle', st === 'idle');
  document.body.classList.toggle('s-hover', st === 'hover');
  document.body.classList.toggle('s-active', st === 'active');

  $('dot').classList.toggle('live', listening);
  $('dot').classList.toggle('proc', proc);
  $('wds').textContent = words + ' WORDS';
  if (window.WisteriaWave) window.WisteriaWave.setMode(listening ? 'listening' : (proc ? 'processing' : 'idle'));
}

function setPhase(p) {
  phase = p;
  if (p === 'listening') startTimer(); else stopTimer();
  layout();
  render();
}

function startTimer() { seconds = 0; updateTime(); clearInterval(timer); timer = setInterval(() => { seconds++; updateTime(); }, 1000); }
function stopTimer() { clearInterval(timer); }
function updateTime() { $('time').textContent = String(Math.floor(seconds / 60)).padStart(2, '0') + ':' + String(seconds % 60).padStart(2, '0'); }

function applyHover(on) {
  if (hovering === on) return;
  hovering = on;
  layout();
  render();
}

// Enter is immediate; leave is debounced so the brief pointerout the OS emits while the window
// is resizing doesn't collapse the dock and start an expand/collapse loop.
function hoverEnter() {
  clearTimeout(collapseTimer);
  applyHover(true);
}
function hoverLeave() {
  clearTimeout(collapseTimer);
  collapseTimer = setTimeout(() => applyHover(false), 180);
}

async function init() {
  const cfg = await invoke('get_config') || {};
  const key = (cfg.ptt_key || 'F8').split('+').map((s) => s.trim().replace(/^Meta.*/i, 'Win').replace(/^Control.*/i, 'Ctrl').replace(/Left|Right/i, '')).join(' ');
  $('hint-key').textContent = key;

  const status = await invoke('engine_status') || { enabled: true, phase: 'idle' };
  enabled = status.enabled;
  phase = status.phase;

  await listen('engine-event', (e) => {
    const p = e.payload;
    if (p.kind === 'phase') {
      if (p.phase === 'disabled') { enabled = false; setPhase('idle'); }
      else { enabled = true; setPhase(p.phase); }
    } else if (p.kind === 'transcript') {
      words += (p.words || 0);
      render();
    }
  });

  // The whole (tiny) window is the hover target. Track hover at the document root with pointer
  // events; enter cancels any pending collapse, leave debounces it (see hoverLeave).
  const root = document.documentElement;
  root.addEventListener('pointerenter', hoverEnter);
  root.addEventListener('pointerleave', hoverLeave);
  root.addEventListener('pointermove', hoverEnter);

  layout();
  render();
}

window.addEventListener('DOMContentLoaded', init);
