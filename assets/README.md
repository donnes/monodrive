# App icon

`prod/app-icon.icon` is the Icon Composer project, and every PNG here is exported from it. Its one layer, `folder.svg`, is a folder of 120 px squares on a 1024 px canvas whose white fades from full on the top-left to 20% on the bottom-right, the same range as Monocode's ring. The fill is black, as Monocode's is. `prod/logo.svg` is the same mark on a flat black square, for places that can't show the rendered icon.

Run `scripts/icons` after changing the project to export `prod/monodrive-universal-1024.png`, the full-bleed square for the README, a website or a Linux build. `scripts/icons --check` exports it to a scratch folder and fails if the tracked copy differs. Exporting needs Icon Composer 2 or newer, from Xcode 26 or installed on its own; the script picks the newest one and pins design generation 26. Set `ICON_COMPOSER_TOOL` to `Icon Composer.app/Contents/Executables/ictool` to choose one.

Don't edit the PNGs by hand.

## The macOS icon

The app needs no exported image: `scripts/bundle` compiles `prod/app-icon.icon` with `actool` into the bundle's `Assets.car`, which macOS 26 draws as Liquid Glass, and an `.icns` for older releases.

## The installer window

`prod/dmg-background.tiff` is what the disk image's window shows behind its icons. `scripts/dmg-background.swift` draws it, and its comments give the constraints. `scripts/release` has Finder lay the window out, so it needs a logged-in session that can automate Finder.
