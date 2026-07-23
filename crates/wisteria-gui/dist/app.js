// Wisteria — frontend logic. Wires every visible control to the Rust backend via Tauri.
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
  gui: { scratch: '' },
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
  apiModels: {},     // provider id -> { loading, error, models: [] } (fetched from the service)
  editingProvider: null,  // the API-service object being added/edited inline, or null
};

const NAV = [
  ['Dictation', '●'], ['Insights', '▤'], ['Dictionary', '▦'],
  ['Snippets', '✂'], ['Transforms', '✦'], ['Style', '❖'], ['Ask AI', '✧'], ['Scratchpad', '▭'],
];

/* ---------- init ---------- */
// Every backend call is isolated: one failing command must never blank the whole UI.
async function safeInvoke(cmd, args, fallback) {
  try { const r = await invoke(cmd, args); return r == null ? fallback : r; }
  catch (e) { console.error('invoke failed:', cmd, e); return fallback; }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Load the config, retrying until we get a real one (a valid config always has a ptt_key). On the
// first launch after an update the backend can be momentarily not-ready, and a single failed read
// used to get cached as an empty {} for the whole session (settings looked reset until relaunch).
async function fetchConfigStable() {
  for (let i = 0; i < 30; i++) {
    let c = null;
    try { c = await invoke('get_config'); } catch (_) { /* not ready yet */ }
    if (c && typeof c === 'object' && typeof c.ptt_key === 'string' && c.ptt_key) return c;
    await sleep(120);
  }
  return null;
}

// Pull the latest config + history + engine status from disk into state. Used on startup AND
// whenever the window regains focus, so re-showing a hidden window always reflects what's on disk
// (no more "quit from Task Manager and relaunch to see my history/settings").
async function loadFromBackend() {
  const cfg = await fetchConfigStable();
  if (cfg) {
    if (!Array.isArray(cfg.dictionary)) cfg.dictionary = [];
    if (!Array.isArray(cfg.snippets)) cfg.snippets = [];
    state.config = cfg;
  }
  const gui = await safeInvoke('get_gui_state', undefined, {});
  Object.assign(state.gui, gui);
  // One-time migration: dictionary + snippets used to live in gui-state; move any saved ones into
  // the config (which the pipeline actually reads) so existing custom entries keep working.
  let migrated = false;
  if (configOk() && !state.config.dictionary.length && Array.isArray(gui.dictionary) && gui.dictionary.length) {
    state.config.dictionary = gui.dictionary.slice(); migrated = true;
  }
  if (configOk() && !state.config.snippets.length && Array.isArray(gui.snippets) && gui.snippets.length) {
    state.config.snippets = gui.snippets.slice(); migrated = true;
  }
  if (migrated) saveConfigNow();
  const hist = await safeInvoke('get_history', undefined, null);
  if (hist && Array.isArray(hist.history)) {
    state.history = hist.history;
    state.stats = { totalWords: hist.totalWords || 0, totalDictations: hist.totalDictations || 0 };
  }
  const status = await safeInvoke('engine_status', undefined, null);
  if (status) { state.enabled = status.enabled; state.phase = status.phase; }
}

async function init() {
  try {
    wireWindowControls();
    await loadFromBackend();
    wireSidebar();
    await listenEngine().catch((e) => console.error('listenEngine failed:', e));
    wireFocusRefresh();
    renderAll();
  } catch (e) {
    // Last-resort: never leave the user staring at a black window.
    console.error('init failed:', e);
    const m = $('main');
    if (m) m.innerHTML = `<pre style="color:#f5d0fe;padding:24px;white-space:pre-wrap;font-size:13px;line-height:1.6">Wisteria hit a startup error but is still running:\n\n${esc(e && e.stack || e)}</pre>`;
    try { renderNav(); } catch (_) {}
  }
}

// Re-sync from disk when the window is brought to the foreground (the app hides to the tray on
// close and is re-shown by the tray / a second launch). Debounced, and skipped while actively
// recording a hotkey so it can't interrupt that.
let lastRefresh = 0;
async function refreshFromDisk() {
  if (state.recording) return;
  const now = Date.now();
  if (now - lastRefresh < 500) return;
  lastRefresh = now;
  // Flush any pending debounced writes first so we don't re-read stale disk over a fresh edit.
  if (saveCfgTimer) { clearTimeout(saveCfgTimer); saveCfgTimer = null; if (configOk()) await invoke('save_config', { config: state.config }); }
  if (saveGuiTimer) { clearTimeout(saveGuiTimer); saveGuiTimer = null; invoke('save_gui_state', { data: state.gui }); }
  await loadFromBackend();
  renderAll();
  if (state.settingsOpen && !state.openDD) renderSettings();
}
function wireFocusRefresh() {
  try { if (appWindow && appWindow.onFocusChanged) appWindow.onFocusChanged(({ payload: focused }) => { if (focused) refreshFromDisk(); }); } catch (_) {}
  window.addEventListener('focus', refreshFromDisk);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshFromDisk(); });
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
  // Report is a footer action (not a feature nav item): open its page in the main area.
  $('open-report').onclick = () => { state.active = 'Report'; renderNav(); renderMain(); renderRight(); };
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
  const ts = now.getTime();   // epoch ms, so history can be grouped by day
  // Persist via the backend (atomic, append-only) so it can't be clobbered; update the mirror too.
  invoke('append_history', { time, text, words: words || 0, ts });
  state.history.unshift({ time, text, ts });
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
  // Reflect the Report page (a footer item) as active on its footer link.
  const rep = $('open-report');
  if (rep) rep.classList.toggle('active', state.active === 'Report');
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
    case 'Ask AI': m.innerHTML = viewAskAi(); wireAskAi(); break;
    case 'Report': m.innerHTML = viewReport(); wireReport(); break;
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
  return `
    <h1 class="hero-title">Get back into the flow with ${capsHtml}</h1>
    <div class="hero-card">
      <div class="hero-stripes"></div><div class="hero-fade"></div>
      <div class="hero-inner">
        <div class="hero-lead">Get the most done with <span>Wisteria.</span></div>
        <button class="btn-start ${on ? 'on' : ''}" id="btn-start"><span class="pulse"></span>${state.enabled ? (on ? 'LISTENING…' : 'ENGINE ON') : 'ENGINE OFF'}</button>
      </div>
    </div>
    <p class="page-sub" style="font-size:12px;margin-top:14px">Hold to talk — or <b>double-tap</b> your hotkey to lock hands-free recording, then tap once more to stop and paste.</p>
    <div class="today-row">
      <span class="today-label">HISTORY</span>
      <input class="input search" id="search" placeholder="Search dictations…">
    </div>
    <div class="history" id="history"></div>`;
}

