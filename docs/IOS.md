# iPhone and iPad app

A native SwiftUI app in `ios/`: sign in (several accounts), browse folders, open files with
preview, upload from Photos and Files, copy links and set share options. *Konto verwalten* opens
the account website of the server (password, devices, links; users and settings for admins). The
iOS app is in German for now.

## Install with Xcode (free, an Apple ID is enough)

```sh
brew install xcodegen
cd ios && xcodegen generate && open DropCraft.xcodeproj
```

1. Xcode › Settings › Accounts: add your Apple ID.
2. Target *DropCraft* › Signing & Capabilities: Team = *(Personal Team)*. If Xcode reports that the
   identifier is taken, replace `tech.dropcraft.ios` with your own (e.g. `de.yourname.dropcraft`).
3. Connect the iPhone by cable, choose it as the destination at the top, press ▶.
4. On the iPhone: Settings › General › VPN & Device Management › trust the developer. You may also
   need Settings › Privacy & Security › Developer Mode.

With a free Apple ID the signature expires after **7 days** (your data stays): just press ▶ again.

## Renew without a cable: SideStore, AltStore, Sideloadly

CI builds an unsigned `DropCraft.ipa` (GitHub › Actions › CI › `DropCraft-iOS`, or the
`DropCraft-iOS.ipa` file in every release). These tools sign it with your Apple ID and renew it
automatically before the 7 days run out.
