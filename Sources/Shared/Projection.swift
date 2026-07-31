import Foundation

/// One observation of a single limit.
struct UsageSample: Codable, Equatable {
    var date: Date
    var percent: Double
    var resetsAt: Date?
}

/// Per-limit history, persisted so projections survive a relaunch.
struct SampleHistory: Codable {
    var series: [String: [UsageSample]] = [:]

    static let retention: TimeInterval = 14 * 24 * 3600
    static let maxPerSeries = 2000

    mutating func record(key: String, sample: UsageSample) {
        var list = series[key] ?? []
        // Skip duplicates — the API only moves every few minutes.
        if let last = list.last, abs(last.percent - sample.percent) < 0.001,
           last.resetsAt == sample.resetsAt,
           sample.date.timeIntervalSince(last.date) < 60 {
            return
        }
        list.append(sample)
        let cutoff = Date().addingTimeInterval(-Self.retention)
        list.removeAll { $0.date < cutoff }
        if list.count > Self.maxPerSeries { list.removeFirst(list.count - Self.maxPerSeries) }
        series[key] = list
    }

    /// Samples belonging to the window instance currently in flight. A window
    /// instance is identified by its reset time — when that changes, the window
    /// rolled over and the old samples must not contaminate the new slope.
    func currentWindow(key: String, resetsAt: Date?) -> [UsageSample] {
        guard let list = series[key] else { return [] }
        guard let resetsAt else { return list }
        return list.filter { sample in
            guard let sampleReset = sample.resetsAt else { return false }
            return abs(sampleReset.timeIntervalSince(resetsAt)) < 300
        }
    }
}

/// Turns raw percentages into the numbers the UI is built around: a pace ratio,
/// a projected run-dry moment, and — the part nobody else does — an allowance
/// expressed in tokens and dollars rather than opaque percentage points.
enum Projection {

    struct Calibration {
        var tokensPerPercent: Double
        var dollarsPerPercent: Double
    }

    /// Least-squares slope in percentage points per hour, or nil when there
    /// isn't enough signal to say anything honest.
    static func burnRate(samples: [UsageSample], now: Date = Date()) -> Double? {
        guard samples.count >= 3, let first = samples.first, let last = samples.last else { return nil }
        let span = last.date.timeIntervalSince(first.date)
        guard span >= 600 else { return nil }   // < 10 minutes of history says nothing

        let originHours = first.date.timeIntervalSince1970 / 3600
        var sumX = 0.0, sumY = 0.0, sumXY = 0.0, sumXX = 0.0
        for sample in samples {
            let x = sample.date.timeIntervalSince1970 / 3600 - originHours
            let y = sample.percent
            sumX += x; sumY += y; sumXY += x * y; sumXX += x * x
        }
        let n = Double(samples.count)
        let denominator = n * sumXX - sumX * sumX
        guard abs(denominator) > 1e-9 else { return nil }
        let slope = (n * sumXY - sumX * sumY) / denominator
        return max(0, slope)
    }

    /// How many tokens (and dollars) one percentage point of a limit represents.
    ///
    /// Derived by pairing consecutive API observations with the local token
    /// volume recorded in between, then taking the median ratio — median rather
    /// than mean because a single mis-aligned pair would otherwise dominate.
    static func calibrate(samples: [UsageSample], records: [UsageRecord]) -> Calibration? {
        guard samples.count >= 2 else { return nil }

        var tokenRatios: [Double] = []
        var dollarRatios: [Double] = []

        for (previous, current) in zip(samples, samples.dropFirst()) {
            let delta = current.percent - previous.percent
            guard delta >= 0.5 else { continue }   // ignore noise and roll-offs

            let interval = DateInterval(start: previous.date, end: current.date)
            let inWindow = records.within(interval)
            guard !inWindow.isEmpty else { continue }

            let tokens = Double(inWindow.tokens.fresh)
            let dollars = inWindow.cost
            guard tokens > 0 else { continue }

            tokenRatios.append(tokens / delta)
            dollarRatios.append(dollars / delta)
        }

        guard tokenRatios.count >= 3 else { return nil }
        return Calibration(
            tokensPerPercent: median(tokenRatios),
            dollarsPerPercent: median(dollarRatios)
        )
    }

    private static func median(_ values: [Double]) -> Double {
        let sorted = values.sorted()
        guard !sorted.isEmpty else { return 0 }
        let mid = sorted.count / 2
        return sorted.count.isMultiple(of: 2) ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid]
    }

    /// Assembles the full derived picture for one limit.
    static func snapshot(
        kind: LimitKind,
        label: String,
        percent: Double,
        resetsAt: Date?,
        isActive: Bool,
        history: [UsageSample],
        records: [UsageRecord],
        now: Date = Date()
    ) -> LimitSnapshot {
        var snapshot = LimitSnapshot(
            kind: kind, label: label, percent: percent,
            resetsAt: resetsAt, isActive: isActive
        )

        let remainingPercent = max(0, 100 - percent)
        let hoursRemaining = resetsAt.map { max(0, $0.timeIntervalSince(now)) / 3600 }

        // Allowance: spend this fast from now on and you land exactly at 100%
        // the moment the window resets. Everything else is measured against it.
        if let hoursRemaining, hoursRemaining > 0.01 {
            snapshot.allowancePercentPerHour = remainingPercent / hoursRemaining
        }

        let rate = burnRate(samples: history, now: now)

        if let rate, let allowance = snapshot.allowancePercentPerHour, allowance > 0.0001 {
            snapshot.paceRatio = rate / allowance
        }

        if let rate, rate > 0.01, remainingPercent > 0 {
            snapshot.exhaustsAt = now.addingTimeInterval(remainingPercent / rate * 3600)
        }

        if let calibration = calibrate(samples: history, records: records) {
            snapshot.remainingTokens = remainingPercent * calibration.tokensPerPercent
            snapshot.remainingValueUSD = remainingPercent * calibration.dollarsPerPercent
            if let allowance = snapshot.allowancePercentPerHour {
                snapshot.allowanceTokensPerHour = allowance * calibration.tokensPerPercent
            }
        }

        return snapshot
    }
}
