import Foundation
import Security
import SwiftUI
import UIKit

/// Tokens im Schlüsselbund, Konten in den UserDefaults.
enum Keychain {
    private static let service = "tech.dropcraft.token"

    static func set(_ token: String, for id: String) {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: id]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = Data(token.utf8)
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        if SecItemAdd(add as CFDictionary, nil) == errSecSuccess {
            UserDefaults.standard.removeObject(forKey: fallbackKey(id))
        } else {
            // Ohne Schlüsselbund-Berechtigung (unsignierte Builds): Ersatzablage
            UserDefaults.standard.set(token, forKey: fallbackKey(id))
        }
    }

    private static func fallbackKey(_ id: String) -> String { "token." + id }

    static func get(_ id: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: id,
            kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var out: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data else {
            return UserDefaults.standard.string(forKey: fallbackKey(id))
        }
        return String(data: data, encoding: .utf8)
    }

    static func delete(_ id: String) {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: id]
        SecItemDelete(query as CFDictionary)
        UserDefaults.standard.removeObject(forKey: fallbackKey(id))
    }
}

/// Laufender Up- oder Download (für die Fortschrittsleiste).
struct Transfer: Identifiable, Equatable {
    let id = UUID()
    let name: String
    let upload: Bool
    var progress: Double = 0
}

@MainActor
final class AppModel: ObservableObject {
    @Published var accounts: [Account] = []
    @Published var selectedID: String?
    @Published var me: Me?
    @Published var needsLogin = false
    @Published var transfers: [Transfer] = []
    @Published var toast: String?

    private let accountsKey = "accounts"
    private let selectedKey = "selectedAccount"

    init() {
        if let data = UserDefaults.standard.data(forKey: accountsKey), let list = try? JSONDecoder().decode([Account].self, from: data) {
            accounts = list
        }
        selectedID = UserDefaults.standard.string(forKey: selectedKey) ?? accounts.first?.id
        if selected == nil { selectedID = accounts.first?.id }
        #if DEBUG
        // Für automatische Tests: DC_AUTOLOGIN="server|benutzer|passwort"
        if let auto = ProcessInfo.processInfo.environment["DC_AUTOLOGIN"], accounts.isEmpty {
            let p = auto.split(separator: "|").map(String.init)
            if p.count == 3 { Task { try? await login(server: p[0], username: p[1], password: p[2]) } }
        }
        #endif
    }

    var selected: Account? { accounts.first { $0.id == selectedID } }

    /// API-Client für das gewählte Konto.
    var api: API? {
        guard let a = selected, let t = Keychain.get(a.id) else { return nil }
        return try? API(server: a.server, token: t)
    }

    func select(_ id: String) {
        selectedID = id
        UserDefaults.standard.set(id, forKey: selectedKey)
        me = nil
        needsLogin = false
        Task { await refreshMe() }
    }

    private func save() {
        if let data = try? JSONEncoder().encode(accounts) { UserDefaults.standard.set(data, forKey: accountsKey) }
    }

    func login(server: String, username: String, password: String) async throws {
        let device = UIDevice.current.name
        let api = try await API.login(server: server, username: username, password: password, device: device)
        let host = api.base.host ?? server
        // Gleiches Konto erneut anmelden ersetzt das alte
        let id = accounts.first { $0.username == username && $0.host == host }?.id ?? UUID().uuidString
        Keychain.set(api.token ?? "", for: id)
        let acc = Account(id: id, server: api.server, username: username, deviceName: device)
        if let i = accounts.firstIndex(where: { $0.id == id }) { accounts[i] = acc } else { accounts.append(acc) }
        save()
        select(id)
    }

    func remove(_ id: String) async {
        if let a = accounts.first(where: { $0.id == id }), let t = Keychain.get(a.id), let api = try? API(server: a.server, token: t) {
            await api.logout()
        }
        Keychain.delete(id)
        accounts.removeAll { $0.id == id }
        save()
        if selectedID == id {
            if let first = accounts.first { select(first.id) } else { selectedID = nil; me = nil }
        }
    }

    func refreshMe() async {
        guard let api else { return }
        do {
            me = try await api.me()
            needsLogin = false
        } catch APIError.unauthorized {
            needsLogin = true
        } catch {}
    }

    /// Fehler einheitlich melden; abgelaufene Anmeldung merken.
    func report(_ error: Error) {
        if case APIError.unauthorized = error { needsLogin = true }
        show(error.localizedDescription)
    }

    func show(_ message: String) {
        toast = message
        Task {
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            if toast == message { toast = nil }
        }
    }

    // MARK: Übertragungen

    func startTransfer(_ name: String, upload: Bool) -> UUID {
        let t = Transfer(name: name, upload: upload)
        transfers.append(t)
        return t.id
    }

    func updateTransfer(_ id: UUID, _ p: Double) {
        if let i = transfers.firstIndex(where: { $0.id == id }) { transfers[i].progress = p }
    }

    func endTransfer(_ id: UUID) { transfers.removeAll { $0.id == id } }

    /// Lokale Kopie einer Datei (Cache), z. B. für Vorschau und Teilen.
    func cacheURL(for path: String) -> URL? {
        guard let a = selected else { return nil }
        let base = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("DropCraft").appendingPathComponent(a.id)
        return path.split(separator: "/").reduce(base) { $0.appendingPathComponent(String($1)) }
    }
}

// MARK: Formatierung

enum Fmt {
    static func size(_ bytes: Int64) -> String {
        let f = ByteCountFormatter()
        f.countStyle = .file
        f.allowedUnits = [.useKB, .useMB, .useGB, .useTB]
        return f.string(fromByteCount: bytes)
    }

    static func date(_ d: Date) -> String {
        if Calendar.current.isDateInToday(d) { return d.formatted(date: .omitted, time: .shortened) }
        return d.formatted(.dateTime.day().month(.twoDigits).year())
    }

    /// "bis 18.11.2026, 23:57 · noch 6 Tage" bzw. "abgelaufen"
    static func expiry(_ d: Date) -> String {
        let left = d.timeIntervalSinceNow
        if left <= 0 { return "abgelaufen" }
        let hours = Int(left / 3600)
        let rest: String
        switch hours {
        case 0: rest = "unter 1 Stunde"
        case 1: rest = "noch 1 Stunde"
        case 2...47: rest = "noch \(hours) Stunden"
        default: rest = "noch \(hours / 24) Tage"
        }
        return "bis \(d.formatted(date: .numeric, time: .shortened)) · \(rest)"
    }
}
