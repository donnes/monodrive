//! The inspector and Open With, driven through the real window over a temp
//! folder, with `Native` made up: nothing walks a real folder or launches an
//! app, and each read and total lands when the test says.

use super::inspector::{PATIENCE, READ_DELAY, REFRESH_DELAY, Read, Subject, Total, kinds_summary};
use super::pane::Pane;
use super::places_tests::world;
use super::tests::{browse, folder, in_pane, on_pane, own, press, release, shown};
use super::*;
use crate::menu::{Choice, ContextMenu};
use crate::native::{MadeUp, Native, SizeDone};
use crate::thumbnails::{Thumb, Thumbnail};
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use monodrive_core::apps::App as MacApp;
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::info::{self, Info};
use monodrive_core::sizes::FolderSize;
use std::sync::atomic::{AtomicBool, Ordering};

/// A walk that hasn't finished.
struct Walk {
    folder: PathBuf,
    done: SizeDone,
    cancelled: Arc<AtomicBool>,
}

struct Cancels(Arc<AtomicBool>);

impl Drop for Cancels {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Fake {
    reads: Mutex<Vec<PathBuf>>,
    gated: Mutex<Vec<PathBuf>>,
    walks: Mutex<Vec<Walk>>,
    /// How many walks began, finished or not.
    walked: Mutex<usize>,
    /// Each walk's flag, kept after it finishes.
    cancels: Mutex<Vec<(PathBuf, Arc<AtomicBool>)>>,
    known: Mutex<HashMap<PathBuf, FolderSize>>,
    invalidated: Mutex<Vec<PathBuf>>,
    opened: Mutex<Vec<(Vec<PathBuf>, PathBuf)>>,
    defaults: Mutex<Vec<(PathBuf, PathBuf)>>,
    /// What Other… picks.
    chosen: Mutex<Option<PathBuf>>,
}

fn app(name: &str) -> MacApp {
    MacApp {
        name: name.into(),
        bundle_id: None,
        path: format!("/Applications/{name}.app").into(),
    }
}

impl Native for Fake {
    fn info(&self, path: &Path) -> Result<Info, String> {
        self.reads.lock().unwrap().push(path.to_path_buf());
        info::info(path)
    }

    fn needs_consent(&self, path: &Path) -> bool {
        self.gated
            .lock()
            .unwrap()
            .iter()
            .any(|g| path.starts_with(g))
    }

    fn known_size(&self, folder: &Path) -> Option<FolderSize> {
        self.known.lock().unwrap().get(folder).copied()
    }

    fn measure(&self, folder: PathBuf, done: SizeDone) -> Box<dyn Send> {
        let cancelled = Arc::new(AtomicBool::new(false));
        *self.walked.lock().unwrap() += 1;
        self.cancels
            .lock()
            .unwrap()
            .push((folder.clone(), cancelled.clone()));
        self.walks.lock().unwrap().push(Walk {
            folder,
            done,
            cancelled: cancelled.clone(),
        });
        Box::new(Cancels(cancelled))
    }

    fn invalidate(&self, path: &Path) {
        self.invalidated.lock().unwrap().push(path.to_path_buf());
        self.known
            .lock()
            .unwrap()
            .retain(|folder, _| !(path.starts_with(folder) || folder.starts_with(path)));
    }

    fn forget_sizes(&self) {
        self.known.lock().unwrap().clear();
    }

    fn can_open(&self, _: &Path) -> Vec<MacApp> {
        vec![app("Zed"), app("TextEdit")]
    }

    fn default_for(&self, _: &Path) -> Option<MacApp> {
        Some(app("TextEdit"))
    }

    fn open_with(&self, files: &[PathBuf], app: &Path) -> Result<(), String> {
        self.opened
            .lock()
            .unwrap()
            .push((files.to_vec(), app.to_path_buf()));
        Ok(())
    }

    fn make_default(&self, app: &Path, like: &Path) -> Result<(), String> {
        self.defaults
            .lock()
            .unwrap()
            .push((app.to_path_buf(), like.to_path_buf()));
        Ok(())
    }

    fn choose_app(&self, done: Box<dyn FnOnce(Option<PathBuf>)>) {
        done(self.chosen.lock().unwrap().clone());
    }

    fn installed(&self, _: &str) -> bool {
        true
    }
}

impl Fake {
    /// The newest walk finishes with this many bytes, and its total is kept,
    /// as `Sizes` keeps it.
    fn finish_walk(&self, bytes: u64) -> PathBuf {
        let walk = self.walks.lock().unwrap().pop().unwrap();
        let total = FolderSize {
            bytes,
            complete: true,
            ..FolderSize::default()
        };
        self.known
            .lock()
            .unwrap()
            .insert(walk.folder.clone(), total);
        (walk.done)(Ok(total));
        walk.folder
    }