function wireDictation() {
  const btn = $('btn-start');
  if (btn) btn.onclick = async () => { state.enabled = !state.enabled; await invoke('engine_set_enabled', { on: state.enabled }); renderMain(); renderRight(); };
  paintHistory(state.history);
  const search = $('search');
  if (search) search.oninput = () => {
    const q = search.value.trim().toLowerCase();
    const filtered = q ? state.history.filter((h) => h.text.toLowerCase().includes(q)) : state.history;
    paintHistory(filtered, q ? `No dictations match "${esc(q)}"` : undefined);
  };
}

/* ---------- history rendering (day grouping + per-item copy) ---------- */
const COPY_ICON = '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>';
const CHECK_ICON = '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';

// Bucket a timestamp into Today / Yesterday / an absolute date. Entries saved before timestamps
// existed have no `ts` and fall under "Earlier" (still visible, just not day-labeled).
function dayLabel(ts) {
  if (!ts) return 'Earlier';
  const startOf = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const diff = Math.round((startOf(new Date()) - startOf(new Date(ts))) / 86400000);
  if (diff <= 0) return 'Today';
  if (diff === 1) return 'Yesterday';
  return new Date(ts).toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' });
}

// history is newest-first, so day labels are already in descending order — a simple run-length
// grouping keeps them contiguous under one header each.
function groupHistory(items) {
  const groups = [];
  let cur = null;
  for (const h of items) {
    const label = dayLabel(h.ts);
    if (!cur || cur.label !== label) { cur = { label, items: [] }; groups.push(cur); }
    cur.items.push(h);
  }
  return groups;
}

function paintHistory(items, emptyMsg) {
  const box = $('history');
  if (!box) return;
  if (!items.length) {
    box.innerHTML = `<div class="empty">${emptyMsg || 'Hold your hotkey and speak — your dictations land here.'}</div>`;
    return;
  }
  const groups = groupHistory(items);
  const flat = [];
  let html = '';
  for (const g of groups) {
    html += `<div class="history-day">${esc(g.label)}</div>`;
    for (const h of g.items) {
      html += `<div class="history-item">
        <span class="history-time">${esc(h.time)}</span>
        <span class="history-text">${esc(h.text)}</span>
        <button class="history-copy" data-i="${flat.length}" title="Copy transcription" aria-label="Copy">${COPY_ICON}</button>
      </div>`;
      flat.push(h);
    }
  }
  box.innerHTML = html;
  box.querySelectorAll('.history-copy').forEach((btn) => {
    btn.onclick = async () => {
      const ok = await copyText(flat[+btn.dataset.i].text);
      if (!ok) return;
      btn.innerHTML = CHECK_ICON;
      btn.classList.add('copied');
      setTimeout(() => { btn.innerHTML = COPY_ICON; btn.classList.remove('copied'); }, 1200);
    };
  });
}

// Copy via the Clipboard API, with a hidden-textarea fallback for webviews that reject it.
async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return true; }
  catch (_) {
    try {
      const ta = document.createElement('textarea');
      ta.value = text; ta.style.position = 'fixed'; ta.style.opacity = '0';
      document.body.appendChild(ta); ta.select();
      const ok = document.execCommand('copy'); ta.remove(); return ok;
    } catch (_) { return false; }
  }
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

/* ---------- Dictionary (maps to config.dictionary; used by the pipeline) ---------- */
function dictWords() { return Array.isArray(state.config.dictionary) ? state.config.dictionary : (state.config.dictionary = []); }
function viewDictionary() {
  const words = dictWords();
  return `
    <div class="today-row" style="margin-top:0">
      <h1 class="page-title" style="margin:0">Dictionary</h1>
      <div class="row" style="gap:8px">
        <button class="btn-rec" id="dict-import">IMPORT</button>
        <button class="btn-rec" id="dict-export">EXPORT</button>
      </div>
    </div>
    <p class="page-sub">Teach Wisteria proper spellings — names, brands, and jargon. It fixes sound-alike transcriptions to the exact spelling you add here (e.g. your name), and keeps their capitalization. Import merges into your existing words; export saves them as a plain text file.</p>
    <div class="row mt-22">
      <input class="input" id="dict-input" style="flex:1" placeholder="Add a word or phrase…">
      <button class="btn-primary" id="dict-add">ADD</button>
    </div>
    <div class="chips" id="dict-chips">
      ${words.length ? words.map((w, i) => `<div class="chip"><span>${esc(w)}</span><span class="x" data-i="${i}">✕</span></div>`).join('') : '<div class="empty" style="padding:18px 0">No custom words yet.</div>'}
    </div>`;
}
function wireDictionary() {
  // Save immediately (config-backed) so the pipeline picks up new words on the next dictation.
  const add = () => { const v = $('dict-input').value.trim(); if (v && !dictWords().includes(v)) { dictWords().push(v); $('dict-input').value = ''; saveConfigNow(); renderMain(); } };
  $('dict-add').onclick = add;
  $('dict-input').onkeydown = (e) => { if (e.key === 'Enter') add(); };
  $('dict-chips').querySelectorAll('.x').forEach((x) => x.onclick = () => { dictWords().splice(+x.dataset.i, 1); saveConfigNow(); renderMain(); });
  $('dict-export').onclick = exportDictionary;
  $('dict-import').onclick = importDictionary;
}

