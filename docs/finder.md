# Standing in for Finder

Two ways in bring other apps' files to Monodrive: Show in Finder, which Monodrive takes over only when the user switches it on in Settings or onboarding's checklist, and Open in Monodrive in Finder's right-click menu, which is always there. `finder.rs` handles both, and both land the same way: a file shows selected in its folder, and a folder opens in a new tab so the window's place survives.

## Show in Finder

macOS sends a reveal to the app that the global preference `NSFileViewer` names, a bundle ID in the domain `defaults write -g` writes. The key is undocumented, though Path Finder, ForkLift and Cmdr use it too, so it can stop working in any macOS release. The reveal arrives as a plain open-documents event, so Monodrive declares no document types and claims no `public.folder` handler. On a launch the event arrives before `run`'s callback does, so `on_open_urls` is set before `run`, and what arrives waits until a browser window is open, after onboarding if it's showing.

The Settings switch and the onboarding row read the key from macOS each time they show, because another app can take it at any moment, and keep no copy in `settings.json`. Taking the key from another app, say Path Finder, records its bundle ID and where it's installed in the launchd agent's plist below. Switching off changes the key only while it names this Monodrive, so it never unregisters an app that took it since. It gives the key back to the app that had it if that app is still where it was, and otherwise removes the key rather than naming Finder, which is what macOS does without it.

A key naming an app that's gone doesn't fall back to Finder: Show in Finder silently does nothing in every app until someone runs `defaults delete -g NSFileViewer`. Nothing of Monodrive's runs when it's deleted, so taking the key first loads a launchd agent, `~/Library/LaunchAgents/<bundle ID>.show-in-finder.plist`, which macOS lists under Monodrive in Login Items. It runs only `sh`, `defaults` and `launchctl`, when the folder the app is in changes and at each login, in case the app went while nobody was logged in. If the app has been gone for 10 seconds and the key still names it, the agent hands the key back the same way switching off does, then deletes and unloads itself. The updater swaps the app in one rename, so an update never sets it off. If the agent can't be loaded, the key isn't taken. Switching off removes the agent, and only a `scripts/release` build installed in an Applications folder can take the key, since a dev build's bundle gets rebuilt and thrown away.

The key also redirects Monodrive's own Reveal in Finder, and so does `open -a Finder -R`. Setting the key to Finder makes Finder open the file instead of selecting it. An empty value in the process's own argument domain does a real reveal, so `workspace::reveal` sets that while a Monodrive holds the key.

## Open in Monodrive

`NSServices` in the Info.plist `scripts/bundle` writes declares a service for any file or folder, with an empty `NSRequiredContext` so it shows without being switched on in System Settings. It's answered under the bundle ID, its `NSPortName`, so a dev build running beside the installed app gets its own menu item and its own requests. Unlike Show in Finder, each folder the picked files are in gets a tab of its own, since the user chose them together on purpose.
