mod batch_rename;
mod cloud;
pub(super) mod drag;
mod duplicates;
mod inspector;
mod places;
mod preview;
mod remote;
mod search;
mod tabs;

use super::pane::rows::*;
use super::pane::view::span;
use super::pane::*;
use super::pane::{flatten, touches};
use super::*;
use crate::pasteboard::Pasteboard;
use crate::preferences::View;
use crate::thumbnails::{Holders, Thumb};
use gpui::{Keystroke, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, point};
use monodrive_core::Origin;
use monodrive_core::entries::{Entry, Kind, Sort, SortBy};
use monodrive_core::ops::Request;
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
        crate::apply_keymap(cx);
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

/// Presses `key` without running what it started, so a check sees what's
/// drawn before a listing it began lands.
fn press_unlanded(cx: &mut VisualTestContext, key: &str) {
    cx.update(|window, cx| window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx));
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
/// In column view, the keys' column's row.
pub(super) fn row(
    browser: &Entity<Browser>,
    ix: usize,
    cx: &mut VisualTestContext,
) -> Point<Pixels> {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    in_pane(browser, cx, |browser, cx| {
        let height = cx.global::<Preferences>().row_size.metrics().0;
        let gap = match browser.layout() {
            View::Columns => theme::ROW_GAP,
            _ => 0.,
        };
        browser.list_bounds.get().origin
            + gpui::point(
                px(60.),
                px(theme::CARD_PADDING + (height + gap) * ix as f32 + height / 2.),
            )
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

/// A press and release at `at` in one frame, without running the executor,
/// as a quick tap arrives.
pub(super) fn click_unanswered(cx: &mut VisualTestContext, at: Point<Pixels>) {
    let press = gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: at,
        modifiers: Modifiers::none(),
        click_count: 1,
        first_mouse: false,
    };
    let release = gpui::MouseUpEvent {
        button: MouseButton::Left,
        position: at,
        modifiers: Modifiers::none(),
        click_count: 1,
    };
    cx.update(|window, cx| {
        window.dispatch_event(gpui::PlatformInput::MouseDown(press), cx);
        window.dispatch_event(gpui::PlatformInput::MouseUp(release), cx);
    });
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
    press(cx, "cmd-alt-2");
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
fn a_rectangle_in_column_view_counts_the_gaps_between_rows(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("many");
    std::fs::create_dir(&folder).unwrap();
    for ix in 0..20 {
        std::fs::write(folder.join(format!("f{ix:02}.txt")), "").unwrap();
    }
    let (browser, cx) = browse(&folder, cx);
    press(cx, "cmd-alt-3");
    // Far enough down that rows counted without their gaps are a row off.
    let side = gpui::point(px(100.), px(0.));
    let (from, to) = (row(&browser, 16, cx) + side, row(&browser, 17, cx) + side);
    cx.simulate_mouse_down(from, MouseButton::Left, with(""));
    cx.simulate_mouse_move(to, MouseButton::Left, with(""));
    cx.simulate_mouse_up(to, MouseButton::Left, with(""));
    assert_eq!(shown(&browser, cx).1, ["f16.txt", "f17.txt"]);
}

/// A tap releases before the frame that follows the press is drawn. It
/// still lets go of the selection, and moving the pointer afterwards, with
/// the button up, selects nothing.
#[gpui::test]
fn a_quick_tap_on_blank_space_never_leaves_a_rectangle_behind(cx: &mut TestAppContext) {
    let (_dir, folder) = sized();
    let (browser, cx) = browse(&folder, cx);
    let last = row(&browser, 3, cx);
    cx.simulate_click(last, with(""));
    assert_eq!(shown(&browser, cx).1, ["d.txt"]);
    // Below the rows, where a click selects no row.
    let (from, to) = (row(&browser, 6, cx), blank(&browser, 2, cx));
    click_unanswered(cx, from);
    cx.run_until_parked();
    assert!(shown(&browser, cx).1.is_empty());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.simulate_mouse_move(to, None, with(""));
    assert!(shown(&browser, cx).1.is_empty());
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
    assert_eq!(
        toast(&browser, cx),
        Some(("Undid Rename".into(), false, false))
    );
    press(cx, "cmd-shift-z");
    assert!(b.exists() && !a.exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Redid Rename".into(), false, false))
    );
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
    // An owner that no client ever reached waits 30 s for its first one.
    // One that's already gone refuses the connection, which is fine.
    let _ = ops::Client::new(owned.paths.clone(), None).jobs();
    owned.serving.join().unwrap().unwrap();
}

/// The toast's text, whether it's a failure, and whether it offers Undo.
pub(super) fn toast(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
) -> Option<(String, bool, bool)> {
    browser.read_with(cx, |browser, _| {
        let toast = browser.toast.as_ref()?;
        Some((toast.text.clone(), toast.failed, toast.offer.is_some()))
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

fn on_screen(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector).is_some()
}

#[gpui::test]
fn the_path_bar_takes_a_typed_path_and_tab_completes_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-l");
    assert!(on_screen(cx, "address-bar"));
    // The path starts selected, so typing replaces it.
    cx.simulate_input(&format!("{}/do", project.display()));
    cx.run_until_parked();
    press(cx, "tab enter");
    assert_eq!(shown(&browser, cx).0, ["inner.md"]);
    assert!(in_pane(&browser, cx, |pane, _| pane.address.is_none()));
    assert!(keys_on_the_list(&browser, cx));
}

#[gpui::test]
fn a_path_that_isnt_there_says_so_and_stays(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let before = shown(&browser, cx).0;
    press(cx, "cmd-l");
    cx.simulate_input(&format!("{}/nowhere", project.display()));
    press(cx, "enter");
    assert!(on_screen(cx, "address-error"));
    assert_eq!(shown(&browser, cx).0, before);
    assert!(in_pane(&browser, cx, |pane, _| pane.address.is_some()));
}

#[gpui::test]
fn going_elsewhere_while_typing_a_path_hands_the_keys_to_the_list(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-l cmd-up");
    assert!(in_pane(&browser, cx, |pane, _| pane.address.is_none()));
    assert!(keys_on_the_list(&browser, cx));
}

#[gpui::test]
fn a_sheet_opened_while_typing_a_path_hands_the_keys_to_the_list_on_closing(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-l cmd-k escape");
    assert!(keys_on_the_list(&browser, cx));
}

#[gpui::test]
fn esc_hands_the_keys_back_to_the_list_and_a_hidden_path_bar_hides_again(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-cmd-p");
    assert!(!on_screen(cx, "path-bar"));
    press(cx, "cmd-l");
    assert!(on_screen(cx, "address-bar"));
    press(cx, "escape");
    assert!(!on_screen(cx, "path-bar"));
    assert!(keys_on_the_list(&browser, cx));
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
    click(cx, "toast-offer");
    assert!(project.join("a copy.txt").exists());
    assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
    // It says what it undid, and offers no Undo of the undo.
    assert_eq!(
        toast(&browser, cx),
        Some(("Undid Move to Trash".into(), false, false))
    );
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

const ON_A_FILE: [&str; 15] = [
    "Open",
    "Open With",
    "Reveal in Finder",
    "Open in Terminal",
    "Share…",
    "Get Info",
    "Rename",
    "Suggest Names",
    "Duplicate",
    "Quick Look",
    "Open in Quick Look",
    "Compress",
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
        titles[3..8],
        [
            "Open in Terminal",
            "Share…",
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
        "Organize Folder…",
        "Find Duplicates",
        "Get Info",
        "Paste",
        "as List",
        "as Grid",
        "as Columns",
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
    press(cx, "cmd-alt-2");
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
    press(cx, "down down down down down down down down down");
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

/// Protects the ring's card: it waits a moment before showing and before
/// going, stays while the pointer is on the card, and goes with the ring.
#[gpui::test]
fn the_jobs_card_shows_while_the_pointer_rests_on_the_ring_or_the_card(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let running = ops::Job {
        id: 1,
        title: "Copy".into(),
        origin: Origin::Window,
        into: project.join("docs"),
        state: ops::State::Running,
        tasks: vec![ops::Task {
            source: project.join("zeta"),
            done: None,
            name: None,
        }],
        bytes: 0,
        total_bytes: 0,
        started: 0,
        run: 1,
        undoable: false,
    };
    let take = |list: Vec<ops::Job>, cx: &mut VisualTestContext| {
        browser.update(cx, |b, cx| {
            b.jobs.update(cx, |jobs, cx| jobs.take(Ok(list), cx))
        });
    };
    let card = |cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds("jobs-peek").is_some()
    };
    let hover = |card: bool, over: bool, cx: &mut VisualTestContext| {
        browser.update(cx, |b, cx| b.peek_jobs(card, over, cx));
    };
    take(vec![running], cx);
    hover(false, true, cx);
    assert!(!card(cx));
    cx.executor().advance_clock(PEEK_DELAY);
    assert!(card(cx));
    // Onto the card: it says the pointer arrived before the ring says it left.
    hover(true, true, cx);
    hover(false, false, cx);
    cx.executor().advance_clock(PEEK_DELAY);
    assert!(card(cx));
    hover(true, false, cx);
    assert!(card(cx));
    cx.executor().advance_clock(PEEK_DELAY);
    assert!(!card(cx));
    // The ring goes with its job and never says the pointer left.
    hover(false, true, cx);
    cx.executor().advance_clock(PEEK_DELAY);
    assert!(card(cx));
    take(Vec::new(), cx);
    assert!(!card(cx));
    assert!(!browser.read_with(cx, |b, _| b.jobs_peek.shown));
}

#[gpui::test]
fn empty_trash_says_how_many_and_erases_only_once_confirmed(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let trash = owned.state.path().join("Trash");
    std::fs::create_dir(&trash).unwrap();
    // .DS_Store is the Finder's own and isn't counted; other hidden items are.
    for name in ["old.txt", "older.txt", ".hidden", ".DS_Store"] {
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
    assert_eq!(
        said,
        "3 items, including 1 hidden, will be erased for good."
    );
    press(cx, "escape");
    assert!(!asking(cx));
    press(cx, "cmd-shift-backspace");
    click(cx, "empty-trash-cancel");
    assert!(!asking(cx));
    assert_eq!(ops::trash_items(&trash).unwrap().len(), 3);
    // ↩ empties, as Finder's default button does. It's a job the window
    // follows, so the toast comes once the owner has erased everything.
    press(cx, "cmd-shift-backspace enter");
    assert!(!asking(cx));
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(ops::trash_items(&trash).unwrap().is_empty());
    assert_eq!(
        toast(&browser, cx),
        Some(("Erased 3 items for good".into(), false, false))
    );
    press(cx, "cmd-shift-backspace");
    assert!(!asking(cx));
    assert_eq!(
        toast(&browser, cx),
        Some(("The Trash is empty.".into(), false, false))
    );
    release(owned, cx);
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

/// The names listed for `dir`'s column, if it's been listed.
fn column_names(
    browser: &Entity<Browser>,
    dir: &Path,
    cx: &mut VisualTestContext,
) -> Option<Vec<String>> {
    in_pane(browser, cx, |pane, _| {
        let listed = pane.children.get(dir)?;
        Some(listed.iter().map(|entry| entry.name.clone()).collect())
    })
}

#[gpui::test]
fn column_view_moves_the_keys_across_columns(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-alt-3 down");
    assert_eq!(
        in_pane(&browser, cx, |pane, _| pane.layout()),
        View::Columns
    );
    assert_eq!(shown(&browser, cx).1, ["docs"]);
    // The last column lists a folder one step selected at once.
    let docs = project.join("docs");
    assert_eq!(
        column_names(&browser, &docs, cx),
        Some(vec!["inner.md".into()])
    );
    // A step right after another, as a held key makes, waits for the
    // selection to rest.
    let zeta = project.join("zeta");
    press(cx, "down");
    assert_eq!(column_names(&browser, &zeta, cx), None);
    cx.executor().advance_clock(super::facts::READ_DELAY);
    cx.run_until_parked();
    assert_eq!(column_names(&browser, &zeta, cx), Some(vec![]));
    press(cx, "up");
    // →: into it, with its first item selected and the folder a column back.
    // Its column's rows show before the new listing lands, never the
    // folder just left.
    press_unlanded(cx, "right");
    assert_eq!(location(&browser, cx), docs);
    assert_eq!(
        shown(&browser, cx),
        (vec!["inner.md".into()], vec!["inner.md".into()])
    );
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).1, ["inner.md"]);
    assert_eq!(
        in_pane(&browser, cx, |pane, _| pane.trail()),
        [project.as_path()]
    );
    // Commands act on the keys' column: New Folder makes one in docs.
    press(cx, "cmd-shift-n");
    assert!(docs.join("untitled folder").is_dir());
    assert_eq!(
        editing(&browser, cx),
        Some(("untitled folder".into(), None))
    );
    press(cx, "escape");
    // A click in the column back moves the keys there, onto what it hit.
    let column = cx.debug_bounds("column-0").expect("the first column");
    let height = Preferences::default().row_size.metrics().0;
    let zeta = point(
        column.left() + px(40.),
        column.top() + px(4. + height * 1.5),
    );
    cx.simulate_click(zeta, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(location(&browser, cx), project);
    assert_eq!(shown(&browser, cx).1, ["zeta"]);
    // ←: back a column with the folder left selected, then past the first
    // column, where the chain starts again.
    press(cx, "up right");
    press_unlanded(cx, "left");
    assert_eq!(location(&browser, cx), project);
    assert_eq!(
        shown(&browser, cx),
        (
            vec!["docs".into(), "zeta".into(), "a.txt".into()],
            vec!["docs".into()]
        )
    );
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).1, ["docs"]);
    press(cx, "left");
    assert_eq!(location(&browser, cx), project.parent().unwrap());
    assert_eq!(shown(&browser, cx).1, ["project"]);
    assert!(in_pane(&browser, cx, |pane, _| pane.trail()).is_empty());
    press(cx, "cmd-[");
    assert_eq!(location(&browser, cx), project);
    // Opened by other means, as from the sidebar, a folder inside the chain
    // starts one of its own.
    press(cx, "down right");
    assert_eq!(location(&browser, cx), docs);
    assert!(!in_pane(&browser, cx, |pane, _| pane.trail()).is_empty());
    let (pane, untitled) = (pane_of(&browser, cx), docs.join("untitled folder"));
    cx.update(|window, cx| pane.update(cx, |pane, cx| pane.go(untitled, window, cx)));
    cx.run_until_parked();
    assert!(in_pane(&browser, cx, |pane, _| pane.trail()).is_empty());
    release(owned, cx);
}

#[gpui::test]
fn a_compressed_folder_opens_read_only_and_extracts_beside_itself(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    let offered = |cx: &mut VisualTestContext, action: &dyn gpui::Action| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.update(|window, cx| window.is_action_available(action, cx))
    };
    // A folder that takes files compresses its selection beside it.
    press(cx, "down");
    assert!(offered(cx, &Compress) && !offered(cx, &Extract));
    cx.dispatch_action(Compress);
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert_eq!(
        toast(&browser, cx),
        Some(("Compressed into \"docs.zip\"".into(), false, true))
    );
    assert_eq!(shown(&browser, cx).1, ["docs.zip"]);
    assert!(offered(cx, &Extract));

    // Opening it goes in, like a folder, and changes nothing.
    press(cx, "cmd-down");
    let zip = project.join("docs.zip");
    let title = browser.read_with(cx, |b, cx| b.pane().read(cx).title(cx));
    assert_eq!(title, "docs.zip");
    assert_eq!(shown(&browser, cx).0, ["docs"]);
    press(cx, "down");
    for action in [
        &MoveToTrash as &dyn gpui::Action,
        &Rename,
        &Duplicate,
        &text_field::Copy,
        &Compress,
        &NewFolder,
        &RevealInFinder,
        &Share,
        &QuickLook,
    ] {
        assert!(!offered(cx, action), "{}", action.name());
    }
    assert!(offered(cx, &Extract));
    press(cx, "cmd-down");
    assert_eq!(shown(&browser, cx).0, ["inner.md"]);
    // A file inside doesn't open: it isn't on the disk.
    press(cx, "down cmd-down");
    assert_eq!(
        toast(&browser, cx),
        Some(("Extract \"inner.md\" to open it.".into(), false, false))
    );
    // The way out, one level at a time.
    press(cx, "cmd-[");
    assert_eq!(
        shown(&browser, cx),
        (vec!["docs".into()], vec!["docs".into()])
    );

    // Extract All lands beside the archive, where "docs" is taken: the job
    // waits, and Keep Both finishes it.
    press(cx, "escape");
    cx.dispatch_action(Extract);
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::Jobs))));
    click(cx, "job-Keep Both");
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(project.join("docs 2/inner.md").is_file());
    assert!(zip.exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Extracted \"docs\" to project".into(), false, true))
    );
    // Back leaves the archive for the folder holding it, once the popover
    // has closed. Until the folder's listing lands, the rows on screen are
    // still the archive's, and stay read only.
    press(cx, "escape down");
    on_pane(&browser, cx, |pane, cx| pane.step_history(-1, cx));
    assert!(in_pane(&browser, cx, |pane, cx| pane.showing_archive()
        && !pane.can_compress(cx)));
    // Its folder row doesn't open either: it isn't on the disk.
    cx.simulate_keystrokes("cmd-down");
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).1, ["docs.zip"]);
    assert!(!in_pane(&browser, cx, |pane, _| pane.showing_archive()));
    // Two archives at once don't go to another app, which would extract
    // them outside the owner. Nor does a link to one, which opens here.
    std::fs::copy(&zip, project.join("e.zip")).unwrap();
    std::os::unix::fs::symlink(&zip, project.join("f.zip")).unwrap();
    on_pane(&browser, cx, |pane, cx| pane.list(true, cx));
    cx.run_until_parked();
    press(cx, "shift-down cmd-down");
    assert_eq!(shown(&browser, cx).1, ["docs.zip", "e.zip"]);
    assert_eq!(
        toast(&browser, cx),
        Some(("Archives open one at a time.".into(), false, false))
    );
    press(cx, "down cmd-down");
    assert_eq!(shown(&browser, cx).0, ["docs"]);
    assert!(in_pane(&browser, cx, |pane, _| pane.showing_archive()));
    release(owned, cx);
}

fn location(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> PathBuf {
    in_pane(browser, cx, |pane, _| pane.folder().to_path_buf())
}

/// Whether the keys are on the list of the pane commands act on.
pub(super) fn keys_on_the_list(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    let list = in_pane(browser, cx, |pane, _| pane.list_focus.clone());
    cx.update(|window, _| list.is_focused(window))
}