async function exportDictionary() {
  const dialog = TAURI && TAURI.dialog;
  if (!dialog) return;
  const path = await dialog.save({ defaultPath: 'wisteria-dictionary.txt', filters: [{ name: 'Word list', extensions: ['txt'] }] });
  if (!path) return;
  await safeInvoke('export_dictionary', { path });
}

async function importDictionary() {
  const dialog = TAURI && TAURI.dialog;
  if (!dialog) return;
  const selected = await dialog.open({ multiple: false, filters: [{ name: 'Word list', extensions: ['txt', 'csv'] }] });
  if (!selected) return;
  const path = Array.isArray(selected) ? selected[0] : selected;
  const words = await safeInvoke('import_dictionary', { path }, []);
  // Append to existing words, de-duplicating case-insensitively (existing words are kept).
  const cur = dictWords();
  const seen = new Set(cur.map((w) => String(w).toLowerCase()));
  let added = 0;
  for (const w of words) {
    const t = String(w).trim();
    if (t && !seen.has(t.toLowerCase())) { cur.push(t); seen.add(t.toLowerCase()); added++; }
  }
  if (added) saveConfigNow();
  renderMain();
}

/* ---------- Snippets (config-backed; expanded by voice: "<keyword> <trigger>") ---------- */
function snips() { return Array.isArray(state.config.snippets) ? state.config.snippets : (state.config.snippets = []); }
function snipKeyword() { return (state.config.snippet_keyword || 'snippet'); }
function viewSnippets() {
  const kw = snipKeyword();
  const list = snips();
  return `
    <h1 class="page-title">Snippets</h1>
    <p class="page-sub">Say <b>“${esc(kw)}”</b> followed by a trigger to paste its text — e.g. say “<b>${esc(kw)} address</b>” to insert your address. If the words after “${esc(kw)}” aren’t a snippet, nothing is expanded (so “${esc(kw)} coffee” stays as “${esc(kw)} coffee”).</p>
    <div class="row mt-16" style="align-items:center;gap:10px">
      <span class="section-label">TRIGGER WORD</span>
      <input class="input" id="snip-kw" style="width:160px" value="${esc(kw)}" placeholder="snippet">
    </div>
    <div class="row mt-16">
      <input class="input" id="snip-trig" style="width:200px" placeholder="trigger phrase (e.g. work email)">
      <input class="input" id="snip-exp" style="flex:1" placeholder="expands to… (pasted exactly)">
      <button class="btn-primary" id="snip-add">ADD</button>
    </div>
    <div class="history mt-16" id="snip-list">
      ${list.length ? list.map((s, i) => `<div class="toggle-row"><span class="chip" style="border:none;background:rgba(232,121,249,.12);color:var(--magenta-light)">${esc(kw)} ${esc(s.trigger)}</span><span style="color:var(--muted-2)">→</span><span class="history-text" style="flex:1">${esc(s.expansion)}</span><span class="x" data-i="${i}" style="cursor:pointer;color:var(--muted-2)">✕</span></div>`).join('') : '<div class="empty">No snippets yet.</div>'}
    </div>`;
}
function wireSnippets() {
  $('snip-add').onclick = () => {
    const t = $('snip-trig').value.trim(), x = $('snip-exp').value.trim();
    if (t && x) { snips().push({ trigger: t, expansion: x }); saveConfigNow(); renderMain(); }
  };
  $('snip-exp').onkeydown = (e) => { if (e.key === 'Enter') $('snip-add').click(); };
  $('snip-kw').onchange = () => { const v = $('snip-kw').value.trim() || 'snippet'; state.config.snippet_keyword = v; saveConfigNow(); renderMain(); };
  $('snip-list').querySelectorAll('.x').forEach((el) => el.onclick = () => { snips().splice(+el.dataset.i, 1); saveConfigNow(); renderMain(); });
}

