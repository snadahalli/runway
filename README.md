# Runway

A macOS menu bar app for Claude plan limits that answers **"what can I still spend?"** instead of "how much have I used?"

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

## Requirements

- macOS 14 or later
- Xcode 16+ (Xcode 26 tested)
- [`xcodegen`](https://github.com/yonaskolb/XcodeGen) — `brew install xcodegen`
- A Claude Pro / Max / Team plan, signed in via Claude Code. The free plan doesn't expose usage data.

## Build

```sh
./build.sh
open .build/Build/Products/Release/Runway.app
```

That's the whole setup. The menu bar app is **not sandboxed** — reading `~/.claude` and the Claude Code keychain item is the entire point, and a sandboxed app can do neither — so it builds ad-hoc signed with no Apple Developer account.

On first launch macOS asks whether Runway may read the `Claude Code-credentials` keychain item. Choose **Always Allow**. Ad-hoc signatures change on every rebuild, so the prompt reappears after each build; a stable signing identity makes it a one-time question.

### With the widget

A widget extension must be sandboxed, and a sandboxed process can only reach the shared snapshot through an App Group, which Apple validates against a provisioning profile. So the widget — and only the widget — needs a signing team:

```sh
DEVELOPMENT_TEAM=ABCDE12345 ./build.sh
```

A free Apple ID works. Nothing else changes: the app already writes its snapshot to the App Group path, so adding a team lights the widget up without moving any data.

## Layout

```
Sources/
  Shared/       compiled into both targets
    Credentials.swift      keychain + file fallback
    UsageAPI.swift         the OAuth usage endpoint
    TranscriptScanner.swift incremental JSONL reader
    Pricing.swift          per-model rate card incl. cache tiers
    Projection.swift       burn rate, calibration, allowance
    Snapshot.swift         the published value + shared storage
    Design.swift           severity rule, window track, sparkline
  App/          menu bar app
  Widget/       WidgetKit extension
```

The app publishes exactly one value — `RunwaySnapshot` — and every surface renders from it. The widget consumes the same struct off disk.

## Accuracy notes

- **Projections need history.** Pace, run-dry time and allowance appear after roughly three API polls (~10 minutes) in a given window. Before that the panel says it's calibrating rather than showing a number it can't stand behind.
- **The ledger is not a bill.** Subscription plans don't charge per token. The dollar figures answer "what would these tokens have cost on the pay-as-you-go API?", which is the only honest way to compare a month of Claude Code against the plan price.
- **Rates are a snapshot.** `Pricing.swift` holds published list prices at time of writing and needs updating when they change. Sonnet 5 introductory pricing is not applied.
- **The 5-hour window rolls.** Old requests age out of it, so its percentage can fall as well as rise. Calibration ignores negative deltas for exactly this reason.

## License

MIT
