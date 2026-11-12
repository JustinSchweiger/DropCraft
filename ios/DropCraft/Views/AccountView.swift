import SwiftUI

/// Konto-Tab: Speicher, Konten, Benutzerverwaltung (nur Admin).
@MainActor
struct AccountView: View {
    @EnvironmentObject var model: AppModel
    @State private var addAccount = false
    @State private var confirmRemove = false
    @State private var cacheSize: Int64 = 0

    var body: some View {
        NavigationStack {
            Form {
                if let me = model.me {
                    Section {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(alignment: .firstTextBaseline) {
                                Text("\(Fmt.size(me.usedBytes)) belegt").font(.title3)
                                Spacer()
                                Text("von \(Fmt.size(me.quotaBytes))").font(.footnote).foregroundStyle(DC.dim)
                            }
                            QuotaBar(used: me.usedBytes, quota: me.quotaBytes)
                        }
                        .padding(.vertical, 4)
                    }
                    .listRowBackground(DC.raised)
                }

                Section {
                    ForEach(model.accounts) { a in
                        Button {
                            model.select(a.id)
                        } label: {
                            HStack {
                                Text(String(a.username.prefix(1)).uppercased())
                                    .font(.system(size: 13, weight: .semibold))
                                    .foregroundStyle(DC.accent)
                                    .frame(width: 28, height: 28)
                                    .background(DC.accentDim, in: RoundedRectangle(cornerRadius: 5))
                                VStack(alignment: .leading) {
                                    Text(a.username).foregroundStyle(DC.text)
                                    Text(a.host).font(.caption).foregroundStyle(DC.dim)
                                }
                                Spacer()
                                if a.id == model.selectedID { Image(systemName: "checkmark").foregroundStyle(DC.accent) }
                            }
                        }
                    }
                    Button { addAccount = true } label: { Label("Weiteres Konto hinzufügen", systemImage: "plus") }
                } header: {
                    SectionLabel("Konten")
                }
                .listRowBackground(DC.raised)

                if model.me?.isAdmin == true {
                    Section {
                        NavigationLink { UsersView() } label: { Label("Benutzer verwalten", systemImage: "person.2") }
                    } header: {
                        SectionLabel("Server")
                    }
                    .listRowBackground(DC.raised)
                }

                Section {
                    LabeledContent("Gerät", value: model.selected?.deviceName ?? "")
                    LabeledContent("Zwischenspeicher", value: Fmt.size(cacheSize))
                    Button("Zwischenspeicher leeren") { clearCache() }
                } header: {
                    SectionLabel("Dieses Gerät")
                } footer: {
                    Text("Geöffnete Dateien werden hier zwischengespeichert, damit sie beim nächsten Öffnen nicht erneut geladen werden.")
                }
                .listRowBackground(DC.raised)

                Section {
                    Button("Abmelden und Konto entfernen", role: .destructive) { confirmRemove = true }
                }
                .listRowBackground(DC.raised)

                Section {
                } footer: {
                    Text("DropCraft \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "")")
                        .frame(maxWidth: .infinity)
                }
            }
            .scrollContentBackground(.hidden)
            .background(DC.surface)
            .navigationTitle(model.selected?.label ?? "Konto")
            .navigationBarTitleDisplayMode(.inline)
            .refreshable { await model.refreshMe() }
            .onAppear { cacheSize = Self.cacheBytes() }
            .sheet(isPresented: $addAccount) { LoginView(isFirst: false) }
            .confirmationDialog("Konto entfernen?", isPresented: $confirmRemove, titleVisibility: .visible) {
                Button("Entfernen", role: .destructive) {
                    if let id = model.selectedID { Task { await model.remove(id) } }
                }
            } message: {
                Text("Die Dateien auf dem Server bleiben erhalten.")
            }
        }
    }

    private static var cacheDir: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("DropCraft")
    }

    private static func cacheBytes() -> Int64 {
        guard let e = FileManager.default.enumerator(at: cacheDir, includingPropertiesForKeys: [.fileSizeKey]) else { return 0 }
        var total: Int64 = 0
        for case let url as URL in e {
            total += Int64((try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
        }
        return total
    }

    private func clearCache() {
        try? FileManager.default.removeItem(at: Self.cacheDir)
        cacheSize = 0
        model.show("Zwischenspeicher geleert")
    }
}

/// Benutzer anlegen, Speicher ändern, Passwort setzen, löschen.
@MainActor
struct UsersView: View {
    @EnvironmentObject var model: AppModel
    @State private var users: [UserInfo] = []
    @State private var adding = false

    var body: some View {
        List {
            ForEach(users) { u in
                NavigationLink {
                    UserEdit(user: u) { await load() }
                } label: {
                    HStack {
                        Text(u.username).foregroundStyle(DC.text)
                        if u.isAdmin { Text("Admin").font(.caption).foregroundStyle(DC.accent) }
                        Spacer()
                        Text("\(Fmt.size(u.usedBytes)) / \(Fmt.size(u.quotaBytes))").font(.caption).foregroundStyle(DC.dim)
                    }
                }
                .disabled(u.isAdmin)
                .listRowBackground(DC.raised)
            }
        }
        .scrollContentBackground(.hidden)
        .background(DC.surface)
        .navigationTitle("Benutzer")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) { Button { adding = true } label: { Image(systemName: "plus") } }
        }
        .sheet(isPresented: $adding) { NewUserSheet { await load() } }
        .refreshable { await load() }
        .task { await load() }
    }

    private func load() async {
        guard let api = model.api else { return }
        do { users = try await api.users() } catch { model.report(error) }
    }
}

