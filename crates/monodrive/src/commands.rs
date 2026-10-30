//! Every command a person can give Monodrive, in one table. The keymap, the
//! menu bar, the command palette and the Keyboard settings page all read it,
//! so a command is defined once. A command's id is its action's name.
//!
//! Navigation and editing keys (arrows, Tab, Esc, the text field's caret
//! keys) are not commands: they stay with the views that own them and can't
//! be rebound. Cut, Copy, Paste and Select All are commands, one action each
//! as in AppKit: a text field acts on its text, the file list on its files.

use crate::browser::{
    AddToSidebar, Back, ClearRecents, CloseTab, ConnectToServer, CopyPath, CopyToOtherPane,
    DeselectAll, Duplicate, EditFavorites, Eject, EmptyTrash, EnclosingFolder, FocusSearch,
    Forward, GoApplications, GoComputer, GoDesktop, GoDocuments, GoDownloads, GoHome, GoRecents,
    GoToFolder, GoTrash, MoveHere, MoveToOtherPane, MoveToTrash, NewFile, NewFolder,
    NewFolderWithSelection, NewTab, NextTab, OpenInNewTab, OpenInTerminal, OpenSelected,
    OpenSettings, PreviousTab, PutBack, QuickLook, Redo, RemoveFromSidebar, Rename, ResetZoom,
    RevealInFinder, ReverseSort, ShowCopiesAndMoves, ShowPackageContents, SortByKind,
    SortByModified, SortByName, SortBySize, ToggleHiddenFiles, TogglePathBar, ToggleSplit, Undo,
    ViewAsGrid, ViewAsList, ZoomIn, ZoomOut,
};
use crate::preferences::Preferences;
use crate::{browser, connect, empty_trash, goto, jobs, menu, palette, settings, text_field};
use gpui::{
    Action, App, Global, KeyBinding, KeyBindingContextPredicate, Keystroke, Menu, MenuItem,
    OsAction, actions,
};
use monodrive_core::entries::SortBy;
use std::collections::HashMap;
use std::rc::Rc;

actions!(
    monodrive,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        CloseWindow,
        OpenPalette
    ]
);

/// User overrides by command id: a shortcut, or `None` to turn it off.
pub type Shortcuts = HashMap<String, Option<String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    App,
    File,
    Edit,
    Go,
    View,
    Window,
}

impl Group {
    pub const ALL: [Group; 6] = [
        Group::App,
        Group::File,
        Group::Edit,
        Group::Go,
        Group::View,
        Group::Window,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::App => "Monodrive",
            Group::File => "File",
            Group::Edit => "Edit",
            Group::Go => "Go",
            Group::View => "View",
            Group::Window => "Window",
        }
    }
}

/// Where a context menu offers a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// On what's selected in the listing.
    Selection,
    /// On two or more selected entries.
    Several,
    /// On a lone app or other package.
    Package,
    /// On the listing's empty space, so the folder shown.
    Folder,
    /// On a place in the sidebar.
    Sidebar,
}

pub struct Command {
    pub title: &'static str,
    pub group: Group,
    /// The default shortcuts.
    pub keys: &'static [&'static str],
    /// Where the shortcut works; `None` is anywhere in the window.
    pub context: Option<&'static str>,
    pub action: fn() -> Box<dyn Action>,
    /// Starts a new section of its menu.
    pub separated: bool,
    /// The standard AppKit command it is, so system panels' fields get it.
    pub os_action: Option<OsAction>,
    /// Where context menus offer it.
    pub scopes: &'static [Scope],
    /// Trashes or erases files. Menus Monodrive draws show it in the danger
    /// color; the native menu bar doesn't.
    pub destructive: bool,
}

impl Command {
    pub fn id(&self) -> &'static str {
        (self.action)().name()
    }

    /// The shortcuts it runs on: the defaults unless overridden. An override
    /// that doesn't parse falls back to the defaults rather than losing them.
    pub fn shortcuts(&self, overrides: &Shortcuts) -> Vec<String> {
        match overrides.get(self.id()) {
            Some(None) => Vec::new(),
            Some(Some(keys)) if valid(keys) => vec![keys.clone()],
            _ => self.keys.iter().map(|keys| keys.to_string()).collect(),
        }
    }
}

