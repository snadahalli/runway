import SwiftUI

// MARK: - Ledger

/// What the subscription actually bought, priced against pay-as-you-go rates.
struct LedgerPanel: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        let ledger = model.snapshot.ledger

        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("API-EQUIVALENT VALUE")
                        .font(.system(size: 9, weight: .semibold))
                        .kerning(0.6)
                        .foregroundStyle(.secondary)
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(Fmt.usd(ledger.costUSD))
                            .font(.system(size: 28, weight: .semibold, design: .rounded))
                        Text(ledger.windowLabel.lowercased())
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                    if let month = model.snapshot.monthlyValueUSD {
                        Text("\(Fmt.usd(month)) over 30 days · not a bill, this is what the same tokens would cost on the API")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .background(RoundedRectangle(cornerRadius: 10).fill(Color.primary.opacity(0.05)))

                TokenSplit(tokens: ledger.tokens)

                if !ledger.topProjects.isEmpty {
                    BreakdownList(title: "By project", entries: ledger.topProjects, total: ledger.costUSD)
                }
                if !ledger.topModels.isEmpty {
                    BreakdownList(title: "By model", entries: ledger.topModels, total: ledger.costUSD)
                }

                Text("Read from ~/.claude/projects — never leaves this Mac.")
                    .font(.system(size: 9))
                    .foregroundStyle(.tertiary)
            }
            .padding(12)
        }
    }
}

/// Cache reads dominate token counts in Claude Code but cost a tenth of input,
/// so showing the split is the difference between a useful number and a scary one.
private struct TokenSplit: View {
    var tokens: TokenTotals

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Token mix").font(.system(size: 11, weight: .medium))
            HStack(spacing: 0) {
                Stat("Input", Fmt.tokens(tokens.input))
                Stat("Output", Fmt.tokens(tokens.output))
                Stat("Cache write", Fmt.tokens(tokens.cacheWrite5m + tokens.cacheWrite1h))
                Stat("Cache read", Fmt.tokens(tokens.cacheRead))
            }
            Text("\(Fmt.tokens(tokens.billable)) billable · \(Fmt.tokens(tokens.fresh)) new work")
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
        }
        .padding(11)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.primary.opacity(0.04)))
    }
}

private struct BreakdownList: View {
    var title: String
    var entries: [LedgerSummary.Entry]
    var total: Double

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title).font(.system(size: 11, weight: .medium))
            ForEach(entries) { entry in
                VStack(alignment: .leading, spacing: 3) {
                    HStack {
                        Text(entry.name)
                            .font(.system(size: 11))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                        Text(Fmt.usd(entry.costUSD))
                            .font(.system(size: 11, design: .rounded))
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                    }
                    GeometryReader { geo in
                        Capsule()
                            .fill(Color.accentColor.opacity(0.55))
                            .frame(width: geo.size.width * (total > 0 ? min(1, entry.costUSD / total) : 0))
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(height: 4)
                }
            }
        }
        .padding(11)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.primary.opacity(0.04)))
    }
}

// MARK: - Alarms

struct AlarmsPanel: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        AlarmsPanelBody(settings: model.settings, alarms: model.alarms)
    }
}

private struct AlarmsPanelBody: View {
    @ObservedObject var settings: Settings
    @ObservedObject var alarms: AlarmEngine

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Toggle("Alarms enabled", isOn: $settings.alarmsEnabled)
                    .font(.system(size: 12))

                Group {
                    SectionCard("Thresholds") {
                        Text("Fire once per window when a limit crosses these marks.")
                            .font(.system(size: 10)).foregroundStyle(.secondary)
                        ThresholdEditor(thresholds: $settings.thresholds)
                    }

                    SectionCard("Predictive") {
                        Toggle("Warn when a limit will run dry before it resets", isOn: $settings.predictiveAlarms)
                            .font(.system(size: 11))
                        HStack {
                            Text("Pace alarm at").font(.system(size: 11))
                            Stepper(
                                value: $settings.paceAlarmRatio, in: 1.2...6, step: 0.1
                            ) {
                                Text(Fmt.ratio(settings.paceAlarmRatio))
                                    .font(.system(size: 11, design: .rounded))
                                    .monospacedDigit()
                            }
                            Text("sustainable").font(.system(size: 11)).foregroundStyle(.secondary)
                        }
                    }

                    SectionCard("Quiet hours") {
                        Toggle("Suppress delivery overnight", isOn: $settings.quietHoursEnabled)
                            .font(.system(size: 11))
                        HStack(spacing: 6) {
                            Text("From").font(.system(size: 11))
                            HourPicker(hour: $settings.quietStartHour)
                            Text("to").font(.system(size: 11))
                            HourPicker(hour: $settings.quietEndHour)
                        }
                        .disabled(!settings.quietHoursEnabled)
                        Text("Alarms still register in the log below; only the banner is held back.")
                            .font(.system(size: 10)).foregroundStyle(.secondary)
                    }
                }
                .disabled(!settings.alarmsEnabled)

