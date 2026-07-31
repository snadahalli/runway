# Runway — working notes

Menu bar / tray app tracking Claude plan limits. Read `README.md` first for the
product concept and the accuracy reasoning; this file is the build/dev context.

**Two implementations live here.** `core/` + `app/` is the cross-platform one
(Rust + Tauri) and is where the project is going. `Sources/` is the original
macOS SwiftUI app, kept until the port reaches parity — and its `Widget/` target
is still the only real Notification Centre widget, so it isn't going away soon.

## Build

```sh
cargo run -p runway-app                 # cross-platform app, any OS
cargo run --bin runway-cli -- --watch   # terminal front end, same engine
cargo test                              # 81 tests, all of them fast

./build.sh --install                    # Swift app + widget, ad-hoc, /Applications
SIGN_IDENTITY="Runway Dev" ./build.sh   # stable self-signed cert, no Apple account
DEVELOPMENT_TEAM=XXXXXXXXXX ./build.sh  # Xcode-managed signing, if you have a team
```

The frontend is static files under `app/src` — no npm, no bundler, no Node.
`tauri-build` embeds them at compile time, so **editing the HTML/JS needs a
`cargo build`**, not just a restart. `cargo tauri dev` gives live reload if you
install the CLI.

## Architecture

One engine, one published value. `Engine` (`core/src/engine.rs`) is a direct
port of the Swift `AppModel`; both publish a `RunwaySnapshot` and every surface
— tray text, popover, desktop panel, widget — renders from it.

Two independent cadences, in both implementations:
- **API poll** every 180s (hard floor, see below)
- **Local log scan** every 15s, free and unlimited

Between API polls the snapshot is marked `estimated` and each limit's percentage
is extrapolated from local token volume using the calibrated tokens-per-percent.
Clamped so it can never run backwards or exceed 100.

`EngineHandle::spawn` runs the loop on its own thread and calls back on every
change. The Tauri layer (`app/src-tauri/src/main.rs`) only ever turns a snapshot
into pixels — no logic lives there.

## Things that will bite you

- **`User-Agent: claude-code/<version>` is mandatory** on the usage endpoint.
  Without it you land in a much stricter rate-limit bucket and get persistent
  429s. Version is scraped from the `version` field in the transcript JSONL
  (`detect_cli_version`) rather than shelling out to `claude`.
- **Never poll faster than 180s.** `usage_api::MINIMUM_POLL_INTERVAL` and
  `Settings::effective_poll_interval` enforce it regardless of the settings file.
- **The 5-hour window rolls**, so its percentage falls as well as rises.
  `projection::calibrate` ignores deltas below +0.5 for this reason.
- **Windows are measured in working time** (`core/src/activity.rs`), not calendar
  time. Pace is `spent ÷ expected-by-now` against a learned hour-of-week
  profile; the least-squares slope no longer drives anything. Two invariants
  hold the change together, both tested: a **uniform profile reproduces the old
  calendar behaviour exactly** (so a user with no history loses nothing), and
  the profile's weights **average 1.0 across the week** (so a full week is worth
  1.0 of work whatever its shape). Break either and every derived number drifts.
- **A "working hour" is a mean over a set, not `Σr²/Σr`.** The closed form was
  tried and is far too sensitive to one outlier: a single heavy afternoon was
  21% of a fortnight here and dragged the reference from 4.3 to 8.6, doubling
  the reported allowance. See `typical_active_intensity`.
- **Cache token pricing is the whole ballgame.** Cache reads are ~97% of tokens
  in real usage and cost 0.1× input. The 5m/1h write split comes from
  `usage.cache_creation.ephemeral_{5m,1h}_input_tokens`.
- **The snapshot's date format is load-bearing.** The Swift widget decodes
  `runway-snapshot.json` with `.iso8601`, which *rejects fractional seconds*. The
  Rust side therefore writes whole seconds via `snapshot::iso8601`. There's a
  test pinning it; don't let serde's default RFC 3339 output back in.
- **`samples.json` and `scan-state.json` use Swift's `.deferredToDate`** — a bare
  `Double` counting seconds from **2001-01-01**, not 1970. `core/src/compat.rs`
  handles it. Get this wrong and every persisted timestamp silently moves 31
  years. This is why a user switching from the Swift app keeps their calibration
  history instead of waiting hours for it to rebuild.
- **Don't run both apps at once.** They write the same state files and will
  clobber each other. Running the *Tauri app* alongside the *Swift widget* is
  fine and supported — the widget just renders whatever last wrote the snapshot.
- **Never call a Tauri tray API off the main thread.** `TrayIcon::set_title`
  and friends dispatch to the event loop and *block waiting for a reply*. The
  engine thread's first callback fires while `setup()` is still running, so the
  loop isn't servicing anything yet and the whole engine wedges — silently, with
  the bootstrap snapshot frozen on screen. `apply_snapshot` posts through
  `run_on_main_thread`, which queues instead of waiting. This cost an hour;
  `sample <pid>` is how you find it.
