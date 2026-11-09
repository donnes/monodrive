//! Downloading from the window, with macOS made up through `Native`. A real
//! placeholder needs a provider, so a test marks a listed entry as one, as
//! its flag would; listing it again reads the disk, as after a download.

use super::inspector::READ_DELAY;
use super::inspector_tests::Fake;
use super::places_tests::world;
use super::tests::{click, folder, in_pane, on_pane, press, shown, toast};
use super::*;
use crate::downloads;
use crate::native::MadeUp;
use gpui::{Action, TestAppContext, VisualTestContext};
use monodrive_core::cloud::{Availability, CloudFolder, Progress};

struct Cloud<'a> {
    browser: Entity<Browser>,
    cx: &'a mut VisualTestContext,
    macos: Arc<Fake>,
    places: Arc<super::places_tests::Fake>,
    project: PathBuf,
    _dir: tempfile::TempDir,
    _world: super::places_tests::World,
}

/// `folder()` in a window, with `project` as a cloud folder and macOS made up.
fn cloud(cx: &mut TestAppContext) -> Cloud<'_> {
    let (dir, project) = folder();
    let world = world(Vec::new(), cx);
    *world.fake.cloud.lock().unwrap() = vec![CloudFolder {
        name: "iCloud Drive".into(),
        path: project.clone(),
    }];
    let macos = Arc::new(Fake::default());
    let native = macos.clone();
    cx.update(|cx| cx.set_global(MadeUp(native)));
    let places = world.fake.clone();
    let (browser, cx) = super::tests::browse(&project, cx);
    Cloud {
        browser,
        cx,
        macos,
        places,
        project,
        _dir: dir,
        _world: world,
    }
}

impl Cloud<'_> {
    /// Marks `name` as a placeholder, in the listing and in what macOS says.
    fn placeholder(&mut self, name: &str) -> PathBuf {
        let path = self.project.join(name);
        self.macos.placeholders.lock().unwrap().insert(path.clone());
        on_pane(&self.browser, self.cx, |pane, cx| {
            let entry = pane.entries.iter_mut().find(|e| e.name == name).unwrap();
            entry.availability = Availability::Placeholder;
            cx.notify();
        });
        self.draw();
        path
    }

    fn draw(&mut self) {
        self.cx.update(|window, _| window.refresh());
        self.cx.run_until_parked();
    }

    /// One check of what's downloading.
    fn check(&mut self) {
        self.cx.executor().advance_clock(downloads::CHECK);
        self.cx.run_until_parked();
        self.draw();
    }

    fn shows(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    fn can(&mut self, action: &dyn Action) -> bool {
        self.cx
            .update(|window, cx| window.is_action_available(action, cx))
    }

    fn asked(&self) -> Vec<PathBuf> {
        self.macos.downloads.lock().unwrap().clone()
    }
}

#[gpui::test]
fn the_badge_downloads_shows_it_going_and_goes_once_its_here(cx: &mut TestAppContext) {
    let mut t = cloud(cx);
    let a = t.placeholder("a.txt");
    assert!(t.shows("cloud-badge-2"));
    click(t.cx, "cloud-badge-2");
    assert_eq!(t.asked(), std::slice::from_ref(&a));
    // The press was the badge's: nothing was selected or opened.
    assert_eq!(shown(&t.browser, t.cx).1, Vec::<String>::new());
    assert!(t.places.opened.lock().unwrap().is_empty());
    t.draw();
    assert!(t.shows("cloud-downloading"));
    assert!(!t.shows("cloud-badge-2"));
    // Still on its way: it stays so, and nothing is asked again.
    t.check();
    assert!(t.shows("cloud-downloading"));
    assert_eq!(t.asked(), std::slice::from_ref(&a));
    // Here: the folder lists again and reads the file as it is on disk.
    t.macos.progress.lock().unwrap().insert(a, Progress::Done);
    t.check();
    assert!(!t.shows("cloud-downloading"));
    assert!(!t.shows("cloud-badge-2"));
    let local = in_pane(&t.browser, t.cx, |pane, _| pane.entries[2].availability);
    assert_eq!(local, Availability::Local);
}

