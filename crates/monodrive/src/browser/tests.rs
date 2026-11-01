use super::pane::*;
use super::*;
use crate::pasteboard::Pasteboard;
use crate::preferences::View;
use crate::thumbnails::{Thumb, Thumbnail};
use gpui::{Keystroke, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, point};
use monodrive_core::entries::{Entry, Kind, Sort, SortBy};
use monodrive_core::ops::Origin;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A browser on `folder` with the app's keys and default preferences,
/// active and focused on its list. Type navigation keys only: a printable key
/// starts a search, and a search starts the whole-disk index.
pub(super) fn browse<'a>(
    folder: &Path,
    cx: &'a mut TestAppContext,
) -> (Entity<Browser>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(Preferences::default());
        commands::apply(cx);
    });
    let folder = folder.to_path_buf();
    let (browser, cx) = cx.add_window_view(|_, cx| Browser::new(folder, cx));
    // Active, as `run` makes it: an inactive window reports no blur.
    cx.update(|window, cx| {
        window.activate_window();
        let list = browser.read(cx).focus_handle(cx);
        window.focus(&list, cx);
    });
    cx.run_until_parked();
    (browser, cx)
}

/// The pane commands act on.
pub(super) fn pane_of(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Entity<Pane> {
    browser.read_with(cx, |browser, _| browser.pane().clone())
}

/// Reads the pane commands act on.
pub(super) fn in_pane<R>(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
    read: impl FnOnce(&Pane, &App) -> R,
) -> R {
    let pane = pane_of(browser, cx);
    pane.read_with(cx, |pane, cx| read(pane, cx))
}

/// Changes the pane commands act on.
pub(super) fn on_pane<R>(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
    change: impl FnOnce(&mut Pane, &mut Context<Pane>) -> R,
) -> R {
    let pane = pane_of(browser, cx);
    pane.update(cx, change)
}

/// The rows shown, folders inside open folders indented, and the
/// selected row.
pub(super) fn shown(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
) -> (Vec<String>, Vec<String>) {
    in_pane(browser, cx, |browser, _| {
        let rows = browser
            .entries
            .iter()
            .zip(&browser.depths)
            .map(|(entry, depth)| format!("{}{}", "  ".repeat(*depth), entry.name))
            .collect();
        let selected = browser
            .selection
            .in_order(&browser.entries)
            .into_iter()
            .map(|entry| entry.name.clone())
            .collect();
        (rows, selected)
    })
}

pub(super) fn press(cx: &mut VisualTestContext, keys: &str) {
    for key in keys.split(' ') {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
    }
}

/// `project/` holding `docs/` (with `inner.md`), `zeta/`, `a.txt` and
/// the hidden `.env`. It sits in a temp folder, since tempfile's own
/// folders are hidden and wouldn't list in their parent.
pub(super) fn folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(project.join("docs/inner.md"), "").unwrap();
    std::fs::create_dir(project.join("zeta")).unwrap();
    std::fs::write(project.join("a.txt"), "").unwrap();
    std::fs::write(project.join(".env"), "").unwrap();
    (dir, project)
}

#[gpui::test]
fn open_goes_into_a_folder_and_back_returns_with_it_selected(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    assert_eq!(
        shown(&browser, cx),
        (vec!["docs".into(), "zeta".into(), "a.txt".into()], vec![])
    );
    press(cx, "down cmd-down");
    assert_eq!(shown(&browser, cx).0, ["inner.md"]);
    press(cx, "cmd-[");
    assert_eq!(
        shown(&browser, cx),
        (
            vec!["docs".into(), "zeta".into(), "a.txt".into()],
            vec!["docs".into()]
        )
    );
}

/// The names Quick Look was asked to show, and the one in front.
fn previewed(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
) -> Option<(Vec<String>, usize)> {
    browser.read_with(cx, |browser, _| {
        let preview = browser.preview.as_ref()?;
        let names = preview
            .items
            .iter()
            .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        Some((names, preview.index))
    })
}

#[gpui::test]
fn space_previews_the_selection_and_quick_look_follows_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let one = |name: &str| Some((vec![name.to_string()], 0));
    press(cx, "space");
    assert_eq!(previewed(&browser, cx), None);
    press(cx, "down space");
    assert_eq!(previewed(&browser, cx), one("docs"));
    // The arrows still move the selection, and the panel follows; it
    // pages through several from the cursor.
    press(cx, "down");
    assert_eq!(previewed(&browser, cx), one("zeta"));
    press(cx, "shift-down");
    assert_eq!(
        previewed(&browser, cx),
        Some((vec!["zeta".into(), "a.txt".into()], 1))
    );
    // Esc closes it and keeps the selection; ⌘Y opens and closes it too.
    press(cx, "escape");
    assert_eq!(previewed(&browser, cx), None);
    assert_eq!(shown(&browser, cx).1, ["zeta", "a.txt"]);
    press(cx, "cmd-y");
    assert!(previewed(&browser, cx).is_some());
    press(cx, "cmd-y");
    assert_eq!(previewed(&browser, cx), None);
    // Going into a folder selects nothing, so it closes; going back
    // selects the folder left, and it shows that.
    press(cx, "alt-up space cmd-down");
    assert_eq!(shown(&browser, cx), (vec!["inner.md".into()], vec![]));
    assert_eq!(previewed(&browser, cx), None);
    press(cx, "down space cmd-[");
    assert_eq!(previewed(&browser, cx), one("docs"));
    // Space types in the search field, and the panel stays as it was.
    // A lone space searches for nothing, so the index never starts.
    press(cx, "cmd-f space");
    let typed = in_pane(&browser, cx, |b, cx| b.search.read(cx).text().to_string());
    assert_eq!(typed, " ");
    assert_eq!(previewed(&browser, cx), one("docs"));
    press(cx, "escape escape");
    assert_eq!(previewed(&browser, cx), None);
    // It types in a name being edited too.
    press(cx, "alt-down enter space");
    assert_eq!(editing(&browser, cx), Some((" .txt".into(), None)));
    assert_eq!(previewed(&browser, cx), None);
}

#[gpui::test]
fn arrows_walk_a_folder_open_in_place_and_out_of_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "down right");
    assert_eq!(
        shown(&browser, cx),
        (
            vec![
                "docs".into(),
                "  inner.md".into(),
                "zeta".into(),
                "a.txt".into()
            ],
            vec!["docs".into()]
        )
    );
    press(cx, "right");
    assert_eq!(shown(&browser, cx).1, ["inner.md"]);
    press(cx, "left left");
    assert_eq!(
        shown(&browser, cx),
        (
            vec!["docs".into(), "zeta".into(), "a.txt".into()],
            vec!["docs".into()]
        )
    );
    // At the top, ← goes up to the enclosing folder with this one selected.
    press(cx, "left");
    assert_eq!(
        shown(&browser, cx),
        (vec!["project".into()], vec!["project".into()])
    );
}

/// `a.txt` to `d.txt`, each bigger than the last, so sorting by size,
/// biggest first, reverses them.
fn sized() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("sized");
    std::fs::create_dir(&folder).unwrap();
    for (name, size) in [("a.txt", 1), ("b.txt", 20), ("c.txt", 300), ("d.txt", 4000)] {
        std::fs::write(folder.join(name), vec![b'x'; size]).unwrap();
    }
    (dir, folder)
}

/// The middle of list row `ix` in the window, as the last frame laid out.
pub(super) fn row(
    browser: &Entity<Browser>,
    ix: usize,
    cx: &mut VisualTestContext,
) -> Point<Pixels> {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    in_pane(browser, cx, |browser, cx| {
        let height = cx.global::<Preferences>().row_size.metrics().0;
        browser.list_bounds.get().origin + gpui::point(px(60.), px(4. + height * (ix as f32 + 0.5)))
    })
}

/// List row `ix` where it holds neither icon nor name, where a press starts
/// a rectangle.
pub(super) fn blank(
    browser: &Entity<Browser>,
    ix: usize,
    cx: &mut VisualTestContext,
) -> Point<Pixels> {
    row(browser, ix, cx) + gpui::point(px(240.), px(0.))
}

pub(super) fn with(modifiers: &str) -> Modifiers {
    Modifiers {
        platform: modifiers.contains("cmd"),
        shift: modifiers.contains("shift"),
        alt: modifiers.contains("alt"),
        ..Modifiers::default()
    }
}

