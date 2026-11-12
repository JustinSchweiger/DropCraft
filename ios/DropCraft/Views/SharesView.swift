import SwiftUI

/// Freigaben-Tab: alle Links, Einstellungen je Freigabe.
@MainActor
struct SharesView: View {
    @EnvironmentObject var model: AppModel
    @State private var shares: [Share] = []
    @State private var loading = true

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(shares) { sh in
                        NavigationLink(value: sh) { ShareRow(share: sh) }
                            .listRowBackground(DC.raised)
                            .swipeActions {
                                Button { copy(sh) } label: { Label("Kopieren", systemImage: "doc.on.doc") }.tint(DC.accentDim)
                            }
                    }
                } header: {
                    if !shares.isEmpty { SectionLabel("\(shares.count) \(shares.count == 1 ? "aktiver Link" : "aktive Links")") }
                }
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(DC.surface)
            .overlay {
                if loading && shares.isEmpty {
                    ProgressView()
                } else if shares.isEmpty {
                    ContentUnavailableView("Noch nichts freigegeben", systemImage: "link", description: Text("Im Dateien-Tab eine Datei oder einen Ordner lange drücken und „Link kopieren“ wählen."))
                }
            }
            .navigationTitle("Freigaben")
            .navigationDestination(for: Share.self) { sh in
                ShareDetail(share: sh) { await load() }
            }
            .refreshable { await load() }
            .task { await load() }
        }
    }

    private func load() async {
        guard let api = model.api else { return }
        do { shares = try await api.shares() } catch { model.report(error) }
        loading = false
    }

    private func copy(_ sh: Share) {
        guard let api = model.api else { return }
        UIPasteboard.general.url = api.shareURL(sh.token)
        model.show("Link kopiert")
    }
}

struct ShareRow: View {
    let share: Share

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: fileSymbol(share.name, isDir: !share.name.contains(".")))
                .foregroundStyle(DC.accent)
                .frame(width: 34, height: 34)
                .background(DC.bg, in: RoundedRectangle(cornerRadius: 6))
            VStack(alignment: .leading, spacing: 5) {
                Text(share.name).foregroundStyle(DC.text).lineLimit(1)
                HStack(spacing: 4) {
                    if share.allowUpload { Chip(icon: "arrow.up", text: "Upload", color: DC.accent, fill: DC.accentDim) }
                    if share.hasPassword { Chip(icon: "lock", text: "Passwort") }
                    if let d = share.expires {
                        let left = d.timeIntervalSinceNow
                        if left <= 0 {
                            Chip(icon: "clock", text: "abgelaufen", color: DC.danger)
                        } else if left < 86_400 {
                            Chip(icon: "clock", text: "läuft heute ab", color: DC.warn)
                        } else {
                            Chip(icon: "clock", text: "noch \(Int(left / 86_400)) Tage")
                        }
                    }
                    if share.notify { Chip(icon: "bell", text: "") }
                }
            }
        }
    }
}

/// Einstellungen einer Freigabe.
@MainActor
struct ShareDetail: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    @State var share: Share
    var onChange: () async -> Void

    @State private var password = ""
    @State private var editingPassword = false
    @State private var confirmDelete = false

    private var url: URL? { model.api?.shareURL(share.token) }

    var body: some View {
        Form {
            Section {
                if let url {
                    Link(destination: url) {
                        Text(url.absoluteString).font(.footnote).foregroundStyle(DC.accent).lineLimit(1).truncationMode(.middle)
                    }
                    HStack {
                        Button { UIPasteboard.general.url = url; model.show("Link kopiert") } label: { Label("Kopieren", systemImage: "doc.on.doc") }
                        Spacer()
                        ShareLink(item: url) { Label("Senden", systemImage: "square.and.arrow.up") }
                    }
                    .buttonStyle(.borderless)
                }
            } header: {
                Text(share.path)
            }
            .listRowBackground(DC.raised)

            Section {
                Toggle(isOn: Binding(get: { share.allowUpload }, set: { v in update(ShareSettings(allowUpload: v)) })) {
                    Label("Gäste dürfen hochladen", systemImage: "arrow.up.doc")
                }
                Toggle(isOn: Binding(get: { share.notify }, set: { v in update(ShareSettings(notify: v)) })) {
                    Label("Benachrichtigen", systemImage: "bell")
                }
            }
            .listRowBackground(DC.raised)

            Section("Passwort") {
                if editingPassword {
                    SecureField("mind. 4 Zeichen", text: $password)
                    HStack {
                        Button("Speichern") {
                            update(ShareSettings(password: password))
                            password = ""
                            editingPassword = false
                        }
                        .disabled(password.count < 4)
                        Spacer()
                        Button("Abbrechen", role: .cancel) { editingPassword = false }
                    }
                    .buttonStyle(.borderless)
                } else {
                    HStack {
                        Label(share.hasPassword ? "Geschützt" : "Kein Passwort", systemImage: share.hasPassword ? "lock.fill" : "lock.open")
                            .foregroundStyle(share.hasPassword ? DC.text : DC.dim)
                        Spacer()
                        if share.hasPassword {
                            Button("Entfernen", role: .destructive) { update(ShareSettings(password: "")) }
                        }
                        Button(share.hasPassword ? "Ändern" : "Festlegen") { editingPassword = true }
                    }
                    .buttonStyle(.borderless)
                }
            }
            .listRowBackground(DC.raised)

            Section {
                Picker("Gültig", selection: Binding(get: { expiryChoice }, set: { setExpiry($0) })) {
                    Text("Immer").tag(0)
                    Text("1 Tag").tag(1)
                    Text("7 Tage").tag(7)
                    Text("30 Tage").tag(30)
                    if expiryChoice == -1 { Text("Eigenes Datum").tag(-1) }
                }
            } header: {
                Text("Gültigkeit")
            } footer: {
                if let d = share.expires { Text(Fmt.expiry(d)) }
            }
            .listRowBackground(DC.raised)

            Section {
                Button("Freigabe beenden", role: .destructive) { confirmDelete = true }
            }
            .listRowBackground(DC.raised)
        }
        .scrollContentBackground(.hidden)
        .background(DC.surface)
        .navigationTitle(share.name)
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog("Freigabe beenden?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Beenden", role: .destructive) { Task { await delete() } }
        } message: {
            Text("Der Link funktioniert danach nicht mehr.")
        }
    }

    /// Auswahl im Picker: 0 = unbegrenzt, -1 = sonstiges Datum. Vorhandene
    /// Daten werden nicht auf 1/7/30 zurückgerechnet.
    private var expiryChoice: Int { share.expiresAt == nil ? 0 : -1 }

    private func setExpiry(_ days: Int) {
        guard days >= 0 else { return }
        let at: Int64 = days == 0 ? 0 : Int64((Date().timeIntervalSince1970 + Double(days) * 86_400) * 1000)
        update(ShareSettings(expiresAt: at))
    }

    private func update(_ s: ShareSettings) {
        guard let api = model.api else { return }
        Task {
            do {
                try await api.updateShare(share.token, s)
                if let fresh = try await api.shares().first(where: { $0.token == share.token }) { share = fresh }
                await onChange()
            } catch { model.report(error) }
        }
    }

    private func delete() async {
        guard let api = model.api else { return }
        do {
            try await api.deleteShare(share.token)
            await onChange()
            dismiss()
        } catch { model.report(error) }
    }
}
