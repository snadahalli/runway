import Foundation

// MARK: - Wire types

/// Response of `GET /api/oauth/usage`.
///
/// The `limits` array is the forward-compatible surface — it carries one entry
/// per active limit with a `kind`/`group`/`scope`, so new limit kinds appear
/// without a schema change. The legacy `five_hour` / `seven_day` objects are
/// decoded as a fallback for older server builds.
struct UsageResponse: Decodable {
    struct Window: Decodable {
        var utilization: Double
        var resetsAt: Date?
        var limitDollars: Double?
        var usedDollars: Double?

        enum CodingKeys: String, CodingKey {
            case utilization
            case resetsAt = "resets_at"
            case limitDollars = "limit_dollars"
            case usedDollars = "used_dollars"
        }
    }

    struct Scope: Decodable {
        struct Model: Decodable { var id: String?; var displayName: String?
            enum CodingKeys: String, CodingKey { case id; case displayName = "display_name" } }
        var model: Model?
        var surface: String?
    }

    struct Limit: Decodable {
        var kind: String
        var group: String
        var percent: Double
        var severity: String?
        var resetsAt: Date?
        var scope: Scope?
        var isActive: Bool?

        enum CodingKeys: String, CodingKey {
            case kind, group, percent, severity, scope
            case resetsAt = "resets_at"
            case isActive = "is_active"
        }
    }

    struct ExtraUsage: Decodable {
        var isEnabled: Bool?
        var utilization: Double?
        var monthlyLimit: Double?
        var usedCredits: Double?

        enum CodingKeys: String, CodingKey {
            case isEnabled = "is_enabled"
            case utilization
            case monthlyLimit = "monthly_limit"
            case usedCredits = "used_credits"
        }
    }

    var fiveHour: Window?
    var sevenDay: Window?
    var limits: [Limit]?
    var extraUsage: ExtraUsage?

    enum CodingKeys: String, CodingKey {
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
        case limits
        case extraUsage = "extra_usage"
    }
}

// MARK: - Client

enum UsageAPIError: LocalizedError {
    case rateLimited(retryAfter: TimeInterval?)
    case unauthorized
    case http(Int, String?)
    case transport(Error)
    case decoding(Error)

    var errorDescription: String? {
        switch self {
        case .rateLimited(let retry):
            if let retry { return "Rate limited by the usage API. Retrying in \(Int(retry))s." }
            return "Rate limited by the usage API."
        case .unauthorized:
            return "Credentials rejected. Run `claude` to refresh your login."
        case .http(let code, let body):
            return "Usage API returned HTTP \(code)." + (body.map { " \($0)" } ?? "")
        case .transport(let error):
            return "Network error: \(error.localizedDescription)"
        case .decoding(let error):
            return "Could not decode the usage response: \(error.localizedDescription)"
        }
    }

    var isRetryable: Bool {
        switch self {
        case .rateLimited, .transport: return true
        case .http(let code, _): return code >= 500
        case .unauthorized, .decoding: return false
        }
    }
}

/// Talks to the OAuth usage endpoint that backs Claude Code's `/usage` command.
///
/// Two details matter and are easy to get wrong:
///
/// 1. `anthropic-beta: oauth-2025-04-20` is required — OAuth tokens are sent on
///    `Authorization: Bearer`, not `x-api-key`.
/// 2. `User-Agent: claude-code/<version>` is required to land in the normal
///    rate-limit bucket. Without it you get a far stricter bucket and persistent
///    429s, which is the single most common way monitors like this break.
struct UsageAPIClient {
    static let endpoint = URL(string: "https://api.anthropic.com/api/oauth/usage")!

    /// The endpoint is documented as safe at 180s intervals with a correct
    /// User-Agent. We never poll faster than this, regardless of settings.
    static let minimumPollInterval: TimeInterval = 180

    var session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 20
        config.waitsForConnectivity = false
        config.httpAdditionalHeaders = [:]
        return URLSession(configuration: config)
    }()

    var userAgentVersion: String

    func fetch(token: String) async throws -> UsageResponse {
        var request = URLRequest(url: Self.endpoint)
        request.httpMethod = "GET"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("oauth-2025-04-20", forHTTPHeaderField: "anthropic-beta")
        request.setValue("claude-code/\(userAgentVersion)", forHTTPHeaderField: "User-Agent")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.cachePolicy = .reloadIgnoringLocalCacheData

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw UsageAPIError.transport(error)
        }

        guard let http = response as? HTTPURLResponse else {
            throw UsageAPIError.http(-1, nil)
        }

        switch http.statusCode {
        case 200:
            do {
                return try Self.decoder.decode(UsageResponse.self, from: data)
            } catch {
                throw UsageAPIError.decoding(error)
            }
        case 401, 403:
            throw UsageAPIError.unauthorized
        case 429:
            let retry = (http.value(forHTTPHeaderField: "retry-after")).flatMap(TimeInterval.init)
            throw UsageAPIError.rateLimited(retryAfter: retry)
        default:
            let body = String(data: data.prefix(300), encoding: .utf8)
            throw UsageAPIError.http(http.statusCode, body)
        }
    }

    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let raw = try container.decode(String.self)
            if let date = ISO8601DateFormatter.fractional.date(from: raw) { return date }
            if let date = ISO8601DateFormatter.plain.date(from: raw) { return date }
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "Unparsable date \(raw)")
        }
        return decoder
    }()
}

extension ISO8601DateFormatter {
    static let fractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    static let plain: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()
}