/* ---------- Transforms (drive the Rust formatter: intensity + per-behavior toggles) ---------- */
// Each toggle maps to a boolean on config.transforms. When OFF it injects a negative override into
// the formatter prompt; when every toggle is ON the built-in prompt runs unmodified.
const TRANSFORM_ITEMS = [
  { key: 'auto_punctuation', name: 'Auto punctuation', desc: 'Add commas, periods, and sentence breaks' },
  { key: 'remove_fillers', name: 'Remove filler words', desc: 'Drop "um", "uh", "like", stutters, and false starts' },
  { key: 'smart_capitalization', name: 'Smart capitalization', desc: 'Capitalize sentence starts, names, and acronyms' },
  { key: 'email_formatting', name: 'Email & number formatting', desc: 'Format dictated emails, URLs, numbers, dates, and times' },
];
function viewTransforms() {
  const fmt = (state.config.format || 'medium');
  const off = fmt === 'off';
  const tf = state.config.transforms || {};
  return `
    <h1 class="page-title">Transforms</h1>
    <p class="page-sub">Cleanups the local model applies after every dictation.</p>
    <div class="card mt-22">
      <div class="section-label">FORMATTER INTENSITY</div>
      <div class="seg" id="fmt-seg">
        ${['off', 'light', 'medium', 'high'].map((l) => `<button class="${fmt === l ? 'on' : ''}" data-lvl="${l}">${l.toUpperCase()}</button>`).join('')}
      </div>
      <div class="page-sub" style="font-size:11px;margin-top:10px">${off
        ? 'Off — the model is skipped entirely; your raw transcript is pasted exactly as spoken.'
        : 'Light removes only obvious fillers; Medium is balanced; High is the most thorough. Meaning is always preserved.'}</div>
    </div>
    <div class="mt-16 ${off ? 'is-disabled' : ''}" id="tf-list" style="display:flex;flex-direction:column;gap:10px${off ? ';opacity:.4;pointer-events:none' : ''}">
      ${TRANSFORM_ITEMS.map((t) => `
        <div class="toggle-row"><div class="meta"><div class="toggle-name">${esc(t.name)}</div><div class="toggle-desc">${esc(t.desc)}</div></div>
        <div class="switch ${tf[t.key] ? 'on' : ''}" data-key="${t.key}"><div class="knob"></div></div></div>`).join('')}
    </div>
    <div class="card mt-22">
      <div class="section-label">EFFECTIVE PROMPT — exactly what the model receives</div>
      <p class="page-sub" style="font-size:11px;margin:6px 0 8px">Updates live as you toggle. Disabled transforms have their whole rule section removed and an explicit "do not" ban added, so you can confirm each switch takes effect.</p>
      <pre class="prompt-preview" id="eff-prompt">Loading…</pre>
    </div>`;
}
// Fetch and show the real system prompt for the current config (reflects toggles + intensity).
async function refreshEffectivePrompt() {
  const pre = $('eff-prompt');
  if (!pre) return;
  const text = await safeInvoke('effective_prompt', { config: state.config }, '');
  if ($('eff-prompt')) $('eff-prompt').textContent = text || '(empty)';
}
function wireTransforms() {
  // Intensity + toggles are discrete choices: save and reload the engine immediately so the change
  // is live on the next dictation (no 200ms debounce window where a quick dictation misses it).
  $('fmt-seg').querySelectorAll('[data-lvl]').forEach((b) => b.onclick = () => { state.config.format = b.dataset.lvl; saveConfigNow(); renderMain(); });
  $('main').querySelectorAll('.switch[data-key]').forEach((sw) => sw.onclick = () => {
    if (!state.config.transforms) state.config.transforms = {};
    const k = sw.dataset.key;
    state.config.transforms[k] = !state.config.transforms[k];
    saveConfigNow();
    renderMain();
  });
  refreshEffectivePrompt();
}

/* ---------- Style (maps to config.style; the formatter rewrites into this voice) ---------- */
// value = the lowercase enum the backend expects; Concise is the neutral default (faithful cleanup).
const STYLES = [
  { value: 'concise', name: 'Concise', desc: 'Keeps the text almost exactly as spoken.' },
  { value: 'professional', name: 'Professional', desc: 'Polished, formal, and business-ready.' },
  { value: 'casual', name: 'Casual', desc: 'Relaxed and conversational.' },
  { value: 'detailed', name: 'Detailed', desc: 'Thorough, structured, and explanatory.' },
];
function viewStyle() {
  const cur = state.config.style || 'concise';
  const off = (state.config.format || 'medium') === 'off';
  return `
    <h1 class="page-title">Style</h1>
    <p class="page-sub">Pick the voice Wisteria writes in. Concise stays faithful to your words; the others rewrite the tone while keeping your meaning.</p>
    ${off ? '<div class="warn-banner">Styles apply only when formatting is on. Set Transforms → Intensity to Light, Medium, or High.</div>' : ''}
    <div class="grid-2 mt-22">
      ${STYLES.map((s) => `<div class="style-card ${s.value === cur ? 'on' : ''}" data-style="${esc(s.value)}"><div class="style-head"><span class="style-name">${esc(s.name)}</span><span class="style-dot"></span></div><div class="style-desc">${esc(s.desc)}</div></div>`).join('')}
    </div>`;
}
function wireStyle() {
  // Save immediately so the engine reloads with the new voice for the next dictation.
  $('main').querySelectorAll('[data-style]').forEach((c) => c.onclick = () => { state.config.style = c.dataset.style; saveConfigNow(); renderMain(); });
}

/* ---------- Ask AI (config.ask_ai_enabled + keyword; uses the formatter model) ---------- */
function viewAskAi() {
  const on = !!state.config.ask_ai_enabled;
  const kw = state.config.ask_ai_keyword || 'assistant';
  const model = state.config.formatter_model || '—';
  return `
    <h1 class="page-title">Ask AI</h1>
    <p class="page-sub">Speak a request and paste the AI's answer instead of a transcript. Say your keyword, then your request — e.g. “<b>${esc(kw)}, write a professional email following up on a lead</b>” pastes the finished email, formatted, with none of the AI's chatter around it (no “Sure, here's your email”).</p>
    <div class="toggle-row mt-22"><div class="meta"><div class="toggle-name">Enable Ask AI</div><div class="toggle-desc">When on, a dictation that starts with the keyword is answered by the model. Otherwise it's normal dictation.</div></div>
      <div class="switch ${on ? 'on' : ''}" id="ai-toggle"><div class="knob"></div></div></div>
    <div class="mt-16" style="${on ? '' : 'opacity:.4;pointer-events:none'}">
      <div class="row" style="align-items:center;gap:10px">
        <span class="section-label">KEYWORD</span>
        <input class="input" id="ai-kw" style="width:180px" value="${esc(kw)}" placeholder="assistant">
      </div>
      <p class="page-sub" style="font-size:11px;margin-top:8px">Say this word first, then your request. Pick something you rarely say by accident.</p>
    </div>
    <div class="warn-banner mt-22" style="border-color:rgba(232,121,249,.3);color:var(--ink)">⚠ Quality depends <b>entirely on the model</b> you've selected as your <b>Formatting Model</b> in Settings (currently <b>${esc(model)}</b>) — Ask AI runs through that same local Ollama model. A small model writes rough drafts; a larger one (7B+/12B) writes noticeably better emails. Ollama must be running.</div>`;
}
function wireAskAi() {
  $('ai-toggle').onclick = () => { state.config.ask_ai_enabled = !state.config.ask_ai_enabled; saveConfigNow(); renderMain(); };
  const kw = $('ai-kw');
  if (kw) kw.onchange = () => { state.config.ask_ai_keyword = kw.value.trim() || 'assistant'; saveConfigNow(); renderMain(); };
}