@MainActor
struct NewUserSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss
    var onDone: () async -> Void

    @State private var name = ""
    @State private var password = ""
    @State private var gb = "50"
    @State private var busy = false

    var body: some View {
        NavigationStack {
            Form {
                TextField("Name", text: $name).textInputAutocapitalization(.never).autocorrectionDisabled()
                SecureField("Passwort (mind. 10 Zeichen)", text: $password)
                HStack {
                    TextField("Speicher", text: $gb).keyboardType(.decimalPad)
                    Text("GB").foregroundStyle(DC.dim)
                }
            }
            .navigationTitle("Neuer Benutzer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Abbrechen") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Anlegen") { Task { await create() } }
                        .disabled(busy || name.trimmingCharacters(in: .whitespaces).count < 3 || password.count < 10 || parseGB(gb) == nil)
                }
            }
        }
    }

    private func create() async {
        guard let api = model.api, let q = parseGB(gb) else { return }
        busy = true
        do {
            try await api.createUser(name.trimmingCharacters(in: .whitespaces), password: password, quotaGB: q)
            await onDone()
            model.show("„\(name)“ angelegt")
            dismiss()
        } catch { model.report(error) }
        busy = false
    }
}

@MainActor
struct UserEdit: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let user: UserInfo
    var onDone: () async -> Void

    @State private var gb = ""
    @State private var password = ""
    @State private var confirmDelete = false

    var body: some View {
        Form {
            Section("Speicher") {
                HStack {
                    TextField("GB", text: $gb).keyboardType(.decimalPad)
                    Text("GB").foregroundStyle(DC.dim)
                }
            }
            Section("Neues Passwort") {
                SecureField("leer = unverändert", text: $password)
            }
            Section {
                Button("Speichern") { Task { await save() } }
                    .disabled(parseGB(gb) == nil || (!password.isEmpty && password.count < 10))
            }
            Section {
                Button("Benutzer löschen", role: .destructive) { confirmDelete = true }
            } footer: {
                Text("Löscht auch alle Dateien des Benutzers.")
            }
        }
        .navigationTitle(user.username)
        .onAppear { gb = String(format: "%g", Double(user.quotaBytes) / 1_073_741_824) }
        .confirmationDialog("„\(user.username)“ löschen?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Löschen", role: .destructive) { Task { await delete() } }
        }
    }

    private func save() async {
        guard let api = model.api else { return }
        do {
            try await api.updateUser(user.id, quotaGB: parseGB(gb), password: password.isEmpty ? nil : password)
            await onDone()
            model.show("Gespeichert")
            dismiss()
        } catch { model.report(error) }
    }

    private func delete() async {
        guard let api = model.api else { return }
        do {
            try await api.deleteUser(user.id)
            await onDone()
            dismiss()
        } catch { model.report(error) }
    }
}

func parseGB(_ s: String) -> Double? {
    guard let v = Double(s.replacingOccurrences(of: ",", with: ".").trimmingCharacters(in: .whitespaces)), v > 0 else { return nil }
    return v
}