                HStack {
                    if alarms.authorization != .authorized {
                        Label("Notifications not authorized", systemImage: "exclamationmark.triangle")
                            .font(.system(size: 10))
                            .foregroundStyle(Severity.watch.color)
                    }
                    Spacer()
                    Button("Send test") { alarms.testNotification() }
                        .controlSize(.small)
                }

                if !alarms.recent.isEmpty {
                    SectionCard("Recent") {
                        ForEach(alarms.recent.prefix(8)) { fired in
                            VStack(alignment: .leading, spacing: 1) {
                                HStack {
                                    Text(fired.title).font(.system(size: 11, weight: .medium))
                                    Spacer()
                                    Text(Fmt.duration(Date().timeIntervalSince(fired.date)) + " ago")
                                        .font(.system(size: 9)).foregroundStyle(.secondary)
                                }
                                Text(fired.body)
                                    .font(.system(size: 10))
                                    .foregroundStyle(.secondary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    }
                }
            }
            .padding(12)
        }
    }
}

private struct ThresholdEditor: View {
    @Binding var thresholds: [Double]
    private let options: [Double] = [25, 50, 60, 70, 75, 80, 85, 90, 95, 98]

    var body: some View {
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 5), count: 5), spacing: 5) {
            ForEach(options, id: \.self) { value in
                let on = thresholds.contains(value)
                Button {
                    if on { thresholds.removeAll { $0 == value } }
                    else { thresholds = (thresholds + [value]).sorted() }
                } label: {
                    Text("\(Int(value))%")
                        .font(.system(size: 10, weight: on ? .semibold : .regular))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 4)
                        .background(
                            RoundedRectangle(cornerRadius: 5)
                                .fill(on ? Color.accentColor.opacity(0.25) : Color.primary.opacity(0.06))
                        )
                }
                .buttonStyle(.plain)
            }
        }
    }
}

private struct HourPicker: View {
    @Binding var hour: Int

    var body: some View {
        Picker("", selection: $hour) {
            ForEach(0..<24, id: \.self) { Text(String(format: "%02d:00", $0)).tag($0) }
        }
        .labelsHidden()
        .frame(width: 84)
        .controlSize(.small)
    }
}

// MARK: - Settings

struct SettingsPanel: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        SettingsPanelBody(settings: model.settings, model: model)
    }
}

private struct SettingsPanelBody: View {
    @ObservedObject var settings: Settings
    @ObservedObject var model: AppModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                SectionCard("Menu bar") {
                    Picker("Show", selection: $settings.menuBarStyle) {
                        ForEach(MenuBarStyle.allCases) { Text($0.label).tag($0) }
                    }
                    .controlSize(.small)
                    Text(settings.menuBarStyle.explanation)
                        .font(.system(size: 10)).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Toggle("Show icon", isOn: $settings.showMenuBarSpark)
                        .font(.system(size: 11))
                }

                SectionCard("Polling") {
                    HStack {
                        Text("Every").font(.system(size: 11))
                        Stepper(value: $settings.pollInterval, in: 180...1800, step: 30) {
                            Text(Fmt.duration(settings.effectivePollInterval))
                                .font(.system(size: 11, design: .rounded)).monospacedDigit()
                        }
                    }
                    Text("The usage endpoint rate-limits per token. 180s is the documented floor and Runway will not go below it — between polls the numbers are extrapolated from your local logs instead.")
                        .font(.system(size: 10)).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                SectionCard("Diagnostics") {
                    LabeledRow("Plan", model.snapshot.planLabel ?? "unknown")
                    LabeledRow("Records held", "\(model.records.count) turns / 30 days")
                    LabeledRow("Widget channel", model.usingAppGroup ? "App Group (widget live)" : "Local only (widget unavailable)")
                    if let error = model.lastError {
                        Text(error)
                            .font(.system(size: 10))
                            .foregroundStyle(Severity.tight.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }

                SectionCard("Advanced") {
                    Text("User-Agent version override")
                        .font(.system(size: 11))
                    TextField("auto-detected from your logs", text: $settings.userAgentOverride)
                        .textFieldStyle(.roundedBorder)
                        .controlSize(.small)
                    Text("Runway sends `claude-code/<version>`. Without a matching User-Agent the endpoint drops you into a much stricter rate-limit bucket. Leave blank unless you're debugging.")
                        .font(.system(size: 10)).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(12)
        }
    }
}

private struct LabeledRow: View {
    var label: String
    var value: String
    init(_ label: String, _ value: String) { self.label = label; self.value = value }

    var body: some View {
        HStack {
            Text(label).font(.system(size: 11)).foregroundStyle(.secondary)
            Spacer()
            Text(value).font(.system(size: 11)).lineLimit(1).truncationMode(.middle)
        }
    }
}

struct SectionCard<Content: View>: View {
    var title: String
    @ViewBuilder var content: Content

    init(_ title: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title.uppercased())
                .font(.system(size: 9, weight: .semibold))
                .kerning(0.6)
                .foregroundStyle(.secondary)
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(11)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.primary.opacity(0.04)))
    }
}
