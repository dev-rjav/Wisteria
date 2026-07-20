// Wisteria Flow App — frontend logic. Wires every visible control to the Rust backend via Tauri.
'use strict';

/* ---------- Tauri bridge (with a no-op fallback so the page can open in a plain browser) ---------- */
const TAURI = window.__TAURI__;
const invoke = TAURI ? TAURI.core.invoke : async (cmd) => { console.warn('stub invoke', cmd); return null; };
const listen = TAURI ? TAURI.event.listen : async () => () => {};
const appWindow = TAURI ? TAURI.window.getCurrentWindow() : null;

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s == null ? '' : s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

/* ---------- app state ---------- */
const state = {
  config: null,
  gui: { dictionary: ['Kubernetes', 'Wisteria', 'oklch', 'async/await'], snippets: [], style: 'Concise', transforms: null, scratch: '' },
  active: 'Dictation',
  phase: 'warming',
  enabled: true,
  history: [],       // { time, text } — display mirror of the Rust-owned history.json
  stats: { totalWords: 0, totalDictations: 0 },  // all-time, persisted by the backend
  recording: false,  // hotkey-recorder mode
  timer: { seconds: 0, handle: null },
  settingsOpen: false,
  recordKeys: [],    // tokens captured during hotkey recording, in press order
  recordDown: null,  // Set of currently-held e.code values while recording
  scratchPasteGuard: false,   // swallow the engine's dictation paste while on the Scratchpad
  scratchPasteGuardTimer: null,
  openDD: null,
  formatterModels: { reachable: false, selected: '', models: [] },
  transcriptionModels: [],
  pulling: {},       // model -> { percent, status }
  defaultPrompt: '', // built-in formatter prompt (fetched lazily for the Settings editor)
};

const NAV = [
  ['Dictation', '●'], ['Insights', '▤'], ['Dictionary', '▦'],
  ['Snippets', '✂'], ['Transforms', '✦'], ['Style', '❖'], ['Scratchpad', '▭'],
];

const DEFAULT_TRANSFORMS = [
  { name: 'Auto punctuation', desc: 'Add commas and periods automatically', on: true },
  { name: 'Remove filler words', desc: 'Drop "um", "uh", "like"', on: true },
  { name: 'Smart capitalization', desc: 'Capitalize names and sentence starts', on: true },
  { name: 'Email formatting', desc: 'Format dictated emails and URLs', on: false },
];

/* ---------- init ---------- */
// Every backend call is isolated: one failing command must never blank the whole UI.
async function safeInvoke(cmd, args, fallback) {
  try { const r = await invoke(cmd, args); return r == null ? fallback : r; }
  catch (e) { console.error('invoke failed:', cmd, e); return fallback; }
}

async function init() {
  try {
    wireWindowControls();
    state.config = await safeInvoke('get_config', undefined, {});
    const gui = await safeInvoke('get_gui_state', undefined, {});
    Object.assign(state.gui, gui);
    if (!state.gui.transforms) state.gui.transforms = DEFAULT_TRANSFORMS.map((t) => ({ ...t }));
    // Restore history + counters from the Rust-owned history store so they survive restarts.
    const hist = await safeInvoke('get_history', undefined, { history: [], totalWords: 0, totalDictations: 0 });
    state.history = Array.isArray(hist.history) ? hist.history : [];
    state.stats = { totalWords: hist.totalWords || 0, totalDictations: hist.totalDictations || 0 };

    const status = await safeInvoke('engine_status', undefined, { enabled: true, phase: 'idle' });
    state.enabled = status.enabled;
    state.phase = status.phase;

    wireSidebar();
    await listenEngine().catch((e) => console.error('listenEngine failed:', e));
    renderAll();
  } catch (e) {
    // Last-resort: never leave the user staring at a black window.
    console.error('init failed:', e);
    const m = $('main');
    if (m) m.innerHTML = `<pre style="color:#f5d0fe;padding:24px;white-space:pre-wrap;font-size:13px;line-height:1.6">Wisteria hit a startup error but is still running:\n\n${esc(e && e.stack || e)}</pre>`;
    try { renderNav(); } catch (_) {}
  }
}

function wireWindowControls() {
  if (!appWindow) return;
  $('win-min').onclick = () => appWindow.minimize();
  $('win-max').onclick = () => appWindow.toggleMaximize();
  $('win-close').onclick = () => appWindow.close();
}

