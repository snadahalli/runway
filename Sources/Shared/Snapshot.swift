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
    ///
    /// Ranked in two classes rather than one number, because seconds and
    /// percentage points aren't comparable quantities. Scoring un-projected
    /// limits as `1000 - percent` against seconds-to-exhaustion put every
    /// un-projected limit ahead of anything running dry more than ~17 minutes
    /// out, which is the opposite of the intent. So: a limit actually projected
    /// to run out before its window resets binds first, soonest wins; otherwise
    /// the fullest one does.
    var headline: LimitSnapshot? {
        // Keep this in step with `RunwaySnapshot::headline` in core/src/snapshot.rs —
        // the widget and the cross-platform app must agree on what the headline is.
        func urgency(_ l: LimitSnapshot) -> (Int, Double) {
            guard let exhausts = l.exhaustsAt, let resets = l.resetsAt, exhausts < resets else {
                return (1, -l.percent)
            }
            return (0, exhausts.timeIntervalSinceNow)
        }
        return limits
            .filter { $0.percent > 0 || $0.isActive }
            .min { lhs, rhs in urgency(lhs) < urgency(rhs) } ?? limits.first
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
/// App Groups are the channel. They do *not* need a signing team — macOS honours
/// the entitlement on an ad-hoc signature — but they do need the entitlement to
/// have been applied, which `build.sh` does after the build. The fallbacks below
/// keep the menu bar app working if it wasn't; only the widget degrades.
enum SnapshotStore {
    static let appGroupID = "group.com.sn.runway"
    static let filename = "runway-snapshot.json"

    /// Three resolution steps, in order of preference:
    ///
    /// 1. The real App Group container. Any entitled process gets this,
    ///    ad-hoc signature included — and a sandboxed one gets *only* this.
    /// 2. The group directory addressed by path. A non-sandboxed process (the
    ///    menu bar app) can write here with no entitlement at all, so an
    ///    unentitled build still publishes where an entitled widget would look.
    /// 3. Application Support, when even step 2 is unwritable.
    ///
    /// The upshot: every build lands the snapshot in the same place, whether or
    /// not the entitlement made it into the signature.
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
