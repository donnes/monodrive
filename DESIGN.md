# Design

## Scope

Every pixel Monodrive draws itself: the window, the sidebar, the file list and grid, menus, popovers, sheets and Settings. It doesn't cover the app icon (`assets/README.md`) or file icons, which macOS draws. Scales and controls live in `crates/monodrive/src/theme.rs`; views use them instead of inventing their own.

## Who it's for

Someone moving through files fast, mostly from the keyboard, who chose this over Finder because it answers at once. Speed comes first: no decoration may cost a frame (`docs/performance.md`). Where a choice is open, do what Finder and macOS do.

## Decisions

### Shape

| What | Radius | In code |
|---|---|---|
| Text buttons, text fields, segmented controls, dropdown triggers | Fully rounded | `rounded_full` |
| Icon buttons and icon tiles | Circles | `rounded_full` |
| Toggles, badges, dots, chips, progress bars | Fully rounded | `rounded_full` |
| List, menu and sidebar rows; every highlight behind content, including the grid's icon and name | 8 pt | `RADIUS_ROW` |
| Cards, sheets, popovers, toasts, the content pane, Settings plates, code boxes | 14 pt | `RADIUS_CARD` |
| Thumbnails and color swatches, which are pictures and not controls | 4 pt | `RADIUS_THUMB` |
| The marquee, which matches Finder's | Square | none |

The header of a sheet with a query (palette, Go to Folder, Connect) is a bare field with no outline, as in Spotlight: there is no shape to round. Any field with a visible outline is a pill.

### Size

- Every text button is 24 pt high, with 12 pt side padding, or 8 pt with no fill.
- Every icon button is a 24 pt circle with a 14 pt icon. Inside a row it's a 20 pt circle with a 12 pt icon.
- Every outlined text field is 28 pt high. The exception is rename, which fits inside the row it edits.
- Rows in menus, pickers and popovers are 28 pt. Sidebar rows are 32 pt, or 40 pt for a drive with its capacity bar. File rows follow the Row size setting: 24, 28 or 34 pt.
- Rows in a stacked list of highlightable rows have a 2 pt gap (`ROW_GAP`), so highlights never touch. The gap belongs to the row above it: `row_slot` holds the role and the handlers, the row inside holds the highlight, so a hover, click or drop in the gap lands on that row. Two lists differ. The file list is virtualized, its rows touch as in Finder, and a change to its row height touches `uniform_list`, marquee math and the scrolling budget. Settings' page list keeps a plain gap, because each page is a Tab stop whose focus ring has to fit the row.
- The top bar, a sheet's header, the Trash bar and the sidebar's foot are 40 pt. The path bar is 22 pt. A toast is 36 pt. A Settings row is at least 44 pt.

### Top bar and path bar

The top bar has one job: where am I, among which tabs. It holds Back and Forward, the current folder's name in semibold, and the jobs ring at the right. Nothing else goes there. The name is where the first tab will go, so it doesn't move when tabs arrive.

The path bar is 22 pt, under the files, and part of the pane, so each pane gets its own. ⌥⌘P or Settings hides it, and it's on by default. On the left, in a folder, are the path crumbs. On the right is the status: `1 of 7 selected, 6 bytes · 49.6 GB free`, or `2 items` for search results and Recents. Toasts sit above it.

Path crumbs show the current folder and as many ancestors as fit, at full width, counted from the right. Everything else collapses into one "…", which is only a marker: Go to Folder reaches the hidden levels. No ancestor is shown cut short. The current folder's name doesn't truncate until the status text beside it has given way.

A name that can't be used turns the rename field's outline red and shows one short line right under it, over the row below. No row moves, and the line stays in the window.

### Type

The system font. Menlo only for code, paths in code boxes and tool names.

| Size | Constant | For |
|---|---|---|
| 11 | `TEXT_CAPTION` | Shortcuts, descriptions under a label, captions |
| 12 | `TEXT_SMALL` | Buttons, section labels, column titles, messages |
| 13 | `TEXT_BODY` | Rows, body copy, Settings labels |
| 14 | `TEXT_TITLE` | Sidebar rows, a card's title, typed fields |