function wireSidebar() {
  $('open-settings').onclick = openSettings;
  $('open-help').onclick = () => openUrl('https://github.com/dev-rjav/Wisteria');
  $('link-github').onclick = (e) => { e.preventDefault(); openUrl('https://github.com/dev-rjav/Wisteria'); };
}

function openUrl(u) { if (TAURI && TAURI.opener) TAURI.opener.openUrl(u); else window.open(u, '_blank'); }

/* ---------- engine events ---------- */
async function listenEngine() {
  await listen('engine-event', (e) => {
    const p = e.payload;
    if (p.kind === 'phase') {
      state.phase = p.phase;
      // A dictation is being transcribed → its paste is imminent. On the Scratchpad we insert the
      // text ourselves from the transcript event, so arm a guard to swallow that synthetic paste.
      if (p.phase === 'processing' && state.active === 'Scratchpad') armScratchPasteGuard();
      if (state.active === 'Dictation') { renderMain(); renderRight(); }
    } else if (p.kind === 'transcript') {
      addTranscript(p.clean, p.words);
    } else if (p.kind === 'error') {
      console.error('engine error:', p.message);
    }
  });
  await listen('model-pull', (e) => onPullProgress(e.payload));
}

const HISTORY_CAP = 500;   // keep the in-memory mirror bounded (backend caps its file too)

function addTranscript(text, words) {
  const now = new Date();
  const time = now.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  // Persist via the backend (atomic, append-only) so it can't be clobbered; update the mirror too.
  invoke('append_history', { time, text, words: words || 0 });
  state.history.unshift({ time, text });
  if (state.history.length > HISTORY_CAP) state.history.length = HISTORY_CAP;
  state.stats.totalWords += (words || 0);
  state.stats.totalDictations += 1;
  if (state.active === 'Dictation') { renderMain(); renderRight(); }
  if (state.active === 'Insights') renderMain();
  if (state.active === 'Scratchpad') appendToScratch(text);
}

function armScratchPasteGuard() {
  state.scratchPasteGuard = true;
  clearTimeout(state.scratchPasteGuardTimer);
  // Clear if no paste arrives (e.g. the textarea wasn't focused, so no paste event fired).
  state.scratchPasteGuardTimer = setTimeout(() => { state.scratchPasteGuard = false; }, 5000);
}

function updateScratchCount() {
  const el = document.querySelector('.scratch-count');
  if (!el) return;
  const v = (state.gui.scratch || '').trim();
  el.textContent = (v ? v.split(/\s+/).length : 0) + ' WORDS';
}

// Append a dictated transcript to the Scratchpad, delivered via the engine event (not the OS
// paste), so it works regardless of focus. Keeps the caret/scroll at the end.
function appendToScratch(text) {
  const t = String(text == null ? '' : text).trim();
  if (!t) return;
  const cur = state.gui.scratch || '';
  const sep = cur && !/\s$/.test(cur) ? ' ' : '';
  state.gui.scratch = cur + sep + t;
  saveGui();
  const ta = $('scratch');
  if (ta) { ta.value = state.gui.scratch; ta.scrollTop = ta.scrollHeight; updateScratchCount(); }
}

/* ---------- render ---------- */
function renderAll() { renderNav(); renderMain(); renderRight(); }

function renderNav() {
  $('nav').innerHTML = NAV.map(([label, icon]) => `
    <div class="nav-item ${label === state.active ? 'active' : ''}" data-nav="${label}">
      <span class="nav-icon">${icon}</span><span class="nav-label">${label}</span>
    </div>`).join('');
  $('nav').querySelectorAll('[data-nav]').forEach((n) => {
    n.onclick = () => { state.active = n.dataset.nav; renderAll(); };
  });
}

function renderRight() {
  const grid = $('app-grid');
  const panel = $('right-panel');
  if (state.active !== 'Dictation') { grid.classList.remove('with-right'); panel.innerHTML = ''; return; }
  grid.classList.add('with-right');
  const stats = statList();
  panel.innerHTML = `
    <div class="card right-stats">
      ${stats.map((s) => `<div class="right-stat"><span class="v">${esc(s.value)}</span><span class="l">${esc(s.label)}</span></div>`).join('')}
    </div>
    <div class="os-card">
      <div class="os-title">Open source</div>
      <div class="os-body">Wisteria is MIT-licensed and 100% local. Fork it, ship it, make the waves your own.</div>
      <a class="os-link" id="repo-link" href="#">★ VIEW REPO</a>
    </div>`;
  const rl = $('repo-link'); if (rl) rl.onclick = (e) => { e.preventDefault(); openUrl('https://github.com/dev-rjav/Wisteria'); };
}

