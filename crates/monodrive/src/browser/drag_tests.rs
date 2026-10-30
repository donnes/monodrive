//! Dragging files through the real `Browser` over temp folders, with mouse
//! events and an operation owner of the test's own. Nothing reaches the real
//! Trash: an undo here is of a move, which goes back by rename.

use super::drag::Spot;
use super::pane::*;
use super::tests::*;
use super::*;
use crate::preferences::View;
use gpui::{
    ExternalDragPayload, ExternalPaths, FileDropEvent, Modifiers, TestAppContext,
    VisualTestContext, point,
};

/// Draws a frame, as the test platform doesn't on its own: what offers a
/// target listens only in frames with a drag.
pub(super) fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// Moves the pointer to `to` with the button down, and shows what's there.
pub(super) fn hover(to: Point<Pixels>, keys: &str, cx: &mut VisualTestContext) {
    cx.simulate_mouse_move(to, MouseButton::Left, with(keys));
    draw(cx);
}

/// Presses on what's at `from`, starts a drag and carries it to `to`.
pub(super) fn carry(
    from: Point<Pixels>,
    to: Point<Pixels>,
    keys: &str,
    cx: &mut VisualTestContext,
) {
    cx.simulate_mouse_down(from, MouseButton::Left, with(keys));
    hover(from + point(px(8.), px(0.)), keys, cx);
    hover(to, keys, cx);
}

