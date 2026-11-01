# Packages

Status: UI picked (A, with its entry moved to the sidebar's foot), not built. This plan authorizes planning only.

## Goal

One read-only list of what's installed on this Mac through mise, Homebrew, bun, pnpm and npm, plus its apps, with versions, sizes and available updates. A person opens it from the sidebar, and an agent gets the same list over MCP.

This version installs, updates and removes nothing. See "Changing packages" for why that's a separate step.

## The view

**Entry.** A `package` icon button at the sidebar's foot, directly after Trash, with the tooltip Packages. It works as Trash does: a press opens Packages in the pane commands act on, and the button has the 0.14 fill while that pane shows it. It carries no count or dot, so it doesn't compete with the app's update button. There's no row in Favorites.

The foot hides with the sidebar, so the `Packages` command also goes in `commands.rs`, in the Go menu and the palette, on ⇧⌘U. Back returns from it, as from Recents.

**Layout, from the top of the pane:**

1. A 40 pt `pane_bar` of filters: All, Updates, mise, Homebrew, bun, pnpm, npm, Apps. Each is a 24 pt pill, styled as tabs are: 0.14 fill and semibold when on, none and 0.65 ink otherwise. A source that isn't on this Mac has no pill. The pills are Tab stops, and ↩ or Space picks one. Pills don't carry counts.
2. Column titles, 28 pt: Name, Source, Version, Size. Name sorts by default, and a press on a title sorts, as in the file list.
3. The rows, in a `uniform_list`, at the Row size setting's height, with stripes when they're on. A grid pane shows Packages as a list, as it does content search results.
4. The path bar: the crumb `Packages`, and on the right the status, `132 packages · 9 updates`. The status counts what the current filter shows.

**A row:**

- Name, using the name a person knows: `opencode`, not `aqua:anomalyco/opencode`. The inspector has the full name.
- Source, as a 16 pt badge at 0.08 fill with 0.65 ink. Sources get no color of their own.
- Version. With an update, a 6 pt `accent()` dot and `65.7.0 → 65.8.0`.
- Size at 0.65, `--` until it's measured.

A narrow pane drops Source, then Size. Name keeps 160 pt, as in the file list.

**Filters.** Each row has one source. A cask with an app has the source Homebrew and shows under both Homebrew and Apps, so a person looking for an app finds it. Updates shows every source's rows that have a newer version.

**Keys and selection.** One row at a time. ↑↓ move, ⌘I toggles the inspector, ⌘[ goes back. Double-click, ⌘O or ⌘↓ launches an app and opens the install folder of anything else. The file commands don't apply here: ⌘⌫, rename, cut, paste, drag and marquee do nothing, because a package row isn't a file the user chose to move.

**Search.** The search field filters this list by name while Packages shows, and never starts the index. The prompt says `Search Packages`. Esc or ✕ clears it and stays in Packages. Leaving the view leaves the filter behind. Everywhere else a search covers the whole disk. This differs because a person here is looking for one of about a hundred names.

**Context menu.** Show in Folder (the same action as the open key for a non-app), Reveal in Finder, Copy Path, Get Info. Each is a command with a Packages scope in `commands.rs`, so the menu only shows what would run.

**Inspector.** The usual 260 pt panel. From the top: a 96 pt icon from `file_icons`, the name, and the kind (`Command-line tool`, `Language runtime`, `Global npm package`, `Formula`, `Cask`, `App`) at 0.65. Then these sections, with unknown values having no row:

| Section | Rows |
|---|---|
| Details | Installed, Latest (only when newer), Size, Runs as (Menlo) |
| Where | Source, Full name, Requested (mise's `latest`, or a pin), Declared (file name of the config that asks for it, full path as the tooltip), Path |

At its foot, one button: Show in Folder. There is no Update or Remove.

**Split panes and tabs.** Each pane shows Packages on its own, and a tab is named Packages. The saved session restores it.

## States

- **Loading.** The filters and column titles appear at once. Each source's rows arrive when that source answers, in the sort order, and the selected row stays selected by identity as others land. There is no spinner in the list. While something is still reading, the status says `Reading…` after the count.
- **Updates not known yet.** Rows show the installed version alone, and the dot and arrow appear later. The Updates filter is always there, and says `Checking for updates…` in the middle of the pane until the first answer.
- **A source fails.** A row in that source's place, not selectable, in `danger()`: `mise: <first line of stderr>`. Other sources are unaffected. Errors pass through as they arrive.
- **A source can't report updates.** bun, pnpm, npm and App Store apps have no update check in this version. Their rows have no dot, and the inspector has no Latest row. It doesn't say "up to date", because nothing checked.
- **Empty.** With no packages at all, or none under a filter or search, one line in the middle of the pane at 0.65: `No packages from mise.` or `No package named "foo".`
- **Staleness.** It reads on opening. A package installed while it's showing appears on the next open. Watching managers' folders isn't in this version.

## Core

A `packages` module in `monodrive-core`. GPUI stays out, and manager quirks stay here.

```rust
pub enum Source { Mise, Homebrew, Bun, Pnpm, Npm, App }
pub enum Kind { Tool, Runtime, NpmPackage, Formula, Cask, App }

pub struct Package {
    pub name: String,
    pub full_name: String,
    pub source: Source,
    pub kind: Kind,
    pub version: String,
    pub latest: Option<String>,
    pub path: PathBuf,
    pub commands: Vec<String>,
    pub requested: Option<String>,
    pub declared_in: Option<PathBuf>,
}

/// Where each manager lives. The app passes the real ones, tests a temp folder.
pub struct Roots { .. }
```

Each source is one function returning `Result<Vec<Package>, String>`, so one can fail alone and the view can show each as it lands.

| Source | Reads | Measured here |
|---|---|---|
| mise | `mise ls --json`: version, `active`, `install_path`, `requested_version`, `source.path` | 181 ms |
| Homebrew formulae | `Cellar/<name>/<version>/INSTALL_RECEIPT.json`; only `installed_on_request`, so dependencies stay out | Disk |
| Homebrew casks | `Caskroom/<name>/<version>/` and the `.app` inside it | Disk |
| bun | `~/.bun/install/global/package.json`, then each package's own `package.json` for version and `bin` | Disk |
| pnpm | `pnpm ls -g --json` | 61 ms |
| npm | `npm ls -g --json`, or the active Node's `lib/node_modules` if that's the same answer | 414 ms |
| Apps | Top level of `/Applications` and `~/Applications`, `Info.plist` for the version, `Contents/_MASReceipt` for the App Store | Disk |

`updates` is a separate function, run after the list: `mise outdated --json` and `brew outdated --json`. It joins by source and name, so a dependency's update adds no row. On this Mac Homebrew reports 72 outdated formulae, most of them probably dependencies, so the view's count will be lower. Set `HOMEBREW_NO_AUTO_UPDATE=1`: Monodrive never runs `brew update` or changes a tap, so Homebrew's answer is as fresh as its local data. Say that in the inspector's Latest row tooltip. This call can use the network through mise, so it runs only when Packages opens, or when an agent asks for it, and never at launch.

Sizes come from `sizes::Sizes` over each package's `path`, off the UI thread, one at a time and cached. Sorting by Size places unmeasured rows last.

**Facts to handle at this boundary:**

- A GUI app doesn't inherit the shell's PATH, and neither does an MCP server reliably. Look for managers at known places (`/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`, `~/.local/share/mise/shims`, `~/.bun/bin`). Never start a login shell to find them: that runs the user's rc files.
- A missing manager isn't an error. It has no pill and no rows.
- The Cellar can keep old versions of a formula: `aom` has two here, and there are 195 receipts for 192 formulae. Read the version that `/opt/homebrew/opt/<name>` links to, so a formula is one row. Receipts counted 46 on request across all versions, against `brew list --installed-on-request`'s 42, so the reader has to match that command.
- Each child process gets a time limit, and leaving the view cancels it. Kill only the PID Monodrive started.
- Inactive mise versions (Node 22.23.3 and 24.13.1 here) are real installs, so each gets a row, with `Not active` in the inspector. A project may still pin one, so never call it unused.
- mise's `source.path` can be a symlink: here it's `~/.config/mise/conf.d/donnes.toml`, and the real file lives in another repo. Show what mise reports and don't follow the link.
- Nothing runs a package's own code, and no manager's install scripts run. Only the read commands above.

**Permissions.** Nothing here reads a consent-gated folder. `~/Library` isn't read in this version.

## MCP

A `packages` tool, decoded into the typed `ToolCall` enum, calling the same functions. Follow plan 26's contract if it has landed by then.

- Arguments: `source` (optional, one of the six), `updates` (bool, default false, since it can reach the network).
- Reply: one package per line, `name  source  version[ -> latest]  path`.
- Description: `List packages installed through mise, Homebrew, bun, pnpm and npm, and apps. Read-only.`

It reports each failed source on its own line, and returns what the others found.

## View

- `browser/packages.rs` holds the list, filters and search filter, with its own `uniform_list`. Its rows are `Package`, not `Entry`, so it doesn't reach into the file list's marquee, drag and sort code.
- `Stop` gets a `packages: bool` beside `recents`, and `session.rs`'s `StopState` the same with `#[serde(default)]`, so existing `windows.json` files still load. With three kinds of stop, this may be the point to replace both flags with an enum. Do that if it makes `showing_folder` and its callers simpler, and in its own commit.
- `showing_folder` must return false in Packages. That's what keeps the file commands from running on a folder behind it. Check every caller.
- `inspector.rs` gets a Package body beside its Entry one, reusing its section and row helpers.
- `sidebar.rs`'s `foot` gets the button and `Here` an `in_packages`.
- New icon: `crates/monodrive/assets/icons/package.svg`, in the set's stroke weight.
- `Roots` reaches the view as a global, as `Places` does for the Trash, so a test's Packages reads a temp folder and never runs a real manager.

## Docs when it lands

- `DESIGN.md`: a Packages section with the rules above, and the foot's new button.
- `AGENTS.md`: the `packages` core module and `browser/packages.rs` in "Where code lives".
- This plan's row in the README's tracker.

## Verification

Tests, each protecting something a regression would break:

- Each reader over a fixture of captured output or a temp folder: mise's JSON, a Homebrew receipt with and without `installed_on_request`, bun's `package.json` with a scoped package and a `bin` map, and an app bundle with and without `_MASReceipt`.
- Display names: `aqua:anomalyco/opencode` and `github:realm/SwiftLint` come out as `opencode` and `SwiftLint`, and `npm:cf` as `cf`.
- The join: an outdated dependency that has no row changes nothing, and adds no update to the count.
- One source failing leaves the others' rows and gives its error.
- MCP: decoding `source` and `updates`, and the reply's line format.
- `#[gpui::test]` over a temp `Roots`: press the foot button, arrow to a row, press ⌘⌫, and nothing moves to the Trash and no file changes. This is the risk that matters most in the view. Also: Back returns to the folder, and restoring a saved Packages stop works.

No test runs a real `mise`, `brew`, `bun`, `pnpm` or `npm`, or depends on what's installed.

By hand and in snapshots:

- `snapshot` gets `packages`, `packages-updates`, `packages-inspector`, `packages-failed` and `packages-narrow`, dark and light, from fixture rows. Include the foot with the update button showing, to check it still fits at the sidebar's narrowest.
- On this Mac, compare the rows with `mise ls`, `brew list --installed-on-request` and `bun pm ls -g`.
- Launched from Finder or the Dock, not a terminal, to confirm managers are found without the shell's PATH.
- Time from press to first rows, and to all rows. Target: first rows in the frame after the disk reads, and no dropped frame while later sources land. Record the result in `docs/performance.md`. The list has about 130 rows, so it isn't expected to touch the scrolling budgets; run those benchmarks only if the file list's code changes.

## Hit every surface

| Entry | What applies |
|---|---|
| UI and MCP | Both call `monodrive_core::packages` |
| Entry points | Foot button, ⇧⌘U, Go menu, palette |
| Reverse states | Back leaves it, Esc clears the search, a second press on the button stays put |
| macOS only | Homebrew's paths for Apple silicon and Intel, `.app` bundles |
| Permissions | No gated folder is read, and no prompt can appear |

## Changing packages

Not in this version. It needs a decision on each of these before any design:

1. **Configs.** Some mise tools are declared in a file the user keeps elsewhere, under version control. `mise use -g` writes to a different file. Removing one properly means editing the file that declares it, and Monodrive shouldn't assume it may.
2. **The disk rule.** `brew uninstall`, `bun remove` and `mise uninstall` delete rather than move to the Trash. Those can't be made reversible, so AGENTS.md's third rule needs an explicit sign-off for this case. Apps can go to the Trash, through `ops`.
3. **Leftovers.** An app's sheet could offer folders in `~/Library` that belong to it. Find them by bundle ID, never by a name that merely looks right, because a wrong match offers another app's data for deletion. Reading some of those folders needs Full Disk Access.

Prompted by the same data, but out of scope too: a view of which copy of a command runs (the option not picked), and managers beyond these five, such as cargo and uv.