function statList() {
  return [
    { value: state.stats.totalWords || 0, label: 'TOTAL WORDS' },
    { value: state.stats.totalDictations || 0, label: 'DICTATIONS' },
    { value: state.enabled ? 'ON' : 'OFF', label: 'ENGINE' },
  ];
}

function renderMain() {
  const m = $('main');
  switch (state.active) {
    case 'Dictation': m.innerHTML = viewDictation(); wireDictation(); break;
    case 'Insights': m.innerHTML = viewInsights(); break;
    case 'Dictionary': m.innerHTML = viewDictionary(); wireDictionary(); break;
    case 'Snippets': m.innerHTML = viewSnippets(); wireSnippets(); break;
    case 'Transforms': m.innerHTML = viewTransforms(); wireTransforms(); break;
    case 'Style': m.innerHTML = viewStyle(); wireStyle(); break;
    case 'Scratchpad': m.innerHTML = viewScratchpad(); wireScratchpad(); break;
  }
}

/* ---------- Dictation ---------- */
function hotkeyCaps() {
  const parts = (state.config.ptt_key || 'F8').split('+').map((s) => s.trim()).filter(Boolean);
  return parts.map((p) => p.replace(/^Meta.*/i, 'Win').replace(/^Control.*/i, 'Ctrl').replace(/Left|Right/i, ''));
}

function viewDictation() {
  const caps = hotkeyCaps();
  const capsHtml = caps.map((c, i) => (i ? '<span class="keyplus">+</span>' : '') + `<span class="keycap">${esc(c)}</span>`).join('');
  const on = state.phase === 'listening';
  const items = state.history;
  const rows = items.length
    ? items.map((h) => `<div class="history-item"><span class="history-time">${esc(h.time)}</span><span class="history-text">${esc(h.text)}</span></div>`).join('')
    : `<div class="empty">Hold your hotkey and speak — your dictations land here.</div>`;
  return `
    <h1 class="hero-title">Get back into the flow with ${capsHtml}</h1>
    <div class="hero-card">
      <div class="hero-stripes"></div><div class="hero-fade"></div>
      <div class="hero-inner">
        <div class="hero-lead">Get the most done with <span>Wisteria.</span></div>
        <button class="btn-start ${on ? 'on' : ''}" id="btn-start"><span class="pulse"></span>${state.enabled ? (on ? 'LISTENING…' : 'ENGINE ON') : 'ENGINE OFF'}</button>
      </div>
    </div>
    <div class="today-row">
      <span class="today-label">TODAY</span>
      <input class="input search" id="search" placeholder="Search dictations…">
    </div>
    <div class="history" id="history">${rows}</div>`;
}

function wireDictation() {
  const btn = $('btn-start');
  if (btn) btn.onclick = async () => { state.enabled = !state.enabled; await invoke('engine_set_enabled', { on: state.enabled }); renderMain(); renderRight(); };
  const search = $('search');
  if (search) search.oninput = () => {
    const q = search.value.trim().toLowerCase();
    const filtered = q ? state.history.filter((h) => h.text.toLowerCase().includes(q)) : state.history;
    $('history').innerHTML = filtered.length
      ? filtered.map((h) => `<div class="history-item"><span class="history-time">${esc(h.time)}</span><span class="history-text">${esc(h.text)}</span></div>`).join('')
      : `<div class="empty">No dictations match "${esc(q)}"</div>`;
  };
}

/* ---------- Insights ---------- */
function viewInsights() {
  const stats = statList();
  return `
    <h1 class="page-title">Insights</h1>
    <div class="grid-3 mt-24">
      ${stats.map((s) => `<div class="card"><div class="stat-value">${esc(s.value)}</div><div class="stat-label">${esc(s.label)}</div></div>`).join('')}
    </div>
    <div class="card mt-16">
      <div class="stat-label" style="letter-spacing:2px;font-weight:700">RECENT DICTATIONS</div>
      <div class="history mt-16">
        ${state.history.length ? state.history.slice(0, 6).map((h) => `<div class="history-item"><span class="history-time">${esc(h.time)}</span><span class="history-text">${esc(h.text)}</span></div>`).join('') : '<div class="empty">No dictations yet this session.</div>'}
      </div>
    </div>`;
}

