//! Search in the window, driven through the real `Browser`. `Fake` answers in
//! place of the index, which nothing here starts, and `Store` is a temp
//! folder's.

use super::drag::carry;
use super::places::{Fake, World, world};
use super::*;
use crate::browser::pane::searches::{CONTENT_SETTLE, REMEMBER_AFTER};
use crate::browser::*;
use crate::preferences::View;
use gpui::{TestAppContext, VisualTestContext};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::index::{Found, Hit, LineMatch};
use monodrive_core::places::{RecentSearch, SavedSearch};
use monodrive_core::search::{Looks, Scope as Where, Search};
use std::path::Path;

fn hit(name: &str, lines: &[(usize, &str)]) -> Hit {
    Hit {
        entry: Entry {
            path: Path::new("/made/up").join(name),
            name: name.into(),
            kind: Kind::File,
            size: 10,
            modified: 0,
            availability: Default::default(),
        },
        lines: lines
            .iter()
            .map(|(line, text)| LineMatch {
                line: *line,
                text: (*text).into(),
                matches: std::iter::once(0..text.len()).collect(),
            })
            .collect(),
    }
}

fn answer(fake: &Fake, text: &str, hits: Vec<Hit>) {
    let found = Found {
        hits,
        complete: true,
    };
    fake.found.lock().unwrap().insert(text.into(), found);
}

fn contents(text: &str) -> Search {
    Search {
        text: text.into(),
        looks: Looks::Contents,
        scope: Where::Everywhere,
    }
}

fn run(browser: &Entity<Browser>, search: Search, cx: &mut VisualTestContext) {
    let pane = pane_of(browser, cx);
    cx.update(|window, cx| pane.update(cx, |pane, cx| pane.run_search(search, window, cx)));
}

/// The search asked for has had time to start and land.
fn land(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(CONTENT_SETTLE);
    cx.run_until_parked();
}

fn searched(world: &World) -> Vec<Search> {
    world.fake.searched.lock().unwrap().clone()
}

fn menu_open(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    browser.read_with(cx, |browser, _| browser.searches_open())
}

fn typed(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> String {
    in_pane(browser, cx, |pane, cx| {
        pane.search.read(cx).text().to_string()
    })
}

/// Protects the row model: a file's lines are rows under it, in the grid too,
/// while the keys, the selection and leaving go by files.
#[gpui::test]
fn matching_lines_are_rows_under_each_file_and_the_keys_go_by_files(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    answer(
        &world.fake,
        "todo",
        vec![
            hit("a.rs", &[(3, "todo one"), (9, "todo two")]),
            hit("b.rs", &[(1, "todo three")]),
        ],
    );
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("plain.txt"), "").unwrap();
    let (browser, cx) = browse(&folder, cx);
    on_pane(&browser, cx, |pane, _| pane.view = View::Grid);

    run(&browser, contents("todo"), cx);
    land(cx);
    let names = |list: &[&str]| list.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    assert_eq!(
        shown(&browser, cx),
        (names(&["a.rs", "b.rs"]), names(&["a.rs"]))
    );
    in_pane(&browser, cx, |pane, _| {
        assert_eq!(pane.matches.row_count(pane.entries.len()), 5);
        assert_eq!(pane.layout(), View::List);
    });

    // ↓ goes to the next file, not to a's lines.
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, names(&["b.rs"]));
    press(cx, "up");
    assert_eq!(shown(&browser, cx).1, names(&["a.rs"]));

    // Esc is the way out: the folder, its grid, and names again.
    press(cx, "escape");
    assert_eq!(shown(&browser, cx).0, names(&["plain.txt"]));
    assert_eq!(typed(&browser, cx), "");
    in_pane(&browser, cx, |pane, _| {
        assert!(pane.matches.is_empty());
        assert_eq!(pane.layout(), View::Grid);
        assert_eq!(pane.looks, Looks::Names);
    });
}

