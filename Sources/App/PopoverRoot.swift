import SwiftUI

enum Panel: String, CaseIterable, Identifiable {
    case runway, ledger, alarms, settings
    var id: String { rawValue }

    var title: String {
        switch self {
        case .runway: return "Runway"
        case .ledger: return "Ledger"
        case .alarms: return "Alarms"
        case .settings: return "Settings"
        }
    }

    var icon: String {
        switch self {
        case .runway: return "chart.bar.horizontal.page"
        case .ledger: return "list.bullet.rectangle"
        case .alarms: return "bell"
        case .settings: return "gearshape"
        }
    }
}

struct PopoverRoot: View {
    @EnvironmentObject var model: AppModel
    @State private var panel: Panel = .runway

    var body: some View {
        VStack(spacing: 0) {
            HeaderBar(panel: $panel)
            Divider()

            Group {
                switch panel {
                case .runway:   RunwayPanel()
                case .ledger:   LedgerPanel()
                case .alarms:   AlarmsPanel()
                case .settings: SettingsPanel()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)

            Divider()
            FooterBar()
        }
        .frame(width: 380, height: 480)
    }
}

private struct HeaderBar: View {
    @EnvironmentObject var model: AppModel
    @Binding var panel: Panel

    var body: some View {
        HStack(spacing: 8) {
            ForEach(Panel.allCases) { item in
                Button {
                    panel = item
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: item.icon).font(.system(size: 11))
                        if panel == item {
                            Text(item.title).font(.system(size: 11, weight: .medium))
                        }
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                    .background(
                        RoundedRectangle(cornerRadius: 6)
                            .fill(panel == item ? Color.primary.opacity(0.10) : .clear)
                    )
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            Spacer()
            HealthDot()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
    }
}

private struct HealthDot: View {
    @EnvironmentObject var model: AppModel

    private var color: Color {
        switch model.snapshot.health {
        case .live:          return Severity.calm.color
        case .estimated:     return Severity.calm.color.opacity(0.55)
        case .backingOff:    return Severity.watch.color
        case .error:         return Severity.tight.color
        case .noCredentials: return .secondary
        }
    }

    private var text: String {
        switch model.snapshot.health {
        case .live:          return "live"
        case .estimated:     return "estimated"
        case .backingOff:    return "backing off"
        case .error:         return "error"
        case .noCredentials: return "no login"
        }
    }

    var body: some View {
        HStack(spacing: 4) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(text).font(.system(size: 10)).foregroundStyle(.secondary)
        }
        .help(model.snapshot.message ?? "Usage data status")
    }
}

private struct FooterBar: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        HStack(spacing: 8) {
            if let observed = model.snapshot.apiObservedAt {
                Text("API \(Fmt.duration(Date().timeIntervalSince(observed))) ago")
            } else {
                Text("No API reading yet")
            }

            if let next = model.nextPollAt, next > Date() {
                Text("· next in \(Fmt.duration(next.timeIntervalSinceNow))")
            }

            Spacer()

            Button {
                Task { await model.pollNow(force: true) }
            } label: {
                Image(systemName: model.isPolling ? "arrow.trianglehead.2.clockwise" : "arrow.clockwise")
            }
            .buttonStyle(.plain)
            .disabled(model.isPolling)
            .help("Refresh now")

            Button {
                NSApplication.shared.terminate(nil)
            } label: {
                Image(systemName: "power")
            }
            .buttonStyle(.plain)
            .help("Quit Runway")
        }
        .font(.system(size: 10))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
    }
}

// MARK: - Runway panel

struct RunwayPanel: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if model.snapshot.limits.isEmpty {
                    EmptyState()
                } else {
                    Headline()
                    ForEach(model.snapshot.limits) { limit in
                        LimitCard(limit: limit)
                    }
                }
            }
            .padding(12)
        }
    }
}

