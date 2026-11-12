import SwiftUI

/// Farben aus dem DMXCraft-Design (wie die Desktop-App).
enum DC {
    static func rgb(_ hex: UInt32) -> Color {
        Color(red: Double((hex >> 16) & 0xff) / 255, green: Double((hex >> 8) & 0xff) / 255, blue: Double(hex & 0xff) / 255)
    }

    static let bg = rgb(0x0b0e12)
    static let surface = rgb(0x12161c)
    static let raised = rgb(0x1a2028)
    static let border = rgb(0x28303a)
    static let accent = rgb(0x19e3f0)
    static let accentDim = rgb(0x0c5c64)
    static let text = rgb(0xe6ecf2)
    static let dim = rgb(0x95a2b0)
    static let danger = rgb(0xef4455)
    static let ok = rgb(0x34d399)
    static let warn = rgb(0xfbbf24)
}

/// Überschrift in Großbuchstaben (wie `label` im Design-System).
struct SectionLabel: View {
    let text: String
    init(_ text: String) { self.text = text }
    var body: some View {
        Text(text.uppercased())
            .font(.system(size: 12, weight: .semibold))
            .tracking(0.5)
            .foregroundStyle(DC.dim)
    }
}

/// Kleines Etikett (Upload, Passwort, Ablauf …).
struct Chip: View {
    let icon: String
    let text: String
    var color: Color = DC.dim
    var fill: Color = DC.raised

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: icon).font(.system(size: 9, weight: .semibold))
            if !text.isEmpty { Text(text).font(.system(size: 11)) }
        }
        .foregroundStyle(color)
        .padding(.horizontal, 6)
        .frame(height: 18)
        .background(fill, in: RoundedRectangle(cornerRadius: 4))
    }
}

/// Symbol je Dateityp.
func fileSymbol(_ name: String, isDir: Bool) -> String {
    if isDir { return "folder.fill" }
    switch (name as NSString).pathExtension.lowercased() {
    case "jpg", "jpeg", "png", "gif", "heic", "webp", "bmp", "tiff": return "photo"
    case "mp4", "mov", "m4v", "mkv", "webm", "avi": return "film"
    case "mp3", "wav", "m4a", "aac", "flac", "ogg": return "waveform"
    case "pdf": return "doc.richtext"
    case "zip", "rar", "7z", "tar", "gz": return "doc.zipper"
    case "txt", "md", "rtf", "doc", "docx", "pages": return "doc.text"
    case "xls", "xlsx", "csv", "numbers": return "tablecells"
    default: return "doc"
    }
}

/// Speicherbalken.
struct QuotaBar: View {
    let used: Int64
    let quota: Int64
    var height: CGFloat = 6

    var body: some View {
        GeometryReader { g in
            let f = quota > 0 ? min(1, max(0, Double(used) / Double(quota))) : 0
            ZStack(alignment: .leading) {
                Capsule().fill(DC.raised)
                Capsule().fill(f > 0.95 ? DC.danger : DC.accent).frame(width: g.size.width * f)
            }
        }
        .frame(height: height)
    }
}
