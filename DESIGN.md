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
- Rows in menus, pickers and popovers are 28 pt, or 40 pt with a muted subtitle on a second line. Sidebar rows are 32 pt, or 40 pt for a drive with its capacity bar. File rows follow the Row size setting: 24, 28 or 34 pt.
- Rows in a stacked list of highlightable rows have a 2 pt gap (`ROW_GAP`), so highlights never touch. The gap belongs to the row above it: `row_slot` holds the role and the handlers, the row inside holds the highlight, so a hover, click or drop in the gap lands on that row. Two lists differ. The file list is virtualized, its rows touch as in Finder, and a change to its row height touches `uniform_list`, marquee math and the scrolling budget. Settings' page list keeps a plain gap, because each page is a Tab stop whose focus ring has to fit the row.
- The top bar, a sheet's header, the Trash bar and the sidebar's foot are 40 pt. The path bar and the column titles are 28 pt. A toast is 36 pt. A Settings row is at least 44 pt.

### Pane bars

Every bar at a pane's top or bottom (the Trash header, the column titles, the path bar) comes from `pane_bar`: its text and controls sit `PANE_INSET`, 16 pt, from the pane's edges and are vertically centered. The left text edge of each bar lines up with the others and with the list's rows. A crumb's own padding counts toward the inset, so its text sits on the line and not its hover pill. The 14 pt corners never reach text at that inset. The grid's cells center in their columns, so only their first column comes near the line.

### Sheets with pages

A row that opens a page in the same sheet ends in a 12 pt chevron. → or ↩ opens the page, and the query starts empty with its own filter. The page's name is a button in the header, with a chevron pointing back. Esc goes back, and so does ← with nothing typed, to what was typed before and the row that opened it. A secondary action on a page's row has a row icon button and a key, and the footer names the key.

A sheet with keys of its own has a 32 pt footer under a hairline, with its text 16 pt from the edges: each key in a fully rounded 16 pt cap at 0.08, then what it does, in `TEXT_CAPTION` at 0.65. It lists the arrows, ↩, Esc and what's special to this page, nothing else. Show what would run and dim what wouldn't: the keys skip a dimmed row, and it says why in its subtitle when that isn't obvious.

### Top bar and path bar

The top bar has one job: where am I, among which tabs. It holds Back and Forward, the current folder's name in semibold, and the jobs ring at the right. Nothing else goes there, except the sidebar's button and the search field while the sidebar is closed. The name is the first tab, so it doesn't move when tabs arrive.

With two or more tabs, each is a 24 pt pill named for its folder, 180 pt wide: 0.14 fill and semibold ink for the active one, none and 0.65 ink for the rest, with the usual hover. A press selects it, a drag reorders it, and hovering shows a 20 pt ✕ over the name's end, which moves nothing. A + follows the last pill. Too many to fit, they shrink to 96 pt, truncating their names, and then the strip scrolls sideways; the active one scrolls into view. The bar always leaves 24 pt to drag the window by.

The path bar is 28 pt, under the files, and part of the pane, so each pane gets its own. ⌥⌘P or Settings hides it, and it's on by default. On the left, in a folder, are the path crumbs. On the right is the status: `1 of 7 selected, 6 bytes · 49.6 GB free`, or `2 items` for search results and Recents. Toasts sit above it.

A crumb has 4 pt of side padding, and its 12 pt chevron has 1 pt on each side. Path crumbs show the current folder and as many ancestors as fit, at full width, counted from the right. Everything else collapses into one "…", which is only a marker: Go to Folder reaches the hidden levels. No ancestor is shown cut short. The current folder's name doesn't truncate until the status text beside it has given way.

A name that can't be used turns the rename field's outline red and shows one short line right under it, over the row below. No row moves, and the line stays in the window.

### Closing the sidebar

⌃⌘S or the icon button closes the sidebar, and it stays closed from then on. The button sits at the sidebar's top right, then after Forward in the top bar. The traffic lights stay, with Back and Forward right after them. The sidebar opens and closes at once, with no animation: a width that moves lays out the whole window each frame. The search field lives in the sidebar, so while it's closed the field shows at the right of the top bar, only while it has the keys or a search is on screen.

### Search

The field's leading icon button opens its menu (⇧⌘S), a 360 pt `Popover` under the field. From the top: what a search looks at (Search Names, Search File Contents ⌥⌘F), Search Only and the folder, then Saved, each 40 pt with its query as the subtitle and row icon buttons to rename (R) and remove (⌫), then Save Search… while something's typed, then Recent and Clear Recent Searches. A name is typed in place of the row. Saving never runs anything, and removing asks nothing, because a saved search is only words. The ✕ at the field's end or Esc leaves a search, and leaving goes back to names and everywhere; with nothing typed, the first Esc does that alone. The icon turns to primary ink while the mode isn't plain, and to a file while it looks at contents, and the prompt says the mode.