- **The engine lock is never held across slow work.** Reading the macOS keychain
  can put a consent dialog on screen and the HTTP call has a 20s timeout, so the
  poll is split `begin_poll` / `execute_poll` / `finish_poll` with only the first
  and last under the lock. Callbacks also run unlocked, which is what keeps the
  engine and settings mutexes from inverting against each other.

## macOS signing

**The widget needs no Apple Developer account.** macOS honours
`com.apple.security.application-groups` on an ad-hoc signature — verified: a
sandboxed ad-hoc bundle gets its container and is still denied everything
outside it. What refuses is Xcode's *build system*, which demands a provisioning
profile for any entitlement under manual signing. So `build.sh` builds with
`CODE_SIGNING_ALLOWED=NO` and then codesigns the `.appex` and the `.app` itself,
in that order. Don't "fix" this by putting the entitlements back under Xcode's
control.

**Widget registration needs `/Applications`.** `pkd` registers the extension when
the containing app is launched from a scanned location. Launching out of
`.build` does not register it, so `--install` exists.

`Runway.xcodeproj` is generated by xcodegen from `project.yml` and is
gitignored — never hand-edit it, edit the spec. One spec covers both targets.
Adding a source file needs no spec change: targets reference whole directories.

## Verified working (2026-07-31)

Against a live Max 5x account, on macOS:

- Swift app + widget build ad-hoc with **no Apple account**; `pluginkit -m -p
  com.apple.widgetkit-extension` lists `com.sn.runway.widget` from
  `/Applications`, and the app writes into the App Group container.
- Rust engine reads the Swift app's `scan-state.json` (30 days, 1.3 MB) and
  `samples.json`, hits the live endpoint, and produces the same derived numbers
  — pace 0.611 vs 0.613 across readings 95s apart. Key sets of the two snapshot
  JSONs are **identical**; state round-trips both directions without loss.
- Tauri app runs, polls the live endpoint (plan `max`, three limits), calibrates
  a 987K tokens/hour allowance, flips `live` → `estimated` between polls, and
  publishes snapshots the Swift widget can read.

## Not verified

- **Nothing on Windows has been run.** The code paths are there and the platform
  facts are right (credentials are a plain file at
  `%USERPROFILE%\.claude\.credentials.json`; the tray is icon-only), but no one
  has built or launched it on Windows. Same for Linux.
- **No rendered UI has been eyeballed** — not the Swift widget's faces, not the
  Tauri popover, not the desktop panel, not the tray icon. Screen recording
  permission wasn't available in the session that wrote them. The tray icon's
  *pixel output* is unit-tested; how it looks at 16px is not.

## Not done yet

- No launch-at-login on either implementation. `SMAppService.mainApp.register()`
  on macOS; `tauri-plugin-autostart` covers all three.
- Alarm history is in-memory; it clears on relaunch. The dedupe set persists
  (`fired-alarms.json`, or UserDefaults in the Swift app).
- `pricing.rs` / `Pricing.swift` are hand-maintained rate cards, and they're now
  **two copies of the same table**. Sonnet 5 introductory pricing is deliberately
  not applied. Re-check both when list prices move.
- `format.js` and `severityOf` in the frontend duplicate `core/src/format.rs` and
  `core/src/severity.rs`, because they run on every countdown tick. The Rust side
  has the tests that pin the boundaries; keep them in step.
- The Swift app has no tests. The Rust core has 81; port coverage rather than
  writing Swift tests, given where this is going.

## Known rough edges

- **Allowance in tokens stays null for a while.** `calibrate` needs 3+
  consecutive sample pairs with a Δpercent ≥ 0.5, which on a quiet account can
  take hours. The percent-per-hour allowance shows immediately; the token and
  dollar figures only appear once calibrated. Consider falling back to a coarser
  estimate (total local tokens ÷ total percent moved this window) so the headline
  isn't blank on day one.
- **`tokens_per_percent` is jumpy between polls**, because it's a median over
  very few pairs. Watching the allowance across two consecutive polls can show it
  moving 2x with nothing else changing, which makes the activity model look
  wrong when it isn't. Worth smoothing across windows before trusting the
  absolute token figures. `runway-cli --profile` and dividing
  `remainingTokens ÷ allowanceTokensPerHour` is the way to check the activity
  side independently of calibration noise.
- ~~Weekly pace ratio is noisy early~~ — fixed by the activity model. Pace no
  longer fits a slope, so it needs no minimum sample span and is honest from the
  first reading. `Severity::of` still damps pace below 15% *calendar* elapsed,
  which is now more conservative than it needs to be.
- **The Swift app is behind on the activity model.** `Sources/` still computes
  pace and run-dry from calendar time. The *widget* is fine — it only renders
  precomputed fields, so it shows whatever the Rust engine wrote — but the Swift
  app's own numbers will disagree with the Tauri app's. Don't chase that as a
  bug; it's the port being ahead.
