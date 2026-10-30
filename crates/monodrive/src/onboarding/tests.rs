use super::*;
use crate::browser::{Browser, SharedIndex};
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A Mac where nothing real is read or changed, and that says what was asked.
#[derive(Default)]
pub(crate) struct Fake {
    pub granted: AtomicBool,
    pub saved: bool,
    pub starts: AtomicUsize,
    pub opened_settings: AtomicUsize,
    pub relaunches: AtomicUsize,
    pub detected: Vec<Harness>,
    pub applied: Mutex<Vec<(Harness, Action)>>,
}

impl Setup for Fake {
    fn full_disk_access(&self) -> bool {
        self.granted.load(Ordering::SeqCst)
    }

    fn index_lacks_access(&self) -> bool {
        false
    }

    fn saved_index(&self) -> bool {
        self.saved
    }

    fn start_index(&self) -> Result<Index, String> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Err("Nothing here starts.".into())
    }

    fn open_privacy_settings(&self, _: &App) {
        self.opened_settings.fetch_add(1, Ordering::SeqCst);
    }

    fn app(&self) -> PathBuf {
        "/Applications/Monodrive.app".into()
    }

    fn reveal(&self, _: &Path, _: &App) {}

    fn relaunch(&self, _: &mut App) {
        self.relaunches.fetch_add(1, Ordering::SeqCst);
    }

    fn agents(&self) -> (Vec<Harness>, Vec<Harness>) {
        (self.detected.clone(), Vec::new())
    }

    fn apply_agent(&self, harness: Harness, action: Action) -> Result<(), String> {
        self.applied.lock().unwrap().push((harness, action));
        Ok(())
    }
}

/// Launches as `run` does with `fake` and in-memory preferences, saying how
/// many times the main windows opened.
fn launched(fake: &Arc<Fake>, folder: bool, cx: &mut TestAppContext) -> Rc<Cell<usize>> {
    let main = Rc::new(Cell::new(0));
    let opened = main.clone();
    let fake = fake.clone();
    cx.update(|cx| {
        if !cx.has_global::<Preferences>() {
            cx.set_global(Preferences::default());
            cx.set_global(SharedIndex(Arc::default()));
            commands::apply(cx);
            crate::window_commands(cx);
        }
        launch(folder, fake, move |_| opened.set(opened.get() + 1), cx);
    });
    cx.run_until_parked();
    main
}

/// The onboarding window, ready for keys and clicks.
fn flow(cx: &mut TestAppContext) -> (WindowHandle<Onboarding>, &mut VisualTestContext) {
    let window = cx.update(|cx| cx.global::<Flow>().window);
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    cx.run_until_parked();
    (window, cx)
}

