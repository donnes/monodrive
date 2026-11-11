//! Find Duplicates from the keys, over a temp folder, through a real owner.

use super::tests::{browse, in_pane, own, press, release, toast};
use super::*;
use gpui::{TestAppContext, VisualTestContext};

fn on_screen(cx: &mut VisualTestContext, selector: &str) -> bool {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector.to_string().leak()).is_some()
}

fn reviewing(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    browser.read_with(cx, |b, _| {
        matches!(b.sheet, Some(Sheet::ReviewDuplicates(_)))
    })
}

#[gpui::test]
fn find_duplicates_moves_only_the_chosen_copies_and_always_keeps_one(cx: &mut TestAppContext) {
    let owned = own(cx);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().canonicalize().unwrap().join("Downloads");
    std::fs::create_dir_all(folder.join("Old")).unwrap();
    let big = vec![7u8; 1_200_000];
    std::fs::write(folder.join("a.iso"), &big).unwrap();
    std::fs::write(folder.join("Old/a.iso"), &big).unwrap();
    // The same size, apart in the middle: never one group.
    let mut other = big.clone();
    other[600_000] = 8;
    std::fs::write(folder.join("b.iso"), &other).unwrap();
    // Copies under 1 MB wait to be asked for.
    std::fs::write(folder.join("small.txt"), "same").unwrap();
    std::fs::write(folder.join("Old/small.txt"), "same").unwrap();
    let (browser, cx) = browse(&folder, cx);

    press(cx, "cmd-alt-u");
    assert!(on_screen(cx, "duplicates-page"));
    assert_eq!(
        in_pane(&browser, cx, |pane, cx| pane.title(cx)),
        "Duplicates in Downloads"
    );
    // By path, so the copy in Old comes first.
    assert!(on_screen(cx, "duplicate-group-a.iso"));
    assert!(!on_screen(cx, "duplicate-group-b.iso"));
    assert!(!on_screen(cx, "duplicate-group-small.txt"));

    // Space chooses the first copy; the second is the last one kept, so
    // Space leaves it.
    press(cx, "down right down space down space");
    assert!(on_screen(cx, "duplicates-review"));
    press(cx, "cmd-backspace");
    assert!(reviewing(&browser, cx));
    press(cx, "escape");
    assert!(!reviewing(&browser, cx) && folder.join("Old/a.iso").exists());

    press(cx, "cmd-backspace enter");
    assert!(!folder.join("Old/a.iso").exists());
    assert!(folder.join("a.iso").exists() && folder.join("b.iso").exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Moved 1 copy to the Trash".into(), false, true))
    );
    assert!(!on_screen(cx, "duplicate-group-a.iso"));

    // Back returns to the folder the page was found from.
    press(cx, "cmd-[");
    assert!(!on_screen(cx, "duplicates-page"));
    assert_eq!(
        in_pane(&browser, cx, |pane, _| pane.folder().to_path_buf()),
        folder
    );
    release(owned, cx);
}
