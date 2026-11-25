use super::*;
use crate::browser::*;

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
    std::fs::write(project.join("b.txt"), "").unwrap();
    std::fs::write(project.join("c.txt"), "").unwrap();
    let (browser, cx) = browse(&project, cx);
    let one = |name: &str| Some((vec![name.to_string()], 0));
    press(cx, "space");
    assert_eq!(previewed(&browser, cx), None);
    // A folder has no preview.
    press(cx, "down space");
    assert_eq!(shown(&browser, cx).1, ["docs"]);
    assert_eq!(previewed(&browser, cx), None);
    press(cx, "down down space");
    assert_eq!(previewed(&browser, cx), one("a.txt"));
    // The arrows still move the selection, and the panel follows; it
    // pages through several from the cursor.
    press(cx, "down");
    assert_eq!(previewed(&browser, cx), one("b.txt"));
    press(cx, "shift-down");
    assert_eq!(
        previewed(&browser, cx),
        Some((vec!["b.txt".into(), "c.txt".into()], 1))
    );
    // Space again leaves it open, as it plays and pauses there; Esc closes
    // it and keeps the selection. ⌘Y opens it too.
    press(cx, "space");
    assert!(previewed(&browser, cx).is_some());
    press(cx, "escape");
    assert_eq!(previewed(&browser, cx), None);
    assert_eq!(shown(&browser, cx).1, ["b.txt", "c.txt"]);
    press(cx, "cmd-y");
    assert!(previewed(&browser, cx).is_some());
    press(cx, "escape");
    assert_eq!(previewed(&browser, cx), None);
    // The arrows moving onto a folder close it.
    press(cx, "alt-up down down space up");
    assert_eq!(shown(&browser, cx).1, ["zeta"]);
    assert_eq!(previewed(&browser, cx), None);
    // Going into a folder selects nothing, and going back selects the folder
    // left, so either way it closes.
    press(cx, "alt-up cmd-down");
    assert_eq!(shown(&browser, cx), (vec!["inner.md".into()], vec![]));
    press(cx, "down space");
    assert_eq!(previewed(&browser, cx), one("inner.md"));
    press(cx, "cmd-[");
    assert_eq!(previewed(&browser, cx), None);
    press(cx, "down down space");
    assert_eq!(previewed(&browser, cx), one("a.txt"));
    // Space types in the search field, and the panel stays as it was.
    // A lone space searches for nothing, so the index never starts.
    press(cx, "cmd-f space");
    let typed = in_pane(&browser, cx, |b, cx| b.search.read(cx).text().to_string());
    assert_eq!(typed, " ");
    assert_eq!(previewed(&browser, cx), one("a.txt"));
    press(cx, "escape escape");
    assert_eq!(previewed(&browser, cx), None);
    // It types in a name being edited too.
    press(cx, "alt-down enter space");
    assert_eq!(editing(&browser, cx), Some((" .txt".into(), None)));
    assert_eq!(previewed(&browser, cx), None);
}

/// What the pane's preview is showing: its selector on screen.
fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector).is_some()
}

#[gpui::test]
fn the_preview_takes_the_panes_list_and_gives_it_back(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    std::fs::write(project.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(project.join("b.md"), "# Title\n\nBody.\n").unwrap();
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down");
    assert_eq!(shown(&browser, cx).1, ["b.md"]);
    press(cx, "space");
    assert!(drawn(cx, "preview-markdown"));
    assert!(cx.debug_bounds("preview-text").is_none());
    // The keys stayed on the list, so the arrows move what it shows.
    press(cx, "up");
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    assert!(drawn(cx, "preview-text"));
    // A click in it isn't a click on the list, which would let go of the file.
    let at = cx.debug_bounds("preview-text").unwrap().center();
    cx.simulate_click(at, with(""));
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    press(cx, "escape");
    assert!(!drawn(cx, "preview"));
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    // A folder has no preview.
    press(cx, "alt-up space");
    assert!(!drawn(cx, "preview"));
}

#[gpui::test]
fn text_dragged_over_or_selected_all_in_the_preview_copies(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    std::fs::write(project.join("a.txt"), "one\ntwo\n").unwrap();
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down space");
    assert!(drawn(cx, "preview-text"));
    let copied = |cx: &mut VisualTestContext| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    };
    press(cx, "cmd-a cmd-c");
    assert_eq!(copied(cx), "one\ntwo");
    // The keys stayed on the list, which still has its one file selected.
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    // A drag from the start of the first line to past the end of the second.
    // Rows start below the list's 12 pt of padding.
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new()));
    let text = cx.debug_bounds("preview-text").unwrap();
    let row = |n: f32| text.top() + px(12. + (theme::ROW_HEIGHT - 8.) * (n + 0.5));
    let from = gpui::point(text.left() + px(1.), row(0.));
    let to = gpui::point(text.right() - px(1.), row(1.));
    cx.simulate_mouse_down(from, MouseButton::Left, with(""));
    cx.simulate_mouse_move(to, Some(MouseButton::Left), with(""));
    cx.simulate_mouse_up(to, MouseButton::Left, with(""));
    press(cx, "cmd-c");
    assert_eq!(copied(cx), "one\ntwo");
}