/// Protects results from going stale: a change in a folder holding a result
/// runs the search again and keeps the selection, and a change elsewhere runs
/// nothing.
#[gpui::test]
fn a_change_beside_a_result_runs_the_search_again(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    let hits = |names: &[&str]| names.iter().map(|name| hit(name, &[])).collect();
    answer(&world.fake, "todo", hits(&["a.rs", "b.rs", "c.rs"]));
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    let (browser, cx) = browse(&folder, cx);
    let names = Search {
        text: "todo".into(),
        looks: Looks::Names,
        scope: Where::Everywhere,
    };
    run(&browser, names, cx);
    land(cx);
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, ["b.rs"]);

    let changed = |folder: &str, cx: &mut VisualTestContext| {
        let batch = vec![(PathBuf::from(folder), false)];
        browser.update(cx, |b, cx| b.disk_changed(&batch, cx));
        cx.executor().advance_clock(pane::SEARCH_REFRESH_DELAY);
        cx.run_until_parked();
    };
    changed("/elsewhere", cx);
    assert_eq!(searched(&world).len(), 1);

    answer(&world.fake, "todo", hits(&["b.rs", "c.rs"]));
    changed("/made/up", cx);
    assert_eq!(searched(&world).len(), 2);
    let (rows, selected) = shown(&browser, cx);
    assert_eq!(rows, ["b.rs", "c.rs"]);
    assert_eq!(selected, ["b.rs"]);
}

/// Protects stale handling: a search replaced before it answers never lands,
/// by another search or by leaving for a folder.
#[gpui::test]
fn a_search_replaced_before_it_answers_never_lands(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    answer(&world.fake, "old", vec![hit("old.rs", &[(1, "old")])]);
    answer(&world.fake, "new", vec![hit("new.rs", &[(1, "new")])]);
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("plain.txt"), "").unwrap();
    let (browser, cx) = browse(&folder, cx);

    run(&browser, contents("old"), cx);
    cx.run_until_parked();
    run(&browser, contents("new"), cx);
    land(cx);
    assert_eq!(shown(&browser, cx).0, ["new.rs"]);
    assert_eq!(searched(&world), [contents("new")]);

    run(&browser, contents("old"), cx);
    cx.run_until_parked();
    on_pane(&browser, cx, |pane, cx| pane.show_folder(cx));
    land(cx);
    assert_eq!(shown(&browser, cx).0, ["plain.txt"]);
    assert!(in_pane(&browser, cx, |pane, _| pane.matches.is_empty()));
    assert_eq!(searched(&world), [contents("new")]);
}

/// Protects the modes' ways in and out: each is a command, what's typed runs
/// again, and leaving goes back to a plain search.
#[gpui::test]
fn contents_and_this_folder_turn_on_and_off_and_run_what_is_typed(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    answer(&world.fake, "todo", vec![hit("a.rs", &[(1, "todo")])]);
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    let (browser, cx) = browse(&folder, cx);

    // With nothing typed, ⌥⌘F searches nothing, and Esc undoes it first.
    press(cx, "cmd-alt-f");
    assert_eq!(in_pane(&browser, cx, |p, _| p.looks), Looks::Contents);
    cx.executor().advance_clock(CONTENT_SETTLE);
    cx.run_until_parked();
    assert!(searched(&world).is_empty());
    press(cx, "escape");
    assert_eq!(in_pane(&browser, cx, |p, _| p.looks), Looks::Names);

    run(&browser, contents("todo"), cx);
    land(cx);
    cx.dispatch_action(SearchThisFolder);
    land(cx);
    let scoped = Search {
        scope: Where::Folder(folder.clone()),
        ..contents("todo")
    };
    assert_eq!(searched(&world).last(), Some(&scoped));
    assert!(in_pane(&browser, cx, |p, cx| p.title(cx)).ends_with("in here"));

    press(cx, "cmd-alt-f");
    land(cx);
    let names = Search {
        looks: Looks::Names,
        ..scoped.clone()
    };
    assert_eq!(searched(&world).last(), Some(&names));
    cx.dispatch_action(SearchThisFolder);
    land(cx);
    assert_eq!(
        in_pane(&browser, cx, |p, _| p.search_scope.clone()),
        Where::Everywhere
    );

    // Going elsewhere leaves the search and its mode.
    press(cx, "cmd-alt-f");
    land(cx);
    on_pane(&browser, cx, |pane, cx| pane.navigate(folder.clone(), cx));
    land(cx);
    assert_eq!(typed(&browser, cx), "");
    assert!(in_pane(&browser, cx, |p, _| p.search_mode_is_plain()));
}

