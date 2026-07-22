const { invoke } = window.__TAURI__.core;
const win = window.__TAURI__.window.getCurrentWindow();

const readout = document.getElementById("readout");
const cpsInput = document.getElementById("cps");
const toggleBtn = document.getElementById("toggle");

let running = false;
let lastClicks = 0;
let lastTs = performance.now();

function profileFromUI() {
  return {
    schema: 1,
    cps: Math.max(1, Math.min(4000, parseInt(cpsInput.value) || 100)),
    button: "Left",
    position_mode: "FollowCursor",
    fixed_x: 0,
    fixed_y: 0,
    limit_clicks: 0,
    limit_ns: 0,
    toggle_vk: 0x75,
    hold_vk: 0,
    high_priority: false,
  };
}

async function applyConfig() {
  await invoke("apply_config", { profile: profileFromUI() });
}

toggleBtn.addEventListener("click", async () => {
  running = !running;
  if (running) await applyConfig();
  await invoke("set_running", { run: running });
  toggleBtn.textContent = running ? "Stop" : "Start";
  toggleBtn.classList.toggle("running", running);
});

cpsInput.addEventListener("change", applyConfig);

document.getElementById("min").addEventListener("click", () => win.minimize());
document.getElementById("close").addEventListener("click", () => win.close());

async function poll() {
  try {
    const s = await invoke("get_status");
    const now = performance.now();
    const dt = (now - lastTs) / 1000;
    if (dt > 0) {
      const cps = Math.round((s.clicks - lastClicks) / dt);
      readout.textContent = s.running ? cps : 0;
    }
    lastClicks = s.clicks;
    lastTs = now;
    running = s.running;
    toggleBtn.textContent = running ? "Stop" : "Start";
    toggleBtn.classList.toggle("running", running);
  } catch (e) {
    // backend not ready yet
  }
}
setInterval(poll, 150);

// Load persisted settings into the UI.
invoke("load_profile").then((p) => {
  if (p && p.cps) cpsInput.value = p.cps;
});