#[gpui::test]
fn clicks_build_a_selection_that_outlives_a_sort_and_a_refresh(cx: &mut TestAppContext) {
    let (_dir, folder) = sized();
    let (browser, cx) = browse(&folder, cx);
    let at = row(&browser, 0, cx);
    cx.simulate_click(at, with(""));
    let at = row(&browser, 2, cx);
    cx.simulate_click(at, with("shift"));
    assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt"]);
    let at = row(&browser, 1, cx);
    cx.simulate_click(at, with("cmd"));
    assert_eq!(shown(&browser, cx).1, ["a.txt", "c.txt"]);
    // Sorted by size, the same files stay selected in their new order.
    press(cx, "ctrl-alt-cmd-3");
    assert_eq!(
        shown(&browser, cx),
        (
            vec![
                "d.txt".into(),
                "c.txt".into(),
                "b.txt".into(),
                "a.txt".into()
            ],
            vec!["c.txt".into(), "a.txt".into()]
        )
    );
    // A refresh after the disk changes keeps it too.
    std::fs::write(folder.join("e.txt"), "").unwrap();
    on_pane(&browser, cx, |browser, cx| browser.list(true, cx));
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).0.len(), 5);
    assert_eq!(shown(&browser, cx).1, ["c.txt", "a.txt"]);
    assert_eq!(
        in_pane(&browser, cx, |browser, _| browser.selection.size()),
        301
    );
    press(cx, "escape");
    assert!(shown(&browser, cx).1.is_empty());
}

#[gpui::test]
fn shift_arrows_extend_and_select_all_and_deselect_all(cx: &mut TestAppContext) {
    let (_dir, folder) = sized();
    let (browser, cx) = browse(&folder, cx);
    press(cx, "down shift-down shift-down");
    assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt"]);
    press(cx, "shift-up");
    assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt"]);
    // A plain arrow goes back to one.
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, ["c.txt"]);
    press(cx, "cmd-a");
    assert_eq!(shown(&browser, cx).1.len(), 4);
    press(cx, "cmd-alt-a");
    assert!(shown(&browser, cx).1.is_empty());
    // The cursor can sit at the end on an entry that isn't selected;
    // ↓ there still selects it.
    let (first, last) = (row(&browser, 0, cx), row(&browser, 3, cx));
    cx.simulate_click(first, with(""));
    cx.simulate_click(last, with("cmd"));
    cx.simulate_click(last, with("cmd"));
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, ["d.txt"]);
}

#[gpui::test]
fn a_dragged_rectangle_selects_what_it_touches(cx: &mut TestAppContext) {
    let (_dir, folder) = sized();
    let (browser, cx) = browse(&folder, cx);
    let (from, to) = (blank(&browser, 0, cx), blank(&browser, 2, cx));
    cx.simulate_mouse_down(from, MouseButton::Left, with(""));
    cx.simulate_mouse_move(to, MouseButton::Left, with(""));
    cx.simulate_mouse_up(to, MouseButton::Left, with(""));
    assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt"]);
    // A click on empty space below the rows lets go of it.
    let below = row(&browser, 6, cx);
    cx.simulate_click(below, with(""));
    assert!(shown(&browser, cx).1.is_empty());
    // ⌘ adds a drag to what was selected.
    let last = row(&browser, 3, cx);
    cx.simulate_click(last, with(""));
    let (from, to) = (blank(&browser, 0, cx), blank(&browser, 1, cx));
    cx.simulate_mouse_down(from, MouseButton::Left, with("cmd"));
    cx.simulate_mouse_move(to, MouseButton::Left, with("cmd"));
    cx.simulate_mouse_up(to, MouseButton::Left, with("cmd"));
    assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "d.txt"]);
    // A drag that ends on the row it began on is a drag, not a click
    // that would toggle that row back out.
    let start = blank(&browser, 2, cx);
    let end = start + gpui::point(px(80.), px(0.));
    cx.simulate_mouse_down(start, MouseButton::Left, with("cmd"));
    cx.simulate_mouse_move(end, MouseButton::Left, with("cmd"));
    cx.simulate_mouse_up(end, MouseButton::Left, with("cmd"));
    assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt", "d.txt"]);
    // In the grid it takes the cells it covers, not whole rows.
    press(cx, "cmd-2");
    let cell = |ix: usize, cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        in_pane(&browser, cx, |browser, _| {
            let height = grid_cell(browser.grid_icon).1;
            assert!(browser.across >= 4, "the four files fit in one row");
            browser.list_bounds.get().origin
                + gpui::point(
                    px(8. + browser.cell_width * (ix as f32 + 0.5)),
                    px(4. + height * 0.5),
                )
        })
    };
    let (from, to) = (cell(1, cx), cell(2, cx));
    // Beside the icon, in the blank part of its cell.
    let margin = in_pane(&browser, cx, |browser, _| browser.cell_width / 2. - 2.);
    let from = from - gpui::point(px(margin), px(0.));
    cx.simulate_mouse_down(from, MouseButton::Left, with(""));
    cx.simulate_mouse_move(to, MouseButton::Left, with(""));
    cx.simulate_mouse_up(to, MouseButton::Left, with(""));
    assert_eq!(shown(&browser, cx).1, ["b.txt", "c.txt"]);
}

#[gpui::test]
fn closing_a_folder_keeps_what_was_selected_beside_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "down right right");
    let a = row(&browser, 3, cx);
    cx.simulate_click(a, with("cmd"));
    assert_eq!(shown(&browser, cx).1, ["inner.md", "a.txt"]);
    on_pane(&browser, cx, |browser, cx| {
        browser.toggle_folder(project.join("docs"), cx)
    });
    assert_eq!(shown(&browser, cx).1, ["docs", "a.txt"]);
}

#[gpui::test]
fn opening_many_files_asks_first(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let many = dir.path().join("many");
    std::fs::create_dir(&many).unwrap();
    for ix in 0..=OPEN_WITHOUT_ASKING {
        std::fs::write(many.join(format!("{ix:02}.txt")), "").unwrap();
    }
    let (_browser, cx) = browse(&many, cx);
    press(cx, "cmd-a cmd-o");
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
}

#[gpui::test]
fn a_drag_past_the_bottom_scrolls_and_keeps_selecting(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let tall = dir.path().join("tall");
    std::fs::create_dir(&tall).unwrap();
    for ix in 0..200 {
        std::fs::write(tall.join(format!("{ix:03}.txt")), "").unwrap();
    }
    let (browser, cx) = browse(&tall, cx);
    let start = blank(&browser, 0, cx);
    let below = in_pane(&browser, cx, |browser, _| {
        browser.list_bounds.get().bottom_left()
    }) + gpui::point(px(60.), px(80.));
    cx.simulate_mouse_down(start, MouseButton::Left, with(""));
    cx.simulate_mouse_move(below, MouseButton::Left, with(""));
    let first = shown(&browser, cx).1.len();
    // The test platform draws no frames on its own; deliver the ones
    // auto-scroll waits on.
    for _ in 0..20 {
        cx.update(|window, cx| window.simulate_next_frame(cx));
    }
    let scrolled = shown(&browser, cx).1.len();
    cx.simulate_mouse_up(below, MouseButton::Left, with(""));
    assert!(
        scrolled > first,
        "{first} rows, then {scrolled} after scrolling"
    );
    assert_eq!(shown(&browser, cx).1.len(), scrolled);
}

#[test]
fn a_span_counts_the_cells_it_overlaps() {
    assert_eq!(span(0., 10., 28., 5), 0..1);
    assert_eq!(span(27., 29., 28., 5), 0..2);
    assert_eq!(span(-50., 500., 28., 5), 0..5);
    // Past the last cell, or above the first, it touches nothing.
    assert_eq!(span(200., 300., 28., 5), 5..5);
    assert_eq!(span(-20., -5., 28., 5), 0..0);
}

#[gpui::test]
fn show_hidden_files_lists_dotfiles_and_hides_them_again(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd->");
    assert_eq!(shown(&browser, cx).0, ["docs", "zeta", ".env", "a.txt"]);
    press(cx, "cmd->");
    assert_eq!(shown(&browser, cx).0, ["docs", "zeta", "a.txt"]);
}