Weights: regular by default, medium for section labels, sidebar rows, primary buttons and what's selected, semibold for the current folder and group titles. The Row size and Label size settings choose from 12, 13 and 14.

### Color

Ink is the theme's content color at a fixed alpha, through `content()`. Nothing else makes a grey.

| Alpha | For |
|---|---|
| 1 | Primary text, a hovered or selected icon |
| 0.65 | Muted text, the lowest alpha text may use: about 4.5:1 on white and on the dark pane |
| 0.55 | Icons at rest and meter fills, 3:1 on both |
| 0.3 | Disabled text and icons only |
| 0.2 | A pressed fill, a switch that's off |
| 0.14 | A hovered button, a selected row, a progress track |
| 0.12 | Outlines of fields and floating surfaces, separators |
| 0.08 | A button at rest, a hovered row |
| 0.06 | Hairlines between regions and Settings rows |
| 0.03 | List stripes and Settings plates |

- `accent()` with `on_accent()` is for the selection, the row under the keys, primary buttons, switches and focus. Text on the accent stays opaque, secondary text included: at 0.8 it falls to 3.7:1 on some presets.
- `danger()` is for what failed or can't be undone, and only that. `success()` is for what's done.
- A command that trashes or erases files is marked `destructive` in `commands.rs`: today Move to Trash and Empty Trash, and not Remove from Sidebar, which only forgets a bookmark. In the context menu and the palette its title and shortcut are `danger()`. Under the keys, its row fills with `danger_fill()` and all its text is `on_danger()`, at 4.5:1 or better in both themes. The native menu bar stays native.
- Over the background image, content keeps the same ink because the image is dimmed under the list. Check `background.png` in the snapshots, where the image runs from bright to dark, when changing either.
- No raw color in a view. A swatch that shows a color being offered is the one honest exception.

### States

| State | Looks like |
|---|---|
| Hover | A filled button goes from 0.08 to 0.14, or to 0.9 opacity if it's accent or red. A bare control gains the 0.08 fill. Rows gain 0.08. |
| Pressed | Buttons only: 0.2 fill, or 0.8 opacity. |
| Keyboard focus | `focus_ring()`, 2.5 pt of accent outside the shape. Fields show it as an accent outline. Every Tab stop shows one. |
| Selected, where the keys are | Accent fill, `on_accent` ink. |
| Selected, keys elsewhere | 0.14 fill, primary ink, as Finder greys its selection. |
| You are here (sidebar, Settings pages) | 0.14 fill, primary ink. |
| Disabled | `ButtonLook::Disabled`: 0.3 ink, no hover, no pointer. It still says what it would do. |
| Destructive | `Destructive` (filled red) only for the final confirmation of what can't be undone. `Caution` (tinted red) for the button that leads there, and for retrying a failure. Moving to the Trash isn't destructive. |

### Spacing

2, 4, 6, 8, 12, 16 and 24 pt. Text in a card lines up with the card's title at 16 pt.

### Elevation

Two levels, both from `floating`: `Popover` for menus, popovers and toasts, and `Sheet` for sheets over `scrim`. Nothing else casts a shadow, except a control's knob or raised segment. Floating surfaces are frosted in dark themes and opaque in light ones.

### Motion

Nothing moves continuously: it pegs the GPU on high-refresh displays. A sheet may fade in once, for 200 ms at most. Hover and press are instant.

### Copy

- Sentences, descriptions, section labels and errors are in sentence case and end with a period when they're full sentences. Commands keep macOS title case, as Finder does: Empty Trash, Keep Both.
- A toast says what happened, in past tense: `Renamed to "Notes"`. It offers Undo if it can.
- An error says what's wrong in one line, without apology: `"Brief.pdf" is already taken.`. Errors from fsearch pass through as they are.
- Use a file's name where the name is enough. A full path appears only where the path is the point: search results, Recents, Go to Folder, Copy Path.

### Icons