    fn reads(&self) -> Vec<String> {
        self.reads
            .lock()
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }
}

fn set_up<'a>(
    at: &Path,
    cx: &'a mut TestAppContext,
) -> (Entity<Browser>, &'a mut VisualTestContext, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let native = fake.clone();
    cx.update(|cx| cx.set_global(MadeUp(native)));
    let (browser, cx) = browse(at, cx);
    (browser, cx, fake)
}

/// Past the wait before a read, and the read landed.
fn rest(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(READ_DELAY);
    cx.run_until_parked();
}

fn subject_name(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Option<String> {
    browser.read_with(cx, |browser, _| match &browser.inspector.subject {
        Subject::One { entry, .. } => Some(entry.name.clone()),
        _ => None,
    })
}

/// Whose facts the panel's rows are showing.
fn read_name(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Option<String> {
    browser.read_with(cx, |browser, _| match &browser.inspector.read {
        Read::Known { info, .. } => Some(info.name.clone()),
        _ => None,
    })
}

/// The panel on a.txt, read, with these files beside it.
fn on_a_file<'a>(
    beside: &[(&str, &[u8])],
    project: &Path,
    cx: &'a mut TestAppContext,
) -> (Entity<Browser>, &'a mut VisualTestContext, Arc<Fake>) {
    for (name, bytes) in beside {
        std::fs::write(project.join(name), bytes).unwrap();
    }
    let (browser, cx, fake) = set_up(project, cx);
    press(cx, "alt-up down down cmd-i");
    rest(cx);
    assert_eq!(read_name(&browser, cx).as_deref(), Some("a.txt"));
    (browser, cx, fake)
}

fn total(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Option<u64> {
    browser.read_with(cx, |browser, _| match &browser.inspector.total {
        Total::Done(total) => Some(total.bytes),
        _ => None,
    })
}

/// Whether the panel says the total is on its way.
fn calculating(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    browser.read_with(cx, |browser, _| {
        let Subject::One { entry, .. } = &browser.inspector.subject else {
            return false;
        };
        browser.size_text(entry) == "Calculating…"
    })
}

#[gpui::test]
fn get_info_opens_and_closes_it_and_only_the_list_gives_way(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, _fake) = set_up(&project, cx);
    let shown_prefs =
        |cx: &mut VisualTestContext| cx.update(|_, cx| cx.global::<Preferences>().show_inspector);
    let width = |browser: &Entity<Browser>, cx: &mut VisualTestContext| {
        in_pane(browser, cx, |pane, _| pane.chrome.width)
    };
    assert!(!shown_prefs(cx));
    assert!(cx.debug_bounds("inspector").is_none());
    let wide = width(&browser, cx);
    press(cx, "cmd-i");
    assert!(shown_prefs(cx));
    let panel = cx.debug_bounds("inspector").unwrap();
    assert_eq!(panel.size.width.as_f32(), inspector::WIDTH);
    assert_eq!(
        width(&browser, cx),
        wide - inspector::WIDTH - tabs::CARD_GAP
    );
    press(cx, "cmd-i");
    assert!(!shown_prefs(cx));
    assert!(cx.debug_bounds("inspector").is_none());
    assert_eq!(width(&browser, cx), wide);
    // The way out it shows.
    press(cx, "cmd-i");
    let hide = cx.debug_bounds("inspector-hide").unwrap().center();
    cx.simulate_click(hide, Modifiers::none());
    cx.run_until_parked();
    assert!(!shown_prefs(cx));
    // It's kept, and a settings file from before it leaves it closed.
    let on = Preferences {
        show_inspector: true,
        ..Preferences::default()
    };
    let saved: Preferences = serde_json::from_slice(&serde_json::to_vec(&on).unwrap()).unwrap();
    assert!(saved.show_inspector);
}

#[gpui::test]
fn closed_it_reads_and_runs_nothing(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "down down");
    rest(cx);
    browser.update(cx, |browser, cx| {
        browser.disk_changed(&[(project.clone(), false)], cx);
    });
    cx.executor().advance_clock(REFRESH_DELAY);
    cx.run_until_parked();
    assert!(fake.reads().is_empty());
    assert_eq!(*fake.walked.lock().unwrap(), 0);
    assert!(fake.invalidated.lock().unwrap().is_empty());
    // Opened, it reads what's selected; closed again, a walk stops.
    press(cx, "cmd-i");
    rest(cx);
    assert_eq!(fake.reads(), ["zeta"]);
    assert_eq!(*fake.walked.lock().unwrap(), 1);
    press(cx, "cmd-i");
    assert!(
        fake.walks.lock().unwrap()[0]
            .cancelled
            .load(Ordering::Relaxed)
    );
}

