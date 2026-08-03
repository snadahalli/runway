# Runway

A menu bar / system tray app for Claude plan limits that answers **"what can I still spend?"** instead of "how much have I used?"


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
- **Desktop panel** — an always-on-top glance surface that states the age of its own data on its face
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


### Staying accurate without getting rate limited

The API is polled at its documented 180-second floor, with `retry-after` honoured and exponential backoff to 15 minutes on failure. In between, Runway extrapolates each limit forward from the last API reading using local token volume and the calibrated tokens-per-percent — so the display keeps moving without spending requests. The header says `live` when the number came from the API and `estimated` when it's extrapolated; the estimate is clamped so it can never run backwards or overshoot 100%.

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

### First run

**Windows hides new tray icons.** After installing, click the **^** arrow to the
left of the taskbar clock and drag Runway out onto the taskbar so it stays
visible. Runway will show its window once on first launch so you know it
started. Launching it again just brings that window back — it will not start a
second copy.

An icon appears in the menu bar (macOS) or the system tray (Windows, Linux).

- **Left-click** — the popover: limits, ledger, alarms, settings
- **Right-click** — refresh, toggle the desktop panel, quit
- **Desktop panel** — an always-on-top glance surface. Drag it anywhere; it remembers where you put it.

The pace ratio is meaningful immediately. The **token and dollar figures take a few hours** to appear — turning an opaque percentage into tokens needs several consecutive API readings that actually moved. Until then those fields read `—` rather than a number Runway can't stand behind.

### Updating

Runway checks for a new version an hour after launch and daily after that, and
offers it in the popover — **Install and restart**. There's also *Check for
updates…* in the tray menu. Nothing installs without you clicking.

Every update is signed with a [minisign](https://jedisct1.github.io/minisign/)
key whose public half is compiled into the app; the updater refuses anything
that doesn't verify. That's Tauri's own signing, unrelated to Apple notarisation
or an Authenticode certificate — which is why updates are cryptographically
verified even though first install still trips Gatekeeper and SmartScreen.

Updates on macOS skip the quarantine flag, so `xattr` is a first-install-only
chore. On Windows the installer runs again, so SmartScreen may reappear. The
`.deb` cannot self-update — use the `.AppImage` on Linux if you want updates,
or `apt install` the new file.

**Versions before 0.1.3 have no updater**, so upgrading to 0.1.3 is a manual
download. After that it's self-service.

### Credentials

Runway never mints or refreshes tokens — Claude Code owns that lifecycle. It re-reads the credential store on every poll, so a refresh performed by the CLI is picked up on the next tick. Nothing is ever written back.

| | Where Runway reads your token |
|---|---|
| macOS | login keychain, the `Claude Code-credentials` item. First run raises a keychain prompt — choose **Always Allow**. Falls back to the file below if the keychain is unavailable. |
| Windows | `%USERPROFILE%\.claude\.credentials.json` |
| Linux | `~/.claude/.credentials.json` |

Usage history comes from `~/.claude/projects/**/*.jsonl` (`%USERPROFILE%\.claude\projects\` on Windows), read incrementally and never modified.

**`CLAUDE_CONFIG_DIR`** is honoured. Set it and Runway looks for both the credentials file and the transcripts under that directory instead, matching Claude Code's own behaviour.

### Where Runway keeps its own files

| | |
|---|---|
| macOS | `~/Library/Application Support/Runway/` |
| Windows | `%APPDATA%\Runway\` |
| Linux | `~/.local/share/runway/` |

- `settings.json` — preferences; editable by hand, reloaded on restart
- `samples.json`, `scan-state.json` — sample history and the transcript read cursor. Delete these to reset calibration; you'll wait a few hours for it to rebuild.
- `runway-snapshot.json` — the current snapshot, rewritten every 15 seconds. A supported integration point: read it from a status bar or script. camelCase fields, whole-second RFC 3339 dates.
- `runway.log` — warnings and errors, capped at 512KB, opening with version, OS and paths

### If something looks wrong

Check `runway.log` first. Common cases:

- **"No Claude Code credentials found"** — run `claude` and sign in.
- **Keychain access denied (macOS)** — Runway was rebuilt and its ad-hoc signature changed, so macOS treats it as a different program. Answer the prompt again, or allow the `Claude Code-credentials` item in Keychain Access.
- **Everything reads `—`** — normal for the first few hours; calibration hasn't converged. `runway-cli --profile` shows what it has learned so far.
- **Rate limited** — Runway polls at the documented 180s floor and backs off to 15 minutes on failure. If you're seeing persistent 429s, something else is using the same token.

## Requirements

A Claude Pro / Max / Team plan, signed in via Claude Code. The free plan doesn't expose usage data.

[Rust](https://rustup.rs) 1.77+, and:

- **Windows**: Visual Studio Build Tools with the C++ workload. WebView2 ships with Windows 11 and current Windows 10.
- **macOS**: the Xcode command line tools (`xcode-select --install`). WebKit is already there.
- **Linux**: `webkit2gtk-4.1`, `libayatana-appindicator3`, `librsvg2` (names vary by distro).

No npm, no bundler, no Node — the frontend is static files.

## Build

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
- **There's no widget surface.** The Windows Widgets Board needs an MSIX-packaged Windows App SDK provider, which is a whole distribution story. The glance surface is the **desktop panel** instead — a frameless always-on-top window you can drag anywhere, toggled from the tray menu, which prints the age of its own data on its face.

Credentials are actually *simpler* on Windows: Claude Code stores them as plain JSON at `%USERPROFILE%\.claude\.credentials.json`, so there's no keychain prompt at all.

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
    activity.rs            the learned working-hours profile
    compat.rs              reads state written by the original Swift build
  src/bin/cli.rs         terminal front end

app/                     the cross-platform shell
  src/                     static HTML/CSS/JS — popover and desktop panel
  src-tauri/               tray, windows, notifications

```

Everything publishes exactly one value — `RunwaySnapshot` — and every surface renders from it. It's also written to disk as `runway-snapshot.json` on every update, which is a supported integration point: status bars, scripts and dashboards can read it without touching this codebase. Field names are camelCase and dates are whole-second RFC 3339.

## Accuracy notes

- **The token and dollar figures need history.** Pace appears immediately, because it's measured against your working-hours profile rather than fitted to a slope. But turning percentages into tokens needs several consecutive API readings that moved, which on a quiet account can take hours. Until then the panel says it's calibrating rather than showing a number it can't stand behind.
- **The ledger is not a bill.** Subscription plans don't charge per token. The dollar figures answer "what would these tokens have cost on the pay-as-you-go API?", which is the only honest way to compare a month of Claude Code against the plan price.
- **Rates are a snapshot.** `core/src/pricing.rs` holds published list prices at time of writing and needs updating when they change. Sonnet 5 introductory pricing is not applied.
- **The 5-hour window rolls.** Old requests age out of it, so its percentage can fall as well as rise. Calibration ignores negative deltas for exactly this reason.

## Releasing

```sh
git tag v1.0.0 && git push origin v1.0.0
```

That triggers `.github/workflows/release.yml`, which builds a universal macOS `.dmg`, a Windows `.msi` and `-setup.exe`, and a Linux `.AppImage` and `.deb`, then attaches them to a GitHub Release with the install notes above. `.github/workflows/ci.yml` runs the tests and builds the app on all three platforms for every push.

Nothing is code-signed. If that ever changes — a paid Apple Developer account for notarisation, a purchased Windows certificate — `release.yml` is the only file that needs to know.

## License

MIT — see [LICENSE](LICENSE).