/* ---------- Report an Issue (testing phase — POSTs to a report endpoint) ---------- */
function viewReport() {
  return `
    <h1 class="page-title">Report an Issue</h1>
    <p class="page-sub">This is a testing build — hit a bug or have an improvement? Send it straight to the team. Your name, a title, and a description are required; the app version and OS are attached automatically.</p>
    <div class="card mt-22" style="display:flex;flex-direction:column;gap:14px;max-width:660px">
      <div class="row" style="gap:12px">
        <div style="flex:1"><label class="section-label">YOUR NAME *</label><input class="input" id="rep-name" style="width:100%;margin-top:6px" placeholder="Jane Doe"></div>
        <div style="flex:1"><label class="section-label">EMAIL (optional)</label><input class="input" id="rep-email" style="width:100%;margin-top:6px" placeholder="you@example.com"></div>
      </div>
      <div class="row" style="gap:12px">
        <div style="flex:1"><label class="section-label">TYPE</label><select class="input" id="rep-type" style="width:100%;margin-top:6px"><option>Bug</option><option>Improvement</option><option>Question</option><option>Other</option></select></div>
        <div style="flex:1"><label class="section-label">SEVERITY</label><select class="input" id="rep-sev" style="width:100%;margin-top:6px"><option>—</option><option>Low</option><option>Medium</option><option>High</option></select></div>
      </div>
      <div><label class="section-label">TITLE *</label><input class="input" id="rep-title" style="width:100%;margin-top:6px" placeholder="Short summary of the issue"></div>
      <div><label class="section-label">DESCRIPTION *</label><textarea class="prompt-area" id="rep-desc" style="min-height:120px" placeholder="What happened, and what did you expect instead?"></textarea></div>
      <div><label class="section-label">STEPS TO REPRODUCE (optional)</label><textarea class="prompt-area" id="rep-steps" style="min-height:80px" placeholder="1. …&#10;2. …&#10;3. …"></textarea></div>
      <div class="row" style="justify-content:flex-end;align-items:center;gap:14px">
        <span id="rep-status" class="page-sub" style="margin:0"></span>
        <button class="btn-primary" id="rep-submit">SEND REPORT</button>
      </div>
    </div>`;
}
function wireReport() {
  const status = (msg, ok) => { const s = $('rep-status'); if (s) { s.textContent = msg; s.style.color = ok ? '#4ade80' : '#f87171'; } };
  $('rep-submit').onclick = async () => {
    const report = {
      name: $('rep-name').value.trim(),
      email: $('rep-email').value.trim(),
      kind: $('rep-type').value,
      severity: $('rep-sev').value === '—' ? '' : $('rep-sev').value,
      title: $('rep-title').value.trim(),
      description: $('rep-desc').value.trim(),
      steps: $('rep-steps').value.trim(),
    };
    if (!report.name || !report.title || !report.description) { status('Please fill in your name, a title, and a description.', false); return; }
    $('rep-submit').disabled = true; status('Sending…', true);
    try {
      await invoke('submit_report', { report });
      status('✓ Thank you! Your report was sent.', true);
      ['rep-name', 'rep-email', 'rep-title', 'rep-desc', 'rep-steps'].forEach((id) => { if ($(id)) $(id).value = ''; });
    } catch (e) {
      status(String((e && e.message) || e), false);
    } finally { $('rep-submit').disabled = false; }
  };
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
// A valid config always has a ptt_key. Refuse to persist anything that fails this — otherwise a
// transiently-empty state.config (e.g. a failed startup load) could overwrite config.toml with
// defaults and wipe the user's real settings.
function configOk() { return state.config && typeof state.config === 'object' && typeof state.config.ptt_key === 'string' && state.config.ptt_key.length > 0; }
function saveConfig() { if (!configOk()) return; clearTimeout(saveCfgTimer); saveCfgTimer = setTimeout(() => invoke('save_config', { config: state.config }), 200); }
// Persist + reload the engine immediately (no debounce). Used for discrete controls like the
// Transforms toggles and intensity, so the change applies to the very next dictation. Returns the
// invoke promise so callers can await the round-trip if they want.
function saveConfigNow() { if (!configOk()) return Promise.resolve(); clearTimeout(saveCfgTimer); return invoke('save_config', { config: state.config }); }

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
      <div class="section-label">FORMATTER SOURCE</div>
      <div id="dd-source"></div>
    </div>

    ${isLocalBackend() ? `
    <div class="setting">
      <div class="section-label">FORMATTING MODEL — LOCAL (OLLAMA)</div>
      ${state.formatterModels.reachable ? '' : '<div class="warn-banner">Ollama not reachable at ' + esc(c.formatter_url) + '. Start Ollama to use or download formatting models. Dictation still works with the raw transcript.</div>'}
      <div id="dd-fmt"></div>
      <div id="pull-area"></div>
    </div>` : `
    <div class="setting">
      <div class="section-label">MODEL — ${esc((activeProvider() || {}).name || 'CLOUD SERVICE')}</div>
      <div id="dd-cloud-model"></div>
    </div>`}

    <div class="setting">
      <div class="section-label">API SERVICES (BYOK)</div>
      <div class="page-sub" style="font-size:11px;margin:2px 0 10px">Link a cloud formatter over any OpenAI-compatible API (OpenRouter, OpenAI, Groq, …). Optional — local Ollama stays the default. Keys are stored only in your local config file.</div>
      <div id="api-services"></div>
    </div>

    <div class="setting">
      <div class="section-label">FORMATTER INTENSITY</div>
      <div class="seg" id="fmt-level">
        ${['off', 'light', 'medium', 'high'].map((l) => `<button class="${(c.format || 'medium') === l ? 'on' : ''}" data-lvl="${l}">${l.toUpperCase()}</button>`).join('')}
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
      <div class="footer-links">
        <a class="os-link" id="coffee" href="#" style="border-color:#f5d000;color:#f5d000">★ STAR THE REPO</a>
        <a class="kofi-btn" id="kofi" href="#" title="Support Wisteria on Ko-fi">
          <svg class="kofi-cup" viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
            <path d="M3 7h13.5v4.5A4.5 4.5 0 0 1 12 16H7.5A4.5 4.5 0 0 1 3 11.5V7Z" fill="#fff"/>
            <path d="M16.5 8H18a2.5 2.5 0 0 1 0 5h-1.5" fill="none" stroke="#fff" stroke-width="1.8" stroke-linecap="round"/>
            <path d="M9.7 8.5c1-.8 2.45-.2 2.45 1.05 0 1.15-1.65 2.05-2.45 2.65-.8-.6-2.45-1.5-2.45-2.65 0-1.25 1.45-1.85 2.45-1.05Z" fill="#FF5E5B"/>
          </svg>
          <span>Support me on <b>Ko-fi</b></span>
        </a>
      </div>
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
  $('kofi').onclick = (e) => { e.preventDefault(); openUrl('https://ko-fi.com/C6P623L57Q'); };

  renderDeviceDD();
  renderTransDD();
  renderSourceDD();
  if (isLocalBackend()) { renderFmtDD(); renderPullArea(); }
  else { renderCloudModelDD(); }
  renderApiServices();

  $('fmt-level').querySelectorAll('[data-lvl]').forEach((b) => b.onclick = () => { state.config.format = b.dataset.lvl; saveConfig(); renderSettings(); });
  $('f-url').onchange = () => { state.config.formatter_url = $('f-url').value.trim(); saveConfig(); };
  $('f-timeout').onchange = () => { state.config.formatter_timeout_ms = parseInt($('f-timeout').value, 10) || 20000; saveConfig(); };
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
  const models = (state.formatterModels.models || []).slice();
  // Always surface the currently-selected model as an option, even if Ollama was slow/unreachable
  // when the list was fetched (so it isn't in `models`). Otherwise the model the engine is actually
  // using wouldn't appear in the menu on reopen. Treat it as installed since it's the active model.
  if (cur && !models.some((m) => m.name === cur)) {
    models.unshift({ name: cur, installed: true, size: '', note: 'Currently in use', recommended: false });
  }
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
    mount.querySelectorAll('[data-dl]').forEach((b) => b.onclick = (e) => {
      e.stopPropagation();
      // Collapse the dropdown so its absolutely-positioned menu stops covering the progress bar +
      // cancel button below it (otherwise the cancel click lands on the open menu, not the button).
      state.openDD = null;
      startPull(b.dataset.dl);
      renderSettings();
    });
    mount.querySelectorAll('[data-v]').forEach((o) => o.onclick = () => {
      if (o.dataset.installed !== 'true') return;
      state.config.formatter_model = o.dataset.v; state.openDD = null; saveConfig(); renderSettings();
    });
  });
}