A content search's matching lines are rows under their file, up to 5 a file. A line's row is the height of every file row, so `uniform_list` still measures one row, and the row stands for its file: a press selects, opens or drags the file, and the keys step from file to file. A selection or stripe covers the file and its lines as one block, with the row radius only at its ends. The line number is `TEXT_CAPTION` at 0.65, right-aligned in a 40 pt column, and the line is Menlo `TEXT_SMALL` at 0.65 with each match bold in primary ink, without its indent and cut at the start when the first match is far along. Lines only fit the list, so a grid pane shows these results as a list. `, more not shown` in the status says a limit or the time budget left matches out.

### Batch rename

Rename with several selected puts a bar across the pane, above the column titles, and a grid pane shows its rows as a list until it closes. The first line is the rule: Replace or Number as 24 pt pills, its fields, and its options as pills (0.14 fill and semibold when on, as a tab is). Find and Replace With change the name before its extension, or all of it with Include Extension. Number has Name and Start At, whose typed width pads the count, so 001 counts 001, 002. The line wraps in a narrow pane. The second line is what the rule comes to, then Cancel and Apply.

Each selected row shows its new name, medium, with the old one after it at 0.65. A name that can't be used is `danger()` with the reason after it. Selected rows stay at the 0.14 fill wherever the keys are, so that red reads, and they don't open or drag. Sorting or changing the selection counts again in the new order. With a problem on screen, ↩ in a field does nothing and the button says `Skip 2 and Apply`. Esc and Cancel change nothing, and going to another folder or search closes the bar.

### Inspector

⌘I, or Get Info in a menu, opens a 260 pt panel to the right of the panes, under the top bar, and it stays open from then on; ⌘I again, Hide Inspector or its ✕ closes it. It takes its width from the panes and moves nothing else, and it stays out when the list would keep under 200 pt. It's a card beside the pane's in Rounded, with the 8 pt gap, and a region with a hairline in Boxy.

It's about the pane commands act on: the selection, or with none its folder. From the top: a 96 pt icon or picture, the name in `TEXT_TITLE` medium, centered, the kind at 0.65, then sections. A section is a 14 pt icon at 0.55 and a `TEXT_SMALL` medium label at 0.65, 16 pt below the last, in sentence case, with no color of its own. Under it are 24 pt rows: an 84 pt label at 0.65, and the value at the right in primary ink. What's unknown has no row, not a dash; only what the disk can answer late says so, as `Calculating…`. A press in the panel leaves the keys where they were.

What the listing knows shows at once. The disk is read 100 ms after the selection rests, and a read or a folder's total for what's no longer selected is dropped. Behind a privacy prompt it reads nothing and says so.

### Submenus

A row with a submenu ends in a 12 pt chevron, in place of a shortcut. Hovering opens it at once with its first row level with its parent, overlapping by 2 pt, to the left where the window runs out. It closes 200 ms after the pointer settles on another row, so the way across doesn't close it. → and ↩ take the keys in, ← brings them out, and Esc closes the whole menu. A row that's waiting on the disk, such as `Loading…`, is at 0.65 and the keys skip it.

### Two panes

Split View (⌥⌘S) puts two panes side by side, equal to begin with, each with its own column titles, files and path bar. In Rounded each is its own card with an 8 pt gap, and that gap is the divider; in Boxy they touch. Each pane has a 1 pt border: `accent()` on the one commands act on, 0.06 on the other, so turning it on moves nothing else. The divider drags, stops 240 pt from either side, and a double-click makes them equal again. A narrow list drops columns rather than the name, which keeps 160 pt: Kind goes first, then Date Modified, then Size.

### Dragging files

Pressing an item's icon or name starts a drag once the pointer moves; on a selected list row, any part of the row does. Anywhere else a press starts the marquee. An item outside the selection becomes the selection first, as a click would; ⌘ or ⇧ add to it as they do on a click. The pointer carries a 32 pt icon with a count badge when there's more than one.

What a drop would land on gets `drop_outline`, 2 pt of accent inside its edge: a folder's row or cell, a crumb, a tab, a sidebar row, or the whole pane for the folder it shows. Between favorites, the gap gets `insertion_line`. A 28 pt pill sits by the pointer, on a `Popover` surface, and says what will happen: `Move report.pdf to Inbox`, `Copy 3 items to Projects`, `Pin docs to Favorites`. It changes with ⌥ and ⌘ while the pointer rests, says Copy until the target's volume has been read, and flips to the pointer's other side at the window's edge. A drop that would do nothing has no outline and its pill says why, at 0.65: `Already in Inbox`. Over what takes nothing, there's no pill. 