#[gpui::test]
fn it_follows_the_selection_and_the_pane_with_the_keys(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "cmd-i");
    // Nothing selected: the folder itself, which can't be renamed from here.
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("project"));
    rest(cx);
    assert_eq!(read_name(&browser, cx).as_deref(), Some("project"));
    // One item: the rows stay until its read lands, then all change together.
    press(cx, "down");
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("project"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("project"));
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("docs"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("docs"));
    assert_eq!(fake.reads(), ["project", "docs"]);
    press(cx, "cmd-alt-s");
    // The new pane has the keys and nothing selected, which shows at once.
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("project"));
    press(cx, "down down down");
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    let left = browser.read_with(cx, |browser, _| browser.tabs[browser.active].pane.clone());
    cx.update(|window, cx| {
        let focus = left.read(cx).list_focus.clone();
        window.focus(&focus, cx);
    });
    cx.run_until_parked();
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("docs"));
}

#[gpui::test]
fn a_picture_swaps_in_with_its_facts(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let picture = project.join("pic.png");
    image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
        .save(&picture)
        .unwrap();
    let (browser, cx, _fake) = on_a_file(&[], &project, cx);
    press(cx, "down");
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("a.txt"));
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("pic.png"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("pic.png"));
    browser.read_with(cx, |browser, _| {
        let (thumb, image) = browser.inspector.thumb.as_ref().unwrap();
        assert_eq!(thumb.path, picture);
        assert!(image.is_some(), "decoded before the swap");
    });
}

#[gpui::test]
fn past_its_patience_an_item_shows_without_its_picture(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = on_a_file(&[("bad.png", b"not a picture")], &project, cx);
    press(cx, "alt-up down down down");
    rest(cx);
    // The read has landed, and the picture will never decode.
    assert_eq!(fake.reads().last().map(String::as_str), Some("bad.png"));
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("a.txt"));
    cx.executor().advance_clock(PATIENCE - READ_DELAY);
    cx.run_until_parked();
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("bad.png"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("bad.png"));
}

#[gpui::test]
fn a_newer_selection_lets_go_of_the_one_waiting(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, _fake) = on_a_file(&[("bad.png", b"not a picture")], &project, cx);
    let thumb = cx.update(|window, cx| {
        let pane = browser.read(cx).pane().read(cx);
        let entry = pane.entries.iter().find(|e| e.name == "bad.png").unwrap();
        Thumb {
            path: entry.path.clone(),
            side: (96. * window.scale_factor()).ceil() as u32,
            modified: entry.modified,
        }
    });
    let held = |cx: &mut VisualTestContext| cx.update(|_, cx| cx.has_asset::<Thumbnail>(&thumb));
    press(cx, "alt-up down down down");
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    assert!(held(cx), "its picture decodes before it shows");
    // Another comes along: the picture goes, and nothing of it ever shows.
    press(cx, "up up");
    assert!(!held(cx));
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("zeta"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("zeta"));
    cx.executor().advance_clock(PATIENCE);
    cx.run_until_parked();
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("zeta"));
}

#[gpui::test]
fn selecting_back_keeps_what_is_shown_without_asking_again(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = on_a_file(&[], &project, cx);
    fake.reads.lock().unwrap().clear();
    // Back from one, and from one that replaced another.
    press(cx, "up");
    press(cx, "down");
    press(cx, "up up");
    press(cx, "down down");
    assert_eq!(read_name(&browser, cx).as_deref(), Some("a.txt"));
    cx.executor().advance_clock(PATIENCE);
    cx.run_until_parked();
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("a.txt"));
    assert!(fake.reads().is_empty(), "{:?}", fake.reads());
}

#[gpui::test]
fn selecting_back_catches_up_on_what_changed_or_stopped_meanwhile(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let docs = project.canonicalize().unwrap().join("docs");
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "cmd-i");
    press(cx, "down");
    rest(cx);
    fake.finish_walk(42);
    cx.run_until_parked();
    assert_eq!(total(&browser, cx), Some(42));

    // It changes while the next item waits: its total comes again on return.
    press(cx, "down");
    browser.update(cx, |browser, cx| {
        browser.disk_changed(&[(docs.join("sub"), true)], cx);
    });
    cx.run_until_parked();
    press(cx, "up");
    assert_eq!(total(&browser, cx), Some(42));
    rest(cx);
    assert_eq!(*fake.walked.lock().unwrap(), 2);
    fake.finish_walk(100);
    cx.run_until_parked();
    assert_eq!(total(&browser, cx), Some(100));

    // A refresh that was due when the next item came along isn't lost.
    browser.update(cx, |browser, cx| {
        browser.disk_changed(&[(docs.join("sub"), true)], cx);
    });
    cx.run_until_parked();
    press(cx, "down");
    press(cx, "up");
    cx.executor().advance_clock(REFRESH_DELAY);
    cx.run_until_parked();
    assert_eq!(*fake.walked.lock().unwrap(), 3);

    // Nor is a walk that the next item stopped.
    press(cx, "down");
    press(cx, "up");
    rest(cx);
    assert_eq!(*fake.walked.lock().unwrap(), 4);
    fake.finish_walk(7);
    cx.run_until_parked();
    assert_eq!(total(&browser, cx), Some(7));
}

