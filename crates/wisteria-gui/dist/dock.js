// Wisteria floating dock window — always-on-top, frameless, transparent. Reflects engine state
// and resizes itself to hug the pill (so it barely blocks clicks behind it).
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

const SIZES = { idle: [240, 64], listening: [470, 104], processing: [460, 100] };

async function layout() {
  if (!win || !LogicalSize) return;
  const active = enabled && (phase === 'listening' || phase === 'processing');
  const [w, h] = active ? (phase === 'processing' ? SIZES.processing : SIZES.listening) : SIZES.idle;
  try {
    await win.setSize(new LogicalSize(w, h));
    const x = Math.round((screen.availWidth - w) / 2);
    const y = Math.round(screen.availHeight - h - 16);
    await win.setPosition(new LogicalPosition(x, y));
  } catch (e) { /* ignore */ }
}

function render() {
  const active = enabled && (phase === 'listening' || phase === 'processing');
  const listening = enabled && phase === 'listening';
  const proc = enabled && phase === 'processing';
  $('dock').classList.toggle('bright', active);
  $('dot').classList.toggle('live', listening);
  $('dot').classList.toggle('proc', proc);
  $('readout').classList.toggle('show', active);
  $('wds').textContent = words + ' WORDS';
  $('hint').classList.toggle('show', hovering && !active);
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

  $('dock').addEventListener('mouseenter', () => { hovering = true; render(); });
  $('dock').addEventListener('mouseleave', () => { hovering = false; render(); });

  layout();
  render();
}

window.addEventListener('DOMContentLoaded', init);
