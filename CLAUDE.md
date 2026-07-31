# Runway — working notes

Menu bar / tray app tracking Claude plan limits, for macOS, Windows and Linux.
Read `README.md` first for the product concept and the accuracy reasoning; this
file is the build/dev context.

`core/` is the engine — no UI, no platform assumptions. `app/` is a Tauri shell
around it. There is no other implementation: the original macOS SwiftUI app and
its WidgetKit extension were removed once the port passed it, and live only in
git history before `Remove the Swift implementation`.

## Build

```sh
cargo run -p runway-app                 # the app, any OS
cargo run --bin runway-cli -- --watch   # terminal front end, same engine
cargo run --bin runway-cli -- --profile # dump the learned working-hours profile
cargo test                              # 97 tests, all of them fast
```

The frontend is static files under `app/src` — no npm, no bundler, no Node.
`tauri-build` embeds them at compile time, so **editing the HTML/JS needs a
`cargo build`**, not just a restart. `cargo tauri dev` gives live reload if you
install the CLI.

CI runs `cargo test`, `cargo fmt --check`, `cargo clippy -D warnings` and a
release build on all three platforms. **Run all four locally before pushing** —
skipping clippy once is exactly how a lint that only fires on a constant
expression reached CI.

`main` is protected: no direct pushes, PR required, CI must be green, and a
code-owner review is required. Work on a branch and open a PR:

```sh
git switch -c some-change
# ...
gh pr create --fill && gh pr checks --watch
```

## Architecture

One engine, one published value. `Engine` (`core/src/engine.rs`) publishes a
`RunwaySnapshot` and every surface — tray, popover, desktop panel, CLI — renders
from it.

Two independent cadences:
- **API poll** every 180s (hard floor, see below)
- **Local log scan** every 15s, free and unlimited

Between API polls the snapshot is marked `estimated` and each limit's percentage
is extrapolated from local token volume using the calibrated tokens-per-percent.
Clamped so it can never run backwards or exceed 100.

`EngineHandle::spawn` runs the loop on its own thread and calls back on every
change. `app/src-tauri/src/main.rs` only ever turns a snapshot into pixels — no
logic lives there.

## Things that will bite you

- **`User-Agent: claude-code/<version>` is mandatory** on the usage endpoint.
  Without it you land in a much stricter rate-limit bucket and get persistent
  429s. Version is scraped from the `version` field in the transcript JSONL
  (`detect_cli_version`) rather than shelling out to `claude`.
- **Never poll faster than 180s.** `usage_api::MINIMUM_POLL_INTERVAL` and
  `Settings::effective_poll_interval` enforce it regardless of the settings file.
- **The 5-hour window rolls**, so its percentage falls as well as rises.
  `projection::calibrate` ignores deltas below +0.5 for this reason.
- **Cache token pricing is the whole ballgame.** Cache reads are ~97% of tokens
  in real usage and cost 0.1× input. The 5m/1h write split comes from
  `usage.cache_creation.ephemeral_{5m,1h}_input_tokens`.
- **Windows are measured in working time** (`core/src/activity.rs`), not calendar
  time. Pace is `spent ÷ expected-by-now` against a learned hour-of-week
  profile; the least-squares slope no longer drives anything. Two invariants
  hold the change together, both tested: a **uniform profile reproduces plain
  calendar behaviour exactly** (so a user with no history loses nothing), and
  the profile's weights **average 1.0 across the week** (so a full week is worth
  1.0 of work whatever its shape). Break either and every derived number drifts.
- **A "working hour" is a mean over a set, not `Σr²/Σr`.** The closed form was
  tried and is far too sensitive to one outlier: a single heavy afternoon was
  21% of a fortnight here and dragged the reference from 4.3 to 8.6, doubling
  the reported allowance. See `typical_active_intensity`.
- **`samples.json` and `scan-state.json` store dates as a bare `Double` counting
  seconds from 2001-01-01**, not 1970 — inherited from the Swift build that
  wrote them first, and kept so upgraders don't lose calibration history.
  `core/src/compat.rs` handles it. Get it wrong and every timestamp moves 31
  years.