#[gpui::test]
fn an_env_file_opens_with_its_values_blurred(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    std::fs::write(project.join("y.env"), "KEY=secret\n").unwrap();
    std::fs::write(project.join("z.env"), "KEY=secret\n").unwrap();
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down");
    assert_eq!(shown(&browser, cx).1, ["z.env"]);
    press(cx, "space");
    assert!(drawn(cx, "preview-secret"));
    let reveal = cx.debug_bounds("preview-reveal").unwrap().center();
    cx.simulate_click(reveal, with(""));
    assert!(!drawn(cx, "preview-secret"));
    // The next file is blurred again.
    press(cx, "up");
    assert_eq!(shown(&browser, cx).1, ["y.env"]);
    assert!(drawn(cx, "preview-secret"));
}

/// A preview's real path: the keys and mouse through the real `Browser`, a real
/// recipe flattened and saved through a real owner, and one Undo.
#[gpui::test]
fn a_mark_drawn_in_the_preview_saves_a_copy_that_undo_removes(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let photo = project.join("zz.png");
    image::RgbImage::from_pixel(200, 100, image::Rgb([255, 255, 255]))
        .save(&photo)
        .unwrap();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down");
    assert_eq!(shown(&browser, cx).1, ["zz.png"]);
    press(cx, "space e");
    assert!(drawn(cx, "preview-tools"));
    click(cx, "ink-blue");
    click(cx, "tool-redact");
    assert!(drawn(cx, "preview-edit-surface"));
    // Nothing's changed yet, so there's no Save to press.
    assert!(cx.debug_bounds("edit-save").is_none());

    let surface = cx.debug_bounds("preview-edit-surface").unwrap();
    let start = surface.center() - gpui::point(px(40.), px(20.));
    let end = surface.center() + gpui::point(px(40.), px(20.));
    cx.simulate_mouse_down(start, MouseButton::Left, with(""));
    cx.simulate_mouse_move(end, MouseButton::Left, with(""));
    cx.simulate_mouse_up(end, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert!(drawn(cx, "edit-save"));

    // The first Esc lets go of the mark, the next asks, and Keep Editing keeps it.
    press(cx, "escape");
    assert!(!drawn(cx, "edit-discard"));
    press(cx, "escape");
    assert!(drawn(cx, "edit-discard"));
    click(cx, "edit-keep");
    assert!(drawn(cx, "edit-save"));

    let copy = project.join("zz edited.png");
    click(cx, "edit-save");
    cx.run_until_parked();
    assert!(!drawn(cx, "preview-tools"), "saving leaves the tools");
    assert_eq!(
        toast(&browser, cx).map(|t| t.0),
        Some("Saved \"zz edited.png\"".to_string())
    );
    let saved = image::open(&copy).unwrap().into_rgb8();
    assert_eq!(saved.dimensions(), (200, 100));
    assert_eq!(
        saved.get_pixel(100, 50).0,
        [0, 0, 0],
        "the redact burned in"
    );
    assert_eq!(saved.get_pixel(2, 2).0, [255, 255, 255]);
    let original = image::open(&photo).unwrap().into_rgb8();
    assert_eq!(
        original.get_pixel(100, 50).0,
        [255, 255, 255],
        "the original stays"
    );

    press(cx, "cmd-z");
    assert!(!copy.exists());
    press(cx, "escape");
    release(owned, cx);
}

/// The menus sit over the row's neighbors: a press outside only closes one, and
/// the first Esc is theirs.
#[gpui::test]
fn a_press_or_esc_closes_the_save_menu_and_nothing_else(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    image::RgbImage::from_pixel(50, 50, image::Rgb([255, 255, 255]))
        .save(project.join("zz.png"))
        .unwrap();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down space e");
    click(cx, "tool-pen");
    assert!(drawn(cx, "preview-edit-surface"));
    let center = cx.debug_bounds("preview-edit-surface").unwrap().center();
    // A stroke in the corner, far from the press below, so Save can open.
    let corner = center - gpui::point(px(20.), px(20.));
    cx.simulate_mouse_down(corner, MouseButton::Left, with(""));
    cx.simulate_mouse_up(corner, MouseButton::Left, with(""));
    cx.run_until_parked();
    click(cx, "edit-more");
    assert!(drawn(cx, "edit-replace"));
    cx.simulate_mouse_down(center, MouseButton::Left, with(""));
    cx.simulate_mouse_up(center, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert!(!drawn(cx, "edit-replace"));
    click(cx, "edit-more");
    press(cx, "escape");
    assert!(!drawn(cx, "edit-replace"));
    assert!(!drawn(cx, "edit-discard"));
    press(cx, "escape");
    assert!(drawn(cx, "edit-discard"));
    assert_eq!(shown(&browser, cx).1, ["zz.png"]);
    click(cx, "edit-keep");
    click(cx, "edit-save");
    cx.run_until_parked();
    let saved = image::open(project.join("zz edited.png"))
        .unwrap()
        .into_rgb8();
    assert_ne!(
        saved.get_pixel(5, 5).0,
        [255, 255, 255],
        "the stroke is there"
    );
    assert_eq!(
        saved.get_pixel(25, 25).0,
        [255, 255, 255],
        "the press left no mark"
    );
    release(owned, cx);
}

#[gpui::test]
fn a_video_edited_in_the_preview_saves_and_replaces_through_the_owner(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let clip = project.join("zz.mp4");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../monodrive-core/tests/fixtures/rotated.mp4");
    std::fs::copy(fixture, &clip).unwrap();
    let before = std::fs::read(&clip).unwrap();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down");
    assert_eq!(shown(&browser, cx).1, ["zz.mp4"]);
    press(cx, "space e");
    assert!(drawn(cx, "tool-video-rotate"));
    // Stored to play 36 by 64; a turn comes out 64 by 36.
    click(cx, "tool-video-rotate");
    click(cx, "edit-save");
    cx.run_until_parked();
    let copy = project.join("zz edited.mp4");
    assert_eq!(
        monodrive_core::edits::video_size(&copy).unwrap(),
        (64., 36.)
    );
    assert_eq!(std::fs::read(&clip).unwrap(), before, "the original stays");
    assert!(!drawn(cx, "preview-tools"), "saving leaves the tools");
    press(cx, "cmd-z");
    assert!(!copy.exists());

    press(cx, "e");
    click(cx, "tool-video-mute");
    click(cx, "edit-more");
    click(cx, "edit-replace");
    cx.run_until_parked();
    assert_ne!(std::fs::read(&clip).unwrap(), before);
    assert_eq!(
        monodrive_core::edits::video_size(&clip).unwrap(),
        (36., 64.)
    );
    press(cx, "cmd-z");
    assert_eq!(std::fs::read(&clip).unwrap(), before, "Undo brings it back");
    press(cx, "escape");
    release(owned, cx);
}

#[gpui::test]
fn a_save_the_owner_refuses_keeps_the_edit_and_says_why(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let photo = project.join("zz.png");
    image::RgbImage::new(50, 50).save(&photo).unwrap();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down space e");
    click(cx, "tool-rotate");
    assert!(drawn(cx, "edit-save"));
    // The file changes underneath, as it opened the moment the tools did.
    image::RgbImage::new(60, 60).save(&photo).unwrap();
    click(cx, "edit-save");
    cx.run_until_parked();
    assert!(drawn(cx, "edit-save"), "the edit is still there to save");
    assert!(drawn(cx, "edit-status"), "the reason shows under the row");
    assert!(!project.join("zz edited.png").exists());
    let hidden: Vec<_> = std::fs::read_dir(&project)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".monodrive-edit-")
        })
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");
    assert_eq!(toast(&browser, cx), None);
    release(owned, cx);
}

