import WidgetKit
import SwiftUI

/// A widget that admits when it's stale.
///
/// WidgetKit gives extensions a limited refresh budget, so a widget that polls
/// on its own will silently show old numbers — the single most common complaint
/// about usage widgets. Runway inverts it: the menu bar app owns the polling and
/// pushes reloads, and the widget prints the age of what it's showing. A number
/// you can't trust is worse than a number labelled "8m ago".
struct RunwayEntry: TimelineEntry {
    var date: Date
    var snapshot: RunwaySnapshot
}

struct RunwayProvider: TimelineProvider {
    func placeholder(in context: Context) -> RunwayEntry {
        RunwayEntry(date: Date(), snapshot: .placeholder)
    }

    func getSnapshot(in context: Context, completion: @escaping (RunwayEntry) -> Void) {
        completion(RunwayEntry(date: Date(), snapshot: SnapshotStore.read() ?? .placeholder))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<RunwayEntry>) -> Void) {
        let snapshot = SnapshotStore.read() ?? .placeholder
        let now = Date()

        // Entries every five minutes for the next hour so countdowns and the
        // staleness label keep moving even if no reload arrives. The app calls
        // reloadAllTimelines on every fresh reading, which supersedes these.
        let entries = stride(from: 0, through: 60, by: 5).map { minutes in
            RunwayEntry(date: now.addingTimeInterval(Double(minutes) * 60), snapshot: snapshot)
        }
        completion(Timeline(entries: entries, policy: .after(now.addingTimeInterval(3600))))
    }
}

struct RunwayWidgetView: View {
    @Environment(\.widgetFamily) var family
    var entry: RunwayEntry

    var body: some View {
        Group {
            switch family {
            case .systemSmall: SmallFace(entry: entry)
            default:           MediumFace(entry: entry)
            }
        }
        .containerBackground(.fill.tertiary, for: .widget)
    }
}

private struct SmallFace: View {
    var entry: RunwayEntry

    var body: some View {
        let limit = entry.snapshot.headline
        let severity = limit.map(Severity.of) ?? .calm

        VStack(alignment: .leading, spacing: 6) {
            Text("ALLOWANCE")
                .font(.system(size: 8, weight: .semibold))
                .kerning(0.5)
                .foregroundStyle(.secondary)

            if let limit {
                Text(limit.allowanceTokensPerHour.map { Fmt.tokens($0) + "/h" }
                     ?? limit.allowancePercentPerHour.map { String(format: "%.1f%%/h", $0) }
                     ?? "—")
                    .font(.system(size: 20, weight: .semibold, design: .rounded))
                    .foregroundStyle(severity.color)
                    .minimumScaleFactor(0.6)
                    .lineLimit(1)

                LimitTrack(limit: limit, height: 7)

                HStack(spacing: 3) {
                    Text(Fmt.percent(limit.percent))
                    Text("·")
                    Text(limit.resetsAt.map { Fmt.duration($0.timeIntervalSince(entry.date)) + " left" } ?? "—")
                }
                .font(.system(size: 9))
                .foregroundStyle(.secondary)
                .lineLimit(1)
            } else {
                Text("Open Runway")
                    .font(.system(size: 13, weight: .medium))
                Text(entry.snapshot.message ?? "No data yet")
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 0)
            StalenessLabel(snapshot: entry.snapshot, now: entry.date)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

private struct MediumFace: View {
    var entry: RunwayEntry

    var body: some View {
        let limits = Array(entry.snapshot.limits.prefix(3))

        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                if let headline = entry.snapshot.headline {
                    let severity = Severity.of(headline)
                    Text(headline.allowanceTokensPerHour.map { Fmt.tokens($0) + "/h" } ?? Fmt.percent(headline.percent))
                        .font(.system(size: 22, weight: .semibold, design: .rounded))
                        .foregroundStyle(severity.color)
                    Text("sustainable from now")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                } else {
                    Text("Runway").font(.system(size: 16, weight: .semibold))
                }
                Spacer()
                StalenessLabel(snapshot: entry.snapshot, now: entry.date)
            }

            if limits.isEmpty {
                Text(entry.snapshot.message ?? "Open Runway to connect your Claude login.")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(limits) { limit in
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 4) {
                            Text(limit.label)
                                .font(.system(size: 10))
                                .lineLimit(1)
                            Spacer()
                            if let ratio = limit.paceRatio {
                                Text(Fmt.ratio(ratio))
                                    .font(.system(size: 10, design: .rounded))
                                    .foregroundStyle(ratio > 1 ? Severity.of(limit).color : .secondary)
                            }
                            Text(Fmt.percent(limit.percent))
                                .font(.system(size: 10, weight: .medium, design: .rounded))
                                .monospacedDigit()
                        }
                        LimitTrack(limit: limit, height: 6)
                    }
                }
            }

            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

/// The honesty label. Green while fresh, amber once the data is old enough that
/// a decision based on it could be wrong.
private struct StalenessLabel: View {
    var snapshot: RunwaySnapshot
    var now: Date

    var body: some View {
        let age = now.timeIntervalSince(snapshot.generatedAt)
        let stale = age > 15 * 60

        HStack(spacing: 3) {
            Circle()
                .fill(stale ? Severity.watch.color : Severity.calm.color)
                .frame(width: 5, height: 5)
            Text(age < 90 ? "just now" : "\(Fmt.duration(age)) ago")
                .font(.system(size: 9))
                .foregroundStyle(.secondary)
        }
    }
}

@main
struct RunwayWidgetBundle: WidgetBundle {
    var body: some Widget {
        RunwayWidget()
    }
}

struct RunwayWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "com.sn.runway.widget", provider: RunwayProvider()) { entry in
            RunwayWidgetView(entry: entry)
        }
        .configurationDisplayName("Runway")
        .description("Burn allowance and pace for your Claude plan limits.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}