/* ---------- API services (BYOK cloud formatter backends, OpenAI-compatible) ---------- */
// Common services, to prefill the base URL. "Custom" leaves it blank for any other compatible API.
const API_PRESETS = [
  { name: 'OpenRouter', base: 'https://openrouter.ai/api/v1' },
  { name: 'OpenAI', base: 'https://api.openai.com/v1' },
  { name: 'Groq', base: 'https://api.groq.com/openai/v1' },
  { name: 'Together', base: 'https://api.together.xyz/v1' },
  { name: 'Custom / other', base: '' },
];

function providers() { return (state.config.api_providers = state.config.api_providers || []); }
function providerById(id) { return providers().find((p) => p.id === id); }
// Local (Ollama) is active when the backend is empty/"local" or points at a provider that's gone.
function isLocalBackend() { const b = state.config.formatter_backend || ''; return b === '' || b === 'local' || !providerById(b); }
function activeProvider() { return isLocalBackend() ? null : providerById(state.config.formatter_backend); }
function newProviderId() { return 'p-' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6); }
function maskKey(k) { if (!k) return 'no key'; return k.length <= 8 ? '••••' : k.slice(0, 4) + '…' + k.slice(-4); }

// Backend source selector: Local (Ollama) + each configured provider.
function renderSourceDD() {
  const local = { v: '', l: 'Local — Ollama' + (state.formatterModels.reachable ? '' : ' (offline)') };
  const opts = [local].concat(providers().map((p) => ({ v: p.id, l: (p.name || 'Service') + (p.model ? ' · ' + p.model : ' · no model yet') })));
  const cur = isLocalBackend() ? '' : state.config.formatter_backend;
  const curOpt = opts.find((o) => o.v === cur) || local;
  const html = opts.map((o) => `<div class="dd-opt ${o.v === cur ? 'active' : ''}" data-v="${esc(o.v)}"><div class="opt-main"><span>${esc(o.l)}</span></div></div>`).join('');
  dropdown('dd-source', 'source', esc(curOpt.l), html, (mount) => {
    mount.querySelectorAll('[data-v]').forEach((o) => o.onclick = () => {
      state.config.formatter_backend = o.dataset.v; state.openDD = null; saveConfig(); renderSettings();
    });
  });
}