#[test]
fn arrows_step_and_stop_at_the_ends() {
    assert_eq!(moved(None, 5, Move::Next), Some(0));
    assert_eq!(moved(None, 5, Move::Previous), Some(4));
    assert_eq!(moved(Some(2), 5, Move::Next), Some(3));
    assert_eq!(moved(Some(4), 5, Move::Next), Some(4));
    assert_eq!(moved(Some(0), 5, Move::Previous), Some(0));
    assert_eq!(moved(Some(3), 5, Move::First), Some(0));
    assert_eq!(moved(Some(1), 5, Move::Last), Some(4));
    // A selection past a shorter listing comes back inside it.
    assert_eq!(moved(Some(9), 5, Move::Previous), Some(3));
    assert_eq!(moved(Some(9), 5, Move::Next), Some(4));
    assert_eq!(moved(None, 0, Move::Next), None);
    // The grid steps a row (here 3 across) or one sideways, and stops
    // at the ends rather than wrapping.
    assert_eq!(moved(Some(1), 8, Move::By(3)), Some(4));
    assert_eq!(moved(Some(6), 8, Move::By(3)), Some(7));
    assert_eq!(moved(Some(1), 8, Move::By(-3)), Some(0));
    assert_eq!(moved(None, 8, Move::By(-3)), Some(7));
}

fn entry(path: &str, kind: Kind) -> Entry {
    Entry {
        path: path.into(),
        name: path.rsplit('/').next().unwrap_or(path).into(),
        kind,
        size: 0,
        modified: 0,
        availability: Default::default(),
    }
}

#[test]
fn expanded_folders_list_their_contents_under_them() {
    let root = [entry("/a", Kind::Dir), entry("/b", Kind::File)];
    let children = HashMap::from([
        (
            "/a".into(),
            vec![entry("/a/c", Kind::Dir), entry("/a/d", Kind::File)],
        ),
        ("/a/c".into(), vec![entry("/a/c/e", Kind::File)]),
    ]);
    let rows = |expanded: &[&str]| {
        let expanded = expanded.iter().map(PathBuf::from).collect();
        flatten(&root, &children, &expanded)
            .into_iter()
            .map(|(e, depth)| (e.path.display().to_string(), depth))
            .collect::<Vec<_>>()
    };
    let at = |path: &str, depth| (path.to_string(), depth);
    assert_eq!(rows(&[]), [at("/a", 0), at("/b", 0)]);
    assert_eq!(
        rows(&["/a", "/a/c"]),
        [
            at("/a", 0),
            at("/a/c", 1),
            at("/a/c/e", 2),
            at("/a/d", 1),
            at("/b", 0)
        ]
    );
    // A folder inside a closed one stays closed with it.
    assert_eq!(rows(&["/a/c"]), [at("/a", 0), at("/b", 0)]);
}

#[test]
fn arrows_walk_the_tree_like_monocodes_explorer() {
    // /a (open) > /a/c (open) > /a/c/e, then /a/d, then /b
    let depths = [0, 1, 2, 1, 0];
    let step = |ix, arrow, open| tree_step(&depths, ix, arrow, open);
    assert_eq!(step(4, Arrow::Right, Some(false)), TreeStep::Toggle);
    assert_eq!(step(0, Arrow::Right, Some(true)), TreeStep::Select(1));
    assert_eq!(step(2, Arrow::Right, None), TreeStep::Stay);
    assert_eq!(step(1, Arrow::Left, Some(true)), TreeStep::Toggle);
    assert_eq!(step(2, Arrow::Left, None), TreeStep::Select(1));
    assert_eq!(step(3, Arrow::Left, None), TreeStep::Select(0));
    // At the top, ← leaves for the enclosing folder.
    assert_eq!(step(4, Arrow::Left, None), TreeStep::Up);
    assert_eq!(step(0, Arrow::Left, Some(false)), TreeStep::Up);
}

#[test]
fn sorting_a_column_again_reverses_it_and_a_new_one_starts_its_own_way() {
    let name = Sort::default();
    let newest = sorted_by(name, SortBy::Modified);
    assert_eq!((newest.by, newest.descending), (SortBy::Modified, true));
    let oldest = sorted_by(newest, SortBy::Modified);
    assert_eq!((oldest.by, oldest.descending), (SortBy::Modified, false));
    let kinds = sorted_by(oldest, SortBy::Kind);
    assert_eq!((kinds.by, kinds.descending), (SortBy::Kind, false));
    // Keeping folders on top is a setting of its own, left as it was.
    let mixed = Sort {
        folders_first: false,
        ..name
    };
    assert!(!sorted_by(mixed, SortBy::Size).folders_first);
}

#[test]
fn a_change_refreshes_only_the_folders_on_screen() {
    let shown = [PathBuf::from("/a/b"), PathBuf::from("/a/b/c")];
    assert!(touches(Path::new("/a/b"), false, &shown));
    assert!(touches(Path::new("/a/b/c"), false, &shown));
    assert!(!touches(Path::new("/a/b/c/d"), false, &shown));
    assert!(!touches(Path::new("/a"), false, &shown));
    // A lost track of everything below /a covers what's shown there.
    assert!(touches(Path::new("/a"), true, &shown));
    assert!(!touches(Path::new("/x"), true, &shown));
}

#[test]
fn only_plain_typing_starts_a_search() {
    let key = |source: &str, text: Option<&str>| {
        let mut keystroke = Keystroke::parse(source).unwrap();
        keystroke.key_char = text.map(str::to_owned);
        keystroke
    };
    assert_eq!(typed(&key("a", Some("a"))), Some("a"));
    assert_eq!(typed(&key("shift-a", Some("A"))), Some("A"));
    assert_eq!(typed(&key("alt-e", Some("é"))), Some("é"));
    assert_eq!(typed(&key("cmd-a", Some("a"))), None);
    assert_eq!(typed(&key("ctrl-a", Some("a"))), None);
    assert_eq!(typed(&key("space", Some(" "))), None);
    assert_eq!(typed(&key("enter", Some("\r"))), None);
    assert_eq!(typed(&key("up", None)), None);
}

#[gpui::test]
fn undo_reverses_an_agents_rename_after_that_agent_has_gone(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let state = tempfile::tempdir().unwrap();
    let paths = ops::Paths {
        state: state.path().join("state"),
        settings: state.path().join("settings.json"),
        trash: state.path().join("Trash"),
    };
    let serve = || {
        let owner = ops::Owner::bind(paths.clone()).unwrap();
        std::thread::spawn(move || owner.serve())
    };
    let (a, b) = (project.join("a.txt"), project.join("b.txt"));
    let serving = serve();
    let mut agent = ops::Client::new(paths.clone(), None);
    let rename = Request::Rename {
        path: a.clone(),
        name: "b.txt".into(),
    };
    agent
        .submit(Origin::Agent { client: None }, rename)
        .unwrap();
    // The agent leaves, and its owner with it; the window's undo
    // reaches the next owner.
    drop(agent);
    serving.join().unwrap().unwrap();
    let serving = serve();
    let client = ops::Client::new(paths.clone(), None);
    cx.update(|cx| cx.set_global(Operations(Arc::new(Mutex::new(client)))));
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-z");
    assert!(a.exists() && !b.exists());
    press(cx, "cmd-shift-z");
    assert!(b.exists() && !a.exists());
    press(cx, "cmd-shift-z");
    assert_eq!(
        toast(&browser, cx),
        Some(("Nothing to redo.".into(), true, false))
    );
    cx.update(|_, cx| drop(cx.remove_global::<Operations>()));
    serving.join().unwrap().unwrap();
}

/// An operation owner on a temp state folder, with a Trash of its own,
/// that the window's commands reach, and a private pasteboard so Copy
/// and Paste leave the user's alone.
pub(super) struct Owned {
    state: tempfile::TempDir,
    pub(super) paths: ops::Paths,
    serving: std::thread::JoinHandle<std::io::Result<()>>,
}

/// An owner serving `paths` on its own thread, and the window's client.
fn serve(paths: &ops::Paths) -> (std::thread::JoinHandle<std::io::Result<()>>, Operations) {
    let owner = ops::Owner::bind(paths.clone()).unwrap();
    let serving = std::thread::spawn(move || owner.serve());
    let client = ops::Client::new(paths.clone(), None);
    (serving, Operations(Arc::new(Mutex::new(client))))
}

pub(super) fn own(cx: &mut TestAppContext) -> Owned {
    let state = tempfile::tempdir().unwrap();
    let paths = ops::Paths {
        state: state.path().join("state"),
        settings: state.path().join("settings.json"),
        trash: state.path().join("Trash"),
    };
    let (serving, operations) = serve(&paths);
    cx.update(|cx| {
        cx.set_global(operations);
        let board = objc2_app_kit::NSPasteboard::pasteboardWithUniqueName();
        cx.set_global(Pasteboard(board));
    });
    Owned {
        state,
        paths,
        serving,
    }
}