/// The one sentence the app exists to say.
private struct Headline: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        let limit = model.snapshot.headline
        let severity = limit.map(Severity.of) ?? .calm

        VStack(alignment: .leading, spacing: 6) {
            Text("BURN ALLOWANCE")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(.secondary)
                .kerning(0.6)

            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if let allowance = limit?.allowanceTokensPerHour {
                    Text(Fmt.tokens(allowance))
                        .font(.system(size: 30, weight: .semibold, design: .rounded))
                        .foregroundStyle(severity.color)
                    Text("tokens / hour")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                } else if let allowance = limit?.allowancePercentPerHour {
                    Text(String(format: "%.1f%%", allowance))
                        .font(.system(size: 30, weight: .semibold, design: .rounded))
                        .foregroundStyle(severity.color)
                    Text("/ hour")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                } else {
                    Text("—")
                        .font(.system(size: 30, weight: .semibold, design: .rounded))
                        .foregroundStyle(.secondary)
                }
            }

            Text(sentence)
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 10).fill(severity.color.opacity(0.08)))
    }

    private var sentence: String {
        guard let limit = model.snapshot.headline else {
            return "Waiting for the first reading."
        }
        if limit.allowanceTokensPerHour == nil && limit.paceRatio == nil {
            return "Calibrating against \(limit.label.lowercased()). Projections appear after a few polls."
        }
        if limit.runsDryEarly, let exhausts = limit.exhaustsAt, let resets = limit.resetsAt {
            let early = Fmt.duration(resets.timeIntervalSince(exhausts))
            return "\(limit.label) runs dry around \(Fmt.clock(exhausts)) — \(early) before it resets."
        }
        if let resets = limit.resetsAt {
            return "\(limit.label) is on track to finish at \(Fmt.percent(limit.percent + (limit.paceRatio ?? 0) * (100 - limit.percent))) when it resets \(Fmt.clock(resets))."
        }
        return "\(limit.label) at \(Fmt.percent(limit.percent))."
    }
}

private struct LimitCard: View {
    @EnvironmentObject var model: AppModel
    var limit: LimitSnapshot

    var body: some View {
        let severity = Severity.of(limit)

        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Text(limit.label)
                    .font(.system(size: 12, weight: .medium))
                if limit.isActive {
                    Text("BINDING")
                        .font(.system(size: 8, weight: .bold))
                        .padding(.horizontal, 4).padding(.vertical, 1)
                        .background(RoundedRectangle(cornerRadius: 3).fill(severity.color.opacity(0.18)))
                        .foregroundStyle(severity.color)
                }
                Spacer()
                Text(Fmt.percent(limit.percent))
                    .font(.system(size: 12, weight: .semibold, design: .rounded))
                    .monospacedDigit()
                    .foregroundStyle(severity.color)
            }

            LimitTrack(limit: limit)

            HStack(spacing: 0) {
                Stat("Pace", limit.paceRatio.map(Fmt.ratio) ?? "—",
                     tint: (limit.paceRatio ?? 0) > 1 ? severity.color : nil)
                Stat("Resets", limit.resetsAt.map { Fmt.duration($0.timeIntervalSinceNow) } ?? "—")
                Stat("Left", limit.remainingTokens.map(Fmt.tokens) ?? "—")
                Stat("Worth", limit.remainingValueUSD.map(Fmt.usd) ?? "—")
            }

            Sparkline(samples: model.series(for: limit), color: severity.color)
                .frame(height: 22)
        }
        .padding(11)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.primary.opacity(0.04)))
    }
}

private struct EmptyState: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("No usage data yet")
                .font(.system(size: 13, weight: .medium))
            Text(model.snapshot.message ?? "Runway reads the same credentials Claude Code uses. If you've never signed in, run `claude` once in a terminal.")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button("Try again") { Task { await model.pollNow(force: true) } }
                .controlSize(.small)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.primary.opacity(0.04)))
    }
}