#[gpui::test]
fn a_download_that_fails_says_why_and_tries_again(cx: &mut TestAppContext) {
    let mut t = cloud(cx);
    let a = t.placeholder("a.txt");
    click(t.cx, "cloud-badge-2");
    // The connection drops partway: the provider's reason, and Try Again.
    let offline = "The Internet connection appears to be offline.";
    let progress = Progress::Failed(offline.into());
    t.macos.progress.lock().unwrap().insert(a.clone(), progress);
    t.check();
    assert_eq!(
        toast(&t.browser, t.cx),
        Some((
            format!("Couldn't download \"a.txt\": {offline}"),
            true,
            true
        ))
    );
    assert!(!t.shows("cloud-downloading"));
    assert!(t.shows("cloud-badge-2"));
    // The toast's Try Again asks again, and this time macOS refuses outright.
    t.macos.progress.lock().unwrap().clear();
    let denied = format!("{}: Permission denied (os error 13)", a.display());
    t.macos.refuse.lock().unwrap().insert(a.clone(), denied);
    click(t.cx, "toast-offer");
    assert_eq!(t.asked(), [a.clone(), a.clone()]);
    assert_eq!(
        toast(&t.browser, t.cx),
        Some((
            "Couldn't download \"a.txt\": a.txt: Permission denied (os error 13)".into(),
            true,
            true
        ))
    );
    // The badge's Try Again asks once more.
    t.draw();
    click(t.cx, "cloud-badge-2");
    assert_eq!(t.asked().len(), 3);
}

#[gpui::test]
fn download_and_remove_download_run_only_where_they_apply(cx: &mut TestAppContext) {
    let mut t = cloud(cx);
    press(t.cx, "alt-up down down");
    assert_eq!(shown(&t.browser, t.cx).1, ["a.txt"]);
    // Downloaded and in a cloud folder.
    assert!(!t.can(&Download));
    assert!(t.can(&RemoveDownload));
    t.cx.dispatch_action(RemoveDownload);
    t.cx.run_until_parked();
    let a = t.project.join("a.txt");
    assert_eq!(*t.macos.removed.lock().unwrap(), std::slice::from_ref(&a));
    assert_eq!(
        toast(&t.browser, t.cx),
        Some(("Removed the download of \"a.txt\"".into(), false, true))
    );
    // The way back.
    click(t.cx, "toast-offer");
    assert_eq!(t.asked(), std::slice::from_ref(&a));
    t.macos.progress.lock().unwrap().insert(a, Progress::Done);
    t.check();
    // Not downloaded: the reverse.
    t.placeholder("a.txt");
    press(t.cx, "alt-up down down");
    assert!(t.can(&Download));
    assert!(!t.can(&RemoveDownload));
    // The cloud folder itself is never offered.
    press(t.cx, "cmd-up");
    on_pane(&t.browser, t.cx, |pane, cx| {
        let project = pane.entries.iter().find(|e| e.name == "project").unwrap();
        pane.selection.clear();
        pane.selection.select(&project.clone());
        cx.notify();
    });
    t.draw();
    assert!(!t.can(&RemoveDownload));
}

#[gpui::test]
fn opening_what_isnt_downloaded_downloads_it_then_opens_it(cx: &mut TestAppContext) {
    let mut t = cloud(cx);
    let a = t.placeholder("a.txt");
    press(t.cx, "alt-up down down cmd-i");
    t.cx.executor().advance_clock(READ_DELAY);
    t.cx.run_until_parked();
    t.draw();
    assert!(t.shows("inspector-cloud"));
    press(t.cx, "cmd-down");
    assert_eq!(t.asked(), std::slice::from_ref(&a));
    assert!(t.places.opened.lock().unwrap().is_empty());
    assert_eq!(
        toast(&t.browser, t.cx).map(|t| t.0),
        Some("Downloading \"a.txt\" to open it".into())
    );
    // Opening again while it's on its way asks nothing more.
    press(t.cx, "cmd-down");
    assert_eq!(t.asked(), std::slice::from_ref(&a));
    t.macos
        .progress
        .lock()
        .unwrap()
        .insert(a.clone(), Progress::Done);
    t.check();
    assert_eq!(*t.places.opened.lock().unwrap(), [a]);
}

#[gpui::test]
fn opening_a_package_that_isnt_downloaded_downloads_it_first(cx: &mut TestAppContext) {
    let mut t = cloud(cx);
    // A document package, as Pages and Keynote save, which opens in its app.
    std::fs::create_dir(t.project.join("Deck.app")).unwrap();
    on_pane(&t.browser, t.cx, |pane, cx| pane.reload(cx));
    t.cx.run_until_parked();
    let deck = t.placeholder("Deck.app");
    press(t.cx, "alt-up");
    assert_eq!(shown(&t.browser, t.cx).1, ["Deck.app"]);
    press(t.cx, "cmd-down");
    assert_eq!(t.asked(), std::slice::from_ref(&deck));
    assert!(t.places.opened.lock().unwrap().is_empty());
    t.macos
        .progress
        .lock()
        .unwrap()
        .insert(deck.clone(), Progress::Done);
    t.check();
    assert_eq!(*t.places.opened.lock().unwrap(), [deck]);
}