- **`runway-snapshot.json` is a public contract.** Third parties can read it.
  Field names are camelCase and dates are whole-second RFC 3339 with no
  fractional part; there are tests pinning both.

## Tauri gotchas, both of which cost real time

- **Never call a tray API off the main thread.** `TrayIcon::set_title` and
  friends dispatch to the event loop and *block waiting for a reply*. The engine
  thread's first callback fires while `setup()` is still running, so the loop
  isn't servicing anything yet and the whole engine wedges — silently, with the
  bootstrap snapshot frozen on screen. `apply_snapshot` posts through
  `run_on_main_thread`, which queues instead of waiting. `sample <pid>` is how
  you find it.
- **A missing capability fails silently at runtime.** Commands declared in
  `generate_handler!` are always allowed, but *core plugin* commands are denied
  unless `app/src-tauri/capabilities/default.json` grants them. A denial rejects
  a promise; with no `.catch()` the feature simply doesn't work and nothing is
  logged. This is why the desktop panel wouldn't drag. CI cannot catch this
  class of bug — only running it can.

## Logging

`~/Library/Application Support/Runway/runway.log`, `%APPDATA%\Runway\runway.log`,
`~/.local/share/runway/runway.log`. Capped at 512KB, mirrored to stderr.

`app/src/errors.js` loads first in both windows and forwards `error` and
`unhandledrejection` to the backend, because a frameless webview with no
devtools swallows them otherwise. Use `.or_warn(context)` instead of `let _ =`
anywhere a failure is survivable but shouldn't be invisible. `cargo run` builds
with devtools on, so right-click → Inspect works.

## The engine lock

Never held across slow work. Reading the macOS keychain can put a consent dialog
on screen and the HTTP call has a 20s timeout, so the poll is split
`begin_poll` / `execute_poll` / `finish_poll` with only the first and last under
the lock. Callbacks also run unlocked, which is what keeps the engine and
settings mutexes from inverting against each other.

## Verified working (2026-07-31)

Against a live Max 5x account on macOS: polls the endpoint, three limits parsed
(`session`, `weekly_all`, `weekly_scoped`), calibration produces a token
allowance, `live` → `estimated` flips correctly between polls, the learned
profile matches the user's actual 09:00–19:00 Mon–Fri pattern, and the desktop
panel drags and remembers its position.

CI is green on macOS, Windows and Linux — tests and a release build.

## Not verified

- **Nothing has been *run* on Windows or Linux**, only compiled. The tray icon's
  painted number at 16px and at 125%/150% display scaling, the popover flipping
  above a bottom taskbar, window transparency under WebView2, and notification
  delivery are all unknowns.

## Not done yet

- No launch-at-login. `tauri-plugin-autostart` covers all three platforms.
- Alarm history is in-memory; it clears on relaunch. The dedupe set persists in
  `fired-alarms.json`.
- `pricing.rs` is a hand-maintained rate card. Sonnet 5 introductory pricing is
  deliberately not applied. Re-check when list prices move.
- `format.js` and `severityOf` in the frontend duplicate `core/src/format.rs`
  and `core/src/severity.rs`, because they run on every countdown tick. The Rust
  side has the tests that pin the boundaries; keep them in step.

## Known rough edges

- **Allowance in tokens stays null for a while.** `calibrate` needs 3+
  consecutive sample pairs with a Δpercent ≥ 0.5, which on a quiet account can
  take hours. The percent-per-hour allowance shows immediately; the token and
  dollar figures only appear once calibrated. Consider falling back to a coarser
  estimate (total local tokens ÷ total percent moved this window) so the headline
  isn't blank on day one.
- **`tokens_per_percent` is jumpy between polls**, because it's a median over
  very few pairs. It can move 2× with nothing else changing, which makes the
  activity model look wrong when it isn't. Trust the pace ratio (which is
  calibration-independent) more than the absolute token figures. Check the
  activity side separately with `runway-cli --profile` and
  `remainingTokens ÷ allowanceTokensPerHour`.
- `Severity::of` still damps pace below 15% *calendar* elapsed, which is more
  conservative than the activity model needs.