/// Protects the menu against the store: what's run becomes recent once it has
/// stayed, and saving, running, renaming, removing and clearing all reach the
/// lists through the keys.
#[gpui::test]
fn the_menu_saves_runs_renames_removes_and_clears_searches(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    answer(&world.fake, "todo", vec![hit("a.rs", &[(1, "todo")])]);
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    let (browser, cx) = browse(&folder, cx);

    // A step on the way to typing something isn't a recent search.
    run(&browser, contents("todo"), cx);
    land(cx);
    assert_eq!(world.store.recent_searches(), Ok(Vec::new()));
    cx.executor().advance_clock(REMEMBER_AFTER);
    cx.run_until_parked();
    let recent = RecentSearch {
        query: "contains:todo".into(),
        scope: Where::Everywhere,
    };
    assert_eq!(world.store.recent_searches(), Ok(vec![recent.clone()]));

    // Names, Contents, This Folder, the three filters, then Save Search…
    press(cx, "cmd-shift-s");
    assert!(menu_open(&browser, cx));
    press(cx, "down down down down down down down enter");
    cx.simulate_input("Todos");
    press(cx, "enter");
    let saved = SavedSearch {
        name: "Todos".into(),
        query: "contains:todo".into(),
        scope: Where::Everywhere,
    };
    assert_eq!(world.store.searches(), Ok(vec![saved.clone()]));
    press(cx, "escape");
    assert!(!menu_open(&browser, cx));

    // Out of the search, the saved one brings it back as it was.
    press(cx, "escape");
    assert_eq!(typed(&browser, cx), "");
    press(cx, "cmd-shift-s down down down down down down down enter");
    land(cx);
    assert!(!menu_open(&browser, cx));
    assert_eq!(typed(&browser, cx), "todo");
    assert_eq!(in_pane(&browser, cx, |p, _| p.looks), Looks::Contents);
    assert_eq!(shown(&browser, cx).0, ["a.rs"]);

    // R renames, and Esc in the name leaves the menu open and the name as it was.
    press(cx, "cmd-shift-s down down down down down down down r");
    press(cx, "escape");
    assert!(menu_open(&browser, cx));
    assert_eq!(world.store.searches(), Ok(vec![saved.clone()]));
    press(cx, "r");
    cx.simulate_input("Later");
    press(cx, "enter");
    let renamed = SavedSearch {
        name: "Later".into(),
        ..saved
    };
    assert_eq!(world.store.searches(), Ok(vec![renamed]));

    // ⌫ removes it, with nothing asked: it's a bookmark of some words.
    press(cx, "backspace");
    assert_eq!(world.store.searches(), Ok(Vec::new()));

    // The Clear row is the last, so ↑ from no row lands on it.
    press(cx, "escape cmd-shift-s up enter");
    assert_eq!(world.store.recent_searches(), Ok(Vec::new()));
    assert!(menu_open(&browser, cx));
    press(cx, "escape");
    assert!(!menu_open(&browser, cx));
}

