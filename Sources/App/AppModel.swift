import Foundation
import Combine
import WidgetKit

/// The engine. Owns the poll loop, the local log scanner, the projection state,
/// and publishes the single snapshot that every surface renders from.
@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var snapshot: RunwaySnapshot = .placeholder
    @Published private(set) var lastError: String?
    @Published private(set) var nextPollAt: Date?
    @Published private(set) var isPolling = false
    @Published private(set) var usingAppGroup = SnapshotStore.usingAppGroup

    let settings: Settings
    let alarms: AlarmEngine

    private let scanner: TranscriptScanner
    private var history = SampleHistory()
    private var pollTask: Task<Void, Never>?
    private var scanTimer: Timer?

    /// Set when the API told us to slow down. Cleared on the next success.
    private var consecutiveFailures = 0
    private var backoffUntil: Date?

    /// Percentages from the most recent successful API read, used as the anchor
    /// for local extrapolation between polls.
    private var anchorPercents: [String: Double] = [:]
    private var anchorDate: Date?

    private var cliVersion: String?

    private static var supportDirectory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("Runway", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private var historyURL: URL { Self.supportDirectory.appendingPathComponent("samples.json") }

    init(settings: Settings) {
        self.settings = settings
        self.alarms = AlarmEngine(settings: settings)
        self.scanner = TranscriptScanner(
            stateURL: Self.supportDirectory.appendingPathComponent("scan-state.json")
        )
        loadHistory()
    }

    // MARK: - Lifecycle

    private var started = false

    func start() {
        guard !started else { return }
        started = true

        cliVersion = settings.userAgentOverride.isEmpty
            ? scanner.detectCLIVersion()
            : settings.userAgentOverride

        scanner.scan()
        rebuildSnapshot(health: snapshot.health)

        Task { await alarms.requestAuthorization() }

        // Two independent cadences. The API is polled slowly because it is rate
        // limited; the local logs are read often because they cost nothing and
        // keep the UI honest between polls.
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.pollNow()
                let wait = await self?.sleepInterval() ?? 180
                try? await Task.sleep(nanoseconds: UInt64(wait * 1_000_000_000))
            }
        }

        scanTimer = Timer.scheduledTimer(withTimeInterval: 15, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.localTick() }
        }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
        scanTimer?.invalidate()
        scanTimer = nil
    }

    private func sleepInterval() -> TimeInterval {
        if let backoffUntil {
            return max(10, backoffUntil.timeIntervalSinceNow)
        }
        return settings.effectivePollInterval
    }

    // MARK: - API poll

    func pollNow(force: Bool = false) async {
        if !force, let backoffUntil, backoffUntil > Date() { return }
        guard !isPolling else { return }
        isPolling = true
        defer { isPolling = false }

        let credentials: OAuthCredentials
        do {
            credentials = try CredentialsLoader.load()
        } catch {
            lastError = error.localizedDescription
            rebuildSnapshot(health: .noCredentials, message: error.localizedDescription)
            scheduleNext(after: settings.effectivePollInterval)
            return
        }

        if credentials.isExpired {
            // Claude Code refreshes this itself; we just wait for it to happen.
            rebuildSnapshot(health: .error, message: "Access token expired — run `claude` to refresh.")
        }

        let version = cliVersion ?? scanner.detectCLIVersion() ?? "2.1.220"
        cliVersion = version
        let client = UsageAPIClient(userAgentVersion: version)

        do {
            let response = try await client.fetch(token: credentials.accessToken)
            consecutiveFailures = 0
            backoffUntil = nil
            lastError = nil
            ingest(response, plan: credentials.subscriptionType)
            scheduleNext(after: settings.effectivePollInterval)
        } catch let error as UsageAPIError {
            handle(error)
        } catch {
            handle(.transport(error))
        }
    }

    private func handle(_ error: UsageAPIError) {
        lastError = error.localizedDescription

        guard error.isRetryable else {
            rebuildSnapshot(health: .error, message: error.localizedDescription)
            scheduleNext(after: settings.effectivePollInterval)
            return
        }

        consecutiveFailures += 1
        // Honour retry-after when the server sent one, otherwise exponential
        // backoff from the poll interval, capped at 15 minutes.
        let suggested: TimeInterval
        if case .rateLimited(let retryAfter) = error, let retryAfter {
            suggested = retryAfter + 5
        } else {
            suggested = min(900, settings.effectivePollInterval * pow(2, Double(consecutiveFailures - 1)))
        }
        backoffUntil = Date().addingTimeInterval(suggested)
        rebuildSnapshot(health: .backingOff, message: error.localizedDescription)
        scheduleNext(after: suggested)
    }

    private func scheduleNext(after interval: TimeInterval) {
        nextPollAt = Date().addingTimeInterval(interval)
    }

    // MARK: - Ingestion

    private func ingest(_ response: UsageResponse, plan: String?) {
        let now = Date()
        var observed: [(kind: LimitKind, label: String, percent: Double, resets: Date?, active: Bool)] = []

        if let limits = response.limits, !limits.isEmpty {
            for limit in limits {
                let kind = LimitKind.from(apiKind: limit.kind)
                observed.append((
                    kind: kind,
                    label: Self.label(for: limit, kind: kind),
                    percent: limit.percent,
                    resets: limit.resetsAt,
                    active: limit.isActive ?? false
                ))
            }
        } else {
            if let five = response.fiveHour {
                observed.append((.session, "5-hour session", five.utilization, five.resetsAt, true))
            }
            if let seven = response.sevenDay {
                observed.append((.weeklyAll, "Weekly", seven.utilization, seven.resetsAt, true))
            }
        }

        for item in observed {
            let key = item.kind.rawValue + "|" + item.label
            history.record(key: key, sample: UsageSample(date: now, percent: item.percent, resetsAt: item.resets))
            anchorPercents[key] = item.percent
        }
        anchorDate = now
        saveHistory()

        scanner.scan()
        rebuildSnapshot(health: .live, plan: plan, observedAt: now, observed: observed)
    }

    private static func label(for limit: UsageResponse.Limit, kind: LimitKind) -> String {
        switch kind {
        case .session: return "5-hour session"
        case .weeklyAll: return "Weekly · all models"
        case .weeklyScoped:
            let model = limit.scope?.model?.displayName ?? limit.scope?.model?.id ?? "scoped"
            return "Weekly · \(model)"
        case .other: return limit.kind.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    // MARK: - Local tick

    /// Runs between API polls. Reads new log lines and extrapolates each limit
    /// forward from the last API anchor, so the UI keeps moving without spending
    /// requests against a rate-limited endpoint.
    private func localTick() {
        let fresh = scanner.scan()
        guard !fresh.isEmpty || snapshot.health == .live else {
            rebuildSnapshot(health: snapshot.health, message: snapshot.message)
            return
        }
        rebuildSnapshot(
            health: snapshot.health == .live ? .estimated : snapshot.health,
            message: snapshot.message
        )
    }

    // MARK: - Snapshot assembly

    private func rebuildSnapshot(
        health: SnapshotHealth,
        message: String? = nil,
        plan: String? = nil,
        observedAt: Date? = nil,
        observed: [(kind: LimitKind, label: String, percent: Double, resets: Date?, active: Bool)]? = nil
    ) {
        let records = scanner.state.records
        let now = Date()

        // Either the readings we were just handed, or the last known reading per
        // series when this is a local-only refresh.
        let sources: [(kind: LimitKind, label: String, percent: Double, resets: Date?, active: Bool)]
        if let observed {
            sources = observed
        } else {
            sources = history.series.compactMap { key, samples in
                guard let last = samples.last else { return nil }
                let parts = key.split(separator: "|", maxSplits: 1).map(String.init)
                guard parts.count == 2, let kind = LimitKind(rawValue: parts[0]) else { return nil }
                return (kind, parts[1], last.percent, last.resetsAt, true)
            }
        }

        var limits: [LimitSnapshot] = []
        for source in sources {
            let key = source.kind.rawValue + "|" + source.label
            let windowSamples = history.currentWindow(key: key, resetsAt: source.resets)
            let windowStart = source.resets.map { $0.addingTimeInterval(-source.kind.windowSeconds) }
            let windowRecords = windowStart.map { records.since($0) } ?? records

            var percent = source.percent

            // Between polls, nudge the percentage using local token volume and
            // the calibrated tokens-per-percent. Never let the estimate exceed
            // 100 or run backwards — an estimate that overshoots is worse than
            // one that lags.
            if health == .estimated || health == .backingOff,
               let anchorDate,
               let anchor = anchorPercents[key],
               let calibration = Projection.calibrate(samples: windowSamples, records: windowRecords) {
                let since = records.since(anchorDate)
                let extra = Double(since.tokens.fresh) / max(1, calibration.tokensPerPercent)
                percent = min(100, max(anchor, anchor + extra))
            }

            limits.append(Projection.snapshot(
                kind: source.kind,
                label: source.label,
                percent: percent,
                resetsAt: source.resets,
                isActive: source.active,
                history: windowSamples,
                records: windowRecords,
                now: now
            ))
        }

        limits.sort { lhs, rhs in
            if lhs.kind == rhs.kind { return lhs.label < rhs.label }
            return lhs.kind.windowSeconds < rhs.kind.windowSeconds
        }

        let weekStart = now.addingTimeInterval(-7 * 24 * 3600)
        let weekRecords = records.since(weekStart)
        let monthCost = records.since(now.addingTimeInterval(-30 * 24 * 3600)).cost

        let ledger = LedgerSummary(
            windowLabel: "Last 7 days",
            tokens: weekRecords.tokens,
            costUSD: weekRecords.cost,
            topProjects: weekRecords.breakdown { $0.project }.prefix(6).map {
                LedgerSummary.Entry(name: $0.name, tokens: $0.tokens.billable, costUSD: $0.cost)
            },
            topModels: weekRecords.breakdown { Pricing.family(for: $0.model) }.prefix(5).map {
                LedgerSummary.Entry(name: $0.name, tokens: $0.tokens.billable, costUSD: $0.cost)
            }
        )

        let updated = RunwaySnapshot(
            generatedAt: now,
            apiObservedAt: observedAt ?? snapshot.apiObservedAt,
            health: health,
            message: message,
            limits: limits,
            ledger: ledger,
            planLabel: plan ?? snapshot.planLabel,
            monthlyValueUSD: monthCost
        )

        snapshot = updated
        SnapshotStore.write(updated)
        usingAppGroup = SnapshotStore.usingAppGroup
        WidgetCenter.shared.reloadAllTimelines()

        if health == .live {
            alarms.evaluate(updated)
        }
    }

    // MARK: - Persistence

    private func loadHistory() {
        guard let data = try? Data(contentsOf: historyURL),
              let decoded = try? JSONDecoder().decode(SampleHistory.self, from: data) else { return }
        history = decoded
        if let latest = history.series.compactMap({ $0.value.last }).max(by: { $0.date < $1.date }) {
            anchorDate = latest.date
        }
        for (key, samples) in history.series {
            anchorPercents[key] = samples.last?.percent
        }
    }

    private func saveHistory() {
        guard let data = try? JSONEncoder().encode(history) else { return }
        try? data.write(to: historyURL, options: .atomic)
    }

    // MARK: - Views' helpers

    /// Recent percentage readings for a limit, for the sparkline.
    func series(for limit: LimitSnapshot) -> [UsageSample] {
        history.currentWindow(key: limit.kind.rawValue + "|" + limit.label, resetsAt: limit.resetsAt)
    }

    var records: [UsageRecord] { scanner.state.records }
}