/// A save that starts and can't finish comes back as an outcome holding the
/// failure, not as a refusal. A locked original can't go to the Trash.
#[gpui::test]
fn words_typed_with_the_text_tool_land_where_it_was_clicked(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let photo = project.join("zz.png");
    image::RgbImage::from_pixel(600, 300, image::Rgb([255, 255, 255]))
        .save(&photo)
        .unwrap();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down space e");
    click(cx, "ink-black");
    click(cx, "tool-text");
    assert!(drawn(cx, "preview-edit-surface"));
    // The picture is centered and not enlarged, so this is 100 by 50 inside it.
    let center = cx.debug_bounds("preview-edit-surface").unwrap().center();
    let at = center - gpui::point(px(200.), px(100.));
    cx.simulate_mouse_down(at, MouseButton::Left, with(""));
    cx.simulate_mouse_up(at, MouseButton::Left, with(""));
    // Letters that mean something to the list, and Space, are only words here,
    // and Shift-Return starts another line. The card holds the keys.
    press(cx, "space e space e shift-enter e enter");
    assert!(!drawn(cx, "note-card"));
    assert!(drawn(cx, "edit-save"));
    click(cx, "edit-save");
    cx.run_until_parked();
    let saved = image::open(project.join("zz edited.png"))
        .unwrap()
        .into_rgb8();
    let dark =
        |x0, x1, y0, y1| (x0..x1).any(|x| (y0..y1).any(|y| saved.get_pixel(x, y).0[0] < 128));
    // 20 pt type, on two lines.
    assert!(dark(100, 300, 50, 75), "the words are where the click was");
    assert!(
        dark(100, 300, 75, 100),
        "with the second line under the first"
    );
    assert!(!dark(0, 90, 0, 300), "and nowhere left of it");
    assert!(!dark(100, 600, 150, 300), "or below");
    let _ = browser;
    release(owned, cx);
}