Resting on a folder, a tab, a favorite or a drive for 1 second opens it, in the pane under the pointer, or in the pane commands act on from the sidebar; Space opens it at once, and "Open folders while dragging" in Settings turns this off. Moving off cancels. What would refuse the drop, packages and unavailable places don't open. Just before opening, the outline pulses once, for 150 ms. Esc cancels the drag and returns the tab, the folder, the selection and Back history to where the drag began. A drop stays in what opened, and those folders stay in Back history.

### A location that can't open

A restored tab whose drive or folder is gone, or that would raise a privacy prompt, shows what's wrong in the middle of the pane: its name in `TEXT_TITLE`, one line of why, then Back and Go to Folder, and Open when a person has to ask. No file shows and nothing can land there, and the path bar hides, so nothing reaches for a path another drive may have taken. It opens by itself when its drive comes back.

### Onboarding

A fixed 760 by 520 pt window of its own, not a sheet, because no main window is behind it on first launch: a step list on the left, then the step's title, one or two lines of why, a picture and the buttons, with the primary one bottom right. ↩ does what the primary button does, Esc goes back a step and never closes it, and ⌘W closes it for now, so it comes back next launch. The Full Disk Access picture is drawn from elements, schematic, so it stays crisp in both themes and doesn't date with macOS.

### Type

The system font. Menlo only for code, paths in code boxes, tool names and a file's matching lines.

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

Two levels, both from `floating`: `Popover` for menus, popovers, toasts and the drag's pill, and `Sheet` for sheets over `scrim`. Nothing else casts a shadow, except a control's knob or raised segment. Floating surfaces are frosted in dark themes and opaque in light ones.

### Motion

Nothing moves continuously: it pegs the GPU on high-refresh displays. A sheet may fade in once, for 200 ms at most, and a drag's outline pulses once before a folder opens. Hover and press are instant.

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

- `RADIUS_ROW`, `RADIUS_CARD`, `RADIUS_THUMB`; `TEXT_CAPTION`, `TEXT_SMALL`, `TEXT_BODY`, `TEXT_TITLE`; `TITLE_BAR_HEIGHT`, `PATH_BAR_HEIGHT`, `PANE_INSET`, `ROW_HEIGHT`, `SIDEBAR_ROW_HEIGHT`, `ROW_GAP`, `BUTTON_HEIGHT`, `FIELD_HEIGHT`.
- `content`, `accent`, `on_accent`, `danger`, `danger_fill`, `on_danger`, `success`, `scrim`, `well`, `knob`, `raised`: colors. `focus_ring`, `knob_shadow`, `raised_shadow`: shadows.
- `text_button(id, label, look)`: the usual button. `button(id, look)` is its frame, for a label with an icon or one that changes. `ButtonLook` is `Secondary`, `Primary`, `Destructive`, `Caution`, `Ghost` or `Disabled`.
- `icon_button(id, label, path, enabled)` in chrome, `row_icon_button(id, label, path, ink)` inside a row, `circle_button(id, label, side)` for a circle with other contents, such as the jobs ring.
- `pane_bar(height)`: a bar at a pane's top or bottom.
- `field(border)`: a text field's outline.
- `switch(on)`: a switch's look, without its role or handlers.
- `row_slot(id)`: where a stacked row's role and handlers go, gap included. `row(active)`: the row inside it, in a menu, picker or popover. `row_muted(active)` is its secondary ink. `destructive_row(active)` and `destructive_ink(active)` are the same for a command marked `destructive`.
- `drop_outline()`: what a drop would land on, over it; the caller gives it the shape's radius. `insertion_line()`: where a drop would land between stacked rows, in the gap under the row above.
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
- **A hint for what a disabled control already shows.** A grey Put Back says there's nothing to put back; "Select items to put back." beside it says the same thing twice. Keep text for a real refusal, such as selected items with no known origin.
- **A second shadow.** Each surface once had its own. Use `floating`.
- **Control heights that vary by screen.** A button in a card is the same height as one in Settings.
- **Label mismatch.** A button whose visible label changes still needs an `aria_label` that says what it does.

## How to verify

- `scripts/check-design`, part of `scripts/check`, fails on a raw radius, color, shadow, text size, icon size or ink alpha in a view, on any line layout, and on an import of a raw color. A justified exception is a `// design: <reason>` comment on the line or the line above. It reads text and isn't a Rust parser, and `scripts/check-design-fixtures` holds the cases it must catch and leave alone. Add a fixture when something slips past it.
- `cargo run -p monodrive --example snapshot` draws every surface to `target/snapshots`, dark, and to `target/snapshots/light`. Run it before and after a visual change, and look at both.