#[gpui::test]
fn clicking_the_old_name_doesnt_rename_the_new_selection(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, _fake) = on_a_file(&[], &project, cx);
    press(cx, "up");
    let name = cx.debug_bounds("inspector-name").unwrap().center();
    cx.simulate_click(name, Modifiers::none());
    cx.run_until_parked();
    assert!(in_pane(&browser, cx, |pane, _| pane.renaming.is_none()));
}

#[gpui::test]
fn arrowing_past_rows_asks_nothing_about_them_and_late_answers_are_dropped(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "cmd-i");
    rest(cx);
    fake.reads.lock().unwrap().clear();
    press(cx, "down down down");
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("project"));
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    assert_eq!(fake.reads(), ["a.txt"], "only where it came to rest");
    // A read still waiting when the selection moves never runs.
    press(cx, "up");
    cx.executor().advance_clock(READ_DELAY / 2);
    on_pane(&browser, cx, |pane, cx| pane.select_row(0, cx));
    cx.executor().advance_clock(READ_DELAY / 2);
    cx.run_until_parked();
    assert_eq!(fake.reads(), ["a.txt"]);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("a.txt"));
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("docs"));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("docs"));
}

#[gpui::test]
fn a_folders_total_comes_in_the_background_and_goes_with_the_selection(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let real = project.canonicalize().unwrap();
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "cmd-i");
    press(cx, "down");
    rest(cx);
    assert!(calculating(&browser, cx));
    assert_eq!(fake.finish_walk(42), real.join("docs"));
    cx.run_until_parked();
    assert_eq!(total(&browser, cx), Some(42));
    assert!(!calculating(&browser, cx));

    // Another folder cancels this walk, and its late total lands on nothing.
    press(cx, "down");
    rest(cx);
    assert!(calculating(&browser, cx));
    let (zeta, cancelled) = fake.cancels.lock().unwrap().last().cloned().unwrap();
    assert_eq!(zeta, real.join("zeta"));
    press(cx, "down");
    assert!(cancelled.load(Ordering::Relaxed));
    fake.finish_walk(7);
    cx.run_until_parked();
    assert_eq!(total(&browser, cx), None);

    // Coming back, the kept total shows without a walk.
    let walked = *fake.walked.lock().unwrap();
    press(cx, "up up");
    rest(cx);
    assert_eq!(total(&browser, cx), Some(42));
    assert_eq!(*fake.walked.lock().unwrap(), walked);
}

#[gpui::test]
fn a_change_in_the_folder_measures_it_again(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let docs = project.canonicalize().unwrap().join("docs");
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "cmd-i");
    press(cx, "down");
    rest(cx);
    fake.finish_walk(42);
    cx.run_until_parked();

    let changed = || vec![(docs.join("sub"), true)];
    browser.update(cx, |browser, cx| browser.disk_changed(&changed(), cx));
    cx.run_until_parked();
    assert_eq!(*fake.invalidated.lock().unwrap(), [docs.join("sub")]);
    // Kept until the new total lands, so it doesn't flicker.
    assert_eq!(total(&browser, cx), Some(42));
    cx.executor().advance_clock(REFRESH_DELAY);
    cx.run_until_parked();
    assert_eq!(*fake.walked.lock().unwrap(), 2);
    assert_eq!(total(&browser, cx), Some(42));
    assert_eq!(read_name(&browser, cx).as_deref(), Some("docs"));

    // More changes meanwhile don't restart the walk, or it might never end;
    // one more follows it.
    browser.update(cx, |browser, cx| browser.disk_changed(&changed(), cx));
    cx.executor().advance_clock(REFRESH_DELAY);
    cx.run_until_parked();
    assert_eq!(*fake.walked.lock().unwrap(), 2);
    fake.finish_walk(100);
    cx.run_until_parked();
    assert_eq!(total(&browser, cx), Some(100));
    assert_eq!(*fake.walked.lock().unwrap(), 3);
}