pub fn all() -> Vec<Command> {
    const BROWSER: Option<&str> = Some("Browser");
    fn command(
        title: &'static str,
        group: Group,
        keys: &'static [&'static str],
        context: Option<&'static str>,
        action: fn() -> Box<dyn Action>,
    ) -> Command {
        Command {
            title,
            group,
            keys,
            context,
            action,
            separated: false,
            os_action: None,
            scopes: &[],
            destructive: false,
        }
    }
    fn destructive(command: Command) -> Command {
        Command {
            destructive: true,
            ..command
        }
    }
    fn separated(command: Command) -> Command {
        Command {
            separated: true,
            ..command
        }
    }
    fn standard(os_action: OsAction, command: Command) -> Command {
        Command {
            os_action: Some(os_action),
            ..command
        }
    }
    fn on(scopes: &'static [Scope], command: Command) -> Command {
        Command { scopes, ..command }
    }
    const LIST: Option<&str> = Some("FileList");
    const SELECTION: &[Scope] = &[Scope::Selection];
    const FOLDER: &[Scope] = &[Scope::Folder];
    use Group::*;
    vec![
        command("Settings…", App, &["cmd-,"], None, || {
            Box::new(OpenSettings)
        }),
        destructive(separated(command(
            "Empty Trash…",
            App,
            &["cmd-shift-backspace"],
            BROWSER,
            || Box::new(EmptyTrash),
        ))),
        separated(command("Hide Monodrive", App, &["cmd-h"], None, || {
            Box::new(Hide)
        })),
        command("Hide Others", App, &["cmd-alt-h"], None, || {
            Box::new(HideOthers)
        }),
        command("Show All", App, &[], None, || Box::new(ShowAll)),
        separated(command("Quit Monodrive", App, &["cmd-q"], None, || {
            Box::new(Quit)
        })),
        command("New Tab", File, &["cmd-t"], BROWSER, || Box::new(NewTab)),
        separated(command("Close Tab", File, &["cmd-w"], BROWSER, || {
            Box::new(CloseTab)
        })),
        separated(on(
            FOLDER,
            command("New Folder", File, &["cmd-shift-n"], BROWSER, || {
                Box::new(NewFolder)
            }),
        )),
        on(
            FOLDER,
            command("New File", File, &[], BROWSER, || Box::new(NewFile)),
        ),
        on(
            &[Scope::Selection, Scope::Sidebar],
            separated(command(
                "Open",
                File,
                &["cmd-down", "cmd-o"],
                BROWSER,
                || Box::new(OpenSelected),
            )),
        ),
        // ⌃⌘O, as Finder's.
        on(
            &[Scope::Selection, Scope::Sidebar],
            command("Open in New Tab", File, &["ctrl-cmd-o"], BROWSER, || {
                Box::new(OpenInNewTab)
            }),
        ),
        on(
            &[Scope::Package],
            command("Show Package Contents", File, &[], BROWSER, || {
                Box::new(ShowPackageContents)
            }),
        ),
        on(
            SELECTION,
            command("Reveal in Finder", File, &[], BROWSER, || {
                Box::new(RevealInFinder)
            }),
        ),
        on(
            SELECTION,
            command("Open in Terminal", File, &[], BROWSER, || {
                Box::new(OpenInTerminal)
            }),
        ),
        // What's on offer depends on the row: each is there only where it
        // would run, which the views decide.
        on(
            SELECTION,
            separated(command(
                "Add to Sidebar",
                File,
                &["ctrl-cmd-t"],
                BROWSER,
                || Box::new(AddToSidebar),
            )),
        ),
        on(
            &[Scope::Sidebar],
            separated(command(
                "Remove from Sidebar",
                File,
                &["cmd-backspace"],
                Some("Sidebar"),
                || Box::new(RemoveFromSidebar),
            )),
        ),
        on(
            &[Scope::Sidebar],
            command("Eject", File, &["cmd-e"], BROWSER, || Box::new(Eject)),
        ),
        on(
            &[Scope::Sidebar],
            command("Clear Recents", File, &[], BROWSER, || {
                Box::new(ClearRecents)
            }),
        ),
        // Finder offers it on several items only, though ⌃⌘N takes one.
        on(
            &[Scope::Several],
            separated(command(
                "New Folder with Selection",
                File,
                &["ctrl-cmd-n"],
                LIST,
                || Box::new(NewFolderWithSelection),
            )),
        ),
        // Return renames, as in Finder; → and ⌘↓ still go in.
        on(
            SELECTION,
            command("Rename", File, &["enter"], LIST, || Box::new(Rename)),
        ),
        on(
            SELECTION,
            command("Duplicate", File, &["cmd-d"], LIST, || Box::new(Duplicate)),
        ),
        // Space as in Finder. ⌘Y comes first because the menu bar takes the
        // first key, and a bare Space there would beat typing one.
        on(
            SELECTION,
            command("Quick Look", File, &["cmd-y", "space"], LIST, || {
                Box::new(QuickLook)
            }),
        ),
        // Function keys, as two-pane file managers have long used them.
        on(
            SELECTION,
            separated(command(
                "Copy to Other Pane",
                File,
                &["alt-f5"],
                BROWSER,
                || Box::new(CopyToOtherPane),
            )),
        ),
        on(
            SELECTION,
            command("Move to Other Pane", File, &["alt-f6"], BROWSER, || {
                Box::new(MoveToOtherPane)
            }),
        ),
        on(
            SELECTION,
            destructive(separated(command(
                "Move to Trash",
                File,
                &["cmd-backspace"],
                LIST,
                || Box::new(MoveToTrash),
            ))),
        ),
        on(
            SELECTION,
            command("Put Back", File, &[], BROWSER, || Box::new(PutBack)),
        ),
        // From the list only, so ⌘Z while typing a search never changes
        // files; the menu bar's Undo works anywhere in the window.
        command("Undo", Edit, &["cmd-z"], LIST, || Box::new(Undo)),
        command("Redo", Edit, &["cmd-shift-z"], LIST, || Box::new(Redo)),
        standard(
            OsAction::Cut,
            separated(command("Cut", Edit, &["cmd-x"], None, || {
                Box::new(text_field::Cut)
            })),
        ),
        on(
            SELECTION,
            standard(
                OsAction::Copy,
                command("Copy", Edit, &["cmd-c"], None, || {
                    Box::new(text_field::Copy)
                }),
            ),
        ),
        on(
            FOLDER,
            standard(
                OsAction::Paste,
                command("Paste", Edit, &["cmd-v"], None, || {
                    Box::new(text_field::Paste)
                }),
            ),
        ),
        command("Move Here", Edit, &["cmd-alt-v"], LIST, || {
            Box::new(MoveHere)
        }),
        standard(
            OsAction::SelectAll,
            command("Select All", Edit, &["cmd-a"], None, || {
                Box::new(text_field::SelectAll)
            }),
        ),
        command("Deselect All", Edit, &["cmd-alt-a"], BROWSER, || {
            Box::new(DeselectAll)
        }),
        on(
            SELECTION,
            separated(command("Copy Path", Edit, &["cmd-alt-c"], BROWSER, || {
                Box::new(CopyPath)
            })),
        ),
        command("Back", Go, &["cmd-["], BROWSER, || Box::new(Back)),
        command("Forward", Go, &["cmd-]"], BROWSER, || Box::new(Forward)),
        command("Enclosing Folder", Go, &["cmd-up"], BROWSER, || {
            Box::new(EnclosingFolder)
        }),
        separated(command("Recents", Go, &["cmd-shift-f"], BROWSER, || {
            Box::new(GoRecents)
        })),
        command("Home", Go, &["cmd-shift-h"], BROWSER, || Box::new(GoHome)),
        command("Desktop", Go, &["cmd-shift-d"], BROWSER, || {
            Box::new(GoDesktop)
        }),
        command("Documents", Go, &["cmd-shift-o"], BROWSER, || {
            Box::new(GoDocuments)
        }),
        command("Downloads", Go, &["cmd-alt-l"], BROWSER, || {
            Box::new(GoDownloads)
        }),
        command("Applications", Go, &["cmd-shift-a"], BROWSER, || {
            Box::new(GoApplications)
        }),
        command("Macintosh HD", Go, &["cmd-shift-c"], BROWSER, || {
            Box::new(GoComputer)
        }),
        command("Trash", Go, &[], BROWSER, || Box::new(GoTrash)),
        command("Go to Folder…", Go, &["cmd-shift-g"], BROWSER, || {
            Box::new(GoToFolder)
        }),
        command(
            "Connect to Server…",
            Go,
            &["cmd-shift-k"],
            BROWSER,
            || Box::new(ConnectToServer),
        ),
        separated(command("Search", Go, &["cmd-f"], BROWSER, || {
            Box::new(FocusSearch)
        })),
        on(
            FOLDER,
            command("as List", View, &["cmd-1"], None, || Box::new(ViewAsList)),
        ),
        on(
            FOLDER,
            command("as Grid", View, &["cmd-2"], None, || Box::new(ViewAsGrid)),
        ),
        separated(command("Split View", View, &["cmd-alt-s"], BROWSER, || {
            Box::new(ToggleSplit)
        })),
        separated(command(
            "Command Palette…",
            View,
            &["cmd-k", "cmd-shift-p"],
            None,
            || Box::new(OpenPalette),
        )),
        separated(command("Edit Favorites", View, &[], BROWSER, || {
            Box::new(EditFavorites)
        })),
        on(
            FOLDER,
            separated(command("Show Hidden Files", View, &["cmd->"], None, || {
                Box::new(ToggleHiddenFiles)
            })),
        ),
        command("Show Path Bar", View, &["alt-cmd-p"], None, || {
            Box::new(TogglePathBar)
        }),
        on(
            FOLDER,
            separated(command(
                "Sort by Name",
                View,
                &["ctrl-alt-cmd-1"],
                None,
                || Box::new(SortByName),
            )),
        ),
        on(
            FOLDER,
            command(
                "Sort by Date Modified",
                View,
                &["ctrl-alt-cmd-2"],
                None,
                || Box::new(SortByModified),
            ),
        ),
        on(
            FOLDER,
            command("Sort by Size", View, &["ctrl-alt-cmd-3"], None, || {
                Box::new(SortBySize)
            }),
        ),
        on(
            FOLDER,
            command("Sort by Kind", View, &["ctrl-alt-cmd-4"], None, || {
                Box::new(SortByKind)
            }),
        ),
        command(
            "Reverse Sort Order",
            View,
            &["ctrl-alt-cmd-0"],
            None,
            || Box::new(ReverseSort),
        ),
        separated(command("Zoom In", View, &["cmd-=", "cmd-+"], None, || {
            Box::new(ZoomIn)
        })),
        command("Zoom Out", View, &["cmd--"], None, || Box::new(ZoomOut)),
        command("Actual Size", View, &["cmd-0"], None, || {
            Box::new(ResetZoom)
        }),
        command("Minimize", Window, &["cmd-m"], None, || Box::new(Minimize)),
        command("Close Window", Window, &["cmd-shift-w"], None, || {
            Box::new(CloseWindow)
        }),
        separated(command(
            "Show Next Tab",
            Window,
            &["ctrl-tab"],
            BROWSER,
            || Box::new(NextTab),
        )),
        command(
            "Show Previous Tab",
            Window,
            &["ctrl-shift-tab"],
            BROWSER,
            || Box::new(PreviousTab),
        ),
        separated(command(
            "Copies and Moves",
            Window,
            &["cmd-alt-j"],
            None,
            || Box::new(ShowCopiesAndMoves),
        )),
    ]
}

