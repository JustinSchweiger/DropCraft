import SwiftUI

/// Anmelden bzw. Konto hinzufügen.
struct LoginView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    var isFirst: Bool
    var preset: Account?

    @State private var server = "https://dropcraft.tech"
    @State private var username = ""
    @State private var password = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    VStack(spacing: 10) {
                        Image("Logo")
                            .resizable()
                            .frame(width: 72, height: 72)
                            .clipShape(RoundedRectangle(cornerRadius: 16))
                        Text(preset == nil ? "Bei DropCraft anmelden" : "Bitte neu anmelden")
                            .font(.title3.weight(.semibold))
                    }
                    .frame(maxWidth: .infinity)
                    .listRowBackground(Color.clear)
                }
                Section("Server") {
                    TextField("https://dropcraft.tech", text: $server)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .disabled(preset != nil)
                }
                Section("Konto") {
                    TextField("Benutzername", text: $username)
                        .textContentType(.username)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .disabled(preset != nil)
                    SecureField("Passwort", text: $password)
                        .textContentType(.password)
                        .onSubmit(submit)
                }
                if let error {
                    Section { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(DC.danger) }
                }
                Section {
                    Button(action: submit) {
                        HStack {
                            Spacer()
                            if busy { ProgressView() } else { Text("Anmelden").fontWeight(.semibold) }
                            Spacer()
                        }
                    }
                    .disabled(busy || username.isEmpty || password.isEmpty || server.isEmpty)
                }
            }
            .scrollContentBackground(.hidden)
            .background(DC.surface)
            .toolbar {
                if !isFirst {
                    ToolbarItem(placement: .cancellationAction) { Button("Abbrechen") { dismiss() } }
                }
            }
        }
        .onAppear {
            if let p = preset {
                server = p.server
                username = p.username
            }
        }
    }

    private func submit() {
        guard !busy else { return }
        busy = true
        error = nil
        Task {
            do {
                try await model.login(server: server, username: username.trimmingCharacters(in: .whitespaces), password: password)
                busy = false
                dismiss()
            } catch {
                self.error = error.localizedDescription
                busy = false
            }
        }
    }
}
