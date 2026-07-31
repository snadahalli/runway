//! A terminal front end for the same engine the desktop app uses.
//!
//! Useful in its own right over SSH, and useful as a way to check the engine's
//! numbers without any UI in the way:
//!
//! ```text
//! runway-cli              one reading, human readable
//! runway-cli --json       one reading, the exact snapshot the app publishes
//! runway-cli --watch      stay running, refreshing on the normal cadences
//! ```

use std::io::Write;

use runway_core::format as fmt;
use runway_core::severity::Severity;
use runway_core::{Engine, EngineConfig, EngineHandle, RunwaySnapshot, SnapshotHealth};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let watch = args.iter().any(|a| a == "--watch");

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("runway-cli [--json] [--watch] [--profile]");
        return;
    }

    if args.iter().any(|a| a == "--profile") {
        print_profile();
        return;
    }

    if watch {
        // Held for the process lifetime: dropping it would stop the engine thread.
        let _handle = EngineHandle::spawn(EngineConfig::default(), move |snapshot, alarms| {
            if json {
                if let Ok(s) = serde_json::to_string(snapshot) {
                    println!("{s}");
                }
            } else {
                // Redraw in place rather than scrolling a wall of text.
                print!("\x1b[2J\x1b[H");
                print!("{}", render(snapshot));
                let _ = std::io::stdout().flush();
            }
            for alarm in alarms {
                eprintln!("[alarm] {} — {}", alarm.title, alarm.body);
            }
        });

        // Ctrl-C is the exit path; park until then.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    let mut engine = Engine::new(EngineConfig::default());
    engine.bootstrap();
    engine.poll_now(true);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&engine.snapshot).unwrap_or_default()
        );
    } else {
        print!("{}", render(&engine.snapshot));
        if let Some(error) = &engine.last_error {
            eprintln!("\n{error}");
        }
    }
}

/// Dumps the learned working-hours profile. Pace, run-dry and the allowance are
/// all derived from this, so being able to see it — and check it against what
/// you know about your own week — is the difference between a model you can
/// trust and one you can't.
fn print_profile() {
    let mut engine = Engine::new(EngineConfig::default());
    engine.bootstrap();
    let profile = engine.activity();
    let weights = profile.weights();
    let now = chrono::Utc::now();

    println!(
        "profile: {}",
        if profile.learned {
            "learned from your logs"
        } else {
            "uniform (not enough history)"
        }
    );
    println!();

    let days = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let peak = weights.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
    print!("     ");
    for hour in 0..24 {
        print!(
            "{}",
            if hour % 6 == 0 {
                format!("{hour:<2}")
            } else {
                "  ".into()
            }
        );
    }
    println!();
    for (index, day) in days.iter().enumerate() {
        print!("{day}  ");
        for hour in 0..24 {
            let v = weights[index * 24 + hour] / peak;
            let shade = match (v * 5.0).round() as i32 {
                0 => "  ",
                1 => "..",
                2 => "::",
                3 => "##",
                _ => "██",
            };
            print!("{shade}");
        }
        println!();
    }
    println!();

    // The headline consequence: windows are measured in the left-hand number,
    // not the right-hand one.
    let week = profile.active_hours_between(now, now + chrono::Duration::days(7));
    println!("next 7 days hold {week:.0} working hours, against 168 calendar hours");
    if profile.learned {
        println!(
            "allowances are per working hour, so ~{:.1}x a calendar-hour average",
            168.0 / week.max(1e-9)
        );
    }
}

fn render(snapshot: &RunwaySnapshot) -> String {
    let now = chrono::Utc::now();
    let mut out = String::new();

    let health = match snapshot.health {
        SnapshotHealth::Live => "live",
        SnapshotHealth::Estimated => "estimated",
        SnapshotHealth::BackingOff => "backing off",
        SnapshotHealth::Error => "error",
        SnapshotHealth::NoCredentials => "no credentials",
    };

    match snapshot.headline() {
        Some(headline) => {
            let value = headline
                .allowance_tokens_per_hour
                .map(|t| format!("{} tokens / hour", fmt::tokens(t)))
                .or_else(|| {
                    headline
                        .allowance_percent_per_hour
                        .map(|p| format!("{p:.1}% / hour"))
                })
                .unwrap_or_else(|| "calibrating".into());

            out.push_str(&format!("BURN ALLOWANCE\n{value}\n"));

            if let (Some(exhausts), Some(resets)) = (headline.exhausts_at, headline.resets_at) {
                if headline.runs_dry_early() {
                    let early = (resets - exhausts).num_milliseconds() as f64 / 1000.0;
                    out.push_str(&format!(
                        "{} runs dry around {} — {} before it resets.\n",
                        headline.label,
                        fmt::clock(exhausts, now),
                        fmt::duration(early)
                    ));
                }
            }
        }
        None => {
            out.push_str("BURN ALLOWANCE\nno data yet\n");
        }
    }

    out.push_str(&format!(
        "\n{:<24} {:>7} {:>7} {:>10} {:>12}\n",
        "LIMIT", "USED", "PACE", "RESETS", "ALLOWANCE"
    ));

    for limit in &snapshot.limits {
        let marker = match Severity::of(limit, now) {
            Severity::Calm => " ",
            Severity::Watch => "!",
            Severity::Tight => "!!",
        };
        out.push_str(&format!(
            "{:<24} {:>7} {:>7} {:>10} {:>12} {}\n",
            truncate(&limit.label, 24),
            fmt::percent(limit.percent),
            limit
                .pace_ratio
                .map(fmt::ratio)
                .unwrap_or_else(|| "—".into()),
            limit
                .time_remaining(now)
                .map(fmt::duration)
                .unwrap_or_else(|| "—".into()),
            limit
                .allowance_tokens_per_hour
                .map(|t| format!("{}/h", fmt::tokens(t)))
                .unwrap_or_else(|| "—".into()),
            marker
        ));
    }

    let ledger = &snapshot.ledger;
    out.push_str(&format!(
        "\n{} · {} tokens · {} API-equivalent\n",
        ledger.window_label,
        fmt::tokens(ledger.tokens.billable() as f64),
        fmt::usd(ledger.cost_usd)
    ));
    for project in ledger.top_projects.iter().take(5) {
        out.push_str(&format!(
            "  {:<24} {:>9} {:>9}\n",
            truncate(&project.name, 24),
            fmt::tokens(project.tokens as f64),
            fmt::usd(project.cost_usd)
        ));
    }

    out.push_str(&format!(
        "\n{health} · updated {} ago",
        fmt::duration(snapshot.age_seconds(now))
    ));
    if let Some(message) = &snapshot.message {
        out.push_str(&format!("\n{message}"));
    }
    out.push('\n');
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