pub(super) fn says(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Option<String> {
    browser.read_with(cx, |b, _| b.drag.decision(|_, d| d.says()))
}

fn dragging(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    browser.read_with(cx, |b, _| b.drag.active())
}

/// Lets the job a drop started finish, and the window hear of it.
fn finish_job(owned: &mut Owned, cx: &mut VisualTestContext) {
    restart(owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
}

#[gpui::test]
fn dragging_selects_an_outsider_first_and_takes_the_whole_selection_from_inside(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let (docs, zeta, file) = (
        row(&browser, 0, cx),
        row(&browser, 1, cx),
        row(&browser, 2, cx),
    );
    cx.simulate_click(file, with(""));
    // From outside the selection: that item alone.
    carry(zeta, docs, "", cx);
    assert_eq!(shown(&browser, cx).1, ["zeta"]);
    assert_eq!(says(&browser, cx).unwrap(), "Move zeta to docs");
    press(cx, "escape");
    cx.simulate_mouse_up(docs, MouseButton::Left, with(""));
    // ⌘ adds it to the selection, as a click would.
    carry(file, docs, "cmd", cx);
    assert_eq!(shown(&browser, cx).1, ["zeta", "a.txt"]);
    assert_eq!(says(&browser, cx).unwrap(), "Move 2 items to docs");
    press(cx, "escape");
    cx.simulate_mouse_up(docs, MouseButton::Left, with("cmd"));
    // From inside it, by any part of a selected row: all of it, unchanged.
    carry(blank(&browser, 2, cx), docs, "", cx);
    assert_eq!(shown(&browser, cx).1, ["zeta", "a.txt"]);
    assert_eq!(says(&browser, cx).unwrap(), "Move 2 items to docs");
    press(cx, "escape");
    cx.simulate_mouse_up(docs, MouseButton::Left, with(""));
}

#[gpui::test]
fn a_drop_on_a_folder_row_moves_only_on_release_and_undo_puts_it_back(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    let (docs, file) = (row(&browser, 0, cx), row(&browser, 2, cx));
    carry(file, docs, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
    let pane = pane_of(&browser, cx).entity_id();
    let outlined = browser.read_with(cx, |b, _| b.drag.outlines(&Spot::Row(pane, 0)));
    assert!(outlined);
    assert!(project.join("a.txt").exists() && !project.join("docs/a.txt").exists());
    cx.simulate_mouse_up(docs, MouseButton::Left, with(""));
    assert!(!dragging(&browser, cx));
    finish_job(&mut owned, cx);
    assert!(!project.join("a.txt").exists() && project.join("docs/a.txt").exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Moved \"a.txt\" to docs".into(), false, true))
    );
    click(cx, "toast-undo");
    assert!(project.join("a.txt").exists() && !project.join("docs/a.txt").exists());
    release(owned, cx);
}

#[gpui::test]
fn the_pill_follows_the_keys_and_the_drop_does_what_it_says(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    let (docs, file) = (row(&browser, 0, cx), row(&browser, 2, cx));
    carry(file, docs, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
    // No move of the pointer: the keys alone change it.
    cx.simulate_modifiers_change(with("alt"));
    draw(cx);
    assert_eq!(says(&browser, cx).unwrap(), "Copy a.txt to docs");
    cx.simulate_modifiers_change(Modifiers::default());
    draw(cx);
    assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
    cx.simulate_modifiers_change(with("alt"));
    draw(cx);
    cx.simulate_mouse_up(docs, MouseButton::Left, with("alt"));
    finish_job(&mut owned, cx);
    assert!(project.join("a.txt").exists() && project.join("docs/a.txt").exists());
    // Undo is offered and not taken: it would use the user's own Trash.
    assert_eq!(
        toast(&browser, cx),
        Some(("Copied \"a.txt\" to docs".into(), false, true))
    );
    release(owned, cx);
}

#[gpui::test]
fn its_own_folder_and_itself_take_nothing_and_a_folder_refuses_its_descendants(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    std::fs::create_dir(project.join("docs/deep")).unwrap();
    let (browser, cx) = browse(&project, cx);
    // docs open in place: docs, deep, inner.md, zeta, a.txt.
    press(cx, "down right");
    assert_eq!(shown(&browser, cx).0.len(), 5);
    let pane = pane_of(&browser, cx).entity_id();
    let outlined_any = |cx: &mut VisualTestContext| {
        browser.read_with(cx, |b, _| {
            (0..5).any(|ix| b.drag.outlines(&Spot::Row(pane, ix)))
                || b.drag.outlines(&Spot::Pane(pane))
        })
    };
    let (docs, deep, inner, zeta) = (
        row(&browser, 0, cx),
        row(&browser, 1, cx),
        row(&browser, 2, cx),
        row(&browser, 3, cx),
    );
    // Onto itself, onto what it holds, and a file inside it counts as its folder.
    carry(docs, docs + point(px(0.), px(2.)), "", cx);
    let refused = "\"docs\" can't go inside itself.";
    assert_eq!(says(&browser, cx).unwrap(), refused);
    assert!(!outlined_any(cx));
    hover(deep, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), refused);
    assert!(!outlined_any(cx));
    hover(inner, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), refused);
    // ⌥ doesn't make it a copy into itself either, and nothing happens.
    cx.simulate_mouse_up(inner, MouseButton::Left, with("alt"));
    assert_eq!(shown(&browser, cx).0.len(), 5);
    assert!(project.join("docs/inner.md").exists());
    // The open folder's empty space is where a row of it already is.
    let below = blank(&browser, 8, cx);
    carry(zeta, below, "", cx);
    let name = project.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(says(&browser, cx).unwrap(), format!("Already in {name}"));
    assert!(!outlined_any(cx));
    // A tree row is a folder like any other.
    hover(deep, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Move zeta to deep");
    // Over nothing that takes files, there's no pill.
    hover(point(px(4.), px(4.)), "", cx);
    assert_eq!(says(&browser, cx), None);
    press(cx, "escape");
    cx.simulate_mouse_up(point(px(4.), px(4.)), MouseButton::Left, with(""));
}

#[gpui::test]
fn esc_cancels_and_the_mouse_coming_up_afterwards_does_nothing(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let (docs, file) = (row(&browser, 0, cx), row(&browser, 2, cx));
    cx.simulate_click(file, with(""));
    carry(file, docs, "", cx);
    assert!(dragging(&browser, cx));
    press(cx, "escape");
    draw(cx);
    assert!(!dragging(&browser, cx));
    assert_eq!(says(&browser, cx), None);
    cx.simulate_mouse_up(docs, MouseButton::Left, with(""));
    assert!(project.join("a.txt").exists());
    // Esc went to the drag, and the selection stayed.
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
}

#[gpui::test]
fn a_drop_on_the_other_panes_space_and_on_a_crumb_goes_to_that_folder(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    // The right pane in docs, the keys back on the left's a.txt.
    press(cx, "cmd-alt-s alt-up cmd-down shift-tab");
    draw(cx);
    let (left, right) = browser.read_with(cx, |b, _| {
        let tab = &b.tabs[b.active];
        (tab.pane.clone(), tab.second.clone().unwrap())
    });
    let row_of_left = |ix: f32, cx: &mut VisualTestContext| {
        left.read_with(cx, |pane, cx| {
            let height = cx.global::<Preferences>().row_size.metrics().0;
            pane.list_bounds.get().origin + point(px(60.), px(4. + height * (ix + 0.5)))
        })
    };
    let space = right.read_with(cx, |pane, _| {
        pane.list_bounds.get().origin + point(px(60.), px(300.))
    });
    let file = row_of_left(2., cx);
    carry(file, space, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
    let outlined = browser.read_with(cx, |b, _| b.drag.outlines(&Spot::Pane(right.entity_id())));
    assert!(outlined);
    cx.simulate_mouse_up(space, MouseButton::Left, with(""));
    finish_job(&mut owned, cx);
    assert!(project.join("docs/a.txt").exists());

    // Back up with a crumb, one level above the folder the left pane shows.
    let crumb = project.components().count() - 2;
    let at = cx
        .debug_bounds(format!("crumb-{crumb}").leak())
        .unwrap()
        .center();
    let zeta = row_of_left(1., cx);
    carry(zeta, at, "", cx);
    let parent = project.parent().unwrap();
    let name = parent.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(says(&browser, cx).unwrap(), format!("Move zeta to {name}"));
    cx.simulate_mouse_up(at, MouseButton::Left, with(""));
    finish_job(&mut owned, cx);
    assert!(parent.join("zeta").exists() && !project.join("zeta").exists());
    release(owned, cx);
}

#[gpui::test]
fn a_drop_on_a_tab_goes_to_its_folder_without_switching_to_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-t down cmd-down");
    assert_eq!(shown(&browser, cx).0, ["inner.md"]);
    draw(cx);
    let tab = cx.debug_bounds("tab-0").unwrap().center();
    let inner = row(&browser, 0, cx);
    carry(inner, tab, "", cx);
    let name = project.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        says(&browser, cx).unwrap(),
        format!("Move inner.md to {name}")
    );
    assert!(browser.read_with(cx, |b, _| b.drag.outlines(&Spot::Tab(0))));
    assert_eq!(browser.read_with(cx, |b, _| b.active), 1);
    cx.simulate_mouse_up(tab, MouseButton::Left, with(""));
    finish_job(&mut owned, cx);
    assert!(project.join("inner.md").exists());
    assert_eq!(browser.read_with(cx, |b, _| b.active), 1);
    release(owned, cx);
}