/* ---------- Dictionary ---------- */
function viewDictionary() {
  return `
    <h1 class="page-title">Dictionary</h1>
    <p class="page-sub">Teach Wisteria proper spellings, names and jargon.</p>
    <div class="row mt-22">
      <input class="input" id="dict-input" style="flex:1" placeholder="Add a word or phrase…">
      <button class="btn-primary" id="dict-add">ADD</button>
    </div>
    <div class="chips" id="dict-chips">
      ${state.gui.dictionary.map((w, i) => `<div class="chip"><span>${esc(w)}</span><span class="x" data-i="${i}">✕</span></div>`).join('')}
    </div>`;
}
function wireDictionary() {
  const add = () => { const v = $('dict-input').value.trim(); if (v) { state.gui.dictionary.push(v); $('dict-input').value = ''; saveGui(); renderMain(); } };
  $('dict-add').onclick = add;
  $('dict-input').onkeydown = (e) => { if (e.key === 'Enter') add(); };
  $('dict-chips').querySelectorAll('.x').forEach((x) => x.onclick = () => { state.gui.dictionary.splice(+x.dataset.i, 1); saveGui(); renderMain(); });
}

/* ---------- Snippets ---------- */
function viewSnippets() {
  return `
    <h1 class="page-title">Snippets</h1>
    <p class="page-sub">Say a trigger, Wisteria expands it into full text.</p>
    <div class="row mt-22">
      <input class="input" id="snip-trig" style="width:180px" placeholder="trigger">
      <input class="input" id="snip-exp" style="flex:1" placeholder="expands to…">
      <button class="btn-primary" id="snip-add">ADD</button>
    </div>
    <div class="history mt-16" id="snip-list">
      ${state.gui.snippets.map((s, i) => `<div class="toggle-row"><span class="chip" style="border:none;background:rgba(232,121,249,.12);color:var(--magenta-light)">/${esc(s.trigger)}</span><span style="color:var(--muted-2)">→</span><span class="history-text" style="flex:1">${esc(s.expansion)}</span><span class="x" data-i="${i}" style="cursor:pointer;color:var(--muted-2)">✕</span></div>`).join('') || '<div class="empty">No snippets yet.</div>'}
    </div>`;
}
function wireSnippets() {
  $('snip-add').onclick = () => {
    const t = $('snip-trig').value.trim(), x = $('snip-exp').value.trim();
    if (t && x) { state.gui.snippets.push({ trigger: t, expansion: x }); saveGui(); renderMain(); }
  };
  $('snip-list').querySelectorAll('.x').forEach((el) => el.onclick = () => { state.gui.snippets.splice(+el.dataset.i, 1); saveGui(); renderMain(); });
}

/* ---------- Transforms (maps to format level + local prefs) ---------- */
function viewTransforms() {
  const fmt = (state.config.format || 'medium');
  return `
    <h1 class="page-title">Transforms</h1>
    <p class="page-sub">Cleanups applied after every dictation. Intensity maps to the formatter level.</p>
    <div class="card mt-22">
      <div class="section-label">FORMATTER INTENSITY</div>
      <div class="seg" id="fmt-seg">
        ${['off', 'light', 'medium', 'high'].map((l) => `<button class="${fmt === l ? 'on' : ''}" data-lvl="${l}">${l.toUpperCase()}</button>`).join('')}
      </div>
    </div>
    <div class="mt-16" style="display:flex;flex-direction:column;gap:10px">
      ${state.gui.transforms.map((t, i) => `
        <div class="toggle-row"><div class="meta"><div class="toggle-name">${esc(t.name)}</div><div class="toggle-desc">${esc(t.desc)}</div></div>
        <div class="switch ${t.on ? 'on' : ''}" data-i="${i}"><div class="knob"></div></div></div>`).join('')}
    </div>`;
}
function wireTransforms() {
  $('fmt-seg').querySelectorAll('[data-lvl]').forEach((b) => b.onclick = () => { state.config.format = b.dataset.lvl; saveConfig(); renderMain(); });
  $('main').querySelectorAll('.switch[data-i]').forEach((sw) => sw.onclick = () => { const i = +sw.dataset.i; state.gui.transforms[i].on = !state.gui.transforms[i].on; saveGui(); renderMain(); });
}

