const { invoke } = window.__TAURI__.core;
const win = window.__TAURI__.window.getCurrentWindow();

const $ = (s) => document.querySelector(s);
const $$ = (s) => document.querySelectorAll(s);

// ---------- persisted config ----------
// Engine-relevant fields go to the backend profile; UI prefs live in localStorage.
const prefs = JSON.parse(localStorage.getItem("prefs") || "{}");
function savePrefs() { localStorage.setItem("prefs", JSON.stringify(prefs)); }
function pref(k, d) { return prefs[k] !== undefined ? prefs[k] : d; }

let cfg = {
  schema: 1, cps: 100, button: "Left", position_mode: "FollowCursor",
  fixed_x: 0, fixed_y: 0, limit_clicks: 0, limit_ns: 0,
  toggle_vk: 0x75, hold_vk: 0, high_priority: false,
  duty_pct: 0, randomize_pct: 0,
};

let saveTimer = null;
async function pushConfig() {
  await invoke("apply_config", { profile: cfg });
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => invoke("save_profile", { profile: cfg }).catch(() => {}), 400);
}

// ---------- view routing ----------
function showView(v) {
  $$(".view").forEach((el) => el.classList.toggle("active", el.dataset.view === v));
  $$(".navbtn").forEach((b) => b.classList.toggle("active", b.dataset.view === v));
  prefs.view = v; savePrefs();
}
$$(".navbtn").forEach((b) => b.addEventListener("click", () => showView(b.dataset.view)));

$$(".subnav button").forEach((b) => b.addEventListener("click", () => {
  $$(".subnav button").forEach((x) => x.classList.toggle("active", x === b));
  $$(".subpage").forEach((p) => p.classList.toggle("active", p.dataset.page === b.dataset.page));
}));

// ---------- generic segmented control ----------
function segmented(id, onPick) {
  const root = $("#" + id);
  if (!root) return;
  root.querySelectorAll("button").forEach((b) => b.addEventListener("click", () => {
    root.querySelectorAll("button").forEach((x) => x.classList.toggle("active", x === b));
    onPick(b.dataset.v);
  }));
}
function segSet(id, v) {
  const root = $("#" + id); if (!root) return;
  root.querySelectorAll("button").forEach((b) => b.classList.toggle("active", b.dataset.v === v));
}

// ---------- rate helpers ----------
function unitToCps(val, unit) { return unit === "minute" ? Math.max(1, Math.round(val / 60)) : val; }
function maxCps() { return pref("extended", false) ? 4000 : 2000; }
function setCps(displayVal, unit) {
  const capped = Math.max(1, Math.min(maxCps(), Math.round(displayVal)));
  cfg.cps = unitToCps(capped, unit);
  pushConfig();
  updateIntervalHint();
}
function updateIntervalHint() {
  const ms = (1000 / Math.max(1, cfg.cps)).toFixed(cfg.cps > 100 ? 1 : 0);
  const h = $("#a-interval-hint"); if (h) h.textContent = ms + " ms interval";
}

// ---------- SIMPLE view ----------
const sCps = $("#s-cps"), sUnit = $("#s-unit");
sCps.addEventListener("input", () => setCps(parseInt(sCps.value) || 1, sUnit.value));
sUnit.addEventListener("change", () => setCps(parseInt(sCps.value) || 1, sUnit.value));
segmented("s-button", (v) => { cfg.button = v; segSet("a-button", v); pushConfig(); });
segmented("s-mode", (v) => { cfg.position_mode = v; pushConfig(); });
segmented("s-hotkeymode", (v) => { prefs.hotkeymode = v; segSet("a-hotkeymode", v); savePrefs(); });

let running = false;
async function setRunning(r) {
  running = r;
  await invoke("set_running", { run: r });
  const btn = $("#s-toggle");
  btn.textContent = r ? "Stop" : "Start";
  btn.classList.toggle("running", r);
}
$("#s-toggle").addEventListener("click", () => setRunning(!running));