/// A white 600 by 300 picture in Edit mode, shown at its own size, with its pixel
/// (0, 0) returned for turning picture pixels into window points.
fn marking(
    project: &std::path::Path,
    cx: &mut gpui::VisualTestContext,
) -> gpui::Point<gpui::Pixels> {
    press(cx, "alt-down space e");
    click(cx, "ink-black");
    assert!(drawn(cx, "preview-edit-surface"));
    let center = cx.debug_bounds("preview-edit-surface").unwrap().center();
    let _ = project;
    center - gpui::point(px(300.), px(150.))
}

fn white_picture(project: &std::path::Path) {
    image::RgbImage::from_pixel(600, 300, image::Rgb([255, 255, 255]))
        .save(project.join("zz.png"))
        .unwrap();
}

fn drag_in(
    cx: &mut gpui::VisualTestContext,
    origin: gpui::Point<gpui::Pixels>,
    from: (f32, f32),
    to: (f32, f32),
) {
    let at = |p: (f32, f32)| origin + gpui::point(px(p.0), px(p.1));
    cx.simulate_mouse_down(at(from), MouseButton::Left, with(""));
    cx.simulate_mouse_move(at(to), Some(MouseButton::Left), with(""));
    cx.simulate_mouse_up(at(to), MouseButton::Left, with(""));
    cx.run_until_parked();
}

fn saved_picture(project: &std::path::Path, cx: &mut gpui::VisualTestContext) -> image::RgbImage {
    click(cx, "edit-save");
    cx.run_until_parked();
    image::open(project.join("zz edited.png"))
        .unwrap()
        .into_rgb8()
}

