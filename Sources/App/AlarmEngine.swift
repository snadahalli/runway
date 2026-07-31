import Foundation
import UserNotifications

/// Fires at most once per (limit, window instance, rule).
///
/// The dedupe key includes the window's reset timestamp, so a threshold that
/// fired in this 5-hour window fires again cleanly in the next one without any
/// manual reset — and a limit hovering at 80.1% doesn't spam on every poll.
@MainActor
final class AlarmEngine: ObservableObject {
    @Published private(set) var authorization: UNAuthorizationStatus = .notDetermined
    @Published private(set) var recent: [Fired] = []

    struct Fired: Identifiable, Equatable {
        let id = UUID()
        var date: Date
        var title: String
        var body: String
    }

    private var firedKeys: Set<String> {
        get { Set(UserDefaults.standard.stringArray(forKey: "firedAlarmKeys") ?? []) }
        set {
            // Bounded so the key set can't grow forever across weeks of windows.
            let trimmed = Array(newValue.suffix(500))
            UserDefaults.standard.set(trimmed, forKey: "firedAlarmKeys")
        }
    }

    private let settings: Settings

    init(settings: Settings) {
        self.settings = settings
    }

    func requestAuthorization() async {
        let center = UNUserNotificationCenter.current()
        _ = try? await center.requestAuthorization(options: [.alert, .sound])
        let status = await center.notificationSettings().authorizationStatus
        authorization = status
    }

    func refreshAuthorization() async {
        authorization = await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
    }

    // MARK: - Evaluation

    func evaluate(_ snapshot: RunwaySnapshot) {
        guard settings.alarmsEnabled else { return }

        for limit in snapshot.limits {
            let windowID = limit.resetsAt.map { String(Int($0.timeIntervalSince1970)) } ?? "none"
            let base = "\(limit.kind.rawValue)|\(limit.label)|\(windowID)"

            evaluateThresholds(limit: limit, base: base)
            evaluatePrediction(limit: limit, base: base)
            evaluatePace(limit: limit, base: base)
        }
    }

    private func evaluateThresholds(limit: LimitSnapshot, base: String) {
        for threshold in settings.thresholds.sorted() where limit.percent >= threshold {
            let key = "\(base)|threshold|\(Int(threshold))"
            guard !firedKeys.contains(key) else { continue }
            markFired(key)

            let remaining = limit.timeRemaining.map { " · resets in \(Fmt.duration($0))" } ?? ""
            var body = "\(Fmt.percent(limit.percent)) used\(remaining)."
            if let allowance = limit.allowanceTokensPerHour {
                body += " You can still spend \(Fmt.tokens(allowance))/h to finish level."
            }
            post(title: "\(limit.label) at \(Int(threshold))%", body: body, key: key)
        }
    }

    private func evaluatePrediction(limit: LimitSnapshot, base: String) {
        guard settings.predictiveAlarms,
              limit.runsDryEarly,
              let exhausts = limit.exhaustsAt,
              let resets = limit.resetsAt else { return }

        let early = resets.timeIntervalSince(exhausts)
        // Only worth saying when it's both meaningfully early and close enough
        // to act on. Beyond 3 days out the projection isn't trustworthy.
        guard early > 900, exhausts.timeIntervalSinceNow < 3 * 24 * 3600 else { return }

        let key = "\(base)|predict"
        guard !firedKeys.contains(key) else { return }
        markFired(key)

        post(
            title: "\(limit.label) will run dry early",
            body: "At the current pace you hit 100% around \(Fmt.clock(exhausts)) — \(Fmt.duration(early)) before the window resets.",
            key: key
        )
    }

    private func evaluatePace(limit: LimitSnapshot, base: String) {
        guard let ratio = limit.paceRatio, ratio >= settings.paceAlarmRatio else { return }
        // Pointless to warn about pace when there's barely anything left to burn.
        guard limit.percent < 95 else { return }

        let key = "\(base)|pace"
        guard !firedKeys.contains(key) else { return }
        markFired(key)

        post(
            title: "Burning \(Fmt.ratio(ratio)) sustainable pace",
            body: "\(limit.label) is at \(Fmt.percent(limit.percent)). Sustainable from here is \(Fmt.tokens(limit.allowanceTokensPerHour ?? 0))/h.",
            key: key
        )
    }

    // MARK: - Delivery

    private func markFired(_ key: String) {
        var keys = firedKeys
        keys.insert(key)
        firedKeys = keys
    }

    private func post(title: String, body: String, key: String) {
        recent.insert(Fired(date: Date(), title: title, body: body), at: 0)
        if recent.count > 25 { recent.removeLast(recent.count - 25) }

        // Quiet hours suppress delivery, not evaluation — the event is still
        // recorded in the popover's alarm log so nothing is silently lost.
        guard !settings.isQuietNow() else { return }

        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default

        let request = UNNotificationRequest(identifier: key, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    func testNotification() {
        post(
            title: "Runway alarms are working",
            body: "This is what a threshold alert looks like.",
            key: "test-\(Int(Date().timeIntervalSince1970))"
        )
    }
}