/// Waits for the owner's jobs to end without polling: the window lets go,
/// the owner exits once its jobs are done, and the next one reads how
/// they ended from the journal.
pub(super) fn restart(owned: &mut Owned, cx: &mut VisualTestContext) {
    cx.update(|_, cx| drop(cx.remove_global::<Operations>()));
    let (serving, operations) = {
        let ended = std::mem::replace(&mut owned.serving, std::thread::spawn(|| Ok(())));
        ended.join().unwrap().unwrap();
        serve(&owned.paths)
    };
    owned.serving = serving;
    cx.update(|_, cx| cx.set_global(operations));
}

/// Lets the owner go once its jobs are done, and the pasteboard with it.
pub(super) fn release(owned: Owned, cx: &mut VisualTestContext) {
    cx.update(|_, cx| {
        drop(cx.remove_global::<Operations>());
        let board = cx.remove_global::<Pasteboard>();
        // SAFETY: a unique pasteboard this test made and no longer uses.
        let _: () = unsafe { objc2::msg_send![&*board.0, releaseGlobally] };
    });
    owned.serving.join().unwrap().unwrap();
}

/// The toast's text, whether it's a failure, and whether it offers Undo.
pub(super) fn toast(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
) -> Option<(String, bool, bool)> {
    browser.read_with(cx, |browser, _| {
        let toast = browser.toast.as_ref()?;
        Some((toast.text.clone(), toast.failed, toast.undo.is_some()))
    })
}

/// Draws a frame, then clicks the middle of what `selector` marks.
pub(super) fn click(cx: &mut VisualTestContext, selector: &'static str) {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let at = cx.debug_bounds(selector).unwrap().center();
    cx.simulate_click(at, with(""));
    cx.run_until_parked();
}

/// The name being edited, and why the last one didn't take.
fn editing(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
) -> Option<(String, Option<String>)> {
    in_pane(browser, cx, |browser, cx| {
        let renaming = browser.renaming.as_ref()?;
        let text = renaming.field.read(cx).text().to_string();
        Some((text, renaming.error.clone()))
    })
}

fn retype(cx: &mut VisualTestContext, name: &str) {
    press(cx, "cmd-a");
    if name.is_empty() {
        press(cx, "backspace");
    } else {
        cx.simulate_input(name);
    }
    cx.run_until_parked();
}

#[test]
fn the_path_keeps_what_fits_from_the_right_and_collapses_the_rest() {
    // Disk, Users, me, Projects, here; "…" is 20 wide.
    let widths = [80., 50., 40., 70., 60.];
    assert_eq!(hidden_levels(&widths, 20., 300.), 0);
    assert_eq!(hidden_levels(&widths, 20., 299.), 1);
    // Only levels that fit whole stay, and the hidden ones are one crumb.
    assert_eq!(hidden_levels(&widths, 20., 190.), 2);
    assert_eq!(hidden_levels(&widths, 20., 150.), 3);
    // The current folder stays, however little room there is.
    assert_eq!(hidden_levels(&widths, 20., 10.), 4);
    assert_eq!(hidden_levels(&[60.], 20., 10.), 0);
}

#[gpui::test]
fn escape_after_a_failed_rename_closes_the_field_and_its_error(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let _owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down enter");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let before = cx.debug_bounds("rename-field").unwrap();
    retype(cx, "zeta");
    press(cx, "enter");
    assert!(editing(&browser, cx).unwrap().1.is_some());
    // The reason sits under the field, inside the window, and moves no row.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let note = cx.debug_bounds("rename-error").unwrap();
    let field = cx.debug_bounds("rename-field").unwrap();
    assert_eq!(field, before);
    assert!(note.top() >= field.bottom(), "{note:?} {field:?}");
    // Even on the window's last visible row.
    cx.simulate_resize(gpui::size(px(800.), px(190.)));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let note = cx.debug_bounds("rename-error").unwrap();
    assert!(note.bottom() <= px(190.), "{note:?}");
    press(cx, "escape");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(cx.debug_bounds("rename-error"), None);
    assert_eq!(editing(&browser, cx), None);
    assert!(project.join("a.txt").exists());
    // The keys are back on the list, not in a field that went away.
    press(cx, "up");
    assert_eq!(shown(&browser, cx).1, ["zeta"]);
}

#[gpui::test]
fn a_rename_error_stays_under_sheets_and_menus(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let _owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down enter");
    retype(cx, "zeta");
    press(cx, "enter");
    let error_shown = |cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds("rename-error").is_some()
    };
    assert!(error_shown(cx));
    press(cx, "cmd-,");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_some()));
    assert!(!error_shown(cx));
    press(cx, "escape");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    assert!(error_shown(cx));
    let at = row(&browser, 0, cx);
    right_click(cx, at);
    cx.run_until_parked();
    assert!(menu(&browser, cx).is_some());
    assert!(!error_shown(cx));
}

#[gpui::test]
fn the_path_bar_shows_by_default_and_its_command_hides_it_for_good(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (_browser, cx) = browse(&project, cx);
    let shown = |cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds("path-bar").is_some()
    };
    assert!(shown(cx));
    press(cx, "alt-cmd-p");
    assert!(!shown(cx));
    assert!(!cx.update(|_, cx| cx.global::<Preferences>().show_path_bar));
    press(cx, "alt-cmd-p");
    assert!(shown(cx));
}

#[gpui::test]
fn a_click_in_the_gap_under_a_palette_row_runs_that_row(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "shift-cmd-p");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let row = cx.debug_bounds("palette-row-0").unwrap();
    cx.simulate_click(point(row.center().x, row.bottom() + px(1.)), with(""));
    cx.run_until_parked();
    // Its command runs once the palette has closed, and a press inside the
    // card can't dismiss it, so a closed palette means the row ran.
    browser.read_with(cx, |browser, _| assert!(browser.sheet.is_none()));
}

#[gpui::test]
fn return_renames_in_place_and_the_selection_follows(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down enter");
    // The name is selected up to its extension, so typing keeps it.
    cx.simulate_input("b");
    cx.run_until_parked();
    assert_eq!(editing(&browser, cx), Some(("b.txt".into(), None)));
    // A name that can't be used says why and stays open.
    for (name, why) in [
        ("zeta", "\"zeta\" is already taken."),
        ("x/y", "can't be a name"),
        ("", "can't be a name"),
    ] {
        retype(cx, name);
        press(cx, "enter");
        let (text, error) = editing(&browser, cx).unwrap();
        assert_eq!(text, name);
        assert!(
            error
                .as_deref()
                .is_some_and(|e| e.contains(why) && !e.contains(&*project.to_string_lossy())),
            "{name}: {error:?}"
        );
    }
    assert!(project.join("a.txt").exists());
    retype(cx, "b.txt");
    press(cx, "enter");
    assert_eq!(editing(&browser, cx), None);
    assert!(project.join("b.txt").exists() && !project.join("a.txt").exists());
    assert_eq!(shown(&browser, cx).1, ["b.txt"]);
    // Escape leaves the name as it was.
    press(cx, "enter");
    retype(cx, "gone.txt");
    press(cx, "escape");
    assert_eq!(editing(&browser, cx), None);
    assert_eq!(
        shown(&browser, cx),
        (
            vec!["docs".into(), "zeta".into(), "b.txt".into()],
            vec!["b.txt".into()]
        )
    );
    // Clicking away keeps the new name.
    press(cx, "enter");
    retype(cx, "c.txt");
    let docs = row(&browser, 0, cx);
    cx.simulate_click(docs, with(""));
    assert_eq!(editing(&browser, cx), None);
    assert!(project.join("c.txt").exists());
    assert_eq!(shown(&browser, cx).1, ["docs"]);
    release(owned, cx);
}