// Model picker for the active cloud provider, from the fetched list, plus Fetch + manual entry.
function renderCloudModelDD() {
  const p = activeProvider(); const mount = $('dd-cloud-model'); if (!mount || !p) return;
  const cache = state.apiModels[p.id] || { models: [] };
  const cur = p.model || '';
  const list = (cache.models || []).slice();
  if (cur && !list.includes(cur)) list.unshift(cur);
  const opts = list.map((m) => `<div class="dd-opt ${m === cur ? 'active' : ''}" data-v="${esc(m)}"><div class="opt-main"><span>${esc(m)}</span></div></div>`).join('')
    || '<div class="dd-opt">No models yet — Fetch models or type an id below</div>';
  dropdown('dd-cloud-model', 'cloudmodel', esc(cur || 'Select a model'), opts, (m2) => {
    m2.querySelectorAll('[data-v]').forEach((o) => o.onclick = () => { p.model = o.dataset.v; state.openDD = null; saveConfig(); renderSettings(); });
  });
  const extra = document.createElement('div');
  extra.className = 'row';
  extra.style.cssText = 'margin-top:10px;gap:8px;align-items:center;flex-wrap:wrap';
  extra.innerHTML = `
    <button type="button" class="btn-rec" id="cloud-fetch" ${cache.loading ? 'disabled' : ''}>${cache.loading ? 'FETCHING…' : 'FETCH MODELS'}</button>
    <input id="cloud-model-manual" placeholder="…or type a model id" style="flex:1;min-width:150px">
    ${cache.error ? `<div class="warn-banner" style="flex-basis:100%">${esc(cache.error)}</div>` : ''}`;
  mount.appendChild(extra);
  $('cloud-fetch').onclick = () => fetchApiModels(p);
  $('cloud-model-manual').onchange = (e) => { const v = e.target.value.trim(); if (v) { p.model = v; saveConfig(); renderSettings(); } };
}

// Fetch a provider's model list (also validates the URL/key). Errors are shown, not fatal.
async function fetchApiModels(p) {
  const prev = (state.apiModels[p.id] || {}).models || [];
  state.apiModels[p.id] = { loading: true, error: null, models: prev };
  renderSettings();
  try {
    const models = await invoke('list_api_models', { baseUrl: p.base_url, apiKey: p.api_key });
    state.apiModels[p.id] = { loading: false, error: null, models: models || [] };
  } catch (e) {
    state.apiModels[p.id] = { loading: false, error: 'Fetch failed: ' + String(e), models: prev };
  }
  renderSettings();
}

// The add/remove/edit management list.
function renderApiServices() {
  const mount = $('api-services'); if (!mount) return;
  const ed = state.editingProvider;
  const list = providers().map((p) => `
    <div class="api-card">
      <div class="api-card-main">
        <div class="api-name">${esc(p.name || 'Unnamed service')}</div>
        <div class="api-meta">${esc(p.base_url || 'no URL')} · ${esc(p.model || 'no model')} · key ${esc(maskKey(p.api_key))}</div>
      </div>
      <div class="api-card-actions">
        <button type="button" class="btn-mini" data-edit="${esc(p.id)}">EDIT</button>
        <button type="button" class="btn-mini danger" data-remove="${esc(p.id)}">REMOVE</button>
      </div>
    </div>`).join('');
  const form = ed ? renderProviderForm(ed) : `<button type="button" class="btn-add" id="api-add">+ ADD SERVICE</button>`;
  mount.innerHTML = (list || '') + form;
  mount.querySelectorAll('[data-edit]').forEach((b) => b.onclick = () => startEditProvider(b.dataset.edit));
  mount.querySelectorAll('[data-remove]').forEach((b) => b.onclick = () => removeProvider(b.dataset.remove));
  if ($('api-add')) $('api-add').onclick = () => startAddProvider();
  if (ed) wireProviderForm();
}

function renderProviderForm(ed) {
  const presetOpts = API_PRESETS.map((p) => `<option value="${esc(p.base)}" ${p.base && p.base === ed.base_url ? 'selected' : ''}>${esc(p.name)}</option>`).join('');
  return `
    <div class="api-form">
      <div class="api-form-title">${ed._isNew ? 'Add a service' : 'Edit service'}</div>
      <div class="field"><label>PRESET</label><select id="ap-preset"><option value="__none">— choose to prefill —</option>${presetOpts}</select></div>
      <div class="field"><label>NAME</label><input id="ap-name" value="${esc(ed.name || '')}" placeholder="OpenRouter"></div>
      <div class="field"><label>BASE URL (OpenAI-compatible)</label><input id="ap-base" value="${esc(ed.base_url || '')}" placeholder="https://openrouter.ai/api/v1"></div>
      <div class="field"><label>API KEY</label><input id="ap-key" type="password" value="${esc(ed.api_key || '')}" placeholder="sk-…"></div>
      <div class="row" style="gap:8px;margin-top:10px;align-items:center;flex-wrap:wrap">
        <button type="button" class="btn-rec" id="ap-save">SAVE</button>
        <button type="button" class="btn-mini" id="ap-cancel">CANCEL</button>
      </div>
    </div>`;
}

