import Foundation

/// Schlanker Client für die DropCraft-API (gleiche Endpunkte wie die Desktop-App).
final class API {
    let base: URL
    let token: String?

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()
    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }()

    init(server: String, token: String?) throws {
        var s = server.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.hasSuffix("/") { s.removeLast() }
        if !s.contains("://") { s = "https://" + s }
        guard let url = URL(string: s), url.host != nil else { throw APIError.badURL }
        base = url
        self.token = token
    }

    /// Normalisierte Serveradresse, z. B. "https://dropcraft.tech".
    var server: String { base.absoluteString }

    func shareURL(_ token: String) -> URL { base.appendingPathComponent("s").appendingPathComponent(token) }

    // MARK: Grundlagen

    private func url(_ path: String, _ query: [String: String] = [:]) -> URL {
        var c = URLComponents(url: base.appendingPathComponent("api" + path), resolvingAgainstBaseURL: false)!
        if !query.isEmpty {
            c.queryItems = query.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) }
            // „+“ wird sonst vom Server als Leerzeichen gelesen
            c.percentEncodedQuery = c.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        }
        return c.url!
    }

    private func request(_ method: String, _ path: String, query: [String: String] = [:], json: Encodable? = nil) throws -> URLRequest {
        var r = URLRequest(url: url(path, query))
        r.httpMethod = method
        if let token { r.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let json {
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
            r.httpBody = try API.encoder.encode(AnyEncodable(json))
        }
        return r
    }

    static func check(_ data: Data, _ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse else { return }
        if (200..<300).contains(http.statusCode) { return }
        let message = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["message"] as? String ?? ""
        switch http.statusCode {
        case 401: throw APIError.unauthorized
        case 404: throw APIError.notFound
        case 409: throw APIError.conflict(message)
        case 507: throw APIError.quota
        default: throw APIError.server(http.statusCode, message)
        }
    }

    private func send(_ r: URLRequest) async throws -> Data {
        let (data, resp) = try await URLSession.shared.data(for: r)
        try API.check(data, resp)
        return data
    }

    private func get<T: Decodable>(_ path: String, query: [String: String] = [:]) async throws -> T {
        try API.decoder.decode(T.self, from: try await send(try request("GET", path, query: query)))
    }

    // MARK: Konto

    static func login(server: String, username: String, password: String, device: String) async throws -> API {
        struct Body: Encodable { let username: String; let password: String; let deviceName: String }
        struct Resp: Decodable { let token: String }
        let api = try API(server: server, token: nil)
        let data = try await api.send(try api.request("POST", "/auth/login", json: Body(username: username, password: password, deviceName: device)))
        let r = try decoder.decode(Resp.self, from: data)
        return try API(server: api.server, token: r.token)
    }

    func logout() async { _ = try? await send(try request("POST", "/auth/logout")) }

    func me() async throws -> Me { try await get("/me") }

    // MARK: Dateien

    func list(_ dir: String) async throws -> [RemoteItem] {
        let r: ListResponse = try await get("/files/list", query: ["path": dir])
        return r.entries
    }

    func mkdir(_ path: String) async throws {
        struct B: Encodable { let path: String }
        _ = try await send(try request("POST", "/files/mkdir", json: B(path: path)))
    }

    func delete(_ path: String) async throws {
        struct B: Encodable { let path: String }
        _ = try await send(try request("POST", "/files/delete", json: B(path: path)))
    }

    func move(_ from: String, to: String) async throws {
        struct B: Encodable { let from: String; let to: String }
        _ = try await send(try request("POST", "/files/move", json: B(from: from, to: to)))
    }

    /// Lädt eine lokale Datei hoch. `onProgress` bekommt 0…1.
    func upload(file: URL, to remotePath: String, onProgress: @escaping @Sendable (Double) -> Void) async throws {
        let attrs = try? FileManager.default.attributesOfItem(atPath: file.path)
        let mtime = Int64(((attrs?[.modificationDate] as? Date) ?? Date()).timeIntervalSince1970 * 1000)
        var r = try request("PUT", "/files/upload", query: ["path": remotePath, "mtime": String(mtime)])
        r.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        let delegate = ProgressDelegate(onProgress)
        let (data, resp) = try await URLSession.shared.upload(for: r, fromFile: file, delegate: delegate)
        try API.check(data, resp)
    }

    /// Lädt eine Datei nach `dest` (überschreibt). `onProgress` bekommt 0…1.
    func download(_ remotePath: String, to dest: URL, onProgress: @escaping @Sendable (Double) -> Void) async throws {
        let r = try request("GET", "/files/download", query: ["path": remotePath])
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            var observation: NSKeyValueObservation?
            let task = URLSession.shared.downloadTask(with: r) { tmp, resp, err in
                defer { observation?.invalidate(); observation = nil }
                if let err { return cont.resume(throwing: err) }
                guard let tmp, let resp else { return cont.resume(throwing: APIError.server(0, "")) }
                do {
                    if let http = resp as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                        try API.check((try? Data(contentsOf: tmp)) ?? Data(), resp)
                    }
                    // Die Temp-Datei verschwindet nach diesem Block: jetzt verschieben.
                    let fm = FileManager.default
                    try fm.createDirectory(at: dest.deletingLastPathComponent(), withIntermediateDirectories: true)
                    try? fm.removeItem(at: dest)
                    try fm.moveItem(at: tmp, to: dest)
                    cont.resume()
                } catch {
                    cont.resume(throwing: error)
                }
            }
            observation = task.progress.observe(\.fractionCompleted) { p, _ in onProgress(p.fractionCompleted) }
            task.resume()
        }
    }

    // MARK: Freigaben

    func shares() async throws -> [Share] { try await get("/shares") }

    func createShare(_ path: String) async throws -> Share {
        struct B: Encodable { let path: String }
        return try API.decoder.decode(Share.self, from: try await send(try request("POST", "/shares", json: B(path: path))))
    }

    func updateShare(_ token: String, _ s: ShareSettings) async throws {
        _ = try await send(try request("PATCH", "/shares/\(token)", json: s))
    }

    func deleteShare(_ token: String) async throws {
        _ = try await send(try request("DELETE", "/shares/\(token)"))
    }

    /// Einmal-Link zur Konto-Website auf dem Server, gleich angemeldet.
    func webLoginURL(admin: Bool) async throws -> URL {
        struct B: Encodable { let page: String }
        struct R: Decodable { let url: String }
        let data = try await send(try request("POST", "/account/web-login", json: B(page: admin ? "admin" : "")))
        let r = try API.decoder.decode(R.self, from: data)
        return URL(string: server + r.url) ?? base.appendingPathComponent("account")
    }

    // MARK: Benutzerverwaltung (Admin)

    func users() async throws -> [UserInfo] { try await get("/admin/users") }

    func createUser(_ name: String, password: String, quotaGB: Double) async throws {
        struct B: Encodable { let username: String; let password: String; let quotaGb: Double; let isAdmin: Bool }
        _ = try await send(try request("POST", "/admin/users", json: B(username: name, password: password, quotaGb: quotaGB, isAdmin: false)))
    }

    func updateUser(_ id: Int64, quotaGB: Double?, password: String?) async throws {
        struct B: Encodable { let quotaGb: Double?; let password: String? }
        _ = try await send(try request("PATCH", "/admin/users/\(id)", json: B(quotaGb: quotaGB, password: password)))
    }

    func deleteUser(_ id: Int64) async throws {
        _ = try await send(try request("DELETE", "/admin/users/\(id)"))
    }
}