#[gpui::test]
fn new_folder_counts_on_from_untitled_and_starts_its_name(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-shift-n");
    assert!(project.join("untitled folder").is_dir());
    assert_eq!(
        editing(&browser, cx),
        Some(("untitled folder".into(), None))
    );
    assert_eq!(shown(&browser, cx).1, ["untitled folder"]);
    press(cx, "escape");
    press(cx, "cmd-shift-n");
    assert_eq!(
        editing(&browser, cx),
        Some(("untitled folder 2".into(), None))
    );
    cx.simulate_input("notes");
    press(cx, "enter");
    assert!(project.join("notes").is_dir() && !project.join("untitled folder 2").exists());
    assert_eq!(shown(&browser, cx).1, ["notes"]);
    cx.dispatch_action(NewFile);
    cx.run_until_parked();
    assert!(project.join("untitled").is_file());
    assert_eq!(editing(&browser, cx), Some(("untitled".into(), None)));
    press(cx, "escape");
    // With the selection inside, and one undo to take it out again.
    let (folder, file) = (project.join("New Folder With Items"), project.join("a.txt"));
    let a = row(&browser, 4, cx);
    cx.simulate_click(a, with(""));
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    press(cx, "ctrl-cmd-n");
    assert!(folder.join("a.txt").exists() && !file.exists());
    assert_eq!(
        editing(&browser, cx),
        Some(("New Folder With Items".into(), None))
    );
    press(cx, "escape");
    // The folder waits in the user's own Trash until redo takes it back.
    press(cx, "cmd-z");
    assert!(file.exists() && !folder.exists());
    press(cx, "cmd-shift-z");
    assert!(folder.join("a.txt").exists() && !file.exists());
    release(owned, cx);
}

#[gpui::test]
fn duplicate_selects_the_copy_and_trash_undoes(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down cmd-d");
    assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
    assert!(project.join("a.txt").exists());
    // This goes to the user's own Trash, and comes back out before the
    // test ends.
    press(cx, "cmd-backspace");
    assert!(!project.join("a copy.txt").exists());
    assert_eq!(
        shown(&browser, cx),
        (vec!["docs".into(), "zeta".into(), "a.txt".into()], vec![])
    );
    assert_eq!(
        toast(&browser, cx),
        Some(("Moved \"a copy.txt\" to the Trash".into(), false, true))
    );
    // The toast's Undo is the shared one.
    click(cx, "toast-undo");
    assert!(project.join("a copy.txt").exists());
    assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
    assert_eq!(toast(&browser, cx), None);
    release(owned, cx);
}

#[gpui::test]
fn copy_and_paste_carry_files_through_the_pasteboard(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down cmd-c");
    let on_board = cx.update(|_, cx| cx.global::<Pasteboard>().files());
    assert_eq!(on_board, [project.join("a.txt")]);
    // Pasted where it is, it's duplicated.
    press(cx, "cmd-v");
    assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
    assert_eq!(
        toast(&browser, cx),
        Some(("Duplicated \"a.txt\"".into(), false, true))
    );
    // Elsewhere it's a job, here a move. When it ends is the owner
    // thread's business; the next test waits for it.
    press(cx, "up up cmd-down cmd-alt-v");
    assert!(toast(&browser, cx).is_some_and(|(_, failed, _)| !failed));
    release(owned, cx);
    assert!(project.join("docs/a.txt").exists() && !project.join("a.txt").exists());
}

#[gpui::test]
fn a_paste_that_meets_a_taken_name_asks_and_keep_both_finishes_the_same_job(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let docs = project.join("docs");
    std::fs::write(docs.join("a.txt"), "theirs").unwrap();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down cmd-c up up cmd-down cmd-v");
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    let (popover, ring) = (
        |cx: &mut VisualTestContext| {
            browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::Jobs)))
        },
        |cx: &mut VisualTestContext| {
            browser.read_with(cx, |b, cx| b.jobs.read(cx).ring().is_some())
        },
    );
    // The person just pasted, so the question opens by itself.
    assert!(popover(cx) && ring(cx));
    click(cx, "job-Keep Both");
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(docs.join("a.txt")).unwrap(),
        "theirs"
    );
    assert!(docs.join("a 2.txt").exists());
    assert_eq!(shown(&browser, cx).1, ["a 2.txt"]);
    assert_eq!(
        toast(&browser, cx),
        Some(("Copied \"a.txt\" to docs".into(), false, true))
    );
    // One job, answered in place.
    let jobs = ops::Client::new(owned.paths.clone(), None).jobs().unwrap();
    let ended: Vec<_> = jobs.iter().map(|job| (job.id, job.state)).collect();
    assert_eq!(ended, [(1, ops::State::Completed)]);
    // It leaves the ring a few seconds after it finished.
    press(cx, "escape");
    assert!(!popover(cx) && ring(cx));
    cx.executor().advance_clock(jobs::LINGER);
    assert!(!ring(cx));
    release(owned, cx);
}

/// The context menu's rows and the one highlighted, while it's open.
pub(super) fn menu(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
) -> Option<(Vec<&'static str>, Option<&'static str>)> {
    browser.read_with(cx, |browser, cx| {
        let menu = browser.menu.as_ref()?.view.read(cx);
        let titles: Vec<_> = menu.rows.iter().map(|row| row.title).collect();
        let active = menu.active.map(|ix| titles[ix]);
        Some((titles, active))
    })
}

/// As a pointer does it: it arrives, then presses.
pub(super) fn right_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_move(at, None, with(""));
    cx.simulate_mouse_down(at, MouseButton::Right, with(""));
    cx.simulate_mouse_move(at, MouseButton::Right, with(""));
    cx.simulate_mouse_up(at, MouseButton::Right, with(""));
}

const ON_A_FILE: [&str; 11] = [
    "Open",
    "Open With",
    "Reveal in Finder",
    "Open in Terminal",
    "Get Info",
    "Rename",
    "Duplicate",
    "Quick Look",
    "Move to Trash",
    "Copy",
    "Copy Path",
];

#[gpui::test]
fn a_right_click_selects_first_and_the_menu_offers_what_applies(cx: &mut TestAppContext) {
    let (_dir, folder) = sized();
    let (browser, cx) = browse(&folder, cx);
    let rows: Vec<_> = [0, 1, 2, 3, 6].map(|ix| row(&browser, ix, cx)).into();
    cx.simulate_click(rows[0], with(""));
    // Outside the selection, the entry clicked becomes the selection.
    right_click(cx, rows[2]);
    assert_eq!(shown(&browser, cx).1, ["c.txt"]);
    assert_eq!(menu(&browser, cx), Some((ON_A_FILE.to_vec(), None)));
    // While it's open, the window's keys can't move what it acts on.
    press(cx, "alt-up cmd-[ cmd-alt-a");
    assert_eq!(shown(&browser, cx).1, ["c.txt"]);
    assert!(menu(&browser, cx).is_some());
    // Esc closes it, and leaves the selection as it was.
    press(cx, "escape");
    assert_eq!(menu(&browser, cx), None);
    assert_eq!(shown(&browser, cx).1, ["c.txt"]);
    // Inside the selection it keeps it, and the menu has what runs on
    // several: New Folder with Selection, and Rename, which opens the bar.
    cx.simulate_click(rows[1], with("cmd"));
    right_click(cx, rows[1]);
    assert_eq!(shown(&browser, cx).1, ["b.txt", "c.txt"]);
    let (titles, _) = menu(&browser, cx).unwrap();
    assert_eq!(
        titles[3..7],
        [
            "Open in Terminal",
            "Get Info",
            "New Folder with Selection",
            "Rename"
        ]
    );
    // A press outside closes it and reaches nothing behind it. The menu
    // opened rightward from the pointer, so this is left of it.
    let outside = rows[3] - gpui::point(px(50.), px(0.));
    cx.simulate_click(outside, with(""));
    assert_eq!(menu(&browser, cx), None);
    assert_eq!(shown(&browser, cx).1, ["b.txt", "c.txt"]);
    // Empty space lets go of the selection for the folder's menu.
    right_click(cx, rows[4]);
    assert!(shown(&browser, cx).1.is_empty());
    let folder_menu = vec![
        "New Folder",
        "New File",
        "Get Info",
        "Paste",
        "as List",
        "as Grid",
        "Show Hidden Files",
        "Sort by Name",
        "Sort by Date Modified",
        "Sort by Size",
        "Sort by Kind",
    ];
    assert_eq!(menu(&browser, cx), Some((folder_menu, None)));
    press(cx, "escape");
}

#[gpui::test]
fn in_the_grid_a_right_click_opens_an_items_menu(cx: &mut TestAppContext) {
    let (_dir, folder) = sized();
    let (browser, cx) = browse(&folder, cx);
    press(cx, "cmd-2");
    let cell = |ix: usize, cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        in_pane(&browser, cx, |browser, _| {
            let height = grid_cell(browser.grid_icon).1;
            browser.list_bounds.get().origin
                + gpui::point(
                    px(8. + browser.cell_width * (ix as f32 + 0.5)),
                    px(4. + height * 0.5),
                )
        })
    };
    let at = cell(2, cx);
    right_click(cx, at);
    assert_eq!(shown(&browser, cx).1, ["c.txt"]);
    assert_eq!(menu(&browser, cx), Some((ON_A_FILE.to_vec(), None)));
    press(cx, "escape");
}

