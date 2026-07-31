import Foundation

/// Dollar-equivalent value of subscription usage.
///
/// Subscription plans don't bill per token, so nothing here is an invoice — it
/// answers "what would this have cost on the pay-as-you-go API?", which is the
/// only honest way to compare a month of Claude Code against the plan price.
///
/// Cache tokens are priced separately and correctly, which most trackers skip:
/// a 5-minute cache write costs 1.25x base input, a 1-hour write costs 2x, and
/// a cache read costs 0.1x. On a long Claude Code session cache reads dominate
/// the token count, so pricing them at full input rate overstates value by an
/// order of magnitude.
struct ModelRate {
    /// USD per million tokens.
    var input: Double
    var output: Double

    var cacheWrite5m: Double { input * 1.25 }
    var cacheWrite1h: Double { input * 2.0 }
    var cacheRead: Double { input * 0.1 }
}

enum Pricing {
    /// Longest-prefix match wins, so dated snapshots and `[1m]` suffixes resolve
    /// to their family without needing an entry each.
    static let table: [(prefix: String, rate: ModelRate)] = [
        ("claude-fable-5",    ModelRate(input: 10, output: 50)),
        ("claude-mythos",     ModelRate(input: 10, output: 50)),
        ("claude-opus-5",     ModelRate(input: 5,  output: 25)),
        ("claude-opus-4-8",   ModelRate(input: 5,  output: 25)),
        ("claude-opus-4-7",   ModelRate(input: 5,  output: 25)),
        ("claude-opus-4-6",   ModelRate(input: 5,  output: 25)),
        ("claude-opus-4-5",   ModelRate(input: 5,  output: 25)),
        ("claude-opus-4",     ModelRate(input: 15, output: 75)),
        ("claude-opus",       ModelRate(input: 15, output: 75)),
        ("claude-sonnet-5",   ModelRate(input: 3,  output: 15)),
        ("claude-sonnet-4-6", ModelRate(input: 3,  output: 15)),
        ("claude-sonnet-4-5", ModelRate(input: 3,  output: 15)),
        ("claude-sonnet",     ModelRate(input: 3,  output: 15)),
        ("claude-haiku-4-5",  ModelRate(input: 1,  output: 5)),
        ("claude-haiku",      ModelRate(input: 0.8, output: 4)),
    ]

    static let fallback = ModelRate(input: 5, output: 25)

    static func rate(for model: String) -> ModelRate {
        let key = model.lowercased()
        var best: (Int, ModelRate)?
        for entry in table where key.hasPrefix(entry.prefix) {
            if best == nil || entry.prefix.count > best!.0 {
                best = (entry.prefix.count, entry.rate)
            }
        }
        return best?.1 ?? fallback
    }

    /// Human-facing family label used for grouping in the ledger.
    static func family(for model: String) -> String {
        let key = model.lowercased()
        if key.contains("fable") { return "Fable" }
        if key.contains("mythos") { return "Mythos" }
        if key.contains("opus") { return "Opus" }
        if key.contains("sonnet") { return "Sonnet" }
        if key.contains("haiku") { return "Haiku" }
        return "Other"
    }

    static func cost(of tokens: TokenTotals, model: String) -> Double {
        let rate = rate(for: model)
        let perMillion = 1_000_000.0
        return (Double(tokens.input) * rate.input
              + Double(tokens.output) * rate.output
              + Double(tokens.cacheWrite5m) * rate.cacheWrite5m
              + Double(tokens.cacheWrite1h) * rate.cacheWrite1h
              + Double(tokens.cacheRead) * rate.cacheRead) / perMillion
    }
}

/// Token counts split the way the API actually bills them.
struct TokenTotals: Codable, Equatable {
    var input: Int = 0
    var output: Int = 0
    var cacheWrite5m: Int = 0
    var cacheWrite1h: Int = 0
    var cacheRead: Int = 0

    var billable: Int { input + output + cacheWrite5m + cacheWrite1h + cacheRead }
    /// Tokens that represent genuinely new work, ignoring cache replay.
    var fresh: Int { input + output + cacheWrite5m + cacheWrite1h }

    static func + (lhs: TokenTotals, rhs: TokenTotals) -> TokenTotals {
        TokenTotals(
            input: lhs.input + rhs.input,
            output: lhs.output + rhs.output,
            cacheWrite5m: lhs.cacheWrite5m + rhs.cacheWrite5m,
            cacheWrite1h: lhs.cacheWrite1h + rhs.cacheWrite1h,
            cacheRead: lhs.cacheRead + rhs.cacheRead
        )
    }

    static func += (lhs: inout TokenTotals, rhs: TokenTotals) { lhs = lhs + rhs }
}