The SVGs in `crates/monodrive/assets/icons`, one stroke weight, tinted with `.text_color`. Sizes are 12 pt inside small controls, 14 pt in chrome and fields, and 16 pt in rows.

### macOS conventions

⌘ shortcuts shown with their symbols, Finder's sizes and sort order, Trash instead of delete, ↩ for what a sheet's primary button does, Esc to leave it, and the traffic lights in the 40 pt top bar. Buttons sit at the end of a card, with the primary one last.

## Primitives

All in `theme.rs`. Each returns an element the caller extends.

- `RADIUS_ROW`, `RADIUS_CARD`, `RADIUS_THUMB`; `TEXT_CAPTION`, `TEXT_SMALL`, `TEXT_BODY`, `TEXT_TITLE`; `TITLE_BAR_HEIGHT`, `PATH_BAR_HEIGHT`, `ROW_HEIGHT`, `SIDEBAR_ROW_HEIGHT`, `ROW_GAP`, `BUTTON_HEIGHT`, `FIELD_HEIGHT`.
- `content`, `accent`, `on_accent`, `danger`, `danger_fill`, `on_danger`, `success`, `scrim`, `well`, `knob`, `raised`: colors. `focus_ring`, `knob_shadow`, `raised_shadow`: shadows.
- `text_button(id, label, look)`: the usual button. `button(id, look)` is its frame, for a label with an icon or one that changes. `ButtonLook` is `Secondary`, `Primary`, `Destructive`, `Caution`, `Ghost` or `Disabled`.
- `icon_button(id, label, path, enabled)` in chrome, `row_icon_button(id, label, path, ink)` inside a row, `circle_button(id, label, side)` for a circle with other contents, such as the jobs ring.
- `field(border)`: a text field's outline.
- `row_slot(id)`: where a stacked row's role and handlers go, gap included. `row(active)`: the row inside it, in a menu, picker or popover. `row_muted(active)` is its secondary ink. `destructive_row(active)` and `destructive_ink(active)` are the same for a command marked `destructive`.
- `floating(elevation)`: a floating surface. Its contents need `relative()`.
- `progress_track()`, `progress_fill(fraction)`: a 4 pt bar.
- `icon(path, side)`, `section_label(label)`.

Add a primitive once two views need it. Until then, build from the scales.

## Mistakes to avoid

- **A hand-built button.** `div().role(Role::Button).h(px(22.))` drifts within a release. Use `text_button` or `button`, and chain what's special.
- **A radius picked by eye.** 5, 6, 7 and 10 pt all look fine alone and wrong together. There are three radii and `rounded_full`.
- **A one-off text size.** 12.5 pt came from matching a mockup. Pick from the scale.
- **Faint text.** `content(0.35)` looks refined on the author's display and fails on a light theme. Text stops at 0.65; only disabled text goes lower.
- **An error in the accent.** Go to Folder and the shortcut recorder used to do this. Failure is `danger()`.
- **A local red or green.** Settings had its own for errors and for Added. Use the tokens, which are tuned for both themes.
- **Touching highlights.** A hovered row against a selected one reads as one block. Stack rows in `row_slot`, so the gap is there and still answers.
- **A crowded top bar.** A status, an error or a button added there squeezes the name that says where you are. Put it in the path bar, or next to what it's about.
- **A second shadow.** Each surface once had its own. Use `floating`.
- **Control heights that vary by screen.** A button in a card is the same height as one in Settings.
- **Label mismatch.** A button whose visible label changes still needs an `aria_label` that says what it does.

## How to verify

- `scripts/check-design`, part of `scripts/check`, fails on a raw radius, color, shadow, text size, icon size or ink alpha in a view, on any line layout, and on an import of a raw color. A justified exception is a `// design: <reason>` comment on the line or the line above. It reads text and isn't a Rust parser, and `scripts/check-design-fixtures` holds the cases it must catch and leave alone. Add a fixture when something slips past it.
- `cargo run -p monodrive --example snapshot` draws every surface to `target/snapshots`, dark, and to `target/snapshots/light`. Run it before and after a visual change, and look at both.
