import SwiftUI

@main
struct DropCraftApp: App {
    @StateObject private var model = AppModel()

    init() {
        // Navigations- und Tableiste im DropCraft-Look
        let nav = UINavigationBarAppearance()
        nav.configureWithOpaqueBackground()
        nav.backgroundColor = UIColor(DC.bg)
        nav.shadowColor = UIColor(DC.border)
        nav.titleTextAttributes = [.foregroundColor: UIColor(DC.text)]
        nav.largeTitleTextAttributes = [.foregroundColor: UIColor(DC.text)]
        UINavigationBar.appearance().standardAppearance = nav
        UINavigationBar.appearance().scrollEdgeAppearance = nav
        UINavigationBar.appearance().compactAppearance = nav

        let tab = UITabBarAppearance()
        tab.configureWithOpaqueBackground()
        tab.backgroundColor = UIColor(DC.bg)
        tab.shadowColor = UIColor(DC.border)
        UITabBar.appearance().standardAppearance = tab
        UITabBar.appearance().scrollEdgeAppearance = tab
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .preferredColorScheme(.dark)
                .tint(DC.accent)
        }
    }
}

struct RootView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Group {
            if model.selected == nil {
                LoginView(isFirst: true)
            } else {
                TabView {
                    FilesRoot()
                        .tabItem { Label("Dateien", systemImage: "folder") }
                    SharesView()
                        .tabItem { Label("Freigaben", systemImage: "link") }
                    AccountView()
                        .tabItem { Label("Konto", systemImage: "person.crop.circle") }
                }
                .id(model.selectedID)
                .task(id: model.selectedID) { await model.refreshMe() }
                .sheet(isPresented: $model.needsLogin) {
                    if let a = model.selected {
                        LoginView(isFirst: false, preset: a)
                    }
                }
            }
        }
        .overlay(alignment: .bottom) { ToastView() }
    }
}

/// Kurze Meldung unten.
struct ToastView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        if let t = model.toast {
            Text(t)
                .font(.subheadline)
                .foregroundStyle(DC.text)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(DC.raised, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(DC.border))
                .padding(.bottom, 64)
                .padding(.horizontal, 16)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                .animation(.easeOut(duration: 0.2), value: model.toast)
                .onTapGesture { model.toast = nil }
        }
    }
}