/* ---------- Style ---------- */
const STYLES = [
  { name: 'Professional', desc: 'Polished, formal, business-ready.' },
  { name: 'Casual', desc: 'Relaxed and conversational.' },
  { name: 'Concise', desc: 'Trim the fat. Straight to the point.' },
  { name: 'Detailed', desc: 'Thorough, structured, explanatory.' },
];
function viewStyle() {
  return `
    <h1 class="page-title">Style</h1>
    <p class="page-sub">Pick the voice Wisteria writes in.</p>
    <div class="grid-2 mt-22">
      ${STYLES.map((s) => `<div class="style-card ${s.name === state.gui.style ? 'on' : ''}" data-style="${esc(s.name)}"><div class="style-head"><span class="style-name">${esc(s.name)}</span><span class="style-dot"></span></div><div class="style-desc">${esc(s.desc)}</div></div>`).join('')}
    </div>`;
}
function wireStyle() {
  $('main').querySelectorAll('[data-style]').forEach((c) => c.onclick = () => { state.gui.style = c.dataset.style; saveGui(); renderMain(); });
}

/* ---------- Scratchpad ---------- */
function viewScratchpad() {
  const wc = state.gui.scratch.trim() ? state.gui.scratch.trim().split(/\s+/).length : 0;
  return `
    <div class="scratch-head"><h1 class="page-title">Scratchpad</h1><span class="scratch-count">${wc} WORDS</span></div>
    <textarea class="scratch-area" id="scratch" placeholder="Hold your hotkey and start talking — your words land here…">${esc(state.gui.scratch)}</textarea>`;
}
function wireScratchpad() {
  const ta = $('scratch');
  ta.oninput = () => { state.gui.scratch = ta.value; debouncedSaveGui(); updateScratchCount(); };
  // Swallow the engine's dictation paste — we insert that text via the transcript event instead,
  // so letting the OS Ctrl+V also land here would double it. Manual pastes pass through untouched.
  ta.addEventListener('paste', (e) => {
    if (state.scratchPasteGuard) { e.preventDefault(); state.scratchPasteGuard = false; }
  });
}

/* ---------- persistence ---------- */
let saveGuiTimer = null;
function saveGui() { invoke('save_gui_state', { data: state.gui }); }
function debouncedSaveGui() { clearTimeout(saveGuiTimer); saveGuiTimer = setTimeout(saveGui, 400); }
let saveCfgTimer = null;
function saveConfig() { clearTimeout(saveCfgTimer); saveCfgTimer = setTimeout(() => invoke('save_config', { config: state.config }), 200); }

/* ---------- settings modal ---------- */
async function openSettings() {
  state.settingsOpen = true;
  $('settings-overlay').hidden = false;
  document.addEventListener('keydown', hotkeyCapture, true);
  document.addEventListener('keyup', hotkeyCapture, true);
  // Render the modal shell immediately (so it's never an empty blocking box, and Close always
  // works), then fill in model lists — a failed/slow fetch must not leave the modal blank.
  renderSettings();
  await refreshModels().catch((e) => console.error('refreshModels failed:', e));
  if (state.settingsOpen) renderSettings();
}
function closeSettings() {
  state.settingsOpen = false; state.recording = false; state.openDD = null;
  state.recordKeys = []; state.recordDown = null;
  $('settings-overlay').hidden = true;
  document.removeEventListener('keydown', hotkeyCapture, true);
  document.removeEventListener('keyup', hotkeyCapture, true);
}

async function refreshModels() {
  state.formatterModels = await safeInvoke('list_formatter_models', undefined, { reachable: false, selected: '', models: [] });
  state.transcriptionModels = await safeInvoke('list_transcription_models', undefined, []);
}

