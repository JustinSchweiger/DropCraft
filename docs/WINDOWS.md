# The Windows app

`DropCraft.exe` is the app and its own installer. Settings and sync state live in
`%APPDATA%\DropCraft\` (per account under `accounts\<id>\`).

## Using it

- **Popup above the taskbar.** Click the DropCraft icon to open it; click elsewhere or press Esc to
  close it. Right-click the icon for *Open*, *Pause* and *Quit*.
- **Starts silently.** With *Mit Windows starten* on, DropCraft starts in the tray at sign-in
  and after updates, without showing the popup.
- **Live statistics.** The current file with progress, speed, files and size left, time left, queue.
- **Tray icon.** Spins while syncing; grey = paused, yellow = needs attention. The tooltip shows
  what is left.
- **Several accounts.** *Konto › Konto hinzufügen …*. Each account has its own server, sign-in and
  sync folder. Folders must not overlap. Switch between accounts at the top of the popup.
- **Managing users** (admin accounts only): *Konto › Benutzer verwalten …*. Enter a name,
  password and storage in GB. Change storage, reset passwords, delete users. New users are never admins.
- **Settings.** Upload and download limits; *Pausieren* stops running transfers at once.
- **Conflicts.** If a file changes on two devices at once, the local version is kept as
  `Name (Konflikt Gerät Datum).ext`.
- **Ignored files** include Office lock files `~$…`, `*.tmp`, `desktop.ini` and `Thumbs.db`.

## Share links

Right-click a file or folder in a sync folder › **DropCraft** › *DropCraft-Link kopieren*, or use the
popup: *Konto › Freigaben …*. The link opens a web page with the content, single downloads and
*Alles als ZIP*, no DropCraft needed. Photos and videos show as a gallery with preview.

Per link you can allow uploads by guests, set a password and an expiry date, and turn on
notifications (a Windows notification when others add or delete files there). Links can be ended
from the same place.

## Explorer menu

On Windows 11, right-click any file or folder in a sync folder (or the empty space inside one):

| Entry | What it does |
|---|---|
| **DropCraft-Link kopieren** | Creates the link (or reuses the existing one), copies it and confirms with a notification. No window opens |
| **Freigeben …** | Opens the popup on this link with all its options |
| **Im Browser anzeigen** | Opens the web app in this folder (for a file: the folder it is in) |

The entry is hidden outside the sync folders. The sync folder itself only offers *Im Browser anzeigen*.

**Why the admin prompt?** Windows 11 only puts entries from signed app packages at the top of
the menu. Apps like Dropbox use a purchased certificate. DropCraft instead builds the small
package on your PC, signs it with a freshly created certificate, asks Windows once (admin prompt)
to trust that certificate, and deletes the certificate's private key right away, so nothing else
can ever be signed with it. The menu itself is a small DLL (`DropCraftShell.dll`) next to
`DropCraft.exe`; it is part of the program and is updated with it.

Set it up during installation or later under *Einstellungen › Explorer-Menü*. Uninstalling
DropCraft removes the package; the trusted certificate stays but is useless without its key.

On Windows 10, and on Windows 11 under *Weitere Optionen anzeigen*, the classic entry
*DropCraft-Link kopieren* is available as well.

## Files on demand

Built with `--features cloud-files` (the default download `DropCraft-Windows.exe`):

- The sync folder is registered with Windows as a cloud folder and appears in Explorer's navigation pane.
- New files from the server appear as **placeholders** (cloud icon) and download when opened.
- **Status column** in Explorer: cloud = online only, tick = on this device, filled tick = always available.
- Right-click › **Immer auf diesem Gerät behalten** downloads a folder and keeps it current;
  **Speicherplatz freigeben** turns it back into placeholders.
- Before an account is removed, all placeholders are downloaded.

## Install, update, uninstall

- **Install.** Double-click `DropCraft.exe`. It offers to copy itself to
  `%LOCALAPPDATA%\Programs\DropCraft\` (no admin rights), with an entry under *Settings › Apps*,
  the Start menu and *start with Windows*. *Nein* runs it once from where it is (portable).
- **Already installed?** It asks whether to replace the installed version or start it. A running
  DropCraft is closed first.
- **Updates** come from `https://dropcraft.tech/updates/latest.json` (*Einstellungen › Updates*),
  automatically or after a click. The program folder belongs to you, so no admin rights are needed.
- **Uninstall** under *Settings › Apps › DropCraft*. Your sync folders stay. You choose whether to
  keep accounts and settings for a later reinstall.

## Command line

| Command | Effect |
|---|---|
| `DropCraft.exe --install` | Install without asking and start |
| `DropCraft.exe --uninstall [--quiet]` | Uninstall |
| `DropCraft.exe --setup-explorer-menu` | Set up the Windows 11 Explorer menu |
| `DropCraft.exe --share <path>` | Open the share options for a file or folder |
| `DropCraft.exe --copy-link <path>` | Copy the link without opening a window |
| `DropCraft.exe --open-web <path>` | Open the web app in that folder |
| `DropCraft.exe --autostart` | Start in the tray without the popup |