#[gpui::test]
fn behind_a_privacy_prompt_it_shows_only_what_the_listing_knows(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    fake.gated.lock().unwrap().push(project.join("docs"));
    press(cx, "cmd-i");
    fake.reads.lock().unwrap().clear();
    let walked = *fake.walked.lock().unwrap();
    press(cx, "down");
    rest(cx);
    assert!(fake.reads().is_empty());
    assert_eq!(*fake.walked.lock().unwrap(), walked);
    browser.read_with(cx, |browser, _| {
        assert!(matches!(browser.inspector.read, Read::Gated));
        assert!(matches!(browser.inspector.total, Total::Failed(_)));
    });
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("docs"));
}

#[gpui::test]
fn several_items_say_how_many_how_big_and_of_what_kinds(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    std::fs::write(project.join("a.txt"), "hello").unwrap();
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "cmd-i");
    rest(cx);
    fake.reads.lock().unwrap().clear();
    press(cx, "cmd-a");
    rest(cx);
    browser.read_with(cx, |browser, _| {
        let Subject::Several {
            count,
            bytes,
            kinds,
            folders,
            parent,
            ..
        } = &browser.inspector.subject
        else {
            panic!("several items");
        };
        assert_eq!((*count, *bytes, *folders), (3, 5, 2));
        assert_eq!(kinds, "2 folders, 1 TXT file");
        assert_eq!(parent.as_deref(), Some(project.as_path()));
    });
    assert!(fake.reads().is_empty(), "the listing knows all of it");
    assert!(cx.debug_bounds("inspector-name").is_some());
}

#[test]
fn the_kinds_summary_names_the_commonest_and_counts_the_rest() {
    let entry = |name: &str, kind| Entry {
        path: PathBuf::from("/x").join(name),
        name: name.into(),
        kind,
        size: 0,
        modified: 0,
        availability: Default::default(),
    };
    let entries = [
        entry("a.png", Kind::File),
        entry("b.png", Kind::File),
        entry("c.png", Kind::File),
        entry("d", Kind::Dir),
        entry("e", Kind::Dir),
        entry("f.pdf", Kind::File),
        entry("g.md", Kind::File),
        entry("h", Kind::Link),
    ];
    let all: Vec<&Entry> = entries.iter().collect();
    assert_eq!(
        kinds_summary(&all),
        "3 PNG files, 2 folders, 1 alias, 2 more"
    );
}

#[gpui::test]
fn clicking_the_name_renames_through_the_same_service(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    std::fs::write(project.join("taken.txt"), "").unwrap();
    let owned = own(cx);
    let (browser, cx, _fake) = set_up(&project, cx);
    press(cx, "cmd-i");
    // The folder itself isn't offered: the panes are showing it.
    let name = cx.debug_bounds("inspector-name").unwrap().center();
    cx.simulate_click(name, Modifiers::none());
    cx.run_until_parked();
    assert!(in_pane(&browser, cx, |pane, _| pane.renaming.is_none()));

    press(cx, "alt-up down down");
    rest(cx);
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("a.txt"));
    let name = cx.debug_bounds("inspector-name").unwrap().center();
    cx.simulate_click(name, Modifiers::none());
    cx.run_until_parked();
    assert!(in_pane(&browser, cx, |pane, _| {
        pane.renaming.as_ref().is_some_and(|r| r.in_inspector)
    }));
    // The field is in the panel, not in the row.
    let field = cx.debug_bounds("rename-field").unwrap();
    let panel = cx.debug_bounds("inspector").unwrap();
    assert!(panel.contains(&field.center()));

    // The owner's validation applies: a name that's taken stays open and says so.
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("taken.txt");
    press(cx, "enter");
    let why = in_pane(&browser, cx, |pane, _| {
        pane.renaming.as_ref().and_then(|r| r.error.clone())
    });
    assert_eq!(why.as_deref(), Some("\"taken.txt\" is already taken."));
    assert!(project.join("a.txt").exists());

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("b.txt");
    press(cx, "enter");
    assert!(project.join("b.txt").exists() && !project.join("a.txt").exists());
    assert!(in_pane(&browser, cx, |pane, _| pane.renaming.is_none()));
    assert_eq!(subject_name(&browser, cx).as_deref(), Some("b.txt"));
    release(owned, cx);
}