#[gpui::test]
fn shift_f10_opens_the_menu_on_the_selection_and_return_runs_a_row(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down shift-f10");
    assert_eq!(menu(&browser, cx), Some((ON_A_FILE.to_vec(), None)));
    // ↑ and ↓ come round from the ends.
    press(cx, "up");
    assert_eq!(menu(&browser, cx).unwrap().1, Some("Copy Path"));
    press(cx, "down down down down down down down");
    assert_eq!(menu(&browser, cx).unwrap().1, Some("Duplicate"));
    press(cx, "enter");
    assert_eq!(menu(&browser, cx), None);
    assert!(project.join("a copy.txt").exists());
    assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
    // Only a package has Show Package Contents, in the menu bar too, and
    // it goes inside.
    std::fs::create_dir(project.join("Kit.app")).unwrap();
    on_pane(&browser, cx, |browser, cx| browser.list(true, cx));
    cx.run_until_parked();
    let offered = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| window.is_action_available(&ShowPackageContents, cx))
    };
    for (keys, name) in [("alt-up", "docs"), ("alt-down", "a.txt")] {
        press(cx, keys);
        assert_eq!(shown(&browser, cx).1, [name]);
        assert!(!offered(cx), "{name}");
    }
    press(cx, "alt-up down");
    assert_eq!(shown(&browser, cx).1, ["Kit.app"]);
    assert!(offered(cx));
    press(cx, "shift-f10 down down down down");
    assert_eq!(menu(&browser, cx).unwrap().1, Some("Show Package Contents"));
    press(cx, "enter");
    let inside = in_pane(&browser, cx, |browser, _| browser.folder().to_path_buf());
    assert_eq!(inside, project.join("Kit.app"));
    release(owned, cx);
}

#[gpui::test]
fn empty_trash_says_how_many_and_erases_only_once_confirmed(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let trash = owned.state.path().join("Trash");
    std::fs::create_dir(&trash).unwrap();
    for name in ["old.txt", "older.txt"] {
        std::fs::write(trash.join(name), "").unwrap();
    }
    let (browser, cx) = browse(&project, cx);
    let asking = |cx: &mut VisualTestContext| {
        browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::EmptyTrash(_))))
    };
    // Esc and Cancel keep everything.
    press(cx, "cmd-shift-backspace");
    assert!(asking(cx));
    let said = browser.read_with(cx, |b, cx| match &b.sheet {
        Some(Sheet::EmptyTrash(card)) => card.read(cx).message(),
        _ => String::new(),
    });
    assert_eq!(said, "2 items will be erased for good.");
    press(cx, "escape");
    assert!(!asking(cx));
    press(cx, "cmd-shift-backspace");
    click(cx, "empty-trash-cancel");
    assert!(!asking(cx));
    assert_eq!(ops::trash_items(&trash).unwrap().len(), 2);
    // ↩ empties, as Finder's default button does.
    press(cx, "cmd-shift-backspace enter");
    assert!(!asking(cx));
    assert!(ops::trash_items(&trash).unwrap().is_empty());
    press(cx, "cmd-shift-backspace");
    assert!(!asking(cx));
    assert_eq!(
        toast(&browser, cx),
        Some(("The Trash is empty.".into(), false, false))
    );
    release(owned, cx);
}

#[gpui::test]
fn the_empty_trash_card_counts_hidden_items_and_says_so(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let trash = owned.state.path().join("Trash");
    std::fs::create_dir(&trash).unwrap();
    for name in ["old.txt", ".hidden", ".DS_Store"] {
        std::fs::write(trash.join(name), "").unwrap();
    }
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-shift-backspace");
    let said = browser.read_with(cx, |b, cx| match &b.sheet {
        Some(Sheet::EmptyTrash(card)) => card.read(cx).message(),
        _ => String::new(),
    });
    assert_eq!(
        said,
        "2 items, including 1 hidden, will be erased for good."
    );
    press(cx, "escape");
    release(owned, cx);
}

/// The tab count and which is on screen.
fn tabs(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> (usize, usize) {
    browser.read_with(cx, |b, _| (b.tabs.len(), b.active))
}

fn location(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> PathBuf {
    in_pane(browser, cx, |pane, _| pane.folder().to_path_buf())
}

#[gpui::test]
fn each_tab_has_its_own_folder_history_selection_view_and_sort(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "down cmd-down");
    assert_eq!(location(&browser, cx), project.join("docs"));
    // ⌘T: the same folder, with nothing to go back to.
    press(cx, "cmd-t");
    assert_eq!(tabs(&browser, cx), (2, 1));
    assert_eq!(location(&browser, cx), project.join("docs"));
    assert_eq!(in_pane(&browser, cx, |pane, _| pane.history.len()), 1);
    press(cx, "cmd-up");
    assert_eq!(location(&browser, cx), project);
    press(cx, "alt-down cmd-2 ctrl-alt-cmd-3");
    let in_tab =
        |cx: &mut VisualTestContext| in_pane(&browser, cx, |pane, _| (pane.view, pane.sort.by));
    assert_eq!(in_tab(cx), (View::Grid, SortBy::Size));
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    // ⌃Tab comes round; the first tab is as it was, and Back still works.
    press(cx, "ctrl-tab");
    assert_eq!(tabs(&browser, cx), (2, 0));
    assert_eq!(location(&browser, cx), project.join("docs"));
    assert_eq!(in_tab(cx), (View::List, SortBy::Name));
    assert!(shown(&browser, cx).1.is_empty());
    // Preferences follow what's on screen, for the menu bar's checkmarks.
    let prefs = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let prefs = cx.global::<Preferences>();
            (prefs.view, prefs.sort.by)
        })
    };
    assert_eq!(prefs(cx), (View::List, SortBy::Name));
    press(cx, "cmd-[");
    assert_eq!(location(&browser, cx), project);
    press(cx, "ctrl-shift-tab");
    assert_eq!(tabs(&browser, cx), (2, 1));
    assert_eq!(in_tab(cx), (View::Grid, SortBy::Size));
    assert_eq!(prefs(cx), (View::Grid, SortBy::Size));
}

#[gpui::test]
fn closing_a_tab_shows_a_neighbor_and_the_last_closes_the_window(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-t cmd-t");
    assert_eq!(tabs(&browser, cx), (3, 2));
    press(cx, "cmd-w");
    assert_eq!(tabs(&browser, cx), (2, 1));
    // The keys are on what's left.
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, ["docs"]);
    press(cx, "cmd-w");
    assert_eq!(tabs(&browser, cx), (1, 0));
    press(cx, "cmd-w");
    assert!(cx.windows().is_empty());
}

#[gpui::test]
fn a_hidden_tab_lists_nothing_until_it_is_shown(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let first = pane_of(&browser, cx);
    press(cx, "cmd-t");
    // Hidden, a change on disk and the hidden-files setting leave it alone.
    std::fs::write(project.join("new.txt"), "").unwrap();
    press(cx, "cmd->");
    assert_eq!(shown(&browser, cx).0.len(), 5);
    let rows = |cx: &mut VisualTestContext| {
        first.read_with(cx, |pane, _| (pane.entries.len(), pane.listing))
    };
    assert_eq!(rows(cx), (3, false));
    first.update(cx, |pane, cx| pane.list(true, cx));
    assert_eq!(rows(cx), (3, false));
    // Shown, it reads the disk again, the setting and the new file included.
    press(cx, "ctrl-tab");
    assert_eq!(rows(cx), (5, false));
}