function renderSettings() {
  const c = state.config;
  const caps = state.recording ? [] : hotkeyCaps();
  const capsHtml = caps.length
    ? caps.map((k, i) => (i ? '<span class="hotkey-plus">+</span>' : '') + `<span class="hotkey-cap">${esc(k)}</span>`).join('')
    : `<span class="hotkey-wait">${state.recording ? 'press keys…' : 'no key set'}</span>`;

  $('settings-modal').innerHTML = `
    <div class="modal-head">
      <div class="t"><img class="brand-logo" src="logo.png" alt="" style="width:22px;height:22px">Settings</div>
      <span class="modal-close" id="set-close">✕</span>
    </div>

    <div class="setting">
      <div class="section-label">RECORDING HOTKEY</div>
      <div class="row" style="margin-top:12px;align-items:center">
        <div class="hotkey-box" id="hotkey-box">${capsHtml}</div>
        <button class="btn-rec ${state.recording ? 'recording' : ''}" id="btn-rec">${state.recording ? 'PRESS KEYS…' : 'RECORD'}</button>
      </div>
      <div class="page-sub" style="font-size:11px;margin-top:8px">Tip: a dedicated key like F8 is safest — modifiers get consumed globally while recording.</div>
    </div>

    <div class="setting">
      <div class="section-label">MICROPHONE</div>
      <div id="dd-device"></div>
    </div>

    <div class="setting">
      <div class="section-label">TRANSCRIPTION MODEL</div>
      <div id="dd-trans"></div>
    </div>

    <div class="setting">
      <div class="section-label">FORMATTING MODEL</div>
      ${state.formatterModels.reachable ? '' : '<div class="warn-banner">Ollama not reachable at ' + esc(c.formatter_url) + '. Start Ollama to use or download formatting models. Dictation still works with the raw transcript.</div>'}
      <div id="dd-fmt"></div>
      <div id="pull-area"></div>
    </div>

    <div class="setting">
      <div class="section-label">FORMATTER INTENSITY</div>
      <div class="seg" id="fmt-level">
        ${['off', 'light', 'medium', 'high'].map((l) => `<button class="${(c.format || 'medium') === l ? 'on' : ''}" data-lvl="${l}">${l.toUpperCase()}</button>`).join('')}
      </div>
    </div>

    <div class="setting">
      <div class="section-label">FORMATTING PROMPT</div>
      <div class="page-sub" style="font-size:11px;margin:6px 0 4px">The system prompt sent to the formatting model. Leave the default unless you know what you're changing — it's tuned to avoid the model replying instead of transcribing.</div>
      <textarea class="prompt-area" id="f-prompt" spellcheck="false" placeholder="Loading default prompt…">${esc(c.formatter_prompt || '')}</textarea>
      <div class="row" style="margin-top:8px;justify-content:flex-end;gap:8px">
        <button class="btn-rec" id="prompt-reset">RESET TO DEFAULT</button>
      </div>
    </div>

    <div class="setting">
      <div class="section-label">ADVANCED</div>
      <div class="field-grid">
        <div class="field"><label>OLLAMA URL</label><input id="f-url" value="${esc(c.formatter_url || '')}"></div>
        <div class="field"><label>FORMATTER TIMEOUT (MS)</label><input id="f-timeout" type="number" value="${esc(c.formatter_timeout_ms || 20000)}"></div>
      </div>
    </div>

    <div class="modal-footer">
      <span class="v">WISTERIA v0.1 · MIT · OPEN SOURCE</span>
      <a class="os-link" id="coffee" href="#" style="border-color:#f5d000;color:#f5d000">★ STAR THE REPO</a>
    </div>`;

  // wire
  $('set-close').onclick = closeSettings;
  $('settings-overlay').onclick = (e) => { if (e.target === $('settings-overlay')) closeSettings(); };
  $('btn-rec').onclick = () => {
    state.recording = !state.recording;
    state.recordKeys = [];               // start each recording from a clean slate
    state.recordDown = new Set();
    renderSettings();
  };
  $('coffee').onclick = (e) => { e.preventDefault(); openUrl('https://github.com/dev-rjav/Wisteria'); };

  renderDeviceDD();
  renderTransDD();
  renderFmtDD();
  renderPullArea();

  $('fmt-level').querySelectorAll('[data-lvl]').forEach((b) => b.onclick = () => { state.config.format = b.dataset.lvl; saveConfig(); renderSettings(); });
  $('f-url').onchange = () => { state.config.formatter_url = $('f-url').value.trim(); saveConfig(); };
  $('f-timeout').onchange = () => { state.config.formatter_timeout_ms = parseInt($('f-timeout').value, 10) || 20000; saveConfig(); };

  wirePromptEditor();
}

/* Formatting-prompt editor: empty config value shows the built-in default as a placeholder so the
   user can see what they're overriding. Saving stores the edited text; Reset clears the override. */
async function wirePromptEditor() {
  const ta = $('f-prompt'); if (!ta) return;
  if (!state.defaultPrompt) state.defaultPrompt = await safeInvoke('default_formatter_prompt', undefined, '');
  // Show the default in the box when the user hasn't set a custom prompt.
  if (!ta.value.trim() && state.defaultPrompt) ta.value = state.defaultPrompt;
  ta.oninput = () => {
    const v = ta.value;
    // Treat "same as default" as no override, so future default improvements still apply.
    state.config.formatter_prompt = (v.trim() === state.defaultPrompt.trim()) ? '' : v;
    saveConfig();
  };
  const reset = $('prompt-reset');
  if (reset) reset.onclick = () => { ta.value = state.defaultPrompt; state.config.formatter_prompt = ''; saveConfig(); };
}