// ---------- ADVANCED view ----------
const aCps = $("#a-cps");
aCps.addEventListener("input", () => { setCps(parseInt(aCps.value) || 1, "second"); sCps.value = cfg.cps; });
segmented("a-speedmode", (v) => {
  $("#a-cps-unit").textContent = v === "rate" ? "Clicks / sec" : "Interval (ms)";
});
segmented("a-hotkeymode", (v) => { prefs.hotkeymode = v; segSet("s-hotkeymode", v); savePrefs(); });
segmented("a-type", (v) => { cfg.click_kind = v === "keyboard" ? 1 : 0; pushConfig(); });
segmented("a-button", (v) => { cfg.button = v; segSet("s-button", v); pushConfig(); });

const limitOn = $("#a-limit-on"), limitVal = $("#a-limit-val");
let limitKind = "clicks";
function applyLimit() {
  if (!limitOn.checked) { cfg.limit_clicks = 0; cfg.limit_ns = 0; }
  else if (limitKind === "clicks") { cfg.limit_clicks = parseInt(limitVal.value) || 0; cfg.limit_ns = 0; }
  else { cfg.limit_ns = (parseInt(limitVal.value) || 0) * 1_000_000_000; cfg.limit_clicks = 0; }
  pushConfig();
}
limitOn.addEventListener("change", () => { limitVal.disabled = !limitOn.checked; applyLimit(); });
limitVal.addEventListener("input", applyLimit);
segmented("a-limit-kind", (v) => { limitKind = v; applyLimit(); });

const duty = $("#a-duty"), dutyVal = $("#a-duty-val");
duty.addEventListener("input", () => { dutyVal.textContent = duty.value + "%"; cfg.duty_pct = +duty.value; pushConfig(); });
const randOn = $("#a-rand-on"), rand = $("#a-rand"), randVal = $("#a-rand-val");
function applyRand() { cfg.randomize_pct = randOn.checked ? +rand.value : 0; rand.disabled = !randOn.checked; pushConfig(); }
randOn.addEventListener("change", () => { prefs.randOn = randOn.checked; savePrefs(); applyRand(); });
rand.addEventListener("input", () => { randVal.textContent = rand.value + "%"; applyRand(); });

// ---------- Zones ----------
function pushZones() {
  const vals = (sel) => [...$$(sel + " input")].map((i) => parseInt(i.value) || 0);
  const cornerVals = vals("#z-corner-quad");
  const edgeVals = vals("#z-edge-quad");
  const z = {
    corner: $("#z-corner").checked,
    corner_px: cornerVals.length ? Math.max(...cornerVals) : 0,
    edge: $("#z-edge").checked,
    edge_px: edgeVals.length ? Math.max(...edgeVals) : 0,
    custom: $("#z-custom").checked,
    rects: [],
  };
  prefs.zones = z; savePrefs();
  invoke("set_zones", { zones: z }).catch(() => {});
}
["z-corner", "z-edge", "z-custom"].forEach((id) => $("#" + id).addEventListener("change", pushZones));
$$('.view[data-view="zones"] input[type=number]').forEach((i) => i.addEventListener("input", pushZones));

// ---------- Click Points ----------
let picking = false;
function applySequence() {
  invoke("set_sequence", { enabled: $("#p-enable").checked, stopWhenComplete: $("#p-stopcomplete").checked }).catch(() => {});
}
$("#p-enable").addEventListener("change", applySequence);
$("#p-stopcomplete").addEventListener("change", applySequence);
$("#p-pick").addEventListener("click", async () => {
  picking = !picking;
  $("#p-pick").textContent = picking ? "Stop Picking" : "Start Picking";
  $("#p-pick").classList.toggle("running", picking);
  $("#p-hint").textContent = picking ? "Right-click spots on screen to add them." : "Hit Start Picking, then right-click spots on screen.";
  await invoke(picking ? "start_picking" : "stop_picking").catch(() => {});
});
$("#p-clear").addEventListener("click", () => invoke("clear_points").then(refreshPoints));
async function refreshPoints() {
  const pts = await invoke("get_points").catch(() => []);
  const list = $("#p-list");
  list.innerHTML = "";
  pts.forEach((p, i) => {
    const row = document.createElement("div");
    row.className = "point-row";
    row.textContent = `#${i + 1}   x ${p[0]}, y ${p[1]}`;
    list.appendChild(row);
  });
}
setInterval(() => { if (picking) refreshPoints(); }, 300);