fn menu_of(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Option<Entity<ContextMenu>> {
    browser.read_with(cx, |browser, _| {
        browser.menu.as_ref().map(|menu| menu.view.clone())
    })
}

fn sub_titles(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Vec<String> {
    let menu = menu_of(browser, cx).unwrap();
    menu.read_with(cx, |menu, _| {
        menu.sub_rows()
            .iter()
            .map(|row| row.title.to_string())
            .collect()
    })
}

/// a.txt selected, its menu open, on Open With.
fn on_open_with(browser: &Entity<Browser>, cx: &mut VisualTestContext) {
    press(cx, "alt-up down down shift-f10");
    press(cx, "down down");
    let menu = menu_of(browser, cx).unwrap();
    menu.read_with(cx, |menu, _| {
        let row = &menu.rows[menu.active.unwrap()];
        assert!(row.submenu, "{}", row.title);
    });
}

#[gpui::test]
fn the_submenu_loads_the_apps_and_the_keys_go_in_and_out(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    on_open_with(&browser, cx);
    let menu = menu_of(&browser, cx).unwrap();
    // Until the apps land there's a row to say so.
    menu.read_with(cx, |menu, _| {
        assert!(menu.submenu.is_none());
        assert_eq!(menu.sub_rows()[0].title.as_ref(), "Loading…");
        assert_eq!(menu.sub_rows()[0].choice, None);
    });
    press(cx, "right");
    assert_eq!(
        sub_titles(&browser, cx),
        ["TextEdit (default)", "Zed", "Other…", "Always Open With…"]
    );
    let active = |menu: &Entity<ContextMenu>, cx: &mut VisualTestContext| {
        menu.read_with(cx, |menu, _| menu.submenu.as_ref().and_then(|s| s.active))
    };
    assert_eq!(active(&menu, cx), Some(0));
    // ↑ goes round to its last row, ← comes back out, → goes in again.
    press(cx, "up");
    assert_eq!(active(&menu, cx), Some(3));
    press(cx, "left");
    menu.read_with(cx, |menu, _| assert!(menu.submenu.is_none()));
    press(cx, "right down");
    assert_eq!(active(&menu, cx), Some(1));
    press(cx, "enter");
    assert!(menu_of(&browser, cx).is_none());
    assert_eq!(
        *fake.opened.lock().unwrap(),
        [(
            vec![project.join("a.txt")],
            PathBuf::from("/Applications/Zed.app")
        )]
    );
}

#[gpui::test]
fn the_pointer_opens_the_submenu_and_keeps_it_on_the_way_across(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    on_open_with(&browser, cx);
    press(cx, "right");
    let submenu_row = cx.debug_bounds("submenu-row-1").unwrap().center();
    let its_row = cx.debug_bounds("menu-row-1").unwrap().center();
    let below = cx.debug_bounds("menu-row-2").unwrap().center();
    let menu = menu_of(&browser, cx).unwrap();
    let open = |cx: &mut VisualTestContext| menu.read_with(cx, |menu, _| menu.submenu.is_some());
    press(cx, "left");
    assert!(!open(cx));
    cx.simulate_mouse_move(its_row, None, Modifiers::none());
    assert!(open(cx));
    // Left alone on another row, it closes after a moment.
    cx.simulate_mouse_move(below, None, Modifiers::none());
    assert!(open(cx));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(!open(cx));
    // Past that row on the way into it, it stays.
    cx.simulate_mouse_move(its_row, None, Modifiers::none());
    cx.simulate_mouse_move(below, None, Modifiers::none());
    cx.simulate_mouse_move(submenu_row, None, Modifiers::none());
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    menu.read_with(cx, |menu, _| {
        assert_eq!(menu.submenu.as_ref().and_then(|s| s.active), Some(1));
    });
    cx.simulate_click(submenu_row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(fake.opened.lock().unwrap().len(), 1);
}

#[gpui::test]
fn other_asks_for_an_app_and_opens_with_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    // Cancelled, nothing opens.
    on_open_with(&browser, cx);
    press(cx, "right down down enter");
    assert!(menu_of(&browser, cx).is_none());
    assert!(fake.opened.lock().unwrap().is_empty());

    *fake.chosen.lock().unwrap() = Some("/Applications/Vim.app".into());
    on_open_with(&browser, cx);
    press(cx, "right down down enter");
    assert_eq!(
        *fake.opened.lock().unwrap(),
        [(
            vec![project.join("a.txt")],
            PathBuf::from("/Applications/Vim.app")
        )]
    );
}

#[gpui::test]
fn always_open_with_asks_before_changing_the_default(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    on_open_with(&browser, cx);
    // Its row offers the apps again, under what they'd be for.
    press(cx, "right up enter");
    assert_eq!(
        sub_titles(&browser, cx),
        ["Always open .txt with", "TextEdit (default)", "Zed"]
    );
    // ← comes back to the rest, and the keys skip the title.
    press(cx, "left");
    assert_eq!(sub_titles(&browser, cx).len(), 4);
    press(cx, "up enter down");
    let menu = menu_of(&browser, cx).unwrap();
    menu.read_with(cx, |menu, _| {
        let at = menu.submenu.as_ref().and_then(|s| s.active).unwrap();
        let rows = menu.sub_rows();
        assert_eq!(
            rows[at].choice,
            Some(Choice::AlwaysWith("/Applications/Zed.app".into()))
        );
    });
    press(cx, "enter");
    assert!(cx.has_pending_prompt());
    assert!(fake.defaults.lock().unwrap().is_empty());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(fake.defaults.lock().unwrap().is_empty());
    assert!(fake.opened.lock().unwrap().is_empty());

    on_open_with(&browser, cx);
    press(cx, "right up enter down enter");
    cx.simulate_prompt_answer("Change All");
    cx.run_until_parked();
    assert_eq!(
        *fake.defaults.lock().unwrap(),
        [(
            PathBuf::from("/Applications/Zed.app"),
            project.join("a.txt")
        )]
    );
}

#[gpui::test]
fn several_files_have_no_always_and_open_together(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    std::fs::write(project.join("b.txt"), "").unwrap();
    let (browser, cx, fake) = set_up(&project, cx);
    // The File menu's way in: the same menu, with the keys in the submenu.
    press(cx, "alt-up down down shift-down");
    cx.dispatch_action(OpenWith);
    cx.run_until_parked();
    assert_eq!(
        sub_titles(&browser, cx),
        ["TextEdit (default)", "Zed", "Other…"]
    );
    press(cx, "enter");
    let (files, app) = fake.opened.lock().unwrap()[0].clone();
    assert_eq!(files, [project.join("a.txt"), project.join("b.txt")]);
    assert_eq!(app, PathBuf::from("/Applications/TextEdit.app"));
}

#[gpui::test]
fn the_palette_has_a_page_of_apps(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, fake) = set_up(&project, cx);
    press(cx, "alt-up down down cmd-k");
    let view = browser.read_with(cx, |browser, _| match &browser.sheet {
        Some(Sheet::Palette(view)) => view.clone(),
        _ => panic!("the palette is open"),
    });
    let at = view.read_with(cx, |view, _| {
        view.rows.iter().position(|row| row.title == "Open With")
    });
    let at = at.expect("offered on a selection");
    for _ in 0..at {
        press(cx, "down");
    }
    press(cx, "enter");
    let titles = view.read_with(cx, |view, _| {
        view.rows
            .iter()
            .map(|row| row.title.to_string())
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, ["TextEdit (default)", "Zed", "Other…"]);
    press(cx, "down enter");
    assert_eq!(
        *fake.opened.lock().unwrap(),
        [(
            vec![project.join("a.txt")],
            PathBuf::from("/Applications/Zed.app")
        )]
    );
}

/// A folder holding `Tool.app`, a link to it, and a plain folder.
fn with_package() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (dir, project) = folder();
    let app = project.join("Tool.app");
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    std::os::unix::fs::symlink("Tool.app", project.join("shortcut")).unwrap();
    (dir, project, app)
}

fn row_of(pane: &Pane, name: &str) -> usize {
    pane.entries.iter().position(|e| e.name == name).unwrap()
}

#[gpui::test]
fn opening_a_package_launches_it_instead_of_entering(cx: &mut TestAppContext) {
    let (_dir, project, app) = with_package();
    let world = world(Vec::new(), cx);
    let (browser, cx) = browse(&project, cx);
    let listed = shown(&browser, cx).0;
    for key in ["cmd-down", "cmd-o"] {
        let at = in_pane(&browser, cx, |pane, _| row_of(pane, "Tool.app"));
        on_pane(&browser, cx, |pane, cx| pane.select_row(at, cx));
        cx.run_until_parked();
        press(cx, key);
        assert_eq!(shown(&browser, cx).0, listed, "{key} stays in the folder");
    }
    // A double-click, and a link that leads to one, do the same.
    let at = in_pane(&browser, cx, |pane, _| row_of(pane, "Tool.app"));
    on_pane(&browser, cx, |pane, cx| pane.open(at, cx));
    cx.run_until_parked();
    let link = in_pane(&browser, cx, |pane, _| row_of(pane, "shortcut"));
    on_pane(&browser, cx, |pane, cx| pane.open(link, cx));
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).0, listed);
    assert_eq!(
        *world.fake.opened.lock().unwrap(),
        [
            app.clone(),
            app.clone(),
            app.clone(),
            project.join("shortcut")
        ]
    );
    // A plain folder still goes in.
    on_pane(&browser, cx, |pane, cx| pane.open(0, cx));
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).0, ["inner.md"]);
}

