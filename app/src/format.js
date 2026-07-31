// Formatting, mirrored from core/src/format.rs.
//
// Duplicated rather than round-tripped through the backend because these run on
// every animation frame of the countdowns. If you change a rule here, change it
// there — core/src/format.rs has the tests that pin the boundaries.

const Fmt = {
  tokens(value) {
    const abs = Math.abs(value);
    if (abs >= 1e9) return (value / 1e9).toFixed(2) + "B";
    if (abs >= 1e6) return (value / 1e6).toFixed(1) + "M";
    if (abs >= 1e4) return (value / 1e3).toFixed(0) + "K";
    if (abs >= 1e3) return (value / 1e3).toFixed(1) + "K";
    return value.toFixed(0);
  },

  usd(value) {
    if (value >= 1000) return "$" + value.toFixed(0);
    if (value >= 10) return "$" + value.toFixed(1);
    return "$" + value.toFixed(2);
  },

  percent(value) {
    return value >= 10 ? value.toFixed(0) + "%" : value.toFixed(1) + "%";
  },

  ratio(value) {
    return (value >= 10 ? value.toFixed(0) : value.toFixed(1)) + "×";
  },

  // "2h 14m", "48m", "31s" — compact, no leading zeros, never negative.
  duration(seconds) {
    const total = Math.max(0, Math.floor(seconds));
    const days = Math.floor(total / 86400);
    const hours = Math.floor((total % 86400) / 3600);
    const minutes = Math.floor((total % 3600) / 60);
    if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
    if (hours > 0) return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
    if (minutes > 0) return `${minutes}m`;
    return `${total}s`;
  },

  // A wall clock the user can act on, rather than a countdown they have to add up.
  clock(date, now) {
    const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    const sameDay = (a, b) => a.toDateString() === b.toDateString();
    if (sameDay(date, now)) return time;
    const tomorrow = new Date(now.getTime() + 86400000);
    if (sameDay(date, tomorrow)) return `tomorrow ${time}`;
    return `${date.toLocaleDateString([], { weekday: "short" })} ${time}`;
  },
};

// Severity, mirrored from core/src/severity.rs. The worse of two independent
// readings — how full, and how fast — with pace damped early in a window.
function severityOf(limit, now) {
  let level = limit.percent >= 90 ? 2 : limit.percent >= 70 ? 1 : 0;

  const resets = limit.resetsAt ? new Date(limit.resetsAt) : null;
  const windowSeconds = limit.kind === "session" ? 5 * 3600 : 7 * 24 * 3600;
  const remaining = resets ? Math.max(0, (resets - now) / 1000) : null;
  const elapsedFraction = remaining === null ? 1 : 1 - Math.min(1, remaining / windowSeconds);

  if ((elapsedFraction > 0.15 || limit.percent >= 15) && limit.paceRatio != null) {
    const pace = limit.paceRatio >= 2 ? 2 : limit.paceRatio >= 1.15 ? 1 : 0;
    level = Math.max(level, pace);
  }
  return level;
}

const SEVERITY_NAMES = ["calm", "watch", "tight"];
const severityName = (level) => SEVERITY_NAMES[level];
