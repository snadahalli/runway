# Runway

A menu bar / system tray app for Claude plan limits that answers **"what can I still spend?"** instead of "how much have I used?"

> **Two implementations right now.** `app/` is the cross-platform one (Rust + Tauri, runs on macOS, Windows and Linux) and is where the project is going. `Sources/` is the original macOS-only SwiftUI app, kept until the port reaches parity. Both drive the same engine logic and write the same `runway-snapshot.json`, so the macOS Notification Centre widget works with either. See [Which one do I build?](#which-one-do-i-build).

Most usage monitors show you a percentage. A percentage tells you where you've been. Runway's headline number is a **burn allowance** — the tokens per hour you can sustain from right now until the window resets and land exactly at 100%. The menu bar shows a **pace ratio**: `1.0×` means perfectly paced, `2.4×` means you'll run dry less than halfway through the window.

```
BURN ALLOWANCE
418K tokens / hour
Weekly · Fable runs dry around Thu 14:20 — 2d 6h before it resets.
```

## What it does

- **Burn allowance** — tokens/hour and dollars of headroom you can still spend, per limit
- **Pace ratio** — current burn ÷ sustainable burn, so you know whether to change what you're doing
- **Predictive alarms** — "this limit will run dry Thursday afternoon", not just "you hit 80%"
- **Value ledger** — what the subscription actually bought, priced against pay-as-you-go API rates, broken down per repo, per model, per session
- **Notification Centre widget** that states the age of its own data on its face
- Everything local. No servers, no telemetry, no account.

## Why the numbers are different from other trackers

**Cache tokens are priced correctly.** In a long Claude Code session, cache reads are the overwhelming majority of tokens — on this machine, 366M of 375M billable tokens over a week. They cost **0.1×** input rate. Cache writes cost 1.25× (5-minute TTL) or 2× (1-hour TTL), and Runway reads the TTL split out of the logs rather than lumping them together. Pricing all of that at full input rate — which is the easy mistake — overstates value by roughly an order of magnitude.

**Percentages become tokens.** The API reports plan usage as an opaque percentage. Runway pairs consecutive API readings with the token volume recorded locally in between and takes the **median** ratio, giving a calibrated "tokens per percentage point". That's what turns `8% remaining` into `418K tokens` and `$31 of headroom`.

**Windows are measured in working time, not calendar time.** `remaining ÷ hours until reset` assumes you burn tokens evenly through nights and weekends. Nobody does. Runway buckets your transcript history into an hour-of-week profile and measures a window by how much of *your* typical week's work has gone by — so pace is `spent ÷ expected-by-now`, run-dry lands in a working hour instead of at 3am on a Sunday, and the allowance is per working hour.

The difference isn't cosmetic. On the machine this was developed on, 90% of tokens fell in 30 of the 168 hour-of-week slots. The flat model reported a **2.45× pace and "runs dry three days early"** when the honest figure was **0.28× and comfortably fine** — a false alarm, from fitting a slope to a working afternoon and extrapolating it across a calendar week. `runway-cli --profile` prints what it learned about you.

With no history to learn from, the profile is uniform and every formula reduces exactly to the calendar version.

**Severity isn't just fullness.** A limit at 60% with eight hours left is fine; the same 60% with forty minutes left and a 3× pace is not. Severity is the worse of two independent readings — how full, and how fast — with the pace reading damped early in a window where a couple of heavy minutes would project absurd slopes.

## Data sources

| Source | Provides | Cost |
|---|---|---|
| `GET /api/oauth/usage` with your Claude Code OAuth token | Authoritative **% of plan limits** and reset times — the same numbers as `/usage`. Account-wide, so it covers web, desktop and mobile too. | Rate limited; polled at a 180s floor |
| `~/.claude/projects/**/*.jsonl` | Per-token, per-model, per-project, per-session detail | Free, read incrementally |

Two details about the API that are easy to get wrong and are the usual reason monitors like this break:

1. OAuth tokens go on `Authorization: Bearer`, with `anthropic-beta: oauth-2025-04-20`. Not `x-api-key`.
2. **`User-Agent: claude-code/<version>` is required.** Without a matching User-Agent the endpoint drops you into a far stricter rate-limit bucket and returns persistent 429s. Runway reads the version out of your own transcript logs rather than shelling out to `claude`.

Runway never mints or refreshes tokens — Claude Code owns that lifecycle. Credentials are re-read from the keychain on every poll, so a refresh performed by the CLI is picked up automatically.

### Staying accurate without getting rate limited

The API is polled at its documented 180-second floor, with `retry-after` honoured and exponential backoff to 15 minutes on failure. In between, Runway extrapolates each limit forward from the last API reading using local token volume and the calibrated tokens-per-percent — so the display keeps moving without spending requests. The header says `live` when the number came from the API and `estimated` when it's extrapolated; the estimate is clamped so it can never run backwards or overshoot 100%.

## Which one do I build?

| | `app/` — Rust + Tauri | `Sources/` — Swift |
|---|---|---|
| macOS | yes | yes |
| Windows | yes | no |
| Linux | yes | no |
| Menu bar readout | text (macOS) / painted icon (Windows, Linux) | text |
| Glanceable surface | always-on-top desktop panel | Notification Centre widget **and** the panel, via the Swift widget |
| Build | `cargo run -p runway-app` | `./build.sh --install` |
| Toolchain | Rust | Xcode + xcodegen |

On Windows and Linux the Tauri app is the only option. On macOS, build the Swift app if you specifically want the Notification Centre widget; otherwise the Tauri app is the same product with a desktop panel instead.

They share the snapshot file, so **you can run the Tauri app and keep the Swift widget** — the widget just renders whatever last wrote `runway-snapshot.json`. Don't run both *apps* at once, though: they'd fight over the same state files.

## Install

Download the latest build from [Releases](../../releases). You need a Claude Pro / Max / Team plan signed in via Claude Code — run `claude` once if you never have, so Runway has credentials to read. The free plan doesn't expose usage data.

**macOS** — open the `.dmg`, drag Runway to Applications, then run this once:

```sh
xattr -dr com.apple.quarantine /Applications/Runway.app
```

Skip that and macOS says **"Runway is damaged and can't be opened"**. It isn't damaged. macOS quarantines everything downloaded from a browser, and for an app that isn't *notarised* it reports the quarantine as damage rather than saying so. Notarising requires a paid Apple Developer account, which this project doesn't have; the command clears the flag.

**Windows** — run the `-setup.exe`. SmartScreen will show *"Windows protected your PC"* because the installer isn't code-signed (a certificate costs a few hundred a year). Click **More info → Run anyway**.

**Linux** — `chmod +x` the `.AppImage` and run it, or install the `.deb`.

If you'd rather not click through a security warning, build from source — it's one command and there's no warning to click.

## Requirements

Common to both: a Claude Pro / Max / Team plan, signed in via Claude Code. The free plan doesn't expose usage data.

**Cross-platform app** — [Rust](https://rustup.rs) 1.77+, and:

- **Windows**: Visual Studio Build Tools with the C++ workload. WebView2 ships with Windows 11 and current Windows 10.
- **macOS**: the Xcode command line tools (`xcode-select --install`). WebKit is already there.
- **Linux**: `webkit2gtk-4.1`, `libayatana-appindicator3`, `librsvg2` (names vary by distro).

No npm, no bundler, no Node — the frontend is static files.

**macOS Swift app** — macOS 14+, Xcode 16+ (26 tested), and [`xcodegen`](https://github.com/yonaskolb/XcodeGen) (`brew install xcodegen`). No Apple Developer account is required, for the app or the widget.

## Build — cross-platform

```sh
cargo run -p runway-app
```

That's the whole thing on every platform: clone, build, run. It puts an icon in the menu bar / tray; left-click opens the popover, right-click opens the menu.

For an installable bundle (`.app`/`.dmg`, `.msi`/`.exe`, `.deb`/`.AppImage`):

```sh
cargo install tauri-cli --version "^2"
cargo tauri build
```

Bundles built this way are only for the platform you're on — Tauri can't cross-compile a Windows installer from macOS, because it needs the MSVC toolchain and WebView2. That's what `.github/workflows/release.yml` is for: push a tag and GitHub builds all three.

There's also a terminal front end over the same engine, which is handy over SSH:

```sh
cargo run --bin runway-cli -- --watch     # live dashboard
cargo run --bin runway-cli -- --json      # the exact snapshot the app publishes
```

### What differs on Windows

Two things, both forced by the platform rather than chosen:

- **The tray can't show text.** `Shell_NotifyIcon` takes an icon and a tooltip, nothing else. So on Windows the readout is *painted into* the icon by a small bitmap font built for the size (`app/src-tauri/src/tray_icon.rs`), with the full detail in the tooltip. On macOS the status item carries real text, as before.
- **There's no widget.** The Windows Widgets Board needs an MSIX-packaged Windows App SDK provider, which is a whole distribution story. The stand-in is the **desktop panel** — a frameless always-on-top window, on by default from the tray menu. Like the widget, it prints the age of its own data on its face.

Credentials are actually *simpler* on Windows: Claude Code stores them as plain JSON at `%USERPROFILE%\.claude\.credentials.json`, so there's no keychain prompt at all.

## Build — macOS Swift app

```sh
./build.sh --install
```

That's the whole setup — clone, build, run. **No Apple Developer account, paid or free.** Both the menu bar app and the Notification Centre widget build ad-hoc signed.

`--install` copies the app to `/Applications` and launches it. That matters for the widget: macOS only registers an app extension once the containing app has run from a location it scans, and launching straight out of `.build` doesn't count. Drop the flag to build in place.

On first launch macOS asks whether Runway may read the `Claude Code-credentials` keychain item. Choose **Always Allow**. Ad-hoc signatures change on every rebuild, so the prompt reappears after each build — see *A stable identity* below if that gets old.

### The widget, and why it needs no team

A widget extension must be sandboxed, and a sandboxed process can only reach the shared snapshot through an App Group. The usual assumption is that App Groups require a provisioning profile, and therefore a signing team. They don't: **macOS honours `com.apple.security.application-groups` on an ad-hoc signature.** A sandboxed ad-hoc bundle gets its container at `~/Library/Group Containers/group.com.sn.runway` and can read and write there, while access to anything outside is still correctly refused.

What genuinely blocks is Xcode's *build system*, which refuses to sign a target carrying any entitlement unless it can resolve a profile. `build.sh` sidesteps that: it asks `xcodebuild` for an unsigned bundle, then codesigns the widget and the app itself, inside out, with the entitlements applied directly.

After `./build.sh --install`, add it from **right-click the desktop → Edit Widgets → Runway**.

If you *do* have a signing team, `DEVELOPMENT_TEAM=ABCDE12345 ./build.sh` uses Xcode-managed signing instead. Nothing about the app changes; you get a stable identity out of it.

### A stable identity

Ad-hoc signatures are regenerated on every build, so macOS treats each build as a different program and re-asks for keychain access. A self-signed certificate fixes that without any Apple account:

1. **Keychain Access → Certificate Assistant → Create a Certificate…**
2. Name it `Runway Dev`, Identity Type *Self Signed Root*, Certificate Type **Code Signing**.
3. Build with `SIGN_IDENTITY="Runway Dev" ./build.sh --install`.

Now "Always Allow" sticks across rebuilds.

## Layout

```
core/                    the engine, no UI, no platform assumptions
  src/
    credentials.rs         keychain (macOS) / plain file (everywhere)
    usage_api.rs           the OAuth usage endpoint
    transcript.rs          incremental JSONL reader
    pricing.rs             per-model rate card incl. cache tiers
    projection.rs          burn rate, calibration, allowance
    severity.rs            the severity rule
    readout.rs             the one-line menu bar / tray text
    snapshot.rs            the published value + shared storage
    engine.rs              poll loop, scan loop, snapshot assembly
    alarms.rs              threshold / predictive / pace rules
    compat.rs              reads state files the Swift app left behind
  src/bin/cli.rs         terminal front end

app/                     the cross-platform shell
  src/                     static HTML/CSS/JS — popover and desktop panel
  src-tauri/               tray, windows, notifications

Sources/                 the original macOS app
  Shared/  App/  Widget/   Swift; Widget/ is still the only real widget
```

Everything publishes exactly one value — `RunwaySnapshot` — and every surface renders from it. The macOS widget consumes the same struct off disk, which is why the Rust engine encodes dates the way `JSONEncoder.dateEncodingStrategy = .iso8601` does: whole seconds, `Z` suffix, no fractional part. Swift's decoder rejects anything else.

## Accuracy notes

- **Projections need history.** Pace, run-dry time and allowance appear after roughly three API polls (~10 minutes) in a given window. Before that the panel says it's calibrating rather than showing a number it can't stand behind.
- **The ledger is not a bill.** Subscription plans don't charge per token. The dollar figures answer "what would these tokens have cost on the pay-as-you-go API?", which is the only honest way to compare a month of Claude Code against the plan price.
- **Rates are a snapshot.** `Pricing.swift` holds published list prices at time of writing and needs updating when they change. Sonnet 5 introductory pricing is not applied.
- **The 5-hour window rolls.** Old requests age out of it, so its percentage can fall as well as rise. Calibration ignores negative deltas for exactly this reason.

## Releasing

```sh
git tag v1.0.0 && git push origin v1.0.0
```

That triggers `.github/workflows/release.yml`, which builds a universal macOS `.dmg`, a Windows `.msi` and `-setup.exe`, and a Linux `.AppImage` and `.deb`, then attaches them to a GitHub Release with the install notes above. `.github/workflows/ci.yml` runs the tests and builds the app on all three platforms for every push.

Nothing is code-signed. If that ever changes — a paid Apple Developer account for notarisation, a purchased Windows certificate — `release.yml` is the only file that needs to know.

## License

MIT — see [LICENSE](LICENSE).