#[gpui::test]
fn show_package_contents_goes_in_and_back_comes_out(cx: &mut TestAppContext) {
    let (_dir, project, _app) = with_package();
    let _world = world(Vec::new(), cx);
    let (browser, cx) = browse(&project, cx);
    let at = in_pane(&browser, cx, |pane, _| row_of(pane, "Tool.app"));
    on_pane(&browser, cx, |pane, cx| pane.select_row(at, cx));
    cx.run_until_parked();
    cx.dispatch_action(ShowPackageContents);
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).0, ["Contents"]);
    press(cx, "cmd-[");
    let (rows, selected) = shown(&browser, cx);
    assert!(rows.contains(&"Tool.app".to_string()));
    assert_eq!(selected, ["Tool.app"]);
}

#[gpui::test]
fn packages_have_no_triangle_and_stay_shut(cx: &mut TestAppContext) {
    let (_dir, project, _app) = with_package();
    let (browser, cx) = browse(&project, cx);
    let (package, plain) = in_pane(&browser, cx, |pane, _| {
        (row_of(pane, "Tool.app"), row_of(pane, "docs"))
    });
    in_pane(&browser, cx, |pane, _| {
        assert_eq!(pane.is_open_folder(package), None);
        assert_eq!(pane.is_open_folder(plain), Some(false));
    });
    let listed = shown(&browser, cx).0;
    on_pane(&browser, cx, |pane, cx| pane.select_row(package, cx));
    cx.run_until_parked();
    press(cx, "right");
    assert_eq!(shown(&browser, cx).0, listed);
}