fn offered(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Vec<String> {
    in_pane(browser, cx, |pane, _| {
        pane.completing
            .as_ref()
            .map_or_else(Vec::new, |completing| {
                completing
                    .list
                    .items
                    .iter()
                    .map(|item| item.text.clone())
                    .collect()
            })
    })
}

/// The tokens as their text, and the search they and the words make.
fn tokens(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> (Vec<String>, String) {
    in_pane(browser, cx, |pane, _| {
        let tokens = pane.filters.terms.iter().map(ToString::to_string).collect();
        (tokens, pane.query.clone())
    })
}

fn on_chip(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Option<usize> {
    in_pane(browser, cx, |pane, _| pane.chip)
}

/// Protects filters as tokens from the keys: values show under the field
/// while the caret ends a key, taking one makes a token while the words stay
/// typed, ← or ⌫ at the start of the words steps onto the tokens, where ↩
/// opens a token's menu and ⌫ removes it, Edit as Text gives it back as text,
/// and Esc leaves the search with its tokens.
#[gpui::test]
fn finished_filters_become_tokens_the_keys_can_change(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), "").unwrap();
    let (browser, cx) = browse(&folder, cx);
    let names = |list: &[&str]| list.iter().map(|n| n.to_string()).collect::<Vec<_>>();

    press(cx, "cmd-f");
    cx.simulate_input("report kind:");
    cx.run_until_parked();
    assert_eq!(offered(&browser, cx)[..2], ["kind:image", "kind:video"]);
    assert!(cx.debug_bounds("search-completions").is_some());
    // They're for the caret: moved off the filter, the list goes and the
    // keys are the field's again.
    press(cx, "left");
    assert!(cx.debug_bounds("search-completions").is_none());
    press(cx, "right");
    assert!(cx.debug_bounds("search-completions").is_some());
    press(cx, "down tab");
    assert_eq!(typed(&browser, cx), "report ");
    assert_eq!(
        tokens(&browser, cx),
        (names(&["kind:video"]), "kind:video report".into())
    );
    assert!(cx.debug_bounds("search-chip-0").is_some());

    // The menu's Kind types the key, and the kind taken widens the token.
    press(cx, "cmd-shift-s down down down down enter");
    assert!(!menu_open(&browser, cx));
    assert_eq!(typed(&browser, cx), "report kind:");
    press(cx, "enter");
    assert_eq!(typed(&browser, cx), "report ");
    assert_eq!(tokens(&browser, cx).0, names(&["kind:video,image"]));

    // ⌫ at the start of the words steps onto the token without deleting.
    press(cx, "cmd-left backspace");
    assert_eq!(on_chip(&browser, cx), Some(0));
    assert_eq!(typed(&browser, cx), "report ");
    // ↩ opens its menu on the first kind it has; ↩ there turns one off.
    press(cx, "enter");
    assert!(cx.debug_bounds("search-chip-menu").is_some());
    press(cx, "down enter");
    assert_eq!(tokens(&browser, cx).0, names(&["kind:image"]));
    press(cx, "escape");
    assert!(cx.debug_bounds("search-chip-menu").is_none());
    assert_eq!(on_chip(&browser, cx), Some(0));
    press(cx, "backspace");
    assert_eq!(tokens(&browser, cx), (Vec::new(), "report".into()));
    assert_eq!(on_chip(&browser, cx), None);

    // Typed at the start, a finished filter lifts out there too.
    cx.simulate_input("size:>1gb ");
    cx.run_until_parked();
    assert_eq!(typed(&browser, cx), "report ");
    assert_eq!(tokens(&browser, cx).0, names(&["size:>1gb"]));
    // Edit as Text, two rows past the size it has, gives the text back.
    press(cx, "backspace enter down down enter");
    assert_eq!(typed(&browser, cx), "size:>1gb report ");
    assert_eq!(
        tokens(&browser, cx),
        (Vec::new(), "size:>1gb report".into())
    );

    press(cx, "escape");
    assert_eq!(typed(&browser, cx), "");
    assert_eq!(tokens(&browser, cx), (Vec::new(), String::new()));

    // A kept search comes back as tokens and words, and keeps the same text.
    let kept = Search {
        text: "notes kind:doc".into(),
        looks: Looks::Names,
        scope: Where::Everywhere,
    };
    run(&browser, kept, cx);
    assert_eq!(typed(&browser, cx), "notes");
    assert_eq!(
        tokens(&browser, cx),
        (names(&["kind:document"]), "kind:document notes".into())
    );
    let stored = in_pane(&browser, cx, |pane, _| pane.current_search().stored());
    assert_eq!(stored, Ok("kind:document notes".into()));

    // Leaving the search from a token, as Back does, gives the keys back to
    // the window: ↓ steps through the folder rather than opening a token.
    press(cx, "cmd-left backspace");
    assert_eq!(on_chip(&browser, cx), Some(0));
    on_pane(&browser, cx, |pane, cx| pane.show_folder(cx));
    cx.run_until_parked();
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, names(&["a.txt"]));
}