#[gpui::test]
fn in_the_grid_a_cells_icon_drags_and_a_folder_cell_takes_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "cmd-2");
    draw(cx);
    let cell = |ix: usize, cx: &mut VisualTestContext| {
        in_pane(&browser, cx, |pane, _| {
            assert_eq!(pane.view, View::Grid);
            let height = grid_cell(pane.grid_icon).1;
            pane.list_bounds.get().origin
                + point(
                    px(8. + pane.cell_width * (ix as f32 + 0.5)),
                    px(4. + height * 0.4),
                )
        })
    };
    let (docs, file) = (cell(0, cx), cell(2, cx));
    carry(file, docs, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
    cx.simulate_mouse_up(docs, MouseButton::Left, with(""));
    finish_job(&mut owned, cx);
    assert!(project.join("docs/a.txt").exists());
    release(owned, cx);
}

#[gpui::test]
fn files_from_another_app_arrive_through_the_same_rules_on_release(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let elsewhere = tempfile::tempdir().unwrap();
    let given = elsewhere.path().join("given.txt");
    std::fs::write(&given, "").unwrap();
    let mut owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    let docs = row(&browser, 0, cx);
    cx.simulate_event(FileDropEvent::Entered {
        position: docs,
        paths: ExternalPaths([given.clone()].into_iter().collect()),
    });
    draw(cx);
    cx.simulate_event(FileDropEvent::Pending { position: docs });
    draw(cx);
    assert_eq!(says(&browser, cx).unwrap(), "Move given.txt to docs");
    assert!(given.exists());
    // Leaving the window ends it, and coming back begins it again.
    cx.simulate_event(FileDropEvent::Exited);
    draw(cx);
    assert!(!dragging(&browser, cx));
    cx.simulate_event(FileDropEvent::Entered {
        position: docs,
        paths: ExternalPaths([given.clone()].into_iter().collect()),
    });
    draw(cx);
    cx.simulate_event(FileDropEvent::Pending { position: docs });
    draw(cx);
    cx.simulate_event(FileDropEvent::Submit { position: docs });
    finish_job(&mut owned, cx);
    assert!(!given.exists() && project.join("docs/given.txt").exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Moved \"given.txt\" to docs".into(), false, true))
    );
    release(owned, cx);
}

#[gpui::test]
fn what_leaves_the_window_is_the_selection_as_paths_with_their_kinds(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let (docs, file) = (row(&browser, 0, cx), row(&browser, 2, cx));
    cx.simulate_click(docs, with(""));
    cx.simulate_click(file, with("cmd"));
    carry(file, docs + point(px(0.), px(300.)), "", cx);
    let payload = in_pane(&browser, cx, |pane, _| pane.drag_out()).unwrap();
    let ExternalDragPayload::Files(files) = payload;
    assert_eq!(
        files.entries(),
        [(project.join("docs"), true), (project.join("a.txt"), false)]
    );
    press(cx, "escape");
}