/* generic dropdown renderer */
function dropdown(mountId, key, valueLabel, optionsHtml, onWire) {
  const open = state.openDD === key;
  const mount = $(mountId);
  mount.innerHTML = `
    <div class="dd">
      <div class="dd-trigger ${open ? 'open' : ''}" data-dd="${key}"><span>${valueLabel}</span><span class="dd-caret">▼</span></div>
      ${open ? `<div class="dd-menu">${optionsHtml}</div>` : ''}
    </div>`;
  mount.querySelector('[data-dd]').onclick = () => { state.openDD = open ? null : key; renderSettings(); };
  if (open && onWire) onWire(mount);
}

function renderDeviceDD() {
  const cur = state.config.input_device || '';
  const label = cur || 'Automatic (prefers a real mic)';
  const devs = window.__cachedDevices;
  if (!devs) { invoke('list_input_devices').then((d) => { window.__cachedDevices = d || []; if (state.openDD === 'device') renderSettings(); }); }
  const opts = [{ v: '', l: 'Automatic (prefers a real mic)' }].concat((devs || []).map((d) => ({ v: d, l: d })));
  const html = opts.map((o) => `<div class="dd-opt ${o.v === cur ? 'active' : ''}" data-v="${esc(o.v)}"><div class="opt-main"><span>${esc(o.l)}</span></div></div>`).join('');
  dropdown('dd-device', 'device', esc(label), html, (mount) => {
    mount.querySelectorAll('[data-v]').forEach((o) => o.onclick = () => { state.config.input_device = o.dataset.v; state.openDD = null; saveConfig(); renderSettings(); });
  });
}

function renderTransDD() {
  const cur = state.config.model || '';
  const models = state.transcriptionModels;
  const sel = models.find((m) => m.name === cur);
  const label = sel ? sel.label : (cur || 'Select…');
  const html = models.map((m) => `
    <div class="dd-opt ${m.name === cur ? 'active' : ''} ${m.installed ? '' : 'disabled'}" data-v="${esc(m.name)}" data-installed="${m.installed}">
      <div class="opt-main"><span>${esc(m.label)}</span><span class="opt-note">${esc(m.note)}</span></div>
      <div class="opt-tags">${m.installed ? '<span class="tag tag-installed">INSTALLED</span>' : '<span class="tag tag-size">SOON</span>'}</div>
    </div>`).join('');
  dropdown('dd-trans', 'trans', esc(label), html, (mount) => {
    mount.querySelectorAll('[data-v]').forEach((o) => o.onclick = () => {
      if (o.dataset.installed !== 'true') return;
      state.config.model = o.dataset.v; state.openDD = null; saveConfig(); renderSettings();
    });
  });
}

function renderFmtDD() {
  const cur = state.config.formatter_model || '';
  const models = state.formatterModels.models;
  const label = cur || 'Select a model';
  const html = models.map((m) => {
    const tags = [];
    if (m.recommended) tags.push('<span class="tag tag-rec">RECOMMENDED</span>');
    if (m.installed) tags.push('<span class="tag tag-installed">INSTALLED</span>');
    else tags.push(`<span class="btn-dl" data-dl="${esc(m.name)}">↓ ${esc(m.size)}</span>`);
    if (m.installed && m.size) tags.push(`<span class="tag tag-size">${esc(m.size)}</span>`);
    return `<div class="dd-opt ${m.name === cur ? 'active' : ''}" data-v="${esc(m.name)}" data-installed="${m.installed}">
      <div class="opt-main"><span>${esc(m.name)}</span>${m.note ? `<span class="opt-note">${esc(m.note)}</span>` : ''}</div>
      <div class="opt-tags">${tags.join('')}</div></div>`;
  }).join('') || '<div class="dd-opt">No models — start Ollama</div>';
  dropdown('dd-fmt', 'fmt', esc(label), html, (mount) => {
    mount.querySelectorAll('[data-dl]').forEach((b) => b.onclick = (e) => { e.stopPropagation(); startPull(b.dataset.dl); });
    mount.querySelectorAll('[data-v]').forEach((o) => o.onclick = () => {
      if (o.dataset.installed !== 'true') return;
      state.config.formatter_model = o.dataset.v; state.openDD = null; saveConfig(); renderSettings();
    });
  });
}

