import Foundation
import Combine

enum MenuBarStyle: String, CaseIterable, Identifiable {
    case paceRatio      // 1.8×  — how fast you're burning vs sustainable
    case allowance      // 240K/h — what you may still spend per hour
    case percent        // 42%   — the conventional readout
    case timeLeft       // 2h 14m until the binding limit runs dry

    var id: String { rawValue }

    var label: String {
        switch self {
        case .paceRatio: return "Pace ratio"
        case .allowance: return "Hourly allowance"
        case .percent:   return "Percent used"
        case .timeLeft:  return "Time to dry"
        }
    }

    var explanation: String {
        switch self {
        case .paceRatio: return "1.0× lands exactly at the reset. Above that runs dry early."
        case .allowance: return "Tokens per hour you can still spend and finish the window level."
        case .percent:   return "Percentage of the binding limit consumed."
        case .timeLeft:  return "Projected time until the binding limit hits 100%."
        }
    }
}

final class Settings: ObservableObject {
    private let defaults = UserDefaults.standard

    @Published var pollInterval: Double { didSet { defaults.set(pollInterval, forKey: "pollInterval") } }
    @Published var menuBarStyle: MenuBarStyle { didSet { defaults.set(menuBarStyle.rawValue, forKey: "menuBarStyle") } }
    @Published var showMenuBarSpark: Bool { didSet { defaults.set(showMenuBarSpark, forKey: "showMenuBarSpark") } }

    @Published var alarmsEnabled: Bool { didSet { defaults.set(alarmsEnabled, forKey: "alarmsEnabled") } }
    @Published var thresholds: [Double] { didSet { defaults.set(thresholds, forKey: "thresholds") } }
    @Published var predictiveAlarms: Bool { didSet { defaults.set(predictiveAlarms, forKey: "predictiveAlarms") } }
    @Published var paceAlarmRatio: Double { didSet { defaults.set(paceAlarmRatio, forKey: "paceAlarmRatio") } }
    @Published var resetNotifications: Bool { didSet { defaults.set(resetNotifications, forKey: "resetNotifications") } }

    @Published var quietHoursEnabled: Bool { didSet { defaults.set(quietHoursEnabled, forKey: "quietHoursEnabled") } }
    @Published var quietStartHour: Int { didSet { defaults.set(quietStartHour, forKey: "quietStartHour") } }
    @Published var quietEndHour: Int { didSet { defaults.set(quietEndHour, forKey: "quietEndHour") } }

    @Published var userAgentOverride: String { didSet { defaults.set(userAgentOverride, forKey: "userAgentOverride") } }

    init() {
        defaults.register(defaults: [
            "pollInterval": 180.0,
            "menuBarStyle": MenuBarStyle.paceRatio.rawValue,
            "showMenuBarSpark": true,
            "alarmsEnabled": true,
            "thresholds": [50.0, 80.0, 95.0],
            "predictiveAlarms": true,
            "paceAlarmRatio": 2.0,
            "resetNotifications": false,
            "quietHoursEnabled": false,
            "quietStartHour": 22,
            "quietEndHour": 8,
            "userAgentOverride": "",
        ])

        pollInterval = max(UsageAPIClient.minimumPollInterval, defaults.double(forKey: "pollInterval"))
        menuBarStyle = MenuBarStyle(rawValue: defaults.string(forKey: "menuBarStyle") ?? "") ?? .paceRatio
        showMenuBarSpark = defaults.bool(forKey: "showMenuBarSpark")
        alarmsEnabled = defaults.bool(forKey: "alarmsEnabled")
        thresholds = (defaults.array(forKey: "thresholds") as? [Double]) ?? [50, 80, 95]
        predictiveAlarms = defaults.bool(forKey: "predictiveAlarms")
        paceAlarmRatio = defaults.double(forKey: "paceAlarmRatio")
        resetNotifications = defaults.bool(forKey: "resetNotifications")
        quietHoursEnabled = defaults.bool(forKey: "quietHoursEnabled")
        quietStartHour = defaults.integer(forKey: "quietStartHour")
        quietEndHour = defaults.integer(forKey: "quietEndHour")
        userAgentOverride = defaults.string(forKey: "userAgentOverride") ?? ""
    }

    /// Never allow a poll cadence that would get the token rate limited.
    var effectivePollInterval: TimeInterval {
        max(UsageAPIClient.minimumPollInterval, pollInterval)
    }

    func isQuietNow(_ date: Date = Date()) -> Bool {
        guard quietHoursEnabled else { return false }
        let hour = Calendar.current.component(.hour, from: date)
        if quietStartHour == quietEndHour { return false }
        if quietStartHour < quietEndHour {
            return hour >= quietStartHour && hour < quietEndHour
        }
        // Wraps past midnight.
        return hour >= quietStartHour || hour < quietEndHour
    }
}
