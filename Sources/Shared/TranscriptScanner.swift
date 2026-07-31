import Foundation

/// A single billed assistant turn, recovered from Claude Code's local logs.
struct UsageRecord: Codable, Equatable {
    var id: String          // message id + request id, for dedupe
    var date: Date
    var model: String
    var project: String     // last path component of `cwd`
    var projectPath: String
    var sessionId: String
    var tokens: TokenTotals

    var cost: Double { Pricing.cost(of: tokens, model: model) }
}

/// Incrementally reads `~/.claude/projects/**/*.jsonl`.
///
/// This is the half of Runway that needs no network at all. It gives us
/// per-model, per-project and per-session attribution, and — critically — it
/// lets the UI keep moving between the 180-second API polls without spending
/// extra requests against a rate-limited endpoint.
///
/// Reads are incremental: we remember a byte offset per file and only parse what
/// was appended since last time. A file that shrank was rotated, so we re-read it
/// from the start.
final class TranscriptScanner {
    struct Cursor: Codable {
        var offset: UInt64
        var size: UInt64
    }

    struct State: Codable {
        var cursors: [String: Cursor] = [:]
        var records: [UsageRecord] = []
        var lastSeenCLIVersion: String?
    }

    /// Records older than this are dropped; everything the UI shows is derived
    /// from what's retained, so this bounds both memory and the state file.
    static let retention: TimeInterval = 30 * 24 * 3600

    private(set) var state: State
    private let stateURL: URL
    private let root: URL

    init(root: URL = ClaudeHome.projectsDirectory, stateURL: URL) {
        self.root = root
        self.stateURL = stateURL
        if let data = try? Data(contentsOf: stateURL),
           let decoded = try? JSONDecoder().decode(State.self, from: data) {
            self.state = decoded
        } else {
            self.state = State()
        }
    }

    /// Parses everything appended since the last call. Returns the new records.
    @discardableResult
    func scan() -> [UsageRecord] {
        let files = transcriptFiles()
        var fresh: [UsageRecord] = []
        var known = Set(state.records.map(\.id))

        for file in files {
            let path = file.path
            let size = (try? FileManager.default.attributesOfItem(atPath: path)[.size] as? UInt64) ?? nil
            guard let size else { continue }

            var cursor = state.cursors[path] ?? Cursor(offset: 0, size: 0)
            if size < cursor.offset { cursor = Cursor(offset: 0, size: 0) }   // rotated
            if size == cursor.offset { continue }                            // unchanged

            guard let handle = try? FileHandle(forReadingFrom: file) else { continue }
            defer { try? handle.close() }

            do {
                try handle.seek(toOffset: cursor.offset)
                let data = try handle.readToEnd() ?? Data()
                // Only consume up to the last complete line; a partial trailing
                // line means Claude Code is mid-write and we'll get it next tick.
                guard let lastNewline = data.lastIndex(of: 0x0A) else { continue }
                let complete = data[data.startIndex...lastNewline]

                for line in complete.split(separator: 0x0A, omittingEmptySubsequences: true) {
                    guard let record = Self.parse(line: Data(line)) else { continue }
                    if known.insert(record.id).inserted {
                        fresh.append(record)
                    }
                }

                cursor.offset += UInt64(complete.count)
                cursor.size = size
                state.cursors[path] = cursor
            } catch {
                continue
            }
        }

        if !fresh.isEmpty {
            state.records.append(contentsOf: fresh)
            if let newest = fresh.max(by: { $0.date < $1.date }) {
                _ = newest
            }
        }

        prune()
        persist()
        return fresh
    }

    private func prune() {
        let cutoff = Date().addingTimeInterval(-Self.retention)
        state.records.removeAll { $0.date < cutoff }
        state.records.sort { $0.date < $1.date }

        // Forget cursors for files that no longer exist.
        let existing = Set(transcriptFiles().map(\.path))
        state.cursors = state.cursors.filter { existing.contains($0.key) }
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(state) else { return }
        try? FileManager.default.createDirectory(
            at: stateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: stateURL, options: .atomic)
    }

