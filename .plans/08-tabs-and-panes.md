# Tabs and split panes

Depends on shared operations and places. Reuse the relevant Monocode GPUI tab, resize and focus behavior; start with two panes rather than importing an arbitrary split tree.

## Tabs

- ⌘T opens a new tab, ⌘W closes it, and ⌃Tab and ⌃⇧Tab switch tabs, as in Finder.
- Each tab has its own folder, history, selection and view.
- Tabs restore on relaunch.

## Split panes

- Two folders side by side, for moving and comparing. There's no Finder equivalent, so pick a free shortcut.
- Copy and move to the other pane, plus drag between panes.

## New windows

- ⌘N opens a new window at the Default size. Several windows share one index and one operation journal.

## Restoring and closing

Each pane owns its location, history, selection and view. Commands act on the focused pane. Closing a pane returns focus to its sibling; closing the last pane closes its tab. Restoring a missing drive shows an unavailable location with reconnect or another-location choices. It never opens a different source that happens to reuse the old path. Closing a tab/window does not cancel a shared job implicitly.

Folder comparison is a later service; two panes alone do not claim that their contents match. UI tests cover independent histories, focus and close/restore behavior.
