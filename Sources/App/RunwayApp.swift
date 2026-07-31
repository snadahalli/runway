import SwiftUI

@main
struct RunwayApp: App {
    @StateObject private var settings: Settings
    @StateObject private var model: AppModel

    init() {
        let settings = Settings()
        let model = AppModel(settings: settings)
        _settings = StateObject(wrappedValue: settings)
        _model = StateObject(wrappedValue: model)

        // Start polling at launch, not when the popover is first opened —
        // otherwise the widget and the menu bar title stay empty until someone
        // clicks, and alarms never fire for a user who never opens the menu.
        DispatchQueue.main.async { model.start() }
    }

    var body: some Scene {
        MenuBarExtra {
            PopoverRoot()
                .environmentObject(model)
        } label: {
            MenuBarLabel(model: model, settings: settings)
        }
        .menuBarExtraStyle(.window)
    }
}

/// Deliberately not a percentage by default.
///
/// A percentage tells you where you've been; the pace ratio tells you whether to
/// change what you're doing in the next ten minutes. 1.0x means you'll land
/// exactly on the limit as the window resets — the ideal, not the danger point.
struct MenuBarLabel: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: Settings

    var body: some View {
        let limit = model.snapshot.headline
        let severity = limit.map(Severity.of) ?? .calm

        HStack(spacing: 3) {
            if settings.showMenuBarSpark {
                Image(systemName: symbol(for: limit, severity: severity))
            }
            Text(text(for: limit))
                .monospacedDigit()
        }
    }

    private func symbol(for limit: LimitSnapshot?, severity: Severity) -> String {
        switch model.snapshot.health {
        case .noCredentials: return "person.crop.circle.badge.questionmark"
        case .error:         return "exclamationmark.triangle"
        case .backingOff:    return "clock.arrow.circlepath"
        default:             return severity.symbol
        }
    }

    private func text(for limit: LimitSnapshot?) -> String {
        guard let limit else { return "—" }

        switch settings.menuBarStyle {
        case .paceRatio:
            guard let ratio = limit.paceRatio else { return Fmt.percent(limit.percent) }
            return Fmt.ratio(ratio)
        case .allowance:
            guard let allowance = limit.allowanceTokensPerHour else {
                return limit.allowancePercentPerHour.map { String(format: "%.1f%%/h", $0) } ?? "—"
            }
            return Fmt.tokens(allowance) + "/h"
        case .percent:
            return Fmt.percent(limit.percent)
        case .timeLeft:
            if let exhausts = limit.exhaustsAt, limit.runsDryEarly {
                return Fmt.duration(exhausts.timeIntervalSinceNow)
            }
            return limit.resetsAt.map { Fmt.duration($0.timeIntervalSinceNow) } ?? "—"
        }
    }
}