function renderPullArea() {
  const area = $('pull-area'); if (!area) return;
  const active = Object.entries(state.pulling).filter(([, p]) => !p.done);
  area.innerHTML = active.map(([name, p]) => `
    <div style="margin-top:10px">
      <div class="pull-status">Downloading <b>${esc(name)}</b> · ${esc(p.status)}${p.percent ? ' · ' + p.percent.toFixed(0) + '%' : ''}
        <span class="pull-cancel" data-cancel="${esc(name)}">✕ CANCEL</span>
      </div>
      <div class="pull-bar"><div class="pull-fill" style="width:${p.percent || 0}%"></div></div>
    </div>`).join('');
  area.querySelectorAll('[data-cancel]').forEach((b) => b.onclick = () => cancelPull(b.dataset.cancel));
}

function startPull(name) {
  state.pulling[name] = { percent: 0, status: 'starting', done: false };
  invoke('pull_model', { name });
  renderPullArea();
}

function cancelPull(name) {
  invoke('cancel_pull', { name });
  if (state.pulling[name]) { state.pulling[name].status = 'cancelling…'; renderPullArea(); }
}

function onPullProgress(p) {
  const cancelled = p.status === 'cancelled';
  state.pulling[p.model] = {
    percent: p.percent,
    status: cancelled ? 'cancelled' : (p.error ? 'error: ' + p.error : p.status),
    done: p.done,
  };
  if (state.settingsOpen) renderPullArea();
  if (p.done) {
    if (!p.error) {
      // Completed: show it installed and auto-select it.
      refreshModels().then(() => { state.config.formatter_model = p.model; saveConfig(); if (state.settingsOpen) renderSettings(); });
    }
    // Clear the row after it ends (quick for cancel/error, a beat longer on success).
    setTimeout(() => {
      delete state.pulling[p.model];
      if (state.settingsOpen) renderPullArea();
    }, p.error ? 800 : 2500);
  }
}

/* Map a physical key (e.code) to a token the Rust side (hotkey::parse_key) understands. Only keys
   that can serve as a global push-to-talk binding are accepted — modifiers, function keys, and a
   few standalone keys. Everything else (letters/digits) returns null and is ignored, since the
   listener consumes the bound key globally and grabbing a letter would break normal typing. */
function codeToToken(code) {
  if (/^F([1-9]|1[0-2])$/.test(code)) return code;      // F1..F12
  return ({
    ControlLeft: 'ControlLeft', ControlRight: 'ControlRight',
    AltLeft: 'Alt', AltRight: 'AltGr',
    ShiftLeft: 'ShiftLeft', ShiftRight: 'ShiftRight',
    MetaLeft: 'Win', MetaRight: 'MetaRight',
    Space: 'Space', Tab: 'Tab', CapsLock: 'CapsLock',
  })[code] || null;
}

// Human label for a captured token (Win / Ctrl / Alt / F8 …), matching hotkeyCaps().
function displayKey(tok) {
  return tok.replace(/^Meta.*/i, 'Win').replace(/^Control.*/i, 'Ctrl').replace(/Left|Right/i, '');
}

function updateHotkeyPreview() {
  const box = $('hotkey-box'); if (!box) return;
  const caps = state.recordKeys.map(displayKey);
  box.innerHTML = caps.length
    ? caps.map((k, i) => (i ? '<span class="hotkey-plus">+</span>' : '') + `<span class="hotkey-cap">${esc(k)}</span>`).join('')
    : '<span class="hotkey-wait">press keys…</span>';
}

/* Hotkey recorder: accumulate the actual keys pressed (by physical e.code), then finalize when
   they're ALL released. This records exactly what you held — no stale modifier flags, and combos
   of any length are captured, not just "modifiers + one key". */
function hotkeyCapture(e) {
  if (!state.recording) return;
  e.preventDefault(); e.stopPropagation();
  if (!state.recordDown) state.recordDown = new Set();

  if (e.type === 'keydown') {
    if (e.repeat) return;                       // ignore OS auto-repeat
    const tok = codeToToken(e.code);
    if (!tok) return;                           // unsupported key for a global binding
    if (!state.recordKeys.includes(tok)) state.recordKeys.push(tok);
    state.recordDown.add(e.code);
    updateHotkeyPreview();
  } else if (e.type === 'keyup') {
    state.recordDown.delete(e.code);
    // All keys released and we captured at least one → that's the combo.
    if (state.recordDown.size === 0 && state.recordKeys.length) {
      state.config.ptt_key = state.recordKeys.join('+');
      state.recording = false;
      state.recordDown = new Set();
      saveConfig();
      renderSettings();
    }
  }
}

window.addEventListener('DOMContentLoaded', init);
