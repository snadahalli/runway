// The popover. Renders one `View` from the backend; the backend pushes an event
// whenever the snapshot changes, and a local ticker keeps countdowns moving in
// between without bothering it.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let view = null;
let panel = "runway";

const $ = (id) => document.getElementById(id);
const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

// ---------------------------------------------------------------- lifecycle

async function load() {
  view = await invoke("get_view");
  render();
}

function render() {
  if (!view) return;
  renderHealth();
  renderFooter();
  if (panel === "runway") renderRunway();
  if (panel === "ledger") renderLedger();
  if (panel === "alarms") renderAlarms();
  if (panel === "settings") renderSettings();
}

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    panel = tab.dataset.panel;
    document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("is-active", t === tab));
    document.querySelectorAll(".panel").forEach((p) => {
      p.hidden = p.dataset.panel !== panel;
    });
    render();
  });
});

$("refresh").addEventListener("click", () => invoke("refresh"));
$("quit").addEventListener("click", () => invoke("quit"));

// Clicking away should dismiss, the way a real popover does.
window.addEventListener("blur", () => {
  if (!document.body.classList.contains("pinned")) invoke("close_popover");
});
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") invoke("close_popover");
});

listen("runway://snapshot", load);
// Countdowns and the staleness label must keep moving between polls.
setInterval(render, 1000);
load();

// ------------------------------------------------------------------ header

const HEALTH_TEXT = {
  live: "live",
  estimated: "estimated",
  backingOff: "backing off",
  error: "error",
  noCredentials: "no login",
};

function renderHealth() {
  const health = view.snapshot.health;
  const node = $("health");
  node.className = "health " + health;
  node.title = view.snapshot.message || view.lastError || "Usage data status";
  $("health-text").textContent = HEALTH_TEXT[health] || health;
}

function renderFooter() {
  const now = new Date();
  const observed = view.snapshot.apiObservedAt ? new Date(view.snapshot.apiObservedAt) : null;
  $("footer-text").textContent = observed
    ? `API ${Fmt.duration((now - observed) / 1000)} ago`
    : "No API reading yet";
}

// ------------------------------------------------------------ runway panel

function renderRunway() {
  const root = $("panel-runway");
  root.replaceChildren();
  const now = new Date();
  const limits = view.snapshot.limits;

  if (!limits.length) {
    const card = el("div", "card");
    card.append(
      el("div", "section-title", "No usage data yet"),
      el(
        "div",
        "muted",
        view.snapshot.message ||
          "Runway reads the same credentials Claude Code uses. If you've never signed in, run `claude` once in a terminal."
      )
    );
    const retry = el("button", "icon", "Try again");
    retry.style.marginTop = "8px";
    retry.addEventListener("click", () => invoke("refresh"));
    card.append(retry);
    root.append(card);
    return;
  }

  root.append(headlineCard(now), ...limits.map((l) => limitCard(l, now)));
  if (view.activity.learned) root.append(rhythmCard());
}

// The app bases pace, run-dry and allowance on this profile, so it shows you
// what it learned rather than asking you to trust it.
function rhythmCard() {
  const card = el("div", "card");
  card.append(el("div", "section-title", "Your working rhythm"));
  card.append(
    el("div", "muted", "Learned from the last 28 days. Windows are measured in working time, not calendar time.")
  );

  const weights = view.activity.weights;
  const peak = Math.max(...weights, 1);
  const grid = el("div", "rhythm");
  const days = ["M", "T", "W", "T", "F", "S", "S"];
  for (let d = 0; d < 7; d++) {
    grid.append(el("span", "rhythm-day", days[d]));
    for (let h = 0; h < 24; h++) {
      const cell = el("i", "rhythm-cell");
      const v = weights[d * 24 + h] / peak;
      cell.style.opacity = (0.06 + 0.94 * Math.sqrt(v)).toFixed(3);
      cell.title = `${["Mon","Tue","Wed","Thu","Fri","Sat","Sun"][d]} ${String(h).padStart(2,"0")}:00`;
      grid.append(cell);
    }
  }
  card.append(grid);

  const scale = el("div", "rhythm-scale");
  scale.append(el("span", null, "00"), el("span", null, "06"), el("span", null, "12"),
               el("span", null, "18"), el("span", null, "23"));
  card.append(scale);
  return card;
}

