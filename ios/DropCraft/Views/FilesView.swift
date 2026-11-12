import PhotosUI
import QuickLook
import SwiftUI
import UniformTypeIdentifiers

/// Dateien-Tab: Ordner durchblättern, hochladen, teilen.
struct FilesRoot: View {
    @State private var path: [String] = []

    var body: some View {
        NavigationStack(path: $path) {
            FolderView(dir: "")
                .navigationDestination(for: String.self) { FolderView(dir: $0) }
        }
    }
}

@MainActor
struct FolderView: View {
    @EnvironmentObject var model: AppModel
    let dir: String

    @State private var items: [RemoteItem] = []
    @State private var loading = true
    @State private var loadError: String?
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var showPhotos = false
    @State private var showImporter = false
    @State private var newFolder = false
    @State private var newFolderName = ""
    @State private var renaming: RemoteItem?
    @State private var renameText = ""
    @State private var deleting: RemoteItem?
    @State private var preview: URL?
    @State private var shareFile: URL?

    private var title: String { dir.isEmpty ? "DropCraft" : RemotePath.name(dir) }

    var body: some View {
        List {
            if !model.transfers.isEmpty {
                Section {
                    ForEach(model.transfers) { t in TransferRow(t: t) }
                }
            }
            Section {
                ForEach(items) { item in
                    row(item)
                }
            } footer: {
                if !items.isEmpty {
                    let files = items.filter { !$0.isDir }
                    Text("\(items.count) Einträge · \(Fmt.size(files.reduce(0) { $0 + $1.size }))")
                        .font(.footnote)
                        .foregroundStyle(DC.dim)
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(DC.surface)
        .overlay {
            if loading && items.isEmpty {
                ProgressView()
            } else if let loadError, items.isEmpty {
                ContentUnavailableView("Nicht geladen", systemImage: "wifi.exclamationmark", description: Text(loadError))
            } else if items.isEmpty {
                ContentUnavailableView("Leerer Ordner", systemImage: "folder", description: Text("Mit + Fotos oder Dateien hochladen."))
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(dir.isEmpty ? .large : .inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button { showPhotos = true } label: { Label("Fotos und Videos", systemImage: "photo.on.rectangle") }
                    Button { showImporter = true } label: { Label("Aus Dateien", systemImage: "doc.badge.plus") }
                    Divider()
                    Button { newFolderName = ""; newFolder = true } label: { Label("Neuer Ordner", systemImage: "folder.badge.plus") }
                    if !dir.isEmpty {
                        Button { Task { await share(path: dir, name: title) } } label: { Label("Diesen Ordner freigeben", systemImage: "link") }
                    }
                } label: {
                    Image(systemName: "plus")
                }
            }
        }
        .refreshable { await load() }
        .task { await load() }
        .onChange(of: model.transfers.count) { old, new in
            if new < old { Task { await load() } }
        }
        .photosPicker(isPresented: $showPhotos, selection: $photoItems, matching: .any(of: [.images, .videos]))
        .onChange(of: photoItems) { _, picked in
            guard !picked.isEmpty else { return }
            photoItems = []
            Task { await uploadPhotos(picked) }
        }
        .fileImporter(isPresented: $showImporter, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            if case .success(let urls) = result { Task { await uploadFiles(urls) } }
        }
        .alert("Neuer Ordner", isPresented: $newFolder) {
            TextField("Name", text: $newFolderName)
            Button("Anlegen") { Task { await mkdir() } }
            Button("Abbrechen", role: .cancel) {}
        }
        .alert("Umbenennen", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Name", text: $renameText)
            Button("Umbenennen") { Task { await rename() } }
            Button("Abbrechen", role: .cancel) { renaming = nil }
        }
        .confirmationDialog(
            deleting.map { "„\($0.name)“ löschen?" } ?? "",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible
        ) {
            Button("Löschen", role: .destructive) { Task { await delete() } }
        } message: {
            Text("Wird auf allen Geräten gelöscht.")
        }
        .quickLookPreview($preview)
        .sheet(item: Binding(get: { shareFile.map(IdentURL.init) }, set: { shareFile = $0?.url })) { f in
            ActivityView(items: [f.url])
        }
    }

    @ViewBuilder
    private func row(_ item: RemoteItem) -> some View {
        let content = HStack(spacing: 12) {
            Image(systemName: fileSymbol(item.name, isDir: item.isDir))
                .font(.system(size: 17))
                .foregroundStyle(item.isDir ? DC.accent : DC.dim)
                .frame(width: 34, height: 34)
                .background(DC.bg, in: RoundedRectangle(cornerRadius: 6))
            VStack(alignment: .leading, spacing: 2) {
                Text(item.name).foregroundStyle(DC.text).lineLimit(1)
                Text(item.isDir ? Fmt.date(item.modified) : "\(Fmt.size(item.size)) · \(Fmt.date(item.modified))")
                    .font(.caption)
                    .foregroundStyle(DC.dim)
            }
        }
        .contentShape(Rectangle())

        Group {
            if item.isDir {
                NavigationLink(value: item.path) { content }
            } else {
                Button { Task { await open(item) } } label: { content }
                    .buttonStyle(.plain)
            }
        }
        .listRowBackground(DC.raised)
        .contextMenu { menu(item) }
        .swipeActions(edge: .trailing) {
            Button(role: .destructive) { deleting = item } label: { Label("Löschen", systemImage: "trash") }
            Button { Task { await share(path: item.path, name: item.name) } } label: { Label("Link", systemImage: "link") }
                .tint(DC.accentDim)
        }
    }

    @ViewBuilder
    private func menu(_ item: RemoteItem) -> some View {
        Button { Task { await share(path: item.path, name: item.name) } } label: { Label("Link kopieren", systemImage: "link") }
        if !item.isDir {
            Button { Task { await open(item) } } label: { Label("Öffnen", systemImage: "eye") }
            Button { Task { await exportFile(item) } } label: { Label("Teilen / Sichern …", systemImage: "square.and.arrow.up") }
        }
        Button { renameText = item.name; renaming = item } label: { Label("Umbenennen", systemImage: "pencil") }
        Divider()
        Button(role: .destructive) { deleting = item } label: { Label("Löschen", systemImage: "trash") }
    }

    // MARK: Aktionen

    private func load() async {
        guard let api = model.api else { return }
        do {
            items = try await api.list(dir)
            loadError = nil
        } catch {
            loadError = error.localizedDescription
            if case APIError.unauthorized = error { model.needsLogin = true }
        }
        loading = false
    }

    /// Datei laden (falls nicht aktuell im Cache) und lokale URL liefern.
    private func fetch(_ item: RemoteItem) async -> URL? {
        guard let api = model.api, let dest = model.cacheURL(for: item.path) else { return nil }
        if let attrs = try? FileManager.default.attributesOfItem(atPath: dest.path),
           (attrs[.size] as? NSNumber)?.int64Value == item.size,
           let mod = attrs[.modificationDate] as? Date, mod >= item.modified.addingTimeInterval(-1) {
            return dest
        }
        let id = model.startTransfer(item.name, upload: false)
        defer { model.endTransfer(id) }
        do {
            try await api.download(item.path, to: dest) { p in Task { @MainActor in model.updateTransfer(id, p) } }
            try? FileManager.default.setAttributes([.modificationDate: item.modified], ofItemAtPath: dest.path)
            return dest
        } catch {
            model.report(error)
            return nil
        }
    }

    private func open(_ item: RemoteItem) async {
        if let url = await fetch(item) { preview = url }
    }

    private func exportFile(_ item: RemoteItem) async {
        if let url = await fetch(item) { shareFile = url }
    }

    private func share(path: String, name: String) async {
        guard let api = model.api else { return }
        do {
            // Bestehende Freigabe wiederverwenden
            let share: Share
            if let existing = try await api.shares().first(where: { $0.path == path }) {
                share = existing
            } else {
                share = try await api.createShare(path)
            }
            UIPasteboard.general.url = api.shareURL(share.token)
            model.show("Link für „\(name)“ kopiert")
        } catch {
            model.report(error)
        }
    }

    private func mkdir() async {
        let name = newFolderName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, let api = model.api else { return }
        do {
            try await api.mkdir(RemotePath.join(dir, name))
            await load()
        } catch { model.report(error) }
    }

    private func rename() async {
        guard let item = renaming, let api = model.api else { return }
        renaming = nil
        let name = renameText.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, name != item.name else { return }
        do {
            try await api.move(item.path, to: RemotePath.join(dir, name))
            await load()
        } catch { model.report(error) }
    }

    private func delete() async {
        guard let item = deleting, let api = model.api else { return }
        deleting = nil
        do {
            try await api.delete(item.path)
            items.removeAll { $0.id == item.id }
        } catch { model.report(error) }
    }

    /// Freier Name im aktuellen Ordner: "IMG_0001 (2).heic".
    private func freeName(_ name: String, taken: inout Set<String>) -> String {
        var candidate = name
        let base = (name as NSString).deletingPathExtension
        let ext = (name as NSString).pathExtension
        var n = 2
        while taken.contains(candidate.lowercased()) {
            candidate = ext.isEmpty ? "\(base) (\(n))" : "\(base) (\(n)).\(ext)"
            n += 1
        }
        taken.insert(candidate.lowercased())
        return candidate
    }

    private func uploadFiles(_ urls: [URL]) async {
        var taken = Set(items.map { $0.name.lowercased() })
        for url in urls {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            // Kopie im Temp-Ordner: der Zugriff auf die Originaldatei endet sonst
            let tmp = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString).appendingPathComponent(url.lastPathComponent)
            do {
                try FileManager.default.createDirectory(at: tmp.deletingLastPathComponent(), withIntermediateDirectories: true)
                try FileManager.default.copyItem(at: url, to: tmp)
            } catch {
                model.report(error)
                continue
            }
            await upload(tmp, name: freeName(url.lastPathComponent, taken: &taken))
        }
    }

    private func uploadPhotos(_ picked: [PhotosPickerItem]) async {
        var taken = Set(items.map { $0.name.lowercased() })
        for item in picked {
            do {
                guard let media = try await item.loadTransferable(type: PickedMedia.self) else { continue }
                await upload(media.url, name: freeName(media.url.lastPathComponent, taken: &taken))
            } catch {
                model.report(error)
            }
        }
    }

    private func upload(_ file: URL, name: String) async {
        guard let api = model.api else { return }
        let id = model.startTransfer(name, upload: true)
        defer {
            model.endTransfer(id)
            try? FileManager.default.removeItem(at: file.deletingLastPathComponent())
        }
        do {
            try await api.upload(file: file, to: RemotePath.join(dir, name)) { p in Task { @MainActor in model.updateTransfer(id, p) } }
        } catch {
            model.report(error)
        }
    }
}

/// Foto oder Video aus der Mediathek als Datei (nicht im Speicher).
struct PickedMedia: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .movie) { received in try copy(received.file) }
        FileRepresentation(importedContentType: .image) { received in try copy(received.file) }
    }

    private static func copy(_ file: URL) throws -> PickedMedia {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let dest = dir.appendingPathComponent(file.lastPathComponent)
        try FileManager.default.copyItem(at: file, to: dest)
        return PickedMedia(url: dest)
    }
}

struct TransferRow: View {
    let t: Transfer

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: t.upload ? "arrow.up" : "arrow.down")
                .foregroundStyle(DC.accent)
                .frame(width: 34)
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(t.name).lineLimit(1)
                    Spacer()
                    Text("\(Int(t.progress * 100)) %").font(.caption).foregroundStyle(DC.dim).monospacedDigit()
                }
                ProgressView(value: t.progress).tint(DC.accent)
            }
        }
        .listRowBackground(DC.raised)
    }
}

private struct IdentURL: Identifiable {
    let url: URL
    var id: URL { url }
}

/// System-Teilen-Blatt (Sichern in Fotos/Dateien, AirDrop …).
struct ActivityView: UIViewControllerRepresentable {
    let items: [Any]
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }
    func updateUIViewController(_ vc: UIActivityViewController, context: Context) {}
}