fn valid(keys: &str) -> bool {
    !keys.trim().is_empty() && keys.split_whitespace().all(|k| Keystroke::parse(k).is_ok())
}

/// Contexts drawn inside others, whose keys therefore compete: the list,
/// sidebar and search field inside the browser, the name being edited in the
/// list, the field inside the palette, Go to Folder and Connect to Server.
const NESTED: [(&str, &str); 8] = [
    ("Browser", "FileList"),
    ("Browser", "Sidebar"),
    ("Browser", "Search"),
    ("Browser", "TextField"),
    ("FileList", "Rename"),
    ("Palette", "TextField"),
    ("GoTo", "TextField"),
    ("Connect", "TextField"),
];

fn overlap(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, _) | (_, None) => true,
        (Some(a), Some(b)) => a == b || NESTED.contains(&(a, b)) || NESTED.contains(&(b, a)),
    }
}

/// What `keys` would clash with if command `id` took it in `context`: another
/// command's title, or "a built-in key". Reads the live keymap, so shortcuts
/// freed by an override or turned off are free again.
///
/// Matching is GPUI's own: on macOS ⇧⌘. arrives as ">" with ⌘, and still
/// matches a binding written "cmd-shift-.".
pub fn conflict(stroke: &Keystroke, id: &str, context: Option<&str>, cx: &App) -> Option<String> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let clash = keymap.bindings().find(|binding| {
        let theirs = binding.predicate().and_then(|p| match &*p {
            KeyBindingContextPredicate::Identifier(name) => Some(name.to_string()),
            _ => None,
        });
        matches!(binding.keystrokes(), [only] if stroke.should_match(only))
            && binding.action().name() != id
            && overlap(context, theirs.as_deref())
    })?;
    let name = clash.action().name();
    Some(
        all()
            .iter()
            .find(|command| command.id() == name)
            .map_or_else(|| "a built-in key".to_string(), |c| c.title.to_string()),
    )
}

