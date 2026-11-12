import Foundation

/// Eintrag aus /files/list.
struct RemoteItem: Codable, Identifiable, Hashable {
    let name: String
    let path: String
    let isDir: Bool
    let size: Int64
    let modifiedAt: Int64
    let rev: Int64
    let modifiedBy: String?

    var id: String { path }
    var modified: Date { Date(timeIntervalSince1970: TimeInterval(modifiedAt) / 1000) }
}

struct ListResponse: Codable {
    let path: String
    let entries: [RemoteItem]
}

struct Me: Codable {
    let id: Int64
    let username: String
    let isAdmin: Bool
    let quotaBytes: Int64
    let usedBytes: Int64
}

struct UserInfo: Codable, Identifiable, Hashable {
    let id: Int64
    let username: String
    let isAdmin: Bool
    let quotaBytes: Int64
    let usedBytes: Int64
}

struct Share: Codable, Identifiable, Hashable {
    let token: String
    let path: String
    let createdAt: Int64
    let allowUpload: Bool
    let hasPassword: Bool
    let expiresAt: Int64?
    let notify: Bool

    var id: String { token }
    var name: String { path.split(separator: "/").last.map(String.init) ?? path }
    var expires: Date? { expiresAt.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000) } }
}

/// Teil-Änderung einer Freigabe. `password: ""` entfernt das Passwort,
/// `expiresAt: 0` das Ablaufdatum.
struct ShareSettings: Encodable {
    var allowUpload: Bool?
    var password: String?
    var expiresAt: Int64?
    var notify: Bool?
}

/// Gespeichertes Konto (Token liegt im Schlüsselbund).
struct Account: Codable, Identifiable, Hashable {
    var id: String
    var server: String
    var username: String
    var deviceName: String

    var host: String { URL(string: server)?.host ?? server }
    var label: String { "\(username) · \(host)" }
}

enum APIError: LocalizedError {
    case unauthorized
    case notFound
    case conflict(String)
    case quota
    case server(Int, String)
    case badURL

    var errorDescription: String? {
        switch self {
        case .unauthorized: return "Anmeldung abgelaufen. Bitte neu anmelden."
        case .notFound: return "Nicht gefunden."
        case .conflict(let m): return m.isEmpty ? "Existiert bereits." : m
        case .quota: return "Kein Speicherplatz mehr frei."
        case .server(let s, let m): return m.isEmpty ? "Serverfehler (\(s))." : m
        case .badURL: return "Ungültige Serveradresse."
        }
    }
}
