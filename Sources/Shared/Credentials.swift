import Foundation

/// OAuth credentials issued to Claude Code and stored in the login keychain.
///
/// Runway never mints or refreshes tokens — Claude Code owns that lifecycle. We
/// re-read the keychain on every poll so that a refresh performed by the CLI is
/// picked up automatically on the next tick.
struct OAuthCredentials: Equatable {
    var accessToken: String
    var expiresAt: Date?
    var subscriptionType: String?
    var rateLimitTier: String?

    var isExpired: Bool {
        guard let expiresAt else { return false }
        return expiresAt <= Date()
    }
}

enum CredentialsError: LocalizedError {
    case notFound
    case malformed(String)
    case keychainDenied(OSStatus)

    var errorDescription: String? {
        switch self {
        case .notFound:
            return "No Claude Code credentials found. Run `claude` once to sign in."
        case .malformed(let detail):
            return "Credentials could not be read: \(detail)"
        case .keychainDenied(let status):
            return "Keychain access was denied (OSStatus \(status)). Allow Runway to read the “Claude Code-credentials” item."
        }
    }
}

enum CredentialsLoader {
    static let keychainService = "Claude Code-credentials"

    /// Keychain first (that's where Claude Code puts it on macOS), then the
    /// on-disk fallback the CLI uses when the keychain is unavailable.
    static func load() throws -> OAuthCredentials {
        if let data = try keychainData() {
            return try parse(data)
        }
        if let data = fileData() {
            return try parse(data)
        }
        throw CredentialsError.notFound
    }

    // MARK: - Sources

    private static func keychainData() throws -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            return item as? Data
        case errSecItemNotFound:
            return nil
        default:
            throw CredentialsError.keychainDenied(status)
        }
    }

    private static func fileData() -> Data? {
        for url in credentialFileCandidates() {
            if let data = try? Data(contentsOf: url) { return data }
        }
        return nil
    }

    static func credentialFileCandidates() -> [URL] {
        var urls: [URL] = []
        if let override = ProcessInfo.processInfo.environment["CLAUDE_CONFIG_DIR"], !override.isEmpty {
            urls.append(URL(fileURLWithPath: override).appendingPathComponent(".credentials.json"))
        }
        urls.append(ClaudeHome.directory.appendingPathComponent(".credentials.json"))
        return urls
    }

    // MARK: - Parsing

    private static func parse(_ data: Data) throws -> OAuthCredentials {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw CredentialsError.malformed("payload is not JSON")
        }
        guard let oauth = root["claudeAiOauth"] as? [String: Any] else {
            throw CredentialsError.malformed("missing claudeAiOauth")
        }
        guard let token = oauth["accessToken"] as? String, !token.isEmpty else {
            throw CredentialsError.malformed("missing accessToken")
        }

        var expiry: Date?
        // `expiresAt` is milliseconds since epoch.
        if let ms = oauth["expiresAt"] as? Double {
            expiry = Date(timeIntervalSince1970: ms / 1000)
        }

        return OAuthCredentials(
            accessToken: token,
            expiresAt: expiry,
            subscriptionType: oauth["subscriptionType"] as? String,
            rateLimitTier: oauth["rateLimitTier"] as? String
        )
    }
}

enum ClaudeHome {
    /// `$CLAUDE_CONFIG_DIR` if set, otherwise `~/.claude`.
    static var directory: URL {
        if let override = ProcessInfo.processInfo.environment["CLAUDE_CONFIG_DIR"], !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".claude")
    }

    static var projectsDirectory: URL {
        directory.appendingPathComponent("projects")
    }
}
