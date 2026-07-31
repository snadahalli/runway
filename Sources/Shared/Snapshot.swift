import Foundation

enum LimitKind: String, Codable {
    case session          // rolling 5-hour window
    case weeklyAll        // 7-day, all models
    case weeklyScoped     // 7-day, scoped to one model family
    case other

    var windowSeconds: TimeInterval {
        switch self {
        case .session: return 5 * 3600
        case .weeklyAll, .weeklyScoped: return 7 * 24 * 3600
        case .other: return 7 * 24 * 3600
        }
    }

    static func from(apiKind: String) -> LimitKind {
        switch apiKind {
        case "session": return .session
        case "weekly_all": return .weeklyAll
        case "weekly_scoped": return .weeklyScoped
        default: return .other
        }
    }
}

/// One limit, plus everything Runway derived about it.
struct LimitSnapshot: Codable, Identifiable, Equatable {
    var id: String { kind.rawValue + "|" + label }

    var kind: LimitKind
    var label: String
    var percent: Double
    var resetsAt: Date?
    var isActive: Bool

    /// Actual burn ÷ the burn that would land exactly at 100% at reset.
    /// 1.0 is perfectly paced; above 1.0 runs dry early.
    var paceRatio: Double?
    /// Projected moment this limit reaches 100% at the current rate.
    var exhaustsAt: Date?
    /// Percentage points per hour you may spend from now on and still land at
    /// 100% exactly at reset. This is the number the whole app is built around.
    var allowancePercentPerHour: Double?
    var allowanceTokensPerHour: Double?
    var remainingTokens: Double?
    var remainingValueUSD: Double?

    var windowSeconds: TimeInterval { kind.windowSeconds }

    var timeRemaining: TimeInterval? {
        guard let resetsAt else { return nil }
        return max(0, resetsAt.timeIntervalSinceNow)
    }

    /// True when the projection says we run out before the window resets.
    var runsDryEarly: Bool {
        guard let exhaustsAt, let resetsAt else { return false }
        return exhaustsAt < resetsAt
    }
}

/// Ledger rollup for the popover and widget footer.
struct LedgerSummary: Codable, Equatable {
    var windowLabel: String
    var tokens: TokenTotals
    var costUSD: Double
    var topProjects: [Entry]
    var topModels: [Entry]

    struct Entry: Codable, Equatable, Identifiable {
        var id: String { name }
        var name: String
        var tokens: Int
        var costUSD: Double
    }

    static let empty = LedgerSummary(
        windowLabel: "This week", tokens: TokenTotals(), costUSD: 0, topProjects: [], topModels: [])
}

enum SnapshotHealth: String, Codable {
    case live          // fresh API data
    case estimated     // between polls, extrapolated from local logs
    case backingOff    // rate limited, showing last good data
    case error
    case noCredentials
}

/// The single value the app publishes and the widget consumes.
struct RunwaySnapshot: Codable, Equatable {
    var generatedAt: Date
    var apiObservedAt: Date?
    var health: SnapshotHealth
    var message: String?

    var limits: [LimitSnapshot]
    var ledger: LedgerSummary
    var planLabel: String?
    var monthlyValueUSD: Double?

    /// The limit that will bind first — what you actually care about.
    var headline: LimitSnapshot? {
        limits
            .filter { $0.percent > 0 || $0.isActive }
            .min { lhs, rhs in
                // Sort by "how close to the wall", preferring an early run-dry.
                func urgency(_ l: LimitSnapshot) -> Double {
                    guard let exhausts = l.exhaustsAt else { return 1000 - l.percent }
                    return exhausts.timeIntervalSinceNow
                }
                return urgency(lhs) < urgency(rhs)
            } ?? limits.first
    }

    var age: TimeInterval { Date().timeIntervalSince(generatedAt) }

    static let placeholder = RunwaySnapshot(
        generatedAt: Date(),
        apiObservedAt: nil,
        health: .noCredentials,
        message: "Open Runway to connect",
        limits: [],
        ledger: .empty,
        planLabel: nil,
        monthlyValueUSD: nil
    )
}

// MARK: - Shared storage

/// Writes the snapshot where both the app and the widget can reach it.
///
/// App Groups are the supported channel, but they need a real signing team. When
/// the group container isn't available (ad-hoc signed local build), we fall back
/// to Application Support so the menu bar app still works standalone — the
/// widget is the only thing that degrades.
enum SnapshotStore {
    static let appGroupID = "group.com.sn.runway"
    static let filename = "runway-snapshot.json"

    /// Three resolution steps, in order of preference:
    ///
    /// 1. The real App Group container. Only a signed, entitled process gets
    ///    this — in practice, the widget extension.
    /// 2. The group directory addressed by path. A non-sandboxed process (the
    ///    menu bar app) can write here with no entitlement at all, so the app
    ///    can always publish to where an entitled widget would look.
    /// 3. Application Support, when even step 2 is unwritable.
    ///
    /// The upshot: the app never needs a signing team, and adding one later
    /// lights up the widget without changing where anything is stored.
    static var containerURL: URL {
        if let group = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroupID) {
            return group
        }
        let byPath = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Group Containers", isDirectory: true)
            .appendingPathComponent(appGroupID, isDirectory: true)
        if (try? FileManager.default.createDirectory(at: byPath, withIntermediateDirectories: true)) != nil {
            return byPath
        }
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return support.appendingPathComponent("Runway", isDirectory: true)
    }

    /// True when this process holds the entitlement, i.e. the widget can read us.
    static var usingAppGroup: Bool {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroupID) != nil
    }

    static var snapshotURL: URL { containerURL.appendingPathComponent(filename) }

    static func write(_ snapshot: RunwaySnapshot) {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        guard let data = try? encoder.encode(snapshot) else { return }
        try? FileManager.default.createDirectory(at: containerURL, withIntermediateDirectories: true)
        try? data.write(to: snapshotURL, options: .atomic)
    }

    static func read() -> RunwaySnapshot? {
        guard let data = try? Data(contentsOf: snapshotURL) else { return nil }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try? decoder.decode(RunwaySnapshot.self, from: data)
    }
}
