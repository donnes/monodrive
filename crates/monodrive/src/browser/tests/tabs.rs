use super::*;
use crate::browser::*;

/// The tab count and which is on screen.
fn tabs(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> (usize, usize) {
    browser.read_with(cx, |b, _| (b.tabs.len(), b.active))
}

#[gpui::test]
fn show_in_finder_selects_what_it_names_and_opens_folders_in_new_tabs(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let docs = project.join("docs");
    let (browser, cx) = browse(&docs, cx);
    let land = |folder: &Path, select: &[&str], new_tab: bool, cx: &mut VisualTestContext| {
        let to = crate::finder::Move {
            folder: folder.to_path_buf(),
            select: select.iter().map(|name| project.join(name)).collect(),
            new_tab,
        };
        browser.update_in(cx, |browser, window, cx| browser.land(to, window, cx));
        cx.run_until_parked();
    };
    // Files elsewhere: their folder, in this tab, all of them selected.
    land(&project, &["a.txt", "zeta"], false, cx);
    assert_eq!(
        (location(&browser, cx), tabs(&browser, cx)),
        (project.clone(), (1, 0))
    );
    assert_eq!(shown(&browser, cx).1, ["zeta", "a.txt"]);
    // Asked again where it already is, the selection changes in place.
    land(&project, &["docs", "a.txt"], false, cx);
    assert_eq!(shown(&browser, cx).1, ["docs", "a.txt"]);
    // A folder opens in a new tab, and the one left keeps its place.
    land(&docs, &[], true, cx);
    assert_eq!((location(&browser, cx), tabs(&browser, cx)), (docs, (2, 1)));
    press(cx, "ctrl-shift-tab");
    assert_eq!(location(&browser, cx), project);
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
    press(cx, "alt-down cmd-alt-2 ctrl-alt-cmd-3");
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
    // The slide runs on the clock; Reduce Motion skips it to where it ends.
    cx.update(|_, cx| cx.set_reduce_motion(true));
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
    // Its button holds still, clear of the traffic lights.
    let in_bar = on_screen("toggle-sidebar", cx).unwrap();
    assert!(in_bar.left() > px(crate::traffic_lights::REACH));
    assert_eq!(in_bar, in_sidebar);
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
fn held(thumb: &Thumb, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| {
        cx.default_global::<Holders>()
            .held
            .lock()
            .unwrap()
            .contains_key(thumb)
    })
}

fn drawn_thumbs(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Vec<Thumb> {
    press(cx, "cmd-alt-2");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    in_pane(browser, cx, |pane, _| pane.thumbs.keys().cloned().collect())
}

#[gpui::test]
fn closing_a_tab_or_a_pane_lets_go_of_its_thumbnails(cx: &mut TestAppContext) {
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
    assert!(held(&thumbs[0], cx));
    press(cx, "cmd-w");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!held(&thumbs[0], cx));
    // The same for one of two panes.
    press(cx, "cmd-alt-s");
    on_pane(&browser, cx, |pane, cx| pane.navigate(two.clone(), cx));
    cx.run_until_parked();
    let thumbs = drawn_thumbs(&browser, cx);
    assert!(held(&thumbs[0], cx));
    press(cx, "cmd-alt-s");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!held(&thumbs[0], cx));
}

/// Coming back draws the decoded picture from memory instead of the icon, then
/// the same picture decoded again.
#[gpui::test]
fn a_thumbnail_survives_leaving_its_folder_and_opening_a_preview(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (empty, pictures) = (dir.path().join("empty"), dir.path().join("pictures"));
    std::fs::create_dir(&empty).unwrap();
    std::fs::create_dir(&pictures).unwrap();
    image::RgbaImage::new(40, 40)
        .save(pictures.join("picture.png"))
        .unwrap();
    let (browser, cx) = browse(&pictures, cx);
    let thumbs = drawn_thumbs(&browser, cx);
    let image = |cx: &mut VisualTestContext| {
        in_pane(&browser, cx, |pane, _| {
            pane.thumbs[&thumbs[0]].clone().unwrap()
        })
    };
    let before = image(cx);

    on_pane(&browser, cx, |pane, cx| pane.navigate(empty.clone(), cx));
    cx.run_until_parked();
    assert!(drawn_thumbs(&browser, cx).is_empty());
    assert!(!held(&thumbs[0], cx));
    on_pane(&browser, cx, |pane, cx| pane.navigate(pictures.clone(), cx));
    cx.run_until_parked();
    assert_eq!(drawn_thumbs(&browser, cx), thumbs);
    assert!(Arc::ptr_eq(&before, &image(cx)));

    press(cx, "alt-up space");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!held(&thumbs[0], cx));
    press(cx, "escape");
    assert_eq!(drawn_thumbs(&browser, cx), thumbs);
    assert!(Arc::ptr_eq(&before, &image(cx)));
}

#[gpui::test]
fn a_thumbnail_is_held_until_the_last_pane_showing_it_lets_go(cx: &mut TestAppContext) {
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
    assert!(held(&thumbs[0], cx));
    assert!(Arc::ptr_eq(&before, &image(cx)));
    // The last one out lets go of it.
    press(cx, "cmd-w");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!held(&thumbs[0], cx));
}