function wireProviderForm() {
  const ed = state.editingProvider;
  $('ap-name').oninput = (e) => { ed.name = e.target.value; };
  $('ap-base').oninput = (e) => { ed.base_url = e.target.value.trim(); };
  $('ap-key').oninput = (e) => { ed.api_key = e.target.value.trim(); };
  $('ap-preset').onchange = (e) => {
    const v = e.target.value; if (v === '__none') return;
    ed.base_url = v;
    if (!ed.name) { const pre = API_PRESETS.find((p) => p.base === v); if (pre) ed.name = pre.name; }
    renderApiServices();
  };
  $('ap-save').onclick = () => saveProvider();
  $('ap-cancel').onclick = () => { state.editingProvider = null; renderApiServices(); };
}

function startAddProvider() { state.editingProvider = { id: newProviderId(), name: '', base_url: '', api_key: '', model: '', _isNew: true }; renderApiServices(); }
function startEditProvider(id) { const p = providerById(id); if (!p) return; state.editingProvider = Object.assign({ _isNew: false }, JSON.parse(JSON.stringify(p))); renderApiServices(); }

function saveProvider() {
  const ed = state.editingProvider; if (!ed) return;
  if (!ed.base_url || !ed.base_url.trim()) { alert('Enter the service base URL (OpenAI-compatible).'); return; }
  const clean = { id: ed.id, name: (ed.name || '').trim() || 'Service', base_url: ed.base_url.trim(), api_key: (ed.api_key || '').trim(), model: ed.model || '' };
  const arr = providers();
  const i = arr.findIndex((p) => p.id === ed.id);
  if (i >= 0) arr[i] = clean; else arr.push(clean);
  state.editingProvider = null;
  saveConfig();
  renderSettings();
}

function removeProvider(id) {
  state.config.api_providers = providers().filter((p) => p.id !== id);
  if (state.config.formatter_backend === id) state.config.formatter_backend = '';  // fall back to local
  delete state.apiModels[id];
  if (state.editingProvider && state.editingProvider.id === id) state.editingProvider = null;
  saveConfig();
  renderSettings();
}

function pullStatusText(p) {
  return p.status + (p.percent ? ' · ' + p.percent.toFixed(0) + '%' : '');
}

// Full (re)build of the download rows + cancel buttons. Called ONLY when the set of active
// downloads changes — every progress tick updates in place via updatePullRow so the cancel button
// element stays alive. (Rebuilding it on every tick destroyed it between mousedown and mouseup, so
// the click never registered and cancel appeared dead.)
function renderPullArea() {
  const area = $('pull-area'); if (!area) return;
  const active = Object.entries(state.pulling).filter(([, p]) => !p.done);
  area.innerHTML = active.map(([name, p]) => `
    <div class="pull-row" data-row="${esc(name)}" style="margin-top:10px">
      <div class="pull-status">Downloading <b>${esc(name)}</b> · <span class="pull-msg">${esc(pullStatusText(p))}</span>
        <button type="button" class="pull-cancel" data-cancel="${esc(name)}">✕ CANCEL</button>
      </div>
      <div class="pull-bar"><div class="pull-fill" style="width:${p.percent || 0}%"></div></div>
    </div>`).join('');
  area.querySelectorAll('[data-cancel]').forEach((b) => b.onclick = () => cancelPull(b.dataset.cancel));
}

// Update one download row's bar + status text in place, leaving the cancel button untouched.
// Returns false if no such row is mounted (caller should do a full renderPullArea instead).
function updatePullRow(name) {
  const area = $('pull-area'); if (!area) return false;
  const sel = (window.CSS && CSS.escape) ? CSS.escape(name) : name;
  const row = area.querySelector('.pull-row[data-row="' + sel + '"]');
  if (!row) return false;
  const p = state.pulling[name]; if (!p) return false;
  const msg = row.querySelector('.pull-msg'); if (msg) msg.textContent = pullStatusText(p);
  const fill = row.querySelector('.pull-fill'); if (fill) fill.style.width = (p.percent || 0) + '%';
  return true;
}

function startPull(name) {
  state.pulling[name] = { percent: 0, status: 'starting', done: false };
  invoke('pull_model', { name });
  renderPullArea();
}

function cancelPull(name) {
  invoke('cancel_pull', { name });
  const p = state.pulling[name];
  if (p) { p.status = 'cancelling…'; if (!updatePullRow(name)) renderPullArea(); }
  // Safety net: if the backend is slow to confirm the cancel (e.g. blocked between progress lines),
  // clear the row anyway after a moment so cancel always feels responsive.
  setTimeout(() => {
    if (state.pulling[name] && !state.pulling[name].done) {
      delete state.pulling[name];
      if (state.settingsOpen) renderPullArea();
    }
  }, 1500);
}

function onPullProgress(p) {
  const cancelled = p.status === 'cancelled';
  const existed = !!state.pulling[p.model];
  state.pulling[p.model] = {
    percent: p.percent,
    status: cancelled ? 'cancelled' : (p.error ? 'error: ' + p.error : p.status),
    done: p.done,
  };
  if (state.settingsOpen) {
    // In-place tick keeps the cancel button stable; only rebuild when a row appears or disappears.
    if (p.done || !existed || !updatePullRow(p.model)) renderPullArea();
  }
  if (p.done) {
    if (!p.error && !cancelled) {
      // Completed: show it installed and auto-select it.
      refreshModels().then(() => { state.config.formatter_model = p.model; saveConfig(); if (state.settingsOpen) renderSettings(); });
    }
    // Clear the row after it ends (quick for cancel/error, a beat longer on success).
    setTimeout(() => {
      delete state.pulling[p.model];
      if (state.settingsOpen) renderPullArea();
    }, (p.error || cancelled) ? 600 : 2500);
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
