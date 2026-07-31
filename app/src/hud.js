// The desktop panel. Same snapshot as everything else, at a glance.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

let view = null;

// Dragging. The panel is frameless, so there's no title bar to grab — the whole
// surface is the handle.
//
// `-webkit-app-region: drag` is the obvious way to do this and does nothing
// here: it's a Chromium/Electron feature, and Tauri renders in WKWebView on
// macOS and WebView2 on Windows. `startDragging()` hands the drag to the window
// manager, which is what makes it feel native rather than chasing the cursor a
// frame behind.
document.addEventListener("mousedown", async (e) => {
  if (e.button !== 0) return;
  if (e.target.closest("button, input, select, a")) return;
  await appWindow.startDragging();
});

// `startDragging` swallows the matching mouseup, so the move event is the only
// reliable signal that a drag finished. It fires continuously, hence the debounce.
let saveTimer = null;
appWindow.onMoved(({ payload }) => {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(
    () => invoke("save_hud_position", { x: payload.x, y: payload.y }),
    400
  );
});

async function load() {
  view = await invoke("get_view");
  render();
}

function headline(snapshot, now) {
  // Same rule as core/src/snapshot.rs and app.js.
  const candidates = snapshot.limits.filter((l) => l.percent > 0 || l.isActive);
  if (!candidates.length) return snapshot.limits[0] || null;
  const key = (l) => {
    const exhausts = l.exhaustsAt ? new Date(l.exhaustsAt) : null;
    const resets = l.resetsAt ? new Date(l.resetsAt) : null;
    if (exhausts && resets && exhausts < resets) return [0, (exhausts - now) / 1000];
    return [1, -l.percent];
  };
  return candidates.reduce((best, l) => {
    const [ac, av] = key(l);
    const [bc, bv] = key(best);
    return ac < bc || (ac === bc && av < bv) ? l : best;
  });
}

function render() {
  if (!view) return;
  const now = new Date();
  const snapshot = view.snapshot;
  const limit = headline(snapshot, now);
  const sev = limit ? severityName(severityOf(limit, now)) : "calm";

  const value = document.getElementById("value");
  value.className = "big " + sev;
  value.replaceChildren();

  if (limit && limit.allowanceTokensPerHour != null) {
    value.append(text("span", Fmt.tokens(limit.allowanceTokensPerHour)), text("span", "/h", "unit"));
  } else if (limit && limit.allowancePercentPerHour != null) {
    value.append(text("span", limit.allowancePercentPerHour.toFixed(1) + "%"), text("span", "/h", "unit"));
  } else {
    value.append(text("span", "—"));
  }

  const track = document.getElementById("track");
  track.className = "track " + sev;
  track.firstElementChild.style.width = limit ? Math.min(100, limit.percent) + "%" : "0%";

  document.getElementById("label").textContent = limit ? limit.label : (snapshot.message || "Not connected");
  document.getElementById("pct").textContent = limit ? Fmt.percent(limit.percent) : "";

  // The honesty label: green while fresh, amber once a decision based on this
  // number could be wrong.
  const age = (now - new Date(snapshot.generatedAt)) / 1000;
  document.getElementById("age").textContent = age < 90 ? "just now" : Fmt.duration(age) + " ago";
  document.getElementById("dot").style.background =
    age > 15 * 60 ? "var(--watch)" : "var(--calm)";
}

function text(tag, content, className) {
  const node = document.createElement(tag);
  node.textContent = content;
  if (className) node.className = className;
  return node;
}

listen("runway://snapshot", load);
setInterval(render, 1000);
load();