#[gpui::test]
fn the_tab_strip_keeps_the_name_in_place_and_reorders(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let at = |selector: &'static str, cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds(selector).unwrap()
    };
    let alone = at("tab-0", cx);
    press(cx, "cmd-t");
    // Nothing jumps when the name becomes the first tab.
    assert_eq!(at("tab-0", cx).origin, alone.origin);
    press(cx, "cmd-t cmd-up");
    let titles = |cx: &mut VisualTestContext| {
        browser.read_with(cx, |b, cx| {
            b.tabs
                .iter()
                .map(|tab| tab.focused().read(cx).title(cx))
                .collect::<Vec<_>>()
        })
    };
    let parent = project.parent().unwrap().file_name().unwrap();
    let parent = parent.to_string_lossy().into_owned();
    assert_eq!(titles(cx), ["project", "project", parent.as_str()]);
    browser.update(cx, |b, cx| b.move_tab(2, 0, cx));
    assert_eq!(titles(cx), [parent.as_str(), "project", "project"]);
    // What's on screen stays on screen.
    assert_eq!(tabs(&browser, cx), (3, 0));
    // A press selects a tab, and its ✕ closes it.
    let second = at("tab-1", cx).center();
    cx.simulate_click(second, with(""));
    assert_eq!(tabs(&browser, cx), (3, 1));
    cx.simulate_mouse_move(second, None, with(""));
    let close = at("tab-close-1", cx).center();
    cx.simulate_click(close, with(""));
    assert_eq!(tabs(&browser, cx).0, 2);
    // Too many to fit, they stop shrinking and scroll, and the strip leaves
    // room to drag the window by.
    for _ in 0..30 {
        press(cx, "cmd-t");
    }
    let pill = at("tab-0", cx);
    assert!(pill.size.width >= px(96.), "{pill:?}");
}

/// Both panes' folders, and which the commands act on.
fn split(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> (Vec<PathBuf>, bool) {
    browser.read_with(cx, |b, cx| {
        let tab = &b.tabs[b.active];
        let folders = tab
            .panes()
            .map(|pane| pane.read(cx).folder().to_path_buf())
            .collect();
        (folders, tab.second_focused)
    })
}

#[gpui::test]
fn two_panes_keep_their_own_folder_history_and_selection(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "down");
    press(cx, "cmd-alt-s");
    // A second pane on the same folder, with the keys and nothing selected.
    assert_eq!(
        split(&browser, cx),
        (vec![project.clone(), project.clone()], true)
    );
    assert!(shown(&browser, cx).1.is_empty());
    // Commands act on it alone.
    press(cx, "alt-down cmd-up");
    let parent = project.parent().unwrap().to_path_buf();
    assert_eq!(split(&browser, cx).0, [project.clone(), parent.clone()]);
    // A press in the other pane takes the commands there, its selection intact.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let left = browser.read_with(cx, |b, cx| {
        b.tabs[b.active].pane.read(cx).list_bounds.get().center()
    });
    cx.simulate_mouse_move(left, None, with(""));
    cx.simulate_mouse_down(left, MouseButton::Left, with(""));
    cx.simulate_mouse_up(left, MouseButton::Left, with(""));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!split(&browser, cx).1);
    press(cx, "cmd-down");
    // Tab moves the keys on to the other pane too.
    assert_eq!(location(&browser, cx), project);
    press(cx, "tab");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(split(&browser, cx).1);
    assert_eq!(location(&browser, cx), parent);
    press(cx, "cmd-[");
    assert_eq!(location(&browser, cx), project);
    // Closing a pane gives the keys to its sibling.
    press(cx, "cmd-alt-s");
    assert_eq!(split(&browser, cx), (vec![project.clone()], false));
    press(cx, "alt-down");
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
}

#[gpui::test]
fn the_divider_drags_and_double_click_shares_equally(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-alt-s");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let ratio = |cx: &mut VisualTestContext| browser.read_with(cx, |b, _| b.tabs[b.active].ratio);
    let at = cx.debug_bounds("split-divider").unwrap().center();
    let to = at - gpui::point(px(40.), px(0.));
    cx.simulate_mouse_down(at, MouseButton::Left, with(""));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.simulate_mouse_move(to, MouseButton::Left, with(""));
    cx.simulate_mouse_up(to, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert!(ratio(cx) < 0.5, "{}", ratio(cx));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let at = cx.debug_bounds("split-divider").unwrap().center();
    cx.simulate_event(gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: at,
        modifiers: with(""),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_mouse_up(at, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert_eq!(ratio(cx), 0.5);
}

#[gpui::test]
fn copy_and_move_to_the_other_pane_are_jobs_with_undo(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    let docs = project.join("docs");
    // With one pane, there's nowhere to send it.
    press(cx, "down down down");
    let offered = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| window.is_action_available(&CopyToOtherPane, cx))
    };
    assert!(!offered(cx));
    // Right into docs, then back left, whose a.txt the commands take.
    press(cx, "cmd-alt-s alt-up cmd-down shift-tab");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!split(&browser, cx).1);
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    assert!(offered(cx));
    press(cx, "alt-f5");
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(docs.join("a.txt").exists() && project.join("a.txt").exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Copied \"a.txt\" to docs".into(), false, true))
    );
    // What it made is selected where it landed. Undo is offered and not taken:
    // it would move the copy to the user's own Trash.
    let right = browser.read_with(cx, |b, _| b.tabs[b.active].other().cloned().unwrap());
    let chosen = right.read_with(cx, |pane, _| pane.chosen());
    assert_eq!(chosen, [docs.join("a.txt")]);
    std::fs::rename(docs.join("a.txt"), docs.join("copied.txt")).unwrap();
    // A move goes through the same way, and leaves the source listing.
    press(cx, "alt-f6");
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(docs.join("a.txt").exists() && !project.join("a.txt").exists());
    assert_eq!(shown(&browser, cx).0, ["docs", "zeta"]);
    release(owned, cx);
}

#[gpui::test]
fn closing_the_tab_that_started_a_job_leaves_the_job_running(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    let docs = project.join("docs");
    press(cx, "cmd-t cmd-alt-s alt-up cmd-down shift-tab");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    press(cx, "alt-down alt-f5 cmd-w");
    assert_eq!(tabs(&browser, cx), (1, 0));
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(docs.join("a.txt").exists());
    assert!(toast(&browser, cx).is_some_and(|(_, failed, undo)| !failed && undo));
    release(owned, cx);
}

#[gpui::test]
fn the_sidebar_closes_and_search_still_works_without_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (_browser, cx) = browse(&project, cx);
    let on_screen = |selector: &'static str, cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds(selector)
    };
    let open =
        |cx: &mut VisualTestContext| cx.update(|_, cx| cx.global::<Preferences>().show_sidebar);
    assert!(on_screen("place-0", cx).is_some());
    let in_sidebar = on_screen("toggle-sidebar", cx).unwrap();
    press(cx, "ctrl-cmd-s");
    assert!(!open(cx));
    assert!(on_screen("place-0", cx).is_none());
    assert!(on_screen("search-field", cx).is_none());
    // Its button moves to the top bar, clear of the traffic lights.
    let in_bar = on_screen("toggle-sidebar", cx).unwrap();
    assert!(in_bar.left() > px(crate::traffic_lights::REACH));
    assert_ne!(in_bar, in_sidebar);
    // ⌘F has a field to type in, and Esc puts it away.
    press(cx, "cmd-f");
    assert!(on_screen("search-field", cx).is_some());
    press(cx, "escape");
    assert!(on_screen("search-field", cx).is_none());
    // The button brings the sidebar back.
    click(cx, "toggle-sidebar");
    assert!(open(cx));
    assert!(on_screen("place-0", cx).is_some());
}

#[gpui::test]
fn a_change_on_disk_reaches_both_panes_on_screen(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    // Left on the project, right in docs, with the commands on the right.
    press(cx, "cmd-alt-s down cmd-down");
    let left = browser.read_with(cx, |b, _| b.tabs[b.active].pane.clone());
    assert_eq!(left.read_with(cx, |pane, _| pane.entries.len()), 3);
    std::fs::write(project.join("new.txt"), "").unwrap();
    let changed = vec![(project.canonicalize().unwrap(), false)];
    browser.update(cx, |b, cx| b.disk_changed(&changed, cx));
    cx.executor().advance_clock(REFRESH_DELAY);
    cx.run_until_parked();
    assert_eq!(left.read_with(cx, |pane, _| pane.entries.len()), 4);
}

/// Whether the keys are on the list of the pane commands act on.
fn keys_on_the_list(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    let list = in_pane(browser, cx, |pane, _| pane.list_focus.clone());
    cx.update(|window, _| list.is_focused(window))
}

#[gpui::test]
fn a_rename_that_fails_in_a_hidden_tab_leaves_the_keys_alone(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    // The second tab is the one edited; ⌃Tab comes round to the first.
    press(cx, "cmd-t down down down enter");
    let edited = pane_of(&browser, cx);
    retype(cx, "zeta");
    press(cx, "ctrl-tab");
    assert_eq!(tabs(&browser, cx), (2, 0));
    // The blur sent the name, and it's taken: the reason waits in that tab.
    let why = edited.read_with(cx, |pane, _| {
        pane.renaming.as_ref().and_then(|r| r.error.clone())
    });
    assert!(why.is_some());
    assert!(keys_on_the_list(&browser, cx));
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, ["docs"]);
    // Back in that tab, its field has the keys again.
    press(cx, "ctrl-tab");
    let field = edited.read_with(cx, |pane, cx| {
        pane.renaming.as_ref().unwrap().field.focus_handle(cx)
    });
    assert!(cx.update(|window, _| field.is_focused(window)));
    press(cx, "escape");
    release(owned, cx);
}

