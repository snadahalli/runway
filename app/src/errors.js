// Send uncaught frontend errors somewhere a human will see them.
//
// Both windows are frameless webviews with no devtools open, so by default an
// exception or a rejected promise vanishes without trace. That is not a
// theoretical worry: `startDragging()` was denied by the capability system for
// a while, which rejects a promise, and the only symptom was that dragging
// quietly didn't work.
//
// Loaded before everything else, so it catches failures in the other scripts too.

(function () {
  const invoke = window.__TAURI__?.core?.invoke;
  const surface = document.body?.className || location.pathname;

  function report(context, detail) {
    // Console first, so it's there if devtools *are* open.
    console.error(`[${context}]`, detail);
    try {
      invoke?.("log_error", { context: `${surface}/${context}`, message: String(detail) });
    } catch {
      // Reporting the failure failed. Nothing useful left to do.
    }
  }

  window.addEventListener("error", (e) => {
    report("error", e.error?.stack || e.message || e.type);
  });

  window.addEventListener("unhandledrejection", (e) => {
    report("unhandledrejection", e.reason?.stack || e.reason || "(no reason)");
  });

  // Let the rest of the code report deliberately.
  window.reportError = report;
})();