/// Whether `stroke` is a shortcut bound to command `id` now.
pub fn is_shortcut_of(stroke: &Keystroke, id: &str, cx: &App) -> bool {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap.bindings().any(|binding| {
        binding.action().name() == id
            && matches!(binding.keystrokes(), [only] if stroke.should_match(only))
    })
}

/// Whether command `id` acts on what's selected or the folder shown: anything
/// a context menu offers, past the View menu's display settings, and Move Here.
pub(crate) fn acts_on_target(id: &str) -> bool {
    id == MoveHere.name()
        || all().iter().any(|command| {
            command.id() == id && !command.scopes.is_empty() && command.group != Group::View
        })
}

/// The shortcuts command `id` runs on now.
pub(crate) fn shortcuts_of(id: &str, cx: &App) -> Vec<String> {
    let overrides = &cx.global::<Preferences>().shortcuts;
    all()
        .iter()
        .find(|command| command.id() == id)
        .map_or_else(Vec::new, |command| command.shortcuts(overrides))
}

/// The label of the first shortcut command `id` runs on now, if any.
pub fn shortcut_label(id: &str, cx: &App) -> Option<String> {
    shortcuts_of(id, cx).first().map(|keys| label(keys))
}

/// A shortcut as macOS shows it: "cmd-shift-p" is "⇧⌘P".
pub fn label(keys: &str) -> String {
    keys.split_whitespace()
        .filter_map(|k| Keystroke::parse(k).ok())
        .map(|k| {
            let m = k.modifiers;
            let mut out = String::new();
            for (on, glyph) in [
                (m.control, "⌃"),
                (m.alt, "⌥"),
                (m.shift, "⇧"),
                (m.platform, "⌘"),
            ] {
                if on {
                    out.push_str(glyph);
                }
            }
            out.push_str(&match k.key.as_str() {
                "up" => "↑".into(),
                "down" => "↓".into(),
                "left" => "←".into(),
                "right" => "→".into(),
                "enter" => "↩".into(),
                "escape" => "⎋".into(),
                "tab" => "⇥".into(),
                "backspace" => "⌫".into(),
                "delete" => "⌦".into(),
                "space" => "Space".into(),
                "pageup" => "⇞".into(),
                "pagedown" => "⇟".into(),
                "home" => "↖".into(),
                "end" => "↘".into(),
                key => key.to_uppercase(),
            });
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One row of a context menu.
pub struct MenuRow {
    pub title: &'static str,
    /// Its shortcut as bound now.
    pub keys: Option<String>,
    pub checked: bool,
    /// Starts a new section.
    pub separated: bool,
    pub destructive: bool,
    pub action: Box<dyn Action>,
}

/// The context menu on `scopes`: the commands offered there that `runs` says
/// would run now, in the menu bar's order, with a separator wherever the menu
/// bar starts a new section or menu.
pub fn context_menu(
    scopes: &[Scope],
    prefs: &Preferences,
    runs: impl Fn(&dyn Action) -> bool,
) -> Vec<MenuRow> {
    let ticked = checked(prefs);
    let mut rows = Vec::new();
    let (mut section, mut group, mut last) = (0, None, None);
    for command in all() {
        if command.separated || group != Some(command.group) {
            section += 1;
            group = Some(command.group);
        }
        let action = (command.action)();
        if !command.scopes.iter().any(|scope| scopes.contains(scope)) || !runs(&*action) {
            continue;
        }
        rows.push(MenuRow {
            title: command.title,
            keys: command
                .shortcuts(&prefs.shortcuts)
                .first()
                .map(|k| label(k)),
            checked: ticked.contains(&command.id()),
            separated: last.is_some_and(|last| last != section),
            destructive: command.destructive,
            action,
        });
        last = Some(section);
    }
    rows
}

/// The overrides the keymap was last built from.
struct Applied(Shortcuts);
impl Global for Applied {}

/// Builds the keymap from each view's own keys and every command's effective
/// shortcuts, then the menu bar, which copies shortcuts when it's built.
/// Does nothing when the overrides haven't changed.
pub fn apply(cx: &mut App) {
    let overrides = cx.global::<Preferences>().shortcuts.clone();
    if cx
        .try_global::<Applied>()
        .is_some_and(|applied| applied.0 == overrides)
    {
        return;
    }
    cx.clear_key_bindings();
    text_field::bind(cx);
    browser::bind(cx);
    settings::bind(cx);
    palette::bind(cx);
    goto::bind(cx);
    connect::bind(cx);
    jobs::bind(cx);
    empty_trash::bind(cx);
    menu::bind(cx);
    let mut bindings = Vec::new();
    for command in all() {
        let predicate = command
            .context
            .and_then(|context| KeyBindingContextPredicate::parse(context).ok())
            .map(Rc::new);
        for keys in command.shortcuts(&overrides) {
            if let Ok(binding) = KeyBinding::load(
                &keys,
                (command.action)(),
                predicate.clone(),
                false,
                None,
                &gpui::DummyKeyboardMapper,
            ) {
                bindings.push(binding);
            }
        }
    }
    cx.bind_keys(bindings);
    cx.set_global(Applied(overrides));
    install_menus(cx);
}

/// Set once the window has drawn: setting the main menu makes AppKit load
/// Writing Tools on the main thread (about 70 ms), so launch doesn't wait.
struct MenusReady {
    /// The commands checked in the menus as last installed.
    checked: Option<Vec<&'static str>>,
}
impl Global for MenusReady {}

pub fn menus_ready(cx: &mut App) {
    cx.set_global(MenusReady { checked: None });
    install_menus(cx);
}

/// The commands that show a checkmark: Show Hidden Files when on, and the
/// sort in use.
pub(crate) fn checked(prefs: &Preferences) -> Vec<&'static str> {
    let sort = match prefs.sort.by {
        SortBy::Name => SortByName.name(),
        SortBy::Modified => SortByModified.name(),
        SortBy::Size => SortBySize.name(),
        SortBy::Kind => SortByKind.name(),
    };
    let view = match prefs.view {
        crate::preferences::View::List => ViewAsList.name(),
        crate::preferences::View::Grid => ViewAsGrid.name(),
    };
    let mut ids = vec![sort, view];
    if prefs.show_hidden {
        ids.push(ToggleHiddenFiles.name());
    }
    if prefs.show_path_bar {
        ids.push(TogglePathBar.name());
    }
    ids
}

/// Rebuilds the menu bar when a checkmark is out of date.
pub fn refresh_menus(cx: &mut App) {
    let checked = checked(cx.global::<Preferences>());
    if cx
        .try_global::<MenusReady>()
        .is_some_and(|ready| ready.checked.as_ref() != Some(&checked))
    {
        install_menus(cx);
    }
}

fn install_menus(cx: &mut App) {
    if !cx.has_global::<MenusReady>() {
        return;
    }
    let ticked = checked(cx.global::<Preferences>());
    let commands = all();
    let items = |group: Group| {
        let mut items = Vec::new();
        for command in commands.iter().filter(|c| c.group == group) {
            if command.separated && !items.is_empty() {
                items.push(MenuItem::separator());
            }
            items.push(MenuItem::Action {
                name: command.title.into(),
                action: (command.action)(),
                os_action: command.os_action,
                checked: ticked.contains(&command.id()),
                disabled: false,
            });
        }
        items
    };
    let menu = |group: Group, items: Vec<MenuItem>| Menu {
        name: group.label().into(),
        items,
        disabled: false,
    };
    cx.set_menus(vec![
        menu(Group::App, items(Group::App)),
        menu(Group::File, items(Group::File)),
        menu(Group::Edit, items(Group::Edit)),
        menu(Group::Go, items(Group::Go)),
        menu(Group::View, items(Group::View)),
        menu(Group::Window, items(Group::Window)),
    ]);
    cx.global_mut::<MenusReady>().checked = Some(ticked);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_trashes_or_erases_files_is_destructive() {
        let marked: Vec<_> = all()
            .iter()
            .filter(|command| command.destructive)
            .map(|command| command.title)
            .collect();
        assert_eq!(marked, ["Empty Trash…", "Move to Trash"]);
        // The context menu's rows carry it.
        let rows = context_menu(&[Scope::Selection], &Preferences::default(), |_| true);
        let marked: Vec<_> = rows
            .iter()
            .filter(|row| row.destructive)
            .map(|row| row.title)
            .collect();
        assert_eq!(marked, ["Move to Trash"]);
    }

    #[test]
    fn what_acts_on_the_selection_or_folder_is_told_from_display_settings() {
        for acts in [
            MoveToTrash.name(),
            Duplicate.name(),
            NewFolder.name(),
            MoveHere.name(),
        ] {
            assert!(acts_on_target(acts), "{acts}");
        }
        for not in [
            ViewAsGrid.name(),
            SortByName.name(),
            GoHome.name(),
            Undo.name(),
        ] {
            assert!(!acts_on_target(not), "{not}");
        }
    }

    #[test]
    fn overrides_replace_turn_off_or_fall_back_to_the_defaults() {
        let commands = all();
        let zoom = commands.iter().find(|c| c.title == "Zoom In").unwrap();
        let id = zoom.id().to_string();
        let with = |value: Option<&str>| Shortcuts::from([(id.clone(), value.map(str::to_owned))]);
        assert_eq!(zoom.shortcuts(&Shortcuts::new()), ["cmd-=", "cmd-+"]);
        assert_eq!(zoom.shortcuts(&with(Some("ctrl-z"))), ["ctrl-z"]);
        assert!(zoom.shortcuts(&with(None)).is_empty());
        // A stored shortcut that isn't one keeps the defaults rather than nothing.
        assert_eq!(zoom.shortcuts(&with(Some("  "))), ["cmd-=", "cmd-+"]);
    }

    #[test]
    fn every_command_has_its_own_id_and_parsable_defaults() {
        let commands = all();
        let mut ids: Vec<_> = commands.iter().map(Command::id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), commands.len());
        for command in &commands {
            for keys in command.keys {
                assert!(valid(keys), "{}: {keys}", command.title);
                // macOS reports ⇧ with punctuation as the shifted symbol and
                // no ⇧ (⇧⌘. arrives as ⌘>), so a binding written with
                // shift and punctuation never matches.
                let stroke = Keystroke::parse(keys).unwrap();
                let letter = stroke.key.chars().all(|c| c.is_ascii_alphabetic());
                assert!(
                    !stroke.modifiers.shift || letter,
                    "{}: {keys}",
                    command.title
                );
            }
        }
    }

    #[test]
    fn quick_look_is_in_the_file_menu_on_command_y_and_space() {
        let commands = all();
        let file: Vec<_> = commands
            .iter()
            .filter(|c| c.group == Group::File)
            .map(|c| c.title)
            .collect();
        let at = file
            .iter()
            .position(|title| *title == "Quick Look")
            .unwrap();
        assert_eq!(file[at - 1], "Duplicate");
        // The menu bar shows the first key and AppKit matches it before any
        // view: a bare Space there would stop the search field typing one.
        let quick_look = &commands[commands
            .iter()
            .position(|c| c.title == "Quick Look")
            .unwrap()];
        assert_eq!(quick_look.keys, ["cmd-y", "space"]);
        assert_eq!(quick_look.context, Some("FileList"));
    }

    #[test]
    fn labels_read_as_macos_shows_them() {
        assert_eq!(label("cmd-shift-p"), "⇧⌘P");
        assert_eq!(label("ctrl-alt-shift-cmd-k"), "⌃⌥⇧⌘K");
        assert_eq!(label("cmd--"), "⌘-");
        assert_eq!(label("cmd-+"), "⌘+");
        assert_eq!(label("cmd-["), "⌘[");
        assert_eq!(label("cmd-up"), "⌘↑");
        assert_eq!(label("enter"), "↩");
        assert_eq!(label("f5"), "F5");
    }

    #[test]
    fn a_context_menu_offers_what_applies_and_runs_in_the_menu_bars_sections() {
        let shown = |scopes: &[Scope], prefs: &Preferences, runs: &dyn Fn(&dyn Action) -> bool| {
            context_menu(scopes, prefs, runs)
                .iter()
                .map(|row| {
                    let check = if row.checked { "✓ " } else { "" };
                    let line = if row.separated { "| " } else { "" };
                    let keys = row
                        .keys
                        .as_deref()
                        .map_or(String::new(), |k| format!(" {k}"));
                    format!("{line}{check}{}{keys}", row.title)
                })
                .collect::<Vec<_>>()
        };
        let prefs = Preferences::default();
        let all = |_: &dyn Action| true;
        assert_eq!(
            shown(&[Scope::Selection, Scope::Package], &prefs, &all),
            [
                "Open ⌘↓",
                "Open in New Tab ⌃⌘O",
                "Show Package Contents",
                "Reveal in Finder",
                "Open in Terminal",
                "| Add to Sidebar ⌃⌘T",
                "| Rename ↩",
                "Duplicate ⌘D",
                "Quick Look ⌘Y",
                "| Copy to Other Pane ⌥F5",
                "Move to Other Pane ⌥F6",
                "| Move to Trash ⌘⌫",
                "Put Back",
                "| Copy ⌘C",
                "| Copy Path ⌥⌘C",
            ]
        );
        assert_eq!(
            shown(&[Scope::Folder], &prefs, &all),
            [
                "New Folder ⇧⌘N",
                "New File",
                "| Paste ⌘V",
                "| ✓ as List ⌘1",
                "as Grid ⌘2",
                "| Show Hidden Files ⌘>",
                "| ✓ Sort by Name ⌃⌥⌘1",
                "Sort by Date Modified ⌃⌥⌘2",
                "Sort by Size ⌃⌥⌘3",
                "Sort by Kind ⌃⌥⌘4",
            ]
        );
        assert_eq!(
            shown(&[Scope::Sidebar], &prefs, &all),
            [
                "Open ⌘↓",
                "Open in New Tab ⌃⌘O",
                "| Remove from Sidebar ⌘⌫",
                "Eject ⌘E",
                "Clear Recents"
            ]
        );
        // New Folder with Selection is for several, after the ways to open.
        let several = shown(&[Scope::Selection, Scope::Several], &prefs, &all);
        assert_eq!(
            several[5..7],
            ["| New Folder with Selection ⌃⌘N", "Rename ↩"]
        );
        // What wouldn't run now is left out, and so is a separator with
        // nothing after it; shortcuts are the ones bound now.
        let mut rebound = Preferences::default();
        rebound
            .shortcuts
            .insert(Duplicate.name().into(), Some("ctrl-d".into()));
        rebound.shortcuts.insert(Rename.name().into(), None);
        let some = |action: &dyn Action| {
            [Duplicate.name(), Rename.name(), CopyPath.name()].contains(&action.name())
        };
        assert_eq!(
            shown(&[Scope::Selection], &rebound, &some),
            ["Rename", "Duplicate ⌃D", "| Copy Path ⌥⌘C"]
        );
    }

    #[test]
    fn keys_compete_where_one_context_sits_in_another() {
        assert!(overlap(None, Some("FileList")));
        assert!(overlap(Some("Browser"), Some("TextField")));
        assert!(overlap(Some("TextField"), Some("Palette")));
        assert!(!overlap(Some("FileList"), Some("Sidebar")));
        assert!(!overlap(Some("Settings"), Some("FileList")));
    }
}
