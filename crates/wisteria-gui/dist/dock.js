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
  try {
    await win.setSize(new LogicalSize(w, h));
    const x = Math.round((screen.availWidth - w) / 2);
    const y = Math.round(screen.availHeight - h - 16);
    await win.setPosition(new LogicalPosition(x, y));
  } catch (e) { /* ignore */ }
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

function setHover(on) {
  if (hovering === on) return;
  hovering = on;
  layout();
  render();
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

  // The whole (tiny) window is the hover target, so listen on the document.
  document.addEventListener('mouseenter', () => setHover(true), true);
  document.addEventListener('mouseleave', () => setHover(false), true);
  window.addEventListener('mouseover', () => setHover(true));
  window.addEventListener('mouseout', (e) => { if (!e.relatedTarget) setHover(false); });

  layout();
  render();
}

window.addEventListener('DOMContentLoaded', init);