fn inked(picture: &image::RgbImage, x: std::ops::Range<u32>, y: std::ops::Range<u32>) -> bool {
    x.clone()
        .any(|x| y.clone().any(|y| picture.get_pixel(x, y).0[0] < 128))
}

/// A box is ready to move once drawn, and what's saved is where it was left.
#[gpui::test]
fn a_mark_drawn_can_be_moved_before_saving(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    white_picture(&project);
    let owned = own(cx);
    let (_browser, cx) = browse(&project, cx);
    let origin = marking(&project, cx);
    click(cx, "tool-rectangle");
    drag_in(cx, origin, (50., 50.), (150., 100.));
    // By its left edge, 300 right and 150 down.
    drag_in(cx, origin, (50., 75.), (350., 225.));
    let saved = saved_picture(&project, cx);
    assert!(inked(&saved, 340..360, 200..250), "the box's new left edge");
    assert!(
        !inked(&saved, 0..300, 0..150),
        "and nothing at its old place"
    );
    release(owned, cx);
}

/// A corner scales, and the opposite one stays.
#[gpui::test]
fn dragging_a_corner_scales_a_mark(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    white_picture(&project);
    let owned = own(cx);
    let (_browser, cx) = browse(&project, cx);
    let origin = marking(&project, cx);
    click(cx, "tool-rectangle");
    drag_in(cx, origin, (50., 50.), (150., 100.));
    drag_in(cx, origin, (150., 100.), (450., 250.));
    let saved = saved_picture(&project, cx);
    assert!(inked(&saved, 445..455, 100..200), "the new right edge");
    assert!(inked(&saved, 200..400, 245..255), "the new bottom edge");
    assert!(inked(&saved, 45..55, 100..200), "the left edge held");
    assert!(
        !inked(&saved, 145..155, 60..95),
        "the old right edge is gone"
    );
    release(owned, cx);
}

/// The handle above a mark turns it about its center.
#[gpui::test]
fn the_turning_handle_rotates_a_mark(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    white_picture(&project);
    let owned = own(cx);
    let (_browser, cx) = browse(&project, cx);
    let origin = marking(&project, cx);
    click(cx, "tool-rectangle");
    // 200 wide, 40 tall, about (300, 150). The handle is 22 above its top.
    drag_in(cx, origin, (200., 130.), (400., 170.));
    drag_in(cx, origin, (300., 108.), (342., 150.));
    let saved = saved_picture(&project, cx);
    // A quarter turn: 40 wide, 200 tall.
    assert!(inked(&saved, 275..285, 90..210), "standing on end");
    assert!(
        !inked(&saved, 205..265, 125..175),
        "its old left end is gone"
    );
    release(owned, cx);
}