#[gpui::test]
fn a_new_folder_made_after_leaving_its_tab_leaves_the_keys_alone(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-t");
    let made_in = pane_of(&browser, cx);
    // Both keys go in before the owner answers.
    cx.simulate_keystrokes("cmd-shift-n ctrl-tab");
    cx.run_until_parked();
    assert_eq!(tabs(&browser, cx), (2, 0));
    assert!(project.join("untitled folder").is_dir());
    assert!(made_in.read_with(cx, |pane, _| pane.renaming.is_none()));
    assert!(keys_on_the_list(&browser, cx));
    release(owned, cx);
}

/// A folder holding one picture, and the pane's thumbnails once the grid drew
/// it.
fn drawn_thumbs(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Vec<Thumb> {
    press(cx, "cmd-2");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    in_pane(browser, cx, |pane, _| pane.thumbs.keys().cloned().collect())
}

#[gpui::test]
fn closing_a_tab_or_a_pane_gives_back_its_thumbnails(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (one, two) = (dir.path().join("one"), dir.path().join("two"));
    for (folder, side) in [(&one, 40), (&two, 50)] {
        std::fs::create_dir(folder).unwrap();
        image::RgbaImage::new(side, side)
            .save(folder.join("picture.png"))
            .unwrap();
    }
    let (browser, cx) = browse(&one, cx);
    // A second tab on the other picture, then close it.
    press(cx, "cmd-t");
    on_pane(&browser, cx, |pane, cx| pane.navigate(two.clone(), cx));
    cx.run_until_parked();
    let thumbs = drawn_thumbs(&browser, cx);
    assert_eq!(thumbs.len(), 1);
    let cached = |thumb: &Thumb, cx: &mut VisualTestContext| {
        cx.update(|_, cx| cx.has_asset::<Thumbnail>(thumb))
    };
    assert!(cached(&thumbs[0], cx));
    press(cx, "cmd-w");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!cached(&thumbs[0], cx));
    // The same for one of two panes.
    press(cx, "cmd-alt-s");
    on_pane(&browser, cx, |pane, cx| pane.navigate(two.clone(), cx));
    cx.run_until_parked();
    let thumbs = drawn_thumbs(&browser, cx);
    assert!(cached(&thumbs[0], cx));
    press(cx, "cmd-alt-s");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!cached(&thumbs[0], cx));
}

#[gpui::test]
fn a_thumbnail_stays_until_the_last_pane_showing_it_lets_go(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (empty, pictures) = (dir.path().join("empty"), dir.path().join("pictures"));
    std::fs::create_dir(&empty).unwrap();
    std::fs::create_dir(&pictures).unwrap();
    image::RgbaImage::new(40, 40)
        .save(pictures.join("picture.png"))
        .unwrap();
    let (browser, cx) = browse(&empty, cx);
    press(cx, "cmd-t");
    on_pane(&browser, cx, |pane, cx| pane.navigate(pictures.clone(), cx));
    cx.run_until_parked();
    let thumbs = drawn_thumbs(&browser, cx);
    let cached =
        |cx: &mut VisualTestContext| cx.update(|_, cx| cx.has_asset::<Thumbnail>(&thumbs[0]));
    // A new pane on the same folder draws the same picture; it goes when one
    // of the two does, and the other keeps it.
    press(cx, "cmd-alt-s");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let image = |cx: &mut VisualTestContext| {
        in_pane(&browser, cx, |pane, _| {
            pane.thumbs[&thumbs[0]].clone().unwrap()
        })
    };
    let before = image(cx);
    press(cx, "cmd-alt-s");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    // Still the same picture: it wasn't dropped and decoded again.
    assert!(cached(cx));
    assert!(Arc::ptr_eq(&before, &image(cx)));
    // The last one out takes it.
    press(cx, "cmd-w");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!cached(cx));
}

#[gpui::test]
fn a_search_while_the_crawl_waits_says_why_and_starts_nothing(cx: &mut TestAppContext) {
    use crate::onboarding::tests::Fake;
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let gate = IndexGate::new(fake.clone(), true);
    cx.update(|cx| cx.set_global(gate.clone()));
    let (browser, cx) = browse(dir.path(), cx);

    cx.simulate_input("notes");
    cx.run_until_parked();
    let error = in_pane(&browser, cx, |pane, _| pane.error.clone());
    assert_eq!(
        error.as_deref(),
        Some("Search starts when you finish setting up Monodrive.")
    );
    assert_eq!(fake.starts.load(Ordering::SeqCst), 0);
}

#[gpui::test]
fn the_no_access_notice_stays_after_indexing_and_opens_that_step(cx: &mut TestAppContext) {
    use crate::onboarding::tests::Fake;
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| cx.set_global(IndexGate::new(Arc::new(Fake::default()), false)));
    let (browser, cx) = browse(dir.path(), cx);
    assert!(cx.debug_bounds("no-full-disk-access").is_none());

    browser.update(cx, |browser, cx| {
        browser.index_status = Some(Status {
            progress: Progress::Ready { entries: 1 },
            full_disk_access: Some(false),
        });
        cx.notify();
    });
    cx.run_until_parked();
    let notice = cx.debug_bounds("no-full-disk-access").unwrap();
    cx.simulate_click(notice.center(), Modifiers::default());
    cx.run_until_parked();

    let opened = cx.update(|window, cx| {
        let this = window.window_handle();
        cx.windows()
            .into_iter()
            .find(|other| other.window_id() != this.window_id())
    });
    let mut onboarding = VisualTestContext::from_window(opened.unwrap(), cx);
    assert!(onboarding.debug_bounds("access-grant").is_some());
}

#[test]
fn a_cut_name_keeps_only_a_real_extension() {
    for (name, extension) in [
        ("Denuncia_final.pdf", Some(".pdf")),
        ("archive.tar.gz", Some(".gz")),
        ("Monodrive.app", Some(".app")),
        ("README", None),
        (".gitignore", None),
        ("Report v1.2 for the board", None),
        ("notes.averyveryverylongsuffix", None),
    ] {
        assert_eq!(kept_extension(name), extension, "{name}");
    }
}

/// A browser whose listing never lands, as on a volume that hangs, and a flag
/// for whether launch went ahead and showed it.
fn waiting_to_show<'a>(
    folder: &Path,
    cx: &'a mut TestAppContext,
) -> (Entity<Browser>, Rc<Cell<bool>>, &'a mut VisualTestContext) {
    let (browser, cx) = browse(folder, cx);
    let pane = pane_of(&browser, cx);
    pane.update(cx, |pane, _| pane.listing = true);
    let shown = Rc::new(Cell::new(false));
    let flag = shown.clone();
    let views = vec![browser.clone()];
    cx.update(|_, cx| crate::when_listed(views, move |_| flag.set(true), cx));
    cx.run_until_parked();
    assert!(!shown.get());
    (browser, shown, cx)
}

#[gpui::test]
fn launch_shows_the_window_once_its_listing_lands(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, shown, cx) = waiting_to_show(&project, cx);
    let pane = pane_of(&browser, cx);
    pane.update(cx, |pane, cx| {
        pane.listing = false;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(shown.get());
}

#[gpui::test]
fn launch_shows_the_window_without_a_listing_that_hangs(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (_browser, shown, cx) = waiting_to_show(&project, cx);
    cx.executor().advance_clock(crate::LISTING_WAIT);
    cx.run_until_parked();
    assert!(shown.get());
}

#[gpui::test]
fn launch_waits_for_the_pane_without_focus_too(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-alt-s");
    let left = browser.read_with(cx, |b, _| b.tabs[b.active].pane.clone());
    left.update(cx, |pane, _| pane.listing = true);
    let shown = Rc::new(Cell::new(false));
    let flag = shown.clone();
    let views = vec![browser.clone()];
    cx.update(|_, cx| crate::when_listed(views, move |_| flag.set(true), cx));
    cx.run_until_parked();
    assert!(!shown.get());
    left.update(cx, |pane, cx| {
        pane.listing = false;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(shown.get());
}
