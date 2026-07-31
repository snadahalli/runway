import Foundation

enum Fmt {
    static func tokens(_ value: Int) -> String { tokens(Double(value)) }

    static func tokens(_ value: Double) -> String {
        switch abs(value) {
        case 1_000_000_000...: return String(format: "%.2fB", value / 1_000_000_000)
        case 1_000_000...:     return String(format: "%.1fM", value / 1_000_000)
        case 10_000...:        return String(format: "%.0fK", value / 1_000)
        case 1_000...:         return String(format: "%.1fK", value / 1_000)
        default:               return String(format: "%.0f", value)
        }
    }

    static func usd(_ value: Double) -> String {
        if value >= 1000 { return String(format: "$%.0f", value) }
        if value >= 10 { return String(format: "$%.1f", value) }
        return String(format: "$%.2f", value)
    }

    static func percent(_ value: Double) -> String {
        value >= 10 ? String(format: "%.0f%%", value) : String(format: "%.1f%%", value)
    }

    /// "2h 14m", "48m", "31s" — compact, no leading zeros, never negative.
    static func duration(_ seconds: TimeInterval) -> String {
        let total = Int(max(0, seconds))
        let days = total / 86400
        let hours = (total % 86400) / 3600
        let minutes = (total % 3600) / 60
        if days > 0 { return hours > 0 ? "\(days)d \(hours)h" : "\(days)d" }
        if hours > 0 { return minutes > 0 ? "\(hours)h \(minutes)m" : "\(hours)h" }
        if minutes > 0 { return "\(minutes)m" }
        return "\(total)s"
    }

    /// "today 14:20", "Thu 09:05" — a wall clock the user can act on, rather
    /// than a countdown they have to mentally add to the current time.
    static func clock(_ date: Date, now: Date = Date()) -> String {
        let calendar = Calendar.current
        let time = timeFormatter.string(from: date)
        if calendar.isDate(date, inSameDayAs: now) { return time }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now),
           calendar.isDate(date, inSameDayAs: tomorrow) { return "tomorrow \(time)" }
        return "\(weekdayFormatter.string(from: date)) \(time)"
    }

    static func ratio(_ value: Double) -> String {
        value >= 10 ? String(format: "%.0f×", value) : String(format: "%.1f×", value)
    }

    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.setLocalizedDateFormatFromTemplate("j:mm")
        return f
    }()

    private static let weekdayFormatter: DateFormatter = {
        let f = DateFormatter()
        f.setLocalizedDateFormatFromTemplate("EEE")
        return f
    }()
}
