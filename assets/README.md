# App icon

`prod/app-icon.icon` is the Icon Composer project, and every PNG here is exported from it. Its one layer, `folder.svg`, is a folder of 120 px squares on a 1024 px canvas whose white fades from full on the top-left to 20% on the bottom-right, the same range as Monocode's ring. The fill is black, as Monocode's is. `prod/logo.svg` is the same mark on a flat black square, for places that can't show the rendered icon.

Run `scripts/icons` after changing the project to export `prod/monodrive-universal-1024.png`, the full-bleed square for the README, a website or a Linux build. `scripts/icons --check` exports it to a scratch folder and fails if the tracked copy differs. Exporting needs Icon Composer 2 or newer, from Xcode 26 or installed on its own; the script picks the newest one and pins design generation 26. Set `ICON_COMPOSER_TOOL` to `Icon Composer.app/Contents/Executables/ictool` to choose one.

Don't edit the PNGs by hand.

## The macOS icon

The app itself needs no export: `scripts/bundle` compiles `prod/app-icon.icon` with `actool` into the bundle's `Assets.car` and `.icns`. The PNG below is only for places outside the app that want the shaped macOS icon as an image.

Icon Composer's command-line exporter has no `macOS pre-Tahoe` preset, and its plain `macOS` export is full bleed, so `scripts/icons` leaves `prod/monodrive-macos-1024.png` alone. After changing the project, open it in Icon Composer and export with exactly these settings:

- Platform: `macOS pre-Tahoe`
- Appearance: `Default`
- Size: `1024pt`
- Scale: `1×`

Save it as `prod/monodrive-macos-1024.png`. The result is 1024×1024 with the classic macOS safe area: an 824×824 opaque body inset 100 px on every side, with only Icon Composer's shadow reaching into the transparent margin.

To have an agent with computer use do the export, give it this from the repository root:

```text
Use computer use and the Icon Composer app to export assets/prod/app-icon.icon with Platform: macOS pre-Tahoe, Appearance: Default, Size: 1024pt, Scale: 1×, and save it to assets/prod/monodrive-macos-1024.png. Don't resize, composite or otherwise change the PNG. Verify it is 1024×1024 with an 824×824 opaque body inset 100 px on every side, and only Icon Composer's shadow beyond it.
```