/// Protects a line standing for its file: pressing a line under a file that
/// isn't selected drags that file, where a rectangle would otherwise start.
#[gpui::test]
fn dragging_a_line_drags_its_file_though_it_is_not_selected(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    answer(
        &world.fake,
        "todo",
        vec![
            hit("a.rs", &[(3, "todo one"), (9, "todo two")]),
            hit("b.rs", &[(1, "todo three")]),
        ],
    );
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    let (browser, cx) = browse(&folder, cx);
    run(&browser, contents("todo"), cx);
    land(cx);
    assert_eq!(shown(&browser, cx).1, ["a.rs"]);

    // a.rs, its two lines, b.rs, then b.rs's line.
    let (first, line) = (row(&browser, 0, cx), row(&browser, 4, cx));
    carry(line, first, "", cx);
    assert!(browser.read_with(cx, |b, _| b.drag.active()));
    assert_eq!(shown(&browser, cx).1, ["b.rs"]);
    press(cx, "escape");
    cx.simulate_mouse_up(first, MouseButton::Left, with(""));

    // ⌘ adds it, as on a file's own row.
    press(cx, "up");
    assert_eq!(shown(&browser, cx).1, ["a.rs"]);
    carry(line, first, "cmd", cx);
    assert!(browser.read_with(cx, |b, _| b.drag.active()));
    assert_eq!(shown(&browser, cx).1, ["a.rs", "b.rs"]);
    press(cx, "escape");
    cx.simulate_mouse_up(first, MouseButton::Left, with("cmd"));
}

/// Protects acting on what's seen: the keys bring the row they land on into
/// view, so Enter, R and ⌫ never act on a search out of sight.
#[gpui::test]
fn the_keys_scroll_the_menu_to_the_row_they_land_on(cx: &mut TestAppContext) {
    let world = world(Vec::new(), cx);
    for n in 0..40 {
        let saved = SavedSearch {
            name: format!("Saved {n}"),
            query: format!("q{n}"),
            scope: Where::Everywhere,
        };
        world.store.save_search(saved, None).unwrap();
    }
    let folder = world.dir.path().join("here");
    std::fs::create_dir(&folder).unwrap();
    let (browser, cx) = browse(&folder, cx);
    press(cx, "cmd-shift-s");

    let in_view = |selector: &'static str, cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let row = cx.debug_bounds(selector).unwrap();
        let shown = browser.read_with(cx, |b, cx| match &b.sheet {
            Some(Sheet::Searches(view)) => view.read(cx).scroll.bounds(),
            _ => panic!("the menu is open"),
        });
        row.top() >= shown.top() && row.bottom() <= shown.bottom()
    };
    // Names, Contents, This Folder and the filters, then down to the last
    // saved search.
    assert!(!in_view("saved-search-39", cx));
    for _ in 0..46 {
        press(cx, "down");
    }
    assert!(in_view("saved-search-39", cx));
    // Round to the top, then round to the bottom.
    press(cx, "down");
    assert!(in_view("search-names-0", cx));
    press(cx, "up");
    assert!(in_view("saved-search-39", cx));
    // What's in view is what ⌫ removes.
    press(cx, "backspace");
    let left = world.store.searches().unwrap();
    assert_eq!(left.len(), 39);
    assert!(left.iter().all(|saved| saved.name != "Saved 39"));
    press(cx, "escape");
}
