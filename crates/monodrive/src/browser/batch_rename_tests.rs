//! Rename in the list through the real `Browser`, on temp folders with an
//! owner of their own. The rule's text goes straight into its fields: a
//! printable key that missed them would start a search, and so the index.

use super::batch_rename::BatchRename;
use super::tests::{browse, click, in_pane, on_pane, own, press, release, shown, toast};
use super::*;
use crate::preferences::View;
use crate::text_field::TextField;
use gpui::{TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};

/// `trip/` holding `IMG_a.jpg`, `IMG_b.jpg` and `IMG_c.jpg`.
fn trip() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let trip = dir.path().join("trip");
    std::fs::create_dir(&trip).unwrap();
    for name in ["IMG_a.jpg", "IMG_b.jpg", "IMG_c.jpg"] {
        std::fs::write(trip.join(name), name).unwrap();
    }
    (dir, trip)
}

fn on_disk(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Replaces what `field` holds, as typing would.
fn fill(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
    field: fn(&BatchRename) -> &Entity<TextField>,
    text: &str,
) {
    let field = in_pane(browser, cx, |pane, _| {
        field(pane.batch.as_ref().unwrap()).clone()
    });
    field.update(cx, |field, cx| {
        field.set_text(text, text.len()..text.len(), cx)
    });
    cx.run_until_parked();
}

/// The name each selected row shows, and its note.
fn rows(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Vec<(String, Option<String>)> {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    in_pane(browser, cx, |pane, _| {
        let batch = pane.batch.as_ref().unwrap();
        pane.selection
            .in_order(&pane.entries)
            .iter()
            .map(|entry| {
                let row = batch.row(&entry.path, &entry.name, pane.folder()).unwrap();
                (row.name, row.note)
            })
            .collect()
    })
}

fn names(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Vec<String> {
    let rows = rows(browser, cx);
    rows.into_iter().map(|(name, _)| name).collect()
}

fn in_bar(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    in_pane(browser, cx, |pane, _| pane.batch.is_some())
}

/// The way in is Rename, Esc and Cancel leave, and nothing reaches the disk
/// before Apply. A grid shows the bar's rows as a list until then.
#[gpui::test]
fn rename_with_several_selected_previews_and_esc_or_cancel_change_nothing(cx: &mut TestAppContext) {
    let (_dir, trip) = trip();
    let before = on_disk(&trip);
    let owned = own(cx);
    let (browser, cx) = browse(&trip, cx);
    cx.dispatch_action(ViewAsGrid);
    cx.run_until_parked();
    press(cx, "cmd-a enter");
    assert!(in_bar(&browser, cx));
    in_pane(&browser, cx, |pane, _| {
        assert_eq!(pane.layout(), View::List)
    });
    fill(&browser, cx, |b| &b.find, "IMG");
    fill(&browser, cx, |b| &b.with, "Trip");
    assert_eq!(
        rows(&browser, cx),
        [
            ("Trip_a.jpg".to_string(), Some("IMG_a.jpg".to_string())),
            ("Trip_b.jpg".to_string(), Some("IMG_b.jpg".to_string())),
            ("Trip_c.jpg".to_string(), Some("IMG_c.jpg".to_string())),
        ]
    );
    assert_eq!(on_disk(&trip), before);

    // Esc from a field, and the list has the keys and its grid again.
    press(cx, "escape");
    assert!(!in_bar(&browser, cx));
    in_pane(&browser, cx, |pane, _| {
        assert_eq!(pane.layout(), View::Grid)
    });
    assert_eq!(shown(&browser, cx).1.len(), 3, "the selection stays");
    press(cx, "enter");
    assert!(in_bar(&browser, cx));
    click(cx, "batch-cancel");
    assert!(!in_bar(&browser, cx));

    // With the keys on the list, Esc leaves the bar before the selection.
    press(cx, "enter");
    cx.update(|window, cx| {
        let list = in_list(&browser, cx);
        window.focus(&list, cx);
    });
    cx.run_until_parked();
    press(cx, "escape");
    assert!(!in_bar(&browser, cx));
    assert_eq!(shown(&browser, cx).1.len(), 3);
    assert_eq!(on_disk(&trip), before);
    release(owned, cx);
}

fn in_list(browser: &Entity<Browser>, cx: &App) -> FocusHandle {
    browser.read(cx).pane().read(cx).list_focus.clone()
}

/// Apply renames as one operation the toast and ⌘Z can undo, and the
/// selection follows the new names.
#[gpui::test]
fn apply_numbers_in_the_order_shown_and_undo_puts_every_name_back(cx: &mut TestAppContext) {
    let (_dir, trip) = trip();
    let before = on_disk(&trip);
    let owned = own(cx);
    let (browser, cx) = browse(&trip, cx);
    press(cx, "cmd-a enter");
    click(cx, "batch-number");
    fill(&browser, cx, |b| &b.name, "Trip");
    fill(&browser, cx, |b| &b.start, "007");
    assert_eq!(
        names(&browser, cx),
        ["Trip 007.jpg", "Trip 008.jpg", "Trip 009.jpg"]
    );
    // After the press, ↩ in the field is what applies.
    press(cx, "enter");
    assert!(!in_bar(&browser, cx));
    assert_eq!(
        on_disk(&trip),
        ["Trip 007.jpg", "Trip 008.jpg", "Trip 009.jpg"]
    );
    assert_eq!(
        std::fs::read_to_string(trip.join("Trip 007.jpg")).unwrap(),
        "IMG_a.jpg"
    );
    assert_eq!(
        toast(&browser, cx),
        Some(("Renamed 3 items".into(), false, true))
    );
    assert_eq!(shown(&browser, cx).1.len(), 3, "the selection follows");
    press(cx, "cmd-z");
    assert_eq!(on_disk(&trip), before);
    release(owned, cx);
}

/// Numbers follow the rows: sorting or changing the selection makes a new
/// preview, and a rule typed while one runs replaces what it finds.
#[gpui::test]
fn a_new_order_a_new_selection_or_a_new_rule_replaces_the_preview(cx: &mut TestAppContext) {
    let (_dir, trip) = trip();
    let (browser, cx) = browse(&trip, cx);
    press(cx, "cmd-a enter");
    click(cx, "batch-number");
    fill(&browser, cx, |b| &b.name, "n");
    assert_eq!(names(&browser, cx), ["n 1.jpg", "n 2.jpg", "n 3.jpg"]);
    // Reversed, what was last counts first.
    cx.dispatch_action(ReverseSort);
    cx.run_until_parked();
    let (shown_rows, _) = shown(&browser, cx);
    assert_eq!(shown_rows[0], "IMG_c.jpg");
    assert_eq!(names(&browser, cx), ["n 1.jpg", "n 2.jpg", "n 3.jpg"]);
    let c = in_pane(&browser, cx, |pane, _| {
        pane.batch
            .as_ref()
            .unwrap()
            .row(&trip.join("IMG_c.jpg"), "IMG_c.jpg", pane.folder())
            .unwrap()
            .name
    });
    assert_eq!(c, "n 1.jpg");

    // Two rules with no time between them: only the last shows.
    let name = in_pane(&browser, cx, |pane, _| {
        pane.batch.as_ref().unwrap().name.clone()
    });
    name.update(cx, |field, cx| field.insert("x", cx));
    name.update(cx, |field, cx| field.insert("y", cx));
    assert_eq!(names(&browser, cx)[0], "nxy 1.jpg");

    // A row let go of shows its name again and the rest count again.
    let c_entry = in_pane(&browser, cx, |pane, _| pane.entries[0].clone());
    on_pane(&browser, cx, |pane, cx| {
        pane.selection.toggle(&c_entry);
        cx.notify();
    });
    assert_eq!(names(&browser, cx), ["nxy 1.jpg", "nxy 2.jpg"]);
    let b = in_pane(&browser, cx, |pane, _| {
        pane.batch.as_ref().unwrap().paths[0].clone()
    });
    assert!(b.ends_with("IMG_b.jpg"));

    // Going elsewhere leaves the bar.
    press(cx, "cmd-up");
    assert!(!in_bar(&browser, cx));
}

/// A problem shows on its row, ↩ refuses it, and only the button that says
/// it skips renames the rest.
#[gpui::test]
fn a_collision_shows_and_only_the_skip_button_runs_past_it(cx: &mut TestAppContext) {
    let (_dir, trip) = trip();
    std::fs::write(trip.join("Taken.jpg"), "").unwrap();
    let owned = own(cx);
    let (browser, cx) = browse(&trip, cx);
    press(cx, "down shift-down");
    assert_eq!(shown(&browser, cx).1, ["IMG_a.jpg", "IMG_b.jpg"]);
    press(cx, "enter");
    fill(&browser, cx, |b| &b.find, "IMG_a");
    fill(&browser, cx, |b| &b.with, "Taken");
    assert_eq!(
        rows(&browser, cx)[0],
        (
            "Taken.jpg".to_string(),
            Some("Taken.jpg already exists.".to_string())
        )
    );
    press(cx, "enter");
    assert!(in_bar(&browser, cx));
    assert!(trip.join("IMG_a.jpg").exists());
    // Nothing else gets a new name, so no button can run past it.
    assert_eq!(cx.debug_bounds("batch-apply"), None);

    // The whole extension is matched, so both get one, and one collides.
    click(cx, "batch-whole-name");
    fill(&browser, cx, |b| &b.find, "IMG_");
    fill(&browser, cx, |b| &b.with, "Taken");
    assert_eq!(names(&browser, cx), ["Takena.jpg", "Takenb.jpg"]);
    std::fs::write(trip.join("Takena.jpg"), "").unwrap();
    fill(&browser, cx, |b| &b.with, "Taken");
    assert_eq!(
        rows(&browser, cx)[0].1.as_deref(),
        Some("Takena.jpg already exists.")
    );
    press(cx, "enter");
    assert!(in_bar(&browser, cx), "↩ refuses");
    assert!(trip.join("IMG_a.jpg").exists() && trip.join("IMG_b.jpg").exists());
    // Tab reaches it from Replace With, past the options and Cancel, and ↩
    // on it is the button's: the keys that a person without a mouse has.
    press(cx, "tab tab tab tab tab");
    press(cx, "enter");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    cx.run_until_parked();
    assert!(!in_bar(&browser, cx));
    assert!(trip.join("IMG_a.jpg").exists(), "the problem row stays");
    assert!(trip.join("Takenb.jpg").exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Renamed \"IMG_b.jpg\"".into(), false, true))
    );
    release(owned, cx);
}

/// The disk changing after the preview refuses Apply, which says so and
/// shows the rows as they now are.
#[gpui::test]
fn an_out_of_date_preview_renames_nothing_and_stays_open(cx: &mut TestAppContext) {
    let (_dir, trip) = trip();
    let owned = own(cx);
    let (browser, cx) = browse(&trip, cx);
    press(cx, "cmd-a enter");
    fill(&browser, cx, |b| &b.find, "IMG");
    fill(&browser, cx, |b| &b.with, "New");
    assert_eq!(names(&browser, cx)[0], "New_a.jpg");
    std::fs::write(trip.join("New_b.jpg"), "taken since").unwrap();
    press(cx, "enter");
    assert!(in_bar(&browser, cx));
    let why = in_pane(&browser, cx, |pane, _| {
        pane.batch.as_ref().unwrap().error.clone()
    });
    assert_eq!(
        why.as_deref(),
        Some("Nothing was renamed, since the preview is out of date.")
    );
    assert!(trip.join("IMG_a.jpg").exists() && trip.join("IMG_b.jpg").exists());
    assert_eq!(
        rows(&browser, cx)[1].1.as_deref(),
        Some("New_b.jpg already exists.")
    );

    // Skipping is the button's.
    click(cx, "batch-apply");
    assert!(!in_bar(&browser, cx));
    assert!(trip.join("New_a.jpg").exists() && trip.join("IMG_b.jpg").exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Renamed 2 items".into(), false, true))
    );
    press(cx, "cmd-z");
    assert!(trip.join("IMG_a.jpg").exists() && trip.join("IMG_c.jpg").exists());
    release(owned, cx);
}