// ---------- Behavior ----------
$("#b-ontop").addEventListener("change", (e) => { win.setAlwaysOnTop(e.target.checked); $("#pin").classList.toggle("on", e.target.checked); prefs.ontop = e.target.checked; savePrefs(); });
$("#b-extended").addEventListener("change", (e) => { prefs.extended = e.target.checked; savePrefs(); sCps.max = maxCps(); aCps.max = maxCps(); });
["b-alert", "b-rememberpos"].forEach((id) => $("#" + id).addEventListener("change", (e) => { prefs[id] = e.target.checked; savePrefs(); }));

// ---------- Appearance ----------
function applyTheme(t) { document.documentElement.dataset.theme = t; prefs.theme = t; savePrefs(); segSet("ap-theme", t); }
function applyAccent(c) { document.documentElement.style.setProperty("--accent", c); prefs.accent = c; savePrefs(); $("#ap-accent").value = c; $$(".swatch").forEach((s) => s.classList.toggle("sel", s.dataset.c === c)); }
segmented("ap-theme", applyTheme);
$("#ap-accent").addEventListener("input", (e) => applyAccent(e.target.value));
$("#ap-accent-reset").addEventListener("click", () => applyAccent("#22c55e"));
const SWATCHES = ["#22c55e", "#3b82f6", "#a855f7", "#ef4444", "#f59e0b", "#06b6d4", "#ec4899", "#eab308"];
const sw = $("#ap-swatches");
SWATCHES.forEach((c) => {
  const el = document.createElement("div");
  el.className = "swatch"; el.dataset.c = c; el.style.background = c;
  el.addEventListener("click", () => applyAccent(c));
  sw.appendChild(el);
});

// ---------- Presets ----------
async function refreshPresets() {
  const names = await invoke("list_presets");
  const list = $("#preset-list");
  list.innerHTML = "";
  if (names.length === 0) { list.innerHTML = '<p class="muted" style="text-align:center">No presets saved yet.</p>'; return; }
  for (const name of names) {
    const card = document.createElement("div");
    card.className = "preset-card";
    card.innerHTML = `<h4></h4><div class="meta"></div>
      <div class="preset-actions">
        <button class="mini accent" data-a="apply">Apply</button>
        <button class="mini" data-a="update">Update</button>
        <button class="mini danger" data-a="delete">Delete</button>
      </div>`;
    card.querySelector("h4").textContent = name;
    const p = await invoke("get_preset", { name });
    card.querySelector(".meta").textContent = p ? `${p.cps}/s · ${p.button} · ${p.position_mode === "FixedPoint" ? "Fixed" : "Follow"}` : "";
    card.querySelector('[data-a="apply"]').addEventListener("click", async () => {
      const pr = await invoke("get_preset", { name }); if (pr) { cfg = { ...cfg, ...pr }; syncUIFromCfg(); await pushConfig(); setStatus(name); }
    });
    card.querySelector('[data-a="update"]').addEventListener("click", () => invoke("save_preset", { name, profile: cfg }).then(refreshPresets));
    card.querySelector('[data-a="delete"]').addEventListener("click", () => invoke("delete_preset", { name }).then(refreshPresets));
    list.appendChild(card);
  }
}
$("#preset-add").addEventListener("click", async () => {
  const name = $("#preset-name").value.trim(); if (!name) return;
  await invoke("save_preset", { name, profile: cfg });
  $("#preset-name").value = "";
  refreshPresets();
});
function setStatus(preset) {
  const el = $("#status-left");
  el.textContent = preset ? `Preset: ${preset}` : "No preset active";
  el.classList.toggle("active", !!preset);
}

// ---------- Maintenance ----------
$("#mt-reset").addEventListener("click", async () => {
  localStorage.clear();
  cfg = { schema: 1, cps: 100, button: "Left", position_mode: "FollowCursor", fixed_x: 0, fixed_y: 0, limit_clicks: 0, limit_ns: 0, toggle_vk: 0x75, hold_vk: 0, high_priority: false };
  await pushConfig();
  location.reload();
});