#[gpui::test]
fn in_two_panes_it_leaves_each_its_least(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx, _fake) = set_up(&project, cx);
    // 240 of sidebar and 8 of margin leave 752: room for the list beside it,
    // and 4 short of two panes at 240 with the gap.
    cx.simulate_resize(gpui::size(px(1000.), px(600.)));
    press(cx, "cmd-i");
    assert!(cx.debug_bounds("inspector").is_some());
    press(cx, "cmd-alt-s");
    assert!(cx.debug_bounds("inspector").is_none());
    let widths = browser.read_with(cx, |browser, cx| {
        browser.tabs[browser.active]
            .panes()
            .map(|pane| pane.read(cx).chrome.width)
            .collect::<Vec<_>>()
    });
    assert!(widths.iter().all(|width| *width >= 240. - 2.), "{widths:?}");
    // With room for all of it, it comes back.
    cx.simulate_resize(gpui::size(px(1004.), px(600.)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("inspector").is_some());
}

#[gpui::test]
fn an_open_waiting_on_the_disk_gives_way_to_what_came_after(cx: &mut TestAppContext) {
    let (_dir, project, app) = with_package();
    let world = world(Vec::new(), cx);
    let (browser, cx) = browse(&project, cx);
    let row = |name: &str, browser: &Entity<Browser>, cx: &mut VisualTestContext| {
        in_pane(browser, cx, |pane, _| row_of(pane, name))
    };
    let package = row("Tool.app", &browser, cx);
    let docs = row("docs", &browser, cx);
    let zeta = row("zeta", &browser, cx);

    // Selected elsewhere: the app doesn't launch.
    on_pane(&browser, cx, |pane, cx| {
        pane.select_row(package, cx);
        pane.open(package, cx);
        pane.select_row(zeta, cx);
    });
    cx.run_until_parked();
    assert!(world.fake.opened.lock().unwrap().is_empty());

    // Nor does Show Package Contents go into it.
    on_pane(&browser, cx, |pane, cx| {
        pane.select_row(package, cx);
        pane.show_package_contents(cx);
        pane.select_row(zeta, cx);
    });
    cx.run_until_parked();
    in_pane(&browser, cx, |pane, _| assert_eq!(pane.folder(), project));

    // Went elsewhere: it doesn't come back.
    let there = project.join("zeta");
    on_pane(&browser, cx, |pane, cx| {
        pane.select_row(docs, cx);
        pane.open(docs, cx);
        pane.navigate(there.clone(), cx);
    });
    cx.run_until_parked();
    in_pane(&browser, cx, |pane, _| assert_eq!(pane.folder(), there));
    assert!(world.fake.opened.lock().unwrap().is_empty());
    press(cx, "cmd-[");

    // Opened something else: only that one opens.
    let (package, docs) = in_pane(&browser, cx, |pane, _| {
        (row_of(pane, "Tool.app"), row_of(pane, "docs"))
    });
    on_pane(&browser, cx, |pane, cx| {
        pane.select_row(docs, cx);
        pane.open(docs, cx);
        pane.select_row(package, cx);
        pane.open(package, cx);
    });
    cx.run_until_parked();
    in_pane(&browser, cx, |pane, _| assert_eq!(pane.folder(), project));
    assert_eq!(*world.fake.opened.lock().unwrap(), [app]);
}