fn step(window: WindowHandle<Onboarding>, cx: &mut VisualTestContext) -> Step {
    window.read_with(cx, |view, _| view.step).unwrap()
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds(selector);
    let bounds = bounds.unwrap_or_else(|| panic!("{selector} isn't on screen"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn press(keys: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes(keys);
    cx.run_until_parked();
}

fn on_screen(selector: &'static str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(selector).is_some()
}

fn version(cx: &mut VisualTestContext) -> u32 {
    cx.update(|_, cx| cx.global::<Preferences>().onboarding_version)
}

fn no_grant() -> Arc<Fake> {
    Arc::new(Fake::default())
}

#[gpui::test]
fn return_walks_the_steps_and_escape_only_goes_back(cx: &mut TestAppContext) {
    let fake = no_grant();
    let main = launched(&fake, false, cx);
    assert_eq!(main.get(), 0, "no main window behind onboarding");
    let (window, cx) = flow(cx);
    assert_eq!(step(window, cx), Step::Welcome);

    // Escape has nowhere to go from the first step, and doesn't close anything.
    press("escape", cx);
    assert_eq!(step(window, cx), Step::Welcome);

    press("enter", cx);
    assert_eq!(step(window, cx), Step::FullDiskAccess);
    press("escape", cx);
    assert_eq!(step(window, cx), Step::Welcome);
    press("enter", cx);

    // Without the access, ↩ goes for the access rather than past it.
    press("enter", cx);
    assert_eq!(step(window, cx), Step::FullDiskAccess);
    assert_eq!(fake.opened_settings.load(Ordering::SeqCst), 1);

    click("onboarding-not-now", cx);
    assert_eq!(step(window, cx), Step::Agents);
    press("enter", cx);
    assert_eq!(step(window, cx), Step::Ready);
    click("onboarding-back", cx);
    assert_eq!(step(window, cx), Step::Agents);
    let skipped = window
        .read_with(cx, |view, _| view.skipped.clone())
        .unwrap();
    assert_eq!(skipped, [Step::FullDiskAccess, Step::Agents]);
}

#[gpui::test]
fn the_grant_shows_on_returning_to_the_window_and_lets_it_continue(cx: &mut TestAppContext) {
    let fake = no_grant();
    launched(&fake, false, cx);
    let (window, cx) = flow(cx);
    press("enter", cx);
    assert!(!on_screen("access-granted", cx));

    click("onboarding-primary", cx);
    cx.deactivate_window();
    fake.granted.store(true, Ordering::SeqCst);
    // Not until the window is back: nothing polls.
    assert!(!on_screen("access-granted", cx));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert!(on_screen("access-granted", cx));
    assert!(!on_screen("onboarding-not-now", cx));

    press("enter", cx);
    assert_eq!(step(window, cx), Step::Agents);
}

#[gpui::test]
fn after_opening_settings_with_no_grant_it_offers_to_reopen(cx: &mut TestAppContext) {
    let fake = no_grant();
    launched(&fake, false, cx);
    let (_, cx) = flow(cx);
    press("enter", cx);
    assert!(!on_screen("access-reopen", cx));
    press("enter", cx);
    assert!(on_screen("access-reopen", cx));
    click("access-reopen", cx);
    assert_eq!(fake.relaunches.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn finishing_is_saved_opens_the_windows_and_isnt_shown_again(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake {
        granted: AtomicBool::new(true),
        ..Fake::default()
    });
    let main = launched(&fake, false, cx);
    let (window, visual) = flow(cx);
    // Already granted: past what there's nothing to ask about.
    assert_eq!(step(window, visual), Step::Agents);
    press("enter", visual);
    assert_eq!(step(window, visual), Step::Ready);
    press("enter", visual);
    assert_eq!(main.get(), 1);
    assert_eq!(
        cx.update(|cx| cx.global::<Preferences>().onboarding_version),
        VERSION
    );
    assert!(cx.update(|cx| !cx.has_global::<Flow>()));

    let next = launched(&fake, false, cx);
    assert_eq!(
        next.get(),
        1,
        "the next launch goes straight to the windows"
    );
    assert!(cx.update(|cx| !cx.has_global::<Flow>()));

    // Show Welcome brings it back, and a second time finds the same window.
    cx.update(|cx| show(Step::Welcome, cx));
    let (window, visual) = flow(cx);
    assert_eq!(step(window, visual), Step::Welcome);
    let windows = visual.update(|_, cx| cx.windows().len());
    visual.update(|_, cx| show(Step::FullDiskAccess, cx));
    visual.run_until_parked();
    assert_eq!(step(window, visual), Step::FullDiskAccess);
    assert_eq!(visual.update(|_, cx| cx.windows().len()), windows);
}

#[gpui::test]
fn closing_with_command_w_opens_the_windows_and_asks_again_next_time(cx: &mut TestAppContext) {
    let fake = no_grant();
    let main = launched(&fake, false, cx);
    let (_, visual) = flow(cx);
    press("cmd-w", visual);
    assert_eq!(main.get(), 1);
    assert!(cx.update(|cx| !cx.has_global::<Flow>()));
    assert_eq!(
        cx.update(|cx| cx.global::<Preferences>().onboarding_version),
        0
    );
    assert!(!cx.update(|cx| cx.global::<IndexGate>().held()));

    launched(&fake, false, cx);
    assert!(cx.update(|cx| cx.has_global::<Flow>()));
}

#[gpui::test]
fn a_folder_on_the_command_line_skips_onboarding(cx: &mut TestAppContext) {
    let fake = no_grant();
    let main = launched(&fake, true, cx);
    assert_eq!(main.get(), 1);
    assert!(cx.update(|cx| !cx.has_global::<Flow>()));
    assert!(!cx.update(|cx| cx.global::<IndexGate>().held()));
    assert_eq!(
        cx.update(|cx| cx.global::<Preferences>().onboarding_version),
        0
    );
}

/// A window on a temp folder, starting the index as `open_window` does.
fn browser_starting_the_index(folder: &Path, cx: &mut TestAppContext) {
    let folder = folder.to_path_buf();
    cx.add_window(|_, cx| {
        let mut browser = Browser::new(folder, cx);
        browser.start_index(cx);
        browser
    });
    cx.run_until_parked();
}

#[gpui::test]
fn with_no_saved_index_the_crawl_waits_for_the_access_step(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fake = no_grant();
    launched(&fake, false, cx);
    assert!(cx.update(|cx| cx.global::<IndexGate>().held()));

    // A window opened meanwhile doesn't start it either.
    browser_starting_the_index(dir.path(), cx);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 0);

    let (window, visual) = flow(cx);
    press("enter", visual);
    assert_eq!(step(window, visual), Step::FullDiskAccess);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 0);

    click("onboarding-not-now", visual);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn with_a_saved_index_a_window_starts_it_at_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        saved: true,
        ..Fake::default()
    });
    launched(&fake, false, cx);
    assert!(!cx.update(|cx| cx.global::<IndexGate>().held()));
    browser_starting_the_index(dir.path(), cx);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn closing_during_the_wait_lets_the_crawl_go(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fake = no_grant();
    launched(&fake, false, cx);
    browser_starting_the_index(dir.path(), cx);
    let (_, visual) = flow(cx);
    press("cmd-w", visual);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn the_agents_step_writes_nothing_until_a_switch_is_pressed(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake {
        granted: AtomicBool::new(true),
        detected: vec![Harness::Cursor, Harness::Codex],
        ..Fake::default()
    });
    launched(&fake, false, cx);
    let (window, cx) = flow(cx);
    assert_eq!(step(window, cx), Step::Agents);
    assert!(fake.applied.lock().unwrap().is_empty());

    click("agent-switch-Cursor", cx);
    assert_eq!(
        *fake.applied.lock().unwrap(),
        [(Harness::Cursor, Action::Add)]
    );
    // And pressing it again takes it out.
    click("agent-switch-Cursor", cx);
    assert_eq!(
        fake.applied.lock().unwrap().last(),
        Some(&(Harness::Cursor, Action::Remove))
    );
    // Back to none, so moving on counts as skipping.
    press("enter", cx);
    let skipped = window
        .read_with(cx, |view, _| view.skipped.clone())
        .unwrap();
    assert_eq!(skipped, [Step::Agents]);
    assert_eq!(fake.applied.lock().unwrap().len(), 2);
    assert_eq!(version(cx), 0);
}

#[test]
fn the_app_is_the_bundle_a_binary_runs_from() {
    assert_eq!(
        app_of(Path::new(
            "/Applications/Monodrive.app/Contents/MacOS/monodrive"
        )),
        Path::new("/Applications/Monodrive.app")
    );
    assert_eq!(
        app_of(Path::new("/repo/target/debug/monodrive")),
        Path::new("/repo/target/debug/monodrive")
    );
}