    private func transcriptFiles() -> [URL] {
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles, .skipsPackageDescendants]
        ) else { return [] }

        var result: [URL] = []
        for case let url as URL in enumerator where url.pathExtension == "jsonl" {
            result.append(url)
        }
        return result
    }

    // MARK: - Line parsing

    /// Pulls the usage block out of one assistant record. Returns nil for the
    /// ~70% of lines that carry no billing information (user turns, attachments,
    /// hooks, mode changes, file-history snapshots …).
    static func parse(line: Data) -> UsageRecord? {
        guard let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else { return nil }
        guard (obj["type"] as? String) == "assistant" else { return nil }
        guard let message = obj["message"] as? [String: Any],
              let usage = message["usage"] as? [String: Any],
              let model = message["model"] as? String else { return nil }

        let messageId = message["id"] as? String ?? ""
        let requestId = obj["requestId"] as? String ?? ""
        guard !messageId.isEmpty || !requestId.isEmpty else { return nil }

        let timestamp = (obj["timestamp"] as? String).flatMap {
            ISO8601DateFormatter.fractional.date(from: $0) ?? ISO8601DateFormatter.plain.date(from: $0)
        } ?? Date()

        let cwd = obj["cwd"] as? String ?? ""
        let project = cwd.isEmpty ? "unknown" : URL(fileURLWithPath: cwd).lastPathComponent

        var tokens = TokenTotals()
        tokens.input = usage["input_tokens"] as? Int ?? 0
        tokens.output = usage["output_tokens"] as? Int ?? 0
        tokens.cacheRead = usage["cache_read_input_tokens"] as? Int ?? 0

        // Prefer the TTL-split breakdown so 5m and 1h writes get their own rate;
        // fall back to the flat total when the split isn't present.
        if let split = usage["cache_creation"] as? [String: Any] {
            tokens.cacheWrite5m = split["ephemeral_5m_input_tokens"] as? Int ?? 0
            tokens.cacheWrite1h = split["ephemeral_1h_input_tokens"] as? Int ?? 0
        } else {
            tokens.cacheWrite5m = usage["cache_creation_input_tokens"] as? Int ?? 0
        }

        // A record with no tokens at all is a control message, not a billed turn.
        guard tokens.billable > 0 else { return nil }

        return UsageRecord(
            id: "\(messageId)|\(requestId)",
            date: timestamp,
            model: model,
            project: project,
            projectPath: cwd,
            sessionId: obj["sessionId"] as? String ?? "",
            tokens: tokens
        )
    }

    /// The CLI version seen most recently in the logs, used to build the
    /// `User-Agent` the usage endpoint expects. Avoids shelling out to `claude`.
    func detectCLIVersion() -> String? {
        let files = transcriptFiles().sorted {
            let a = (try? $0.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
            let b = (try? $1.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
            return a > b
        }
        for file in files.prefix(3) {
            guard let data = try? Data(contentsOf: file) else { continue }
            for line in data.split(separator: 0x0A).reversed().prefix(200) {
                guard let obj = try? JSONSerialization.jsonObject(with: Data(line)) as? [String: Any],
                      let version = obj["version"] as? String, !version.isEmpty else { continue }
                return version
            }
        }
        return nil
    }
}

// MARK: - Aggregation

extension Array where Element == UsageRecord {
    func within(_ interval: DateInterval) -> [UsageRecord] {
        filter { interval.contains($0.date) }
    }

    func since(_ date: Date) -> [UsageRecord] {
        filter { $0.date >= date }
    }

    var tokens: TokenTotals {
        reduce(into: TokenTotals()) { $0 += $1.tokens }
    }

    var cost: Double {
        reduce(0) { $0 + $1.cost }
    }

    /// Grouped totals, heaviest first.
    func breakdown(by key: (UsageRecord) -> String) -> [(name: String, tokens: TokenTotals, cost: Double)] {
        var buckets: [String: (TokenTotals, Double)] = [:]
        for record in self {
            var entry = buckets[key(record)] ?? (TokenTotals(), 0)
            entry.0 += record.tokens
            entry.1 += record.cost
            buckets[key(record)] = entry
        }
        return buckets
            .map { (name: $0.key, tokens: $0.value.0, cost: $0.value.1) }
            .sorted { $0.cost > $1.cost }
    }
}