/// Fortschritt für Uploads.
final class ProgressDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    private let onProgress: @Sendable (Double) -> Void

    init(_ onProgress: @escaping @Sendable (Double) -> Void) { self.onProgress = onProgress }

    func urlSession(_ session: URLSession, task: URLSessionTask, didSendBodyData bytesSent: Int64, totalBytesSent: Int64, totalBytesExpectedToSend: Int64) {
        if totalBytesExpectedToSend > 0 { onProgress(Double(totalBytesSent) / Double(totalBytesExpectedToSend)) }
    }
}

/// Hilfstyp, um beliebige `Encodable` zu kodieren.
private struct AnyEncodable: Encodable {
    let value: Encodable
    init(_ value: Encodable) { self.value = value }
    func encode(to encoder: Encoder) throws { try value.encode(to: encoder) }
}

/// Pfade wie auf dem Server: "Ordner/Unterordner/Datei.txt", ohne führenden "/".
enum RemotePath {
    static func join(_ dir: String, _ name: String) -> String { dir.isEmpty ? name : "\(dir)/\(name)" }
    static func parent(_ path: String) -> String {
        guard let i = path.lastIndex(of: "/") else { return "" }
        return String(path[..<i])
    }
    static func name(_ path: String) -> String { path.split(separator: "/").last.map(String.init) ?? path }
}
