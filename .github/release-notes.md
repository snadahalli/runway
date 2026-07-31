## Install

**macOS** — open the `.dmg`, drag Runway to Applications, then run this once:

```sh
xattr -dr com.apple.quarantine /Applications/Runway.app
```

Without it macOS says *"Runway is damaged and can't be opened"*. It isn't. macOS
quarantines everything downloaded from a browser, and for an app that isn't
notarised it reports the quarantine as damage. Notarising needs a paid Apple
Developer account, which this project doesn't have.

**Windows** — run the `-setup.exe`. SmartScreen will say *"Windows protected
your PC"* because the installer isn't code-signed. Click **More info → Run
anyway**.

**Linux** — `chmod +x` the `.AppImage` and run it, or install the `.deb`.

## Before it can show you anything

Runway reads the credentials Claude Code already stored. **Run `claude` once and
sign in** if you never have — a Pro, Max or Team plan. The free plan doesn't
expose usage data.

Nothing is sent anywhere. Runway talks to Anthropic's usage endpoint with your
existing token and reads your local logs; there is no server, no telemetry and
no account.

## First run

**Windows hides new tray icons.** After installing, click the **^** arrow to the
left of the taskbar clock and drag Runway out onto the taskbar so it stays
visible. Runway will show its window once on first launch so you know it
started. Launching it again just brings that window back — it will not start a
second copy.

An icon appears in the menu bar (macOS) or the system tray (Windows, Linux).

- **Left-click** — the popover: limits, ledger, alarms, settings
- **Right-click** — refresh, toggle the desktop panel, quit
- **Desktop panel** — an always-on-top glance surface. Drag it anywhere; it
  remembers where you put it.

The pace ratio is meaningful straight away. The **token and dollar figures need
a few hours** — turning an opaque percentage into tokens requires several
consecutive API readings that actually moved. Until then those fields read `—`
rather than showing a number that can't be stood behind.

## If something looks wrong

`runway.log` is in the state directory, and opens with version, OS and paths:

| | |
|---|---|
| macOS | `~/Library/Application Support/Runway/` |
| Windows | `%APPDATA%\Runway\` |
| Linux | `~/.local/share/runway/` |

## Known unknowns on Windows and Linux

This build compiles and passes its tests on all three platforms in CI, but it
has only been *run* on macOS. Most likely to be wrong:

- the number painted into the tray icon, especially at 125%/150% display scaling
- window transparency and rounded corners under WebView2
- the popover's position when the taskbar is at the bottom of the screen

Reports welcome, with the log attached.
