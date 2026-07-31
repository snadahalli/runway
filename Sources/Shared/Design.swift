import SwiftUI

/// Runway's severity rule.
///
/// Deliberately not "percentage used" alone. A limit at 60% with eight hours
/// left is fine; the same 60% with forty minutes left and a 3x pace is not. So
/// severity is the worse of two independent readings — how full it is, and how
/// fast it's filling — with the pace reading damped early in a window, where a
/// couple of heavy minutes would otherwise project absurd slopes.
enum Severity: Int, Comparable {
    case calm = 0
    case watch = 1
    case tight = 2

    static func < (lhs: Severity, rhs: Severity) -> Bool { lhs.rawValue < rhs.rawValue }

    var color: Color {
        switch self {
        case .calm:  return Color(red: 0.24, green: 0.68, blue: 0.45)
        case .watch: return Color(red: 0.88, green: 0.63, blue: 0.18)
        case .tight: return Color(red: 0.85, green: 0.30, blue: 0.28)
        }
    }

    var symbol: String {
        switch self {
        case .calm:  return "gauge.with.dots.needle.33percent"
        case .watch: return "gauge.with.dots.needle.67percent"
        case .tight: return "gauge.with.dots.needle.100percent"
        }
    }

    static func of(_ limit: LimitSnapshot) -> Severity {
        var level: Severity = .calm

        switch limit.percent {
        case 90...: level = .tight
        case 70..<90: level = .watch
        default: break
        }

        // Pace only earns a vote once there's enough of the window behind us for
        // the slope to mean anything.
        let elapsedFraction: Double
        if let remaining = limit.timeRemaining {
            elapsedFraction = 1 - min(1, remaining / limit.windowSeconds)
        } else {
            elapsedFraction = 1
        }

        if elapsedFraction > 0.15 || limit.percent >= 15, let ratio = limit.paceRatio {
            let paceLevel: Severity = ratio >= 2 ? .tight : (ratio >= 1.15 ? .watch : .calm)
            level = max(level, paceLevel)
        }

        return level
    }
}

extension Color {
    static let trackBase = Color.primary.opacity(0.10)
    static let trackProjection = Color.primary.opacity(0.22)
}

/// A horizontal window track: solid = spent, hatched = where the current pace
/// lands you by reset, notch = the reset itself.
///
/// Chosen over a ring because a window is a span of time, and a span reads
/// naturally left-to-right — you can see "spent", "projected" and "headroom" as
/// three adjacent quantities instead of decoding an arc.
struct LimitTrack: View {
    var limit: LimitSnapshot
    var height: CGFloat = 10

    private var projectedPercent: Double {
        // rate × hoursRemaining simplifies to paceRatio × remainingPercent.
        guard let ratio = limit.paceRatio else { return limit.percent }
        return limit.percent + ratio * max(0, 100 - limit.percent)
    }

    var body: some View {
        let severity = Severity.of(limit)
        GeometryReader { geo in
            let width = geo.size.width
            let spent = width * min(1, limit.percent / 100)
            let projected = width * min(1, projectedPercent / 100)

            ZStack(alignment: .leading) {
                Capsule().fill(Color.trackBase)

                if projected > spent {
                    HatchedBar()
                        .fill(severity.color.opacity(0.30))
                        .frame(width: projected)
                        .clipShape(Capsule())
                }

                Capsule()
                    .fill(severity.color)
                    .frame(width: max(spent, spent > 0 ? 3 : 0))

                if projectedPercent > 100 {
                    // Overflow marker: the projection runs off the end of the window.
                    Image(systemName: "chevron.right.2")
                        .font(.system(size: height * 0.8, weight: .bold))
                        .foregroundStyle(severity.color)
                        .offset(x: width - height)
                }
            }
        }
        .frame(height: height)
    }
}

/// Diagonal hatching so the projection can never be mistaken for measured usage.
struct HatchedBar: Shape {
    var spacing: CGFloat = 5

    func path(in rect: CGRect) -> Path {
        var path = Path()
        var x = -rect.height
        while x < rect.width {
            path.move(to: CGPoint(x: x, y: rect.maxY))
            path.addLine(to: CGPoint(x: x + rect.height, y: rect.minY))
            path.addLine(to: CGPoint(x: x + rect.height + spacing * 0.6, y: rect.minY))
            path.addLine(to: CGPoint(x: x + spacing * 0.6, y: rect.maxY))
            path.closeSubpath()
            x += spacing
        }
        return path
    }
}

/// Percentage-over-time trace for the window currently in flight.
struct Sparkline: View {
    var samples: [UsageSample]
    var color: Color

    var body: some View {
        GeometryReader { geo in
            let points = samples.suffix(120)
            if points.count >= 2,
               let first = points.first?.date,
               let last = points.last?.date,
               last.timeIntervalSince(first) > 0 {
                let span = last.timeIntervalSince(first)
                let maxValue = max(1, points.map(\.percent).max() ?? 1)

                Path { path in
                    for (index, sample) in points.enumerated() {
                        let x = geo.size.width * (sample.date.timeIntervalSince(first) / span)
                        let y = geo.size.height * (1 - sample.percent / maxValue)
                        let point = CGPoint(x: x, y: y)
                        index == 0 ? path.move(to: point) : path.addLine(to: point)
                    }
                }
                .stroke(color, style: StrokeStyle(lineWidth: 1.5, lineCap: .round, lineJoin: .round))
            } else {
                Rectangle()
                    .fill(Color.trackBase)
                    .frame(height: 1)
                    .frame(maxHeight: .infinity, alignment: .center)
            }
        }
    }
}

/// Small label/value pair used throughout the popover.
struct Stat: View {
    var label: String
    var value: String
    var tint: Color?

    init(_ label: String, _ value: String, tint: Color? = nil) {
        self.label = label
        self.value = value
        self.tint = tint
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
            Text(value)
                .font(.system(size: 13, weight: .medium, design: .rounded))
                .foregroundStyle(tint ?? .primary)
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