function headline() {
  // Mirrors RunwaySnapshot::headline in core/src/snapshot.rs: a limit projected
  // to run out before its window resets binds first, soonest wins; otherwise
  // the fullest one does.
  const now = new Date();
  const candidates = view.snapshot.limits.filter((l) => l.percent > 0 || l.isActive);
  if (!candidates.length) return view.snapshot.limits[0] || null;
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

function headlineCard(now) {
  const limit = headline();
  const sev = limit ? severityName(severityOf(limit, now)) : "calm";

  const card = el("div", `headline ${sev}`);
  card.append(el("div", "eyebrow", "Burn allowance"));

  // "working hour" is not a flourish: once a rhythm is learned the allowance is
  // per hour of the kind of time you actually spend, which is a much larger and
  // more actionable number than a calendar-hour average.
  const hour = view.activity.learned ? "working hour" : "hour";
  const big = el("div", `big ${sev}`);
  if (limit && limit.allowanceTokensPerHour != null) {
    big.append(el("span", null, Fmt.tokens(limit.allowanceTokensPerHour)));
    big.append(el("span", "unit", `tokens / ${hour}`));
  } else if (limit && limit.allowancePercentPerHour != null) {
    big.append(el("span", null, limit.allowancePercentPerHour.toFixed(1) + "%"));
    big.append(el("span", "unit", `/ ${hour}`));
  } else {
    big.append(el("span", null, "—"));
  }
  card.append(big, el("div", "sentence", headlineSentence(limit, now)));
  return card;
}

function headlineSentence(limit, now) {
  if (!limit) return "Waiting for the first reading.";
  if (limit.allowanceTokensPerHour == null && limit.paceRatio == null) {
    return `Calibrating against ${limit.label.toLowerCase()}. Projections appear after a few polls.`;
  }
  const exhausts = limit.exhaustsAt ? new Date(limit.exhaustsAt) : null;
  const resets = limit.resetsAt ? new Date(limit.resetsAt) : null;
  if (exhausts && resets && exhausts < resets) {
    const early = Fmt.duration((resets - exhausts) / 1000);
    return `${limit.label} runs dry around ${Fmt.clock(exhausts, now)} — ${early} before it resets.`;
  }
  if (resets) {
    const landing = limit.percent + (limit.paceRatio || 0) * (100 - limit.percent);
    return `${limit.label} is on track to finish at ${Fmt.percent(landing)} when it resets ${Fmt.clock(resets, now)}.`;
  }
  return `${limit.label} at ${Fmt.percent(limit.percent)}.`;
}

function limitCard(limit, now) {
  const sev = severityName(severityOf(limit, now));
  const card = el("div", "card");

  const head = el("div", "limit-head");
  head.append(el("span", "name", limit.label));
  if (limit.isActive) head.append(el("span", `badge ${sev}`, "BINDING"));
  head.append(el("span", `pct ${sev}`, Fmt.percent(limit.percent)));
  card.append(head, trackFor(limit, sev));

  const resets = limit.resetsAt ? new Date(limit.resetsAt) : null;
  const stats = el("div", "stats");
  stats.append(
    stat("Pace", limit.paceRatio != null ? Fmt.ratio(limit.paceRatio) : "—",
      limit.paceRatio > 1 ? sev : null),
    stat("Resets", resets ? Fmt.duration((resets - now) / 1000) : "—"),
    stat("Left", limit.remainingTokens != null ? Fmt.tokens(limit.remainingTokens) : "—"),
    stat("Worth", limit.remainingValueUSD != null ? Fmt.usd(limit.remainingValueUSD) : "—")
  );
  card.append(stats);

  if (limit === headline() && view.series.length >= 2) {
    card.append(sparkline(view.series, sev));
  }
  return card;
}

function stat(key, value, tint) {
  const node = el("div", "stat");
  node.append(el("div", "k", key), el("div", `v ${tint || ""}`, value));
  return node;
}

function trackFor(limit, sev) {
  // rate × hoursRemaining simplifies to paceRatio × remainingPercent.
  const projected =
    limit.paceRatio == null
      ? limit.percent
      : limit.percent + limit.paceRatio * Math.max(0, 100 - limit.percent);

  const track = el("div", `track ${sev}`);
  if (projected > limit.percent) {
    const hatch = el("div", "projected");
    hatch.style.width = Math.min(100, projected) + "%";
    track.append(hatch);
  }
  const spent = el("div", "spent");
  spent.style.width = Math.min(100, limit.percent) + "%";
  track.append(spent);
  if (projected > 100) track.append(el("div", "overflow", "»"));
  return track;
}

function sparkline(series, sev) {
  const w = 340;
  const h = 22;
  const first = series[0].t;
  const span = series[series.length - 1].t - first;
  if (span <= 0) return el("div");
  const max = Math.max(1, ...series.map((p) => p.percent));
  const points = series
    .map((p) => `${((p.t - first) / span) * w},${h - (p.percent / max) * h}`)
    .join(" ");

  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", `spark ${sev}`);
  svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
  svg.setAttribute("preserveAspectRatio", "none");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
  path.setAttribute("points", points);
  path.setAttribute("fill", "none");
  path.setAttribute("stroke", "currentColor");
  path.setAttribute("stroke-width", "1.5");
  path.setAttribute("stroke-linejoin", "round");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("vector-effect", "non-scaling-stroke");
  svg.append(path);
  return svg;
}

// ------------------------------------------------------------ ledger panel

function renderLedger() {
  const root = $("panel-ledger");
  root.replaceChildren();
  const ledger = view.snapshot.ledger;

  const summary = el("div", "card");
  summary.append(el("div", "eyebrow", ledger.windowLabel));
  const big = el("div", "big");
  big.append(el("span", null, Fmt.usd(ledger.costUSD)));
  big.append(el("span", "unit", "API-equivalent"));
  summary.append(big);
  summary.append(
    el(
      "div",
      "muted",
      `${Fmt.tokens(
        ledger.tokens.input +
          ledger.tokens.output +
          ledger.tokens.cacheWrite5m +
          ledger.tokens.cacheWrite1h +
          ledger.tokens.cacheRead
      )} tokens · ${Fmt.tokens(ledger.tokens.cacheRead)} of them cache reads at 0.1× input`
    )
  );
  if (view.snapshot.monthlyValueUSD != null) {
    summary.append(
      el("div", "muted", `${Fmt.usd(view.snapshot.monthlyValueUSD)} over the last 30 days`)
    );
  }
  root.append(summary);

  root.append(breakdownCard("By project", ledger.topProjects));
  root.append(breakdownCard("By model", ledger.topModels));

  root.append(
    el(
      "div",
      "muted",
      "Not a bill. Subscription plans don't charge per token — this is what these tokens would have cost on the pay-as-you-go API."
    )
  );
}

function breakdownCard(title, entries) {
  const card = el("div", "card");
  card.append(el("div", "section-title", title));
  if (!entries.length) {
    card.append(el("div", "muted", "Nothing recorded yet."));
    return card;
  }
  const rows = el("div", "rows");
  for (const entry of entries) {
    const row = el("div", "row");
    row.append(
      el("span", "name", entry.name),
      el("span", "num", Fmt.tokens(entry.tokens)),
      el("span", "num lead", Fmt.usd(entry.costUSD))
    );
    rows.append(row);
  }
  card.append(rows);
  return card;
}

// ------------------------------------------------------------ alarms panel

function renderAlarms() {
  const root = $("panel-alarms");
  root.replaceChildren();
  if (!view.alarms.length) {
    const card = el("div", "card");
    card.append(
      el("div", "section-title", "No alarms yet"),
      el("div", "muted", "Alarms are evaluated only on live API readings — an extrapolated percentage crossing 80% is a guess, not an event.")
    );
    root.append(card);
    return;
  }
  const now = new Date();
  for (const alarm of view.alarms) {
    const node = el("div", "alarm");
    node.append(el("div", "t", alarm.title), el("div", "b", alarm.body));
    const when = Fmt.duration((now - new Date(alarm.date)) / 1000) + " ago";
    node.append(el("div", "when", alarm.deliver ? when : `${when} · suppressed by quiet hours`));
    root.append(node);
  }
}

// ---------------------------------------------------------- settings panel

let settingsBuilt = false;

function renderSettings() {
  if (settingsBuilt) return; // Inputs are live; rebuilding would fight the user.
  settingsBuilt = true;

  const root = $("panel-settings");
  root.replaceChildren();
  const s = view.settings;

  const save = () => invoke("save_settings", { settings: s });

  const card = el("div", "card");
  card.append(el("div", "section-title", "Readout"));

  card.append(
    selectField(
      "Menu bar shows",
      [
        ["paceRatio", "Pace ratio"],
        ["allowance", "Hourly allowance"],
        ["percent", "Percent used"],
        ["timeLeft", "Time to dry"],
      ],
      s.menuBarStyle,
      (v) => {
        s.menuBarStyle = v;
        save();
      }
    )
  );
  card.append(
    checkField("Desktop panel", view.platform === "macos"
      ? "An always-on-top panel. The Notification Centre widget is the other option on macOS."
      : "An always-on-top panel — the stand-in for a widget on this platform.",
      s.showHud, (v) => {
        s.showHud = v;
        invoke("set_hud_visible", { visible: v });
      })
  );
  root.append(card);

  const poll = el("div", "card");
  poll.append(el("div", "section-title", "Polling"));
  poll.append(
    numberField("Interval (seconds)", "Never faster than 180s — a tighter cadence lands your token in a stricter rate-limit bucket.", s.pollInterval, 180, (v) => {
      s.pollInterval = Math.max(180, v);
      save();
    })
  );
  poll.append(
    textField("User-Agent version", "Blank reads it from your transcript logs, which is what you want.", s.userAgentOverride, (v) => {
      s.userAgentOverride = v;
      save();
    })
  );
  root.append(poll);

  const alarms = el("div", "card");
  alarms.append(el("div", "section-title", "Alarms"));
  alarms.append(checkField("Enabled", null, s.alarmsEnabled, (v) => { s.alarmsEnabled = v; save(); }));
  alarms.append(
    textField("Thresholds (%)", "Comma separated.", s.thresholds.join(", "), (v) => {
      const parsed = v.split(",").map((n) => parseFloat(n.trim())).filter((n) => !isNaN(n));
      if (parsed.length) s.thresholds = parsed;
      save();
    })
  );
  alarms.append(
    checkField("Predictive", "Warn when the projection says a limit runs dry before it resets.", s.predictiveAlarms, (v) => { s.predictiveAlarms = v; save(); })
  );
  alarms.append(
    numberField("Pace alarm at", "Multiples of sustainable burn.", s.paceAlarmRatio, 1, (v) => { s.paceAlarmRatio = v; save(); })
  );
  alarms.append(checkField("Quiet hours", null, s.quietHoursEnabled, (v) => { s.quietHoursEnabled = v; save(); }));
  alarms.append(numberField("Quiet from", null, s.quietStartHour, 0, (v) => { s.quietStartHour = v; save(); }));
  alarms.append(numberField("Quiet until", null, s.quietEndHour, 0, (v) => { s.quietEndHour = v; save(); }));
  root.append(alarms);
}

function field(labelText, hintText, control) {
  const wrap = document.createDocumentFragment();
  const row = el("div", "field");
  row.append(el("label", null, labelText), control);
  wrap.append(row);
  if (hintText) wrap.append(el("div", "hint", hintText));
  return wrap;
}

function selectField(label, options, value, onChange) {
  const select = el("select");
  for (const [key, text] of options) {
    const option = el("option", null, text);
    option.value = key;
    if (key === value) option.selected = true;
    select.append(option);
  }
  select.addEventListener("change", () => onChange(select.value));
  return field(label, null, select);
}

function checkField(label, hint, value, onChange) {
  const input = el("input");
  input.type = "checkbox";
  input.checked = !!value;
  input.addEventListener("change", () => onChange(input.checked));
  return field(label, hint, input);
}

function numberField(label, hint, value, min, onChange) {
  const input = el("input");
  input.type = "number";
  input.value = value;
  input.min = min;
  input.step = "any";
  input.addEventListener("change", () => onChange(parseFloat(input.value) || min));
  return field(label, hint, input);
}

function textField(label, hint, value, onChange) {
  const input = el("input");
  input.type = "text";
  input.value = value ?? "";
  input.addEventListener("change", () => onChange(input.value));
  return field(label, hint, input);
}
