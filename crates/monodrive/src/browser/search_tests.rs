//! Search in the window, driven through the real `Browser`. `Fake` answers in
//! place of the index, which nothing here starts, and `Store` is a temp
//! folder's.

use super::places_tests::{Fake, World, world};
use super::searches::{CONTENT_SETTLE, REMEMBER_AFTER};
use super::tests::*;
use super::*;
use crate::preferences::View;
use gpui::{TestAppContext, VisualTestContext};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::index::{Found, Hit, LineMatch};
use monodrive_core::places::{RecentSearch, SavedSearch};
use monodrive_core::search::{Looks, Scope as Where, Search};

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

    // Names, Contents, This Folder, then Save Search…
    press(cx, "cmd-shift-s");
    assert!(menu_open(&browser, cx));
    press(cx, "down down down down enter");
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
    press(cx, "cmd-shift-s down down down down enter");
    land(cx);
    assert!(!menu_open(&browser, cx));
    assert_eq!(typed(&browser, cx), "todo");
    assert_eq!(in_pane(&browser, cx, |p, _| p.looks), Looks::Contents);
    assert_eq!(shown(&browser, cx).0, ["a.rs"]);

    // R renames, and Esc in the name leaves the menu open and the name as it was.
    press(cx, "cmd-shift-s down down down down r");
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