/// Two clicks reopen words; Delete and Undo work on what's picked up, not the file.
#[gpui::test]
fn words_can_change_and_a_mark_can_go_and_come_back(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    white_picture(&project);
    let owned = own(cx);
    let (_browser, cx) = browse(&project, cx);
    let origin = marking(&project, cx);
    click(cx, "tool-text");
    let at = origin + gpui::point(px(100.), px(100.));
    cx.simulate_mouse_down(at, MouseButton::Left, with(""));
    cx.simulate_mouse_up(at, MouseButton::Left, with(""));
    press(cx, "a enter");
    assert!(!drawn(cx, "note-card"));
    // Twice on the words: the card comes back with them in it.
    let on = at + gpui::point(px(4.), px(10.));
    cx.simulate_event(gpui::MouseDownEvent {
        position: on,
        button: MouseButton::Left,
        modifiers: with(""),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_mouse_up(on, MouseButton::Left, with(""));
    assert!(drawn(cx, "note-card"));
    press(cx, "enter");
    assert!(!drawn(cx, "note-card"));
    // Picked up, Delete takes it off, and the file stays.
    click(cx, "tool-rectangle");
    drag_in(cx, origin, (300., 200.), (400., 280.));
    press(cx, "backspace");
    assert!(project.join("zz.png").exists());
    click(cx, "edit-undo");
    cx.run_until_parked();
    let saved = saved_picture(&project, cx);
    assert!(
        inked(&saved, 295..305, 200..280),
        "Undo brought the box back"
    );
    assert!(
        inked(&saved, 100..200, 100..140),
        "the words are still there"
    );
    release(owned, cx);
}

/// Undo takes back a move, and Delete really takes the mark off.
#[gpui::test]
fn undo_takes_back_a_move_and_delete_removes_a_mark(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    white_picture(&project);
    let owned = own(cx);
    let (_browser, cx) = browse(&project, cx);
    let origin = marking(&project, cx);
    click(cx, "tool-rectangle");
    drag_in(cx, origin, (50., 50.), (150., 100.));
    drag_in(cx, origin, (50., 75.), (350., 225.));
    click(cx, "edit-undo");
    cx.run_until_parked();
    // Back where it was drawn, and a second box to take off.
    click(cx, "tool-rectangle");
    drag_in(cx, origin, (300., 200.), (400., 280.));
    press(cx, "backspace");
    cx.run_until_parked();
    let saved = saved_picture(&project, cx);
    assert!(inked(&saved, 45..55, 60..90), "Undo put the box back");
    assert!(
        !inked(&saved, 200..500, 150..290),
        "and Delete took the other off"
    );
    release(owned, cx);
}

/// A crop renumbers the picture's pixels, so what came before it stays put.
#[gpui::test]
fn a_mark_before_a_flip_is_part_of_the_picture(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    white_picture(&project);
    let owned = own(cx);
    let (_browser, cx) = browse(&project, cx);
    let origin = marking(&project, cx);
    click(cx, "tool-rectangle");
    drag_in(cx, origin, (50., 50.), (150., 100.));
    click(cx, "tool-flip");
    cx.run_until_parked();
    // Flipped, its left edge shows at 550. Neither there nor where its recipe
    // still says it is does a press pick anything up.
    drag_in(cx, origin, (550., 75.), (300., 200.));
    drag_in(cx, origin, (50., 75.), (300., 200.));
    let saved = saved_picture(&project, cx);
    assert!(inked(&saved, 545..555, 60..90), "the box didn't move");
    assert!(!inked(&saved, 200..500, 150..290), "nor did a copy of it");
    release(owned, cx);
}

/// The card is modal: a stray press can't lose written words or reach the row.
#[gpui::test]
fn the_text_card_keeps_written_words_until_cancelled(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    image::RgbImage::from_pixel(600, 300, image::Rgb([255, 255, 255]))
        .save(project.join("zz.png"))
        .unwrap();
    let owned = own(cx);
    let (_browser, cx) = browse(&project, cx);
    press(cx, "alt-down space e");
    click(cx, "tool-text");
    let at = cx.debug_bounds("preview-edit-surface").unwrap().origin + gpui::point(px(4.), px(4.));
    let pick = |cx: &mut gpui::VisualTestContext| {
        cx.simulate_mouse_down(at, MouseButton::Left, with(""));
        cx.simulate_mouse_up(at, MouseButton::Left, with(""));
        cx.run_until_parked();
    };
    pick(cx);
    assert!(drawn(cx, "note-card"));
    // Nothing written: a press elsewhere only closes it.
    pick(cx);
    assert!(!drawn(cx, "note-card"));
    pick(cx);
    press(cx, "a");
    pick(cx);
    assert!(drawn(cx, "note-card"), "written words stay");
    click(cx, "tool-pen");
    assert!(drawn(cx, "note-card"), "the row is behind it");
    click(cx, "note-cancel");
    assert!(!drawn(cx, "note-card"));
    assert!(!drawn(cx, "edit-discard"));
    press(cx, "escape");
    assert!(
        !drawn(cx, "preview-edit-surface"),
        "Cancel added nothing to ask about"
    );
    release(owned, cx);
}

/// The crop starts as the whole picture, its edges drag in, and Return, which
/// would otherwise rename the file, applies it.
#[gpui::test]
fn the_crop_starts_whole_and_its_edges_drag_in(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    image::RgbImage::from_pixel(200, 100, image::Rgb([255, 255, 255]))
        .save(project.join("zz.png"))
        .unwrap();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down space e");
    click(cx, "tool-crop");
    assert!(drawn(cx, "edit-crop-apply"));
    // The picture is centered and not enlarged, so its right edge is 100 points
    // right of the middle.
    let center = cx.debug_bounds("preview-edit-surface").unwrap().center();
    let edge = center + gpui::point(px(100.), px(0.));
    let dragged_to = center + gpui::point(px(50.), px(0.));
    cx.simulate_mouse_down(edge, MouseButton::Left, with(""));
    cx.simulate_mouse_move(dragged_to, MouseButton::Left, with(""));
    cx.simulate_mouse_up(dragged_to, MouseButton::Left, with(""));
    press(cx, "enter");
    assert!(!drawn(cx, "edit-crop-apply"));
    assert_eq!(editing(&browser, cx), None, "Return renamed nothing");
    click(cx, "edit-save");
    cx.run_until_parked();
    let saved = image::open(project.join("zz edited.png")).unwrap();
    assert_eq!((saved.width(), saved.height()), (150, 100));
    release(owned, cx);
}

#[gpui::test]
fn a_save_that_fails_partway_keeps_the_edit(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let photo = project.join("zz.png");
    image::RgbImage::new(50, 50).save(&photo).unwrap();
    let before = std::fs::read(&photo).unwrap();
    let flags = |flag: &str| {
        let done = std::process::Command::new("chflags")
            .arg(flag)
            .arg(&photo)
            .status();
        assert!(done.unwrap().success());
    };
    flags("uchg");
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down space e");
    click(cx, "tool-rotate");
    click(cx, "edit-more");
    assert!(drawn(cx, "edit-replace"));
    click(cx, "edit-replace");
    cx.run_until_parked();
    let kept = drawn(cx, "edit-save");
    flags("nouchg");
    assert!(kept, "the edit is still there to save");
    assert_eq!(std::fs::read(&photo).unwrap(), before);
    assert_eq!(toast(&browser, cx), None);
    release(owned, cx);
}

#[gpui::test]
fn unsaved_edits_hold_the_preview_until_discarded(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    for name in ["xa.png", "xb.png"] {
        image::RgbImage::new(50, 50)
            .save(project.join(name))
            .unwrap();
    }
    let (browser, cx) = browse(&project, cx);
    press(cx, "alt-down");
    assert_eq!(shown(&browser, cx).1, ["xb.png"]);
    press(cx, "space e");
    click(cx, "tool-rotate");
    assert!(drawn(cx, "edit-save"));
    // The selection can move, but what's being edited stays on screen.
    press(cx, "up");
    assert_eq!(shown(&browser, cx).1, ["xa.png"]);
    assert!(drawn(cx, "edit-save"));
    let name = |cx: &mut VisualTestContext| {
        in_pane(&browser, cx, |pane, cx| {
            pane.preview.as_ref().map(|view| view.read(cx).path.clone())
        })
    };
    assert_eq!(name(cx), Some(project.join("xb.png")));
    // Done asks, as Esc does, and nothing's lost.
    click(cx, "edit-done");
    assert!(drawn(cx, "edit-discard"));
    click(cx, "edit-keep");
    // Nor does letting go of the selection close it.
    press(cx, "cmd-alt-a");
    assert!(shown(&browser, cx).1.is_empty());
    assert!(drawn(cx, "edit-save"));
    assert_eq!(name(cx), Some(project.join("xb.png")));
    press(cx, "alt-down up");
    assert_eq!(shown(&browser, cx).1, ["xa.png"]);
    // Discarding lands on what's selected now.
    press(cx, "escape");
    click(cx, "edit-discard");
    assert_eq!(name(cx), Some(project.join("xa.png")));
    // So does undoing the only change, with nothing left to lose.
    press(cx, "alt-down e");
    assert_eq!(shown(&browser, cx).1, ["xb.png"]);
    click(cx, "tool-rotate");
    assert!(drawn(cx, "edit-save"));
    press(cx, "up");
    assert_eq!(name(cx), Some(project.join("xb.png")));
    click(cx, "edit-undo");
    assert_eq!(name(cx), Some(project.join("xa.png")));
    assert!(!drawn(cx, "preview-tools"));
    press(cx, "escape");
}