// ---------- window controls ----------
$("#min").addEventListener("click", () => win.minimize());
$("#close").addEventListener("click", () => win.close());
$("#pin").addEventListener("click", () => { const on = !$("#pin").classList.contains("on"); $("#pin").classList.toggle("on", on); $("#b-ontop").checked = on; win.setAlwaysOnTop(on); prefs.ontop = on; savePrefs(); });

// ---------- sync UI from cfg ----------
function syncUIFromCfg() {
  sCps.value = cfg.cps; aCps.value = cfg.cps;
  segSet("s-button", cfg.button); segSet("a-button", cfg.button);
  segSet("s-mode", cfg.position_mode);
  limitOn.checked = cfg.limit_clicks > 0 || cfg.limit_ns > 0;
  limitVal.disabled = !limitOn.checked;
  if (cfg.limit_ns > 0) { limitKind = "time"; limitVal.value = cfg.limit_ns / 1_000_000_000; segSet("a-limit-kind", "time"); }
  else if (cfg.limit_clicks > 0) { limitKind = "clicks"; limitVal.value = cfg.limit_clicks; segSet("a-limit-kind", "clicks"); }
  duty.value = cfg.duty_pct; dutyVal.textContent = cfg.duty_pct + "%";
  if (cfg.randomize_pct > 0) { randOn.checked = true; rand.disabled = false; rand.value = cfg.randomize_pct; randVal.textContent = cfg.randomize_pct + "%"; }
  updateIntervalHint();
}

// ---------- status polling ----------
const STATE = ["idle", "running", "stopped: limit reached", "error: input blocked"];
let lastClicks = 0, lastTs = performance.now();
async function poll() {
  try {
    const s = await invoke("get_status");
    const now = performance.now();
    const dt = (now - lastTs) / 1000;
    if (dt > 0) $("#readout").textContent = s.running ? Math.round((s.clicks - lastClicks) / dt) : 0;
    lastClicks = s.clicks; lastTs = now;
    if (running !== s.running) { running = s.running; const b = $("#s-toggle"); b.textContent = running ? "Stop" : "Start"; b.classList.toggle("running", running); }
    $("#readout-sub").textContent = STATE[s.engine_state] || "";
    $("#mt-engine").textContent = s.has_engine ? `running · pinned core ${s.pinned_core ?? "?"}` : "not started (F8 in use?)";
  } catch {}
}
setInterval(poll, 150);

// ---------- init ----------
async function init() {
  // theme / accent / behavior prefs
  applyTheme(pref("theme", "dark"));
  applyAccent(pref("accent", "#22c55e"));
  if (pref("ontop", false)) { $("#b-ontop").checked = true; $("#pin").classList.add("on"); win.setAlwaysOnTop(true); }
  if (pref("extended", false)) $("#b-extended").checked = true;
  $("#b-alert").checked = pref("b-alert", true);
  $("#b-rememberpos").checked = pref("b-rememberpos", true);
  duty.value = pref("duty", 0); dutyVal.textContent = duty.value + "%";
  randOn.checked = pref("randOn", false); rand.disabled = !randOn.checked;
  rand.value = pref("rand", 0); randVal.textContent = rand.value + "%";
  segSet("s-hotkeymode", pref("hotkeymode", "toggle")); segSet("a-hotkeymode", pref("hotkeymode", "toggle"));
  segSet("a-type", cfg.click_kind === 1 ? "keyboard" : "mouse");
  sCps.max = maxCps(); aCps.max = maxCps();

  // engine config from backend profile
  try { const p = await invoke("load_profile"); if (p) { cfg = { ...cfg, ...p }; } } catch {}
  syncUIFromCfg();
  await pushConfig();
  refreshPresets();
  // Restore zone toggles/sizes and push to the backend monitor.
  if (prefs.zones) {
    $("#z-corner").checked = !!prefs.zones.corner;
    $("#z-edge").checked = !!prefs.zones.edge;
    $("#z-custom").checked = !!prefs.zones.custom;
  }
  pushZones();
  refreshPoints();
  showView(pref("view", "simple"));
}
init();
