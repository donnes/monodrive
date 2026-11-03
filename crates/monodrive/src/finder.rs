//! Standing in for Finder: other apps' Show in Finder, and Open in Monodrive
//! in Finder's right-click menu. Both arrive as files to show, which land in
//! the window in front. `docs/finder.md` has why it works this way.

mod guard;

use crate::browser::Browser;
use crate::{frame, pasteboard, workspace};
use gpui::{App, Global, WindowHandle};
use monodrive_core::apps;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSPasteboard, NSRegisterServicesProvider};
use objc2_core_foundation::{
    CFPreferencesCopyValue, CFPreferencesSetValue, CFPreferencesSynchronize, CFString, CFType,
    kCFPreferencesAnyApplication, kCFPreferencesAnyHost, kCFPreferencesCurrentUser,
};
use objc2_foundation::{
    NSArgumentDomain, NSBundle, NSMutableCopying, NSObject, NSString, NSUserDefaults,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};

/// The global preference naming the app that gets other apps' Show in
/// Finder. Undocumented, and what Path Finder and ForkLift set too.
const VIEWER_KEY: &str = "NSFileViewer";

/// Every Monodrive's bundle ID starts with this: the release, dev and agent
/// builds.
const MONODRIVE_IDS: &str = "dev.donnes.monodrive";

/// Who gets other apps' Show in Finder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Viewer {
    Finder,
    Monodrive,
    /// Another app, by name, or by bundle ID when it isn't installed.
    Other(String),
}

/// Why this copy can't take Show in Finder. A key naming an app that's gone
/// stops Show in Finder in every app, with nothing saying why, so only a
/// copy that stays put takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Not built by `scripts/release`.
    DevBuild,
    /// In Downloads, on a disk image, or anywhere else it gets moved from.
    OutsideApplications,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub viewer: Viewer,
    /// Why switching it on would do nothing. Never set while Monodrive has
    /// it: switching off always works.
    pub blocked: Option<Blocked>,
    /// Why switching it on just now failed.
    pub failed: Option<String>,
}

/// Who has Show in Finder now, read from macOS each time: another app may
/// have taken it since. Asks LaunchServices for that app's name, so call it
/// off the UI thread.
pub fn status() -> Status {
    let viewer = match viewer_key() {
        None => Viewer::Finder,
        Some(id) if Some(&id) == own_id().as_ref() => Viewer::Monodrive,
        Some(id) if id == "com.apple.finder" => Viewer::Finder,
        Some(id) => Viewer::Other(apps::with_bundle_id(&id).map_or(id, |app| app.name)),
    };
    let blocked = (viewer != Viewer::Monodrive).then(blocker).flatten();
    Status {
        viewer,
        blocked,
        failed: None,
    }
}

/// Gives Show in Finder to Monodrive, or hands it back to Finder, and says
/// who has it after. Taking it first adds the login item that hands it back
/// if Monodrive is deleted; switching off removes that item. Call it off the
/// UI thread.
pub fn set(on: bool) -> Status {
    let now = status();
    let guard = own_id().map(|id| guard::Guard::new(id, bundle_path(), &crate::browser::home()));
    let mut failed = None;
    match (write_for(on, &now), &guard) {
        (Some(Write::Take), Some(guard)) => match guard.install() {
            Ok(()) => write_viewer_key(Some(&guard.id)),
            Err(why) => failed = Some(why),
        },
        (Some(Write::Release), _) => write_viewer_key(None),
        _ => {}
    }
    if let (false, Some(guard)) = (on, &guard) {
        guard.remove();
    }
    Status { failed, ..status() }
}

#[derive(Debug, PartialEq, Eq)]
enum Write {
    Take,
    /// Removed rather than set to Finder, which is what macOS does without it.
    Release,
}

/// What switching Show in Finder `on` or off writes. Off never touches
/// another app's key: someone who chose Path Finder keeps it.
fn write_for(on: bool, now: &Status) -> Option<Write> {
    match (on, &now.viewer) {
        (true, Viewer::Monodrive) | (false, Viewer::Finder | Viewer::Other(_)) => None,
        (true, _) => now.blocked.is_none().then_some(Write::Take),
        (false, Viewer::Monodrive) => Some(Write::Release),
    }
}

fn blocker() -> Option<Blocked> {
    if monodrive_core::DEV {
        return Some(Blocked::DevBuild);
    }
    let bundle = bundle_path();
    let user_apps = crate::browser::home().join("Applications");
    let settled = bundle.starts_with("/Applications") || bundle.starts_with(user_apps);
    (!settled).then_some(Blocked::OutsideApplications)
}

fn bundle_path() -> PathBuf {
    PathBuf::from(NSBundle::mainBundle().bundlePath().to_string())
}

fn own_id() -> Option<String> {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|id| id.to_string())
}

/// The key in the global domain, the one `defaults write -g` writes.
fn viewer_key() -> Option<String> {
    // SAFETY: CoreFoundation's constants, read once it's loaded.
    let (any_app, user, any_host) = unsafe {
        (
            kCFPreferencesAnyApplication,
            kCFPreferencesCurrentUser,
            kCFPreferencesAnyHost,
        )
    };
    let value = CFPreferencesCopyValue(&CFString::from_str(VIEWER_KEY), any_app, user, any_host)?;
    let id = value.downcast::<CFString>().ok()?.to_string();
    (!id.is_empty()).then_some(id)
}

/// Sets or removes the key, then saves it at once: until then other apps
/// still read the old value.
fn write_viewer_key(id: Option<&str>) {
    // SAFETY: as in `viewer_key`.
    let (any_app, user, any_host) = unsafe {
        (
            kCFPreferencesAnyApplication,
            kCFPreferencesCurrentUser,
            kCFPreferencesAnyHost,
        )
    };
    let id = id.map(CFString::from_str);
    let value: Option<&CFType> = id.as_deref().map(|id| &**id);
    // SAFETY: a CFString is a property list value, and None removes the key.
    unsafe {
        CFPreferencesSetValue(
            &CFString::from_str(VIEWER_KEY),
            value,
            any_app,
            user,
            any_host,
        );
    }
    CFPreferencesSynchronize(any_app, user, any_host);
}

/// Makes this process's own Reveal in Finder reach Finder while a Monodrive
/// has the key, as it would otherwise come straight back. Naming Finder
/// doesn't work: Finder then opens the file. An empty value in this
/// process's argument domain does a real reveal, after AppKit logs "Finder
/// replacement not found". Never undone, which is right: once another app
/// has the key, Reveal in Finder going to Finder is what it says.
pub fn keep_reveals_in_finder() {
    static ONCE: Once = Once::new();
    if !viewer_key().is_some_and(|id| id.starts_with(MONODRIVE_IDS)) {
        return;
    }
    ONCE.call_once(|| {
        let defaults = NSUserDefaults::standardUserDefaults();
        // SAFETY: Foundation's constant, read once it's loaded.
        let domain = unsafe { NSArgumentDomain };
        // Setting a volatile domain replaces it: keep what the command line set.
        let merged = defaults.volatileDomainForName(domain).mutableCopy();
        let empty = NSString::from_str("");
        let empty: &AnyObject = &empty;
        let key = NSString::from_str(VIEWER_KEY);
        // SAFETY: a defaults domain maps strings to property list values, and
        // a string is one.
        unsafe {
            merged.setObject_forKey(empty, ProtocolObject::from_ref(&*key));
            defaults.setVolatileDomain_forName(&merged, domain);
        }
    });
}

/// Files another app asked to see.
pub struct Request {
    paths: Vec<PathBuf>,
    from: Asker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asker {
    /// Another app's Show in Finder, or `open -a Monodrive <path>`.
    ShowInFinder,
    /// Open in Monodrive, in Finder's right-click menu.
    Service,
}

impl Request {
    /// What an open-documents event carries: one `file://` URL a file.
    /// Monodrive declares no document types, so these come from Show in
    /// Finder or `open`.
    pub fn opened(urls: &[String]) -> Self {
        let paths = urls
            .iter()
            .filter_map(|url| pasteboard::path_of(&NSString::from_str(url)))
            .collect();
        Self {
            paths,
            from: Asker::ShowInFinder,
        }
    }
}

/// Where a request takes the window: a folder, and what to select in it.
#[derive(Debug, PartialEq, Eq)]
pub struct Move {
    pub folder: PathBuf,
    pub select: Vec<PathBuf>,
    pub new_tab: bool,
}

/// Turns what was asked for into moves. A file shows selected in its folder,
/// and a folder opens in a new tab, so where the window was survives. Show
/// in Finder keeps the current tab for files, which share one folder there:
/// files elsewhere are left out. Open in Monodrive opens a tab for each
/// folder the files are in, since they were picked on purpose.
fn plan(items: Vec<(PathBuf, bool)>, from: Asker) -> Vec<Move> {
    let mut files: Vec<Move> = Vec::new();
    let mut folders = Vec::new();
    for (path, is_folder) in items {
        if is_folder {
            folders.push(Move {
                folder: path,
                select: Vec::new(),
                new_tab: true,
            });
            continue;
        }
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            continue;
        };
        match files.iter_mut().find(|group| group.folder == parent) {
            Some(group) => group.select.push(path),
            None => files.push(Move {
                folder: parent,
                select: vec![path],
                new_tab: from == Asker::Service,
            }),
        }
    }
    if from == Asker::ShowInFinder {
        files.truncate(1);
    }
    files.extend(folders);
    files
}

/// Whether each path opens as a folder. An app or other package, or a link
/// to a folder, shows selected in its folder, as Finder does. Reads the
/// disk, so call it off the UI thread.
fn sort_out(paths: Vec<PathBuf>) -> Vec<(PathBuf, bool)> {
    paths
        .into_iter()
        .map(|path| {
            let is_folder = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir())
                && !workspace::is_package(&path);
            (path, is_folder)
        })
        .collect()
}

/// Moves waiting for a browser window, as on a launch that onboarding holds
/// back.
#[derive(Default)]
struct Waiting(Vec<Move>);
impl Global for Waiting {}

/// Lands each request that `arrived` in the window in front, and answers
/// Open in Monodrive. `requests` is the sending end, for the service.
pub fn serve(
    requests: async_channel::Sender<Request>,
    arrived: async_channel::Receiver<Request>,
    cx: &mut App,
) {
    offer_service(requests);
    cx.spawn(async move |cx| {
        while let Ok(request) = arrived.recv().await {
            let from = request.from;
            let items = cx
                .background_executor()
                .spawn(async move { sort_out(request.paths) })
                .await;
            cx.update(|cx| {
                cx.default_global::<Waiting>().0.extend(plan(items, from));
                deliver(cx);
            });
        }
    })
    .detach();
}

/// Lands what's waiting in the window in front, and brings it forward. Call
/// it again once a window opens.
pub fn deliver(cx: &mut App) {
    let front = cx
        .active_window()
        .and_then(|window| window.downcast::<Browser>())
        .or_else(|| cx.windows().iter().find_map(|window| window.downcast()));
    let Some(front): Option<WindowHandle<Browser>> = front else {
        return;
    };
    let moves = std::mem::take(&mut cx.default_global::<Waiting>().0);
    if moves.is_empty() {
        return;
    }
    // A command may be running inside that window's own update.
    cx.defer(move |cx| {
        let _ = front.update(cx, |browser, window, cx| {
            for to in moves {
                browser.land(to, window, cx);
            }
            window.activate_window();
        });
        frame::bring_forward(cx);
    });
}

static SERVICE_REQUESTS: OnceLock<async_channel::Sender<Request>> = OnceLock::new();

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Service`
    // doesn't implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MonodriveOpenService"]
    struct Service;

    unsafe impl NSObjectProtocol for Service {}

    impl Service {
        /// `NSMessage` in the bundle's Info.plist, and the arguments every
        /// service method takes.
        #[unsafe(method(openInMonodrive:userData:error:))]
        fn open(&self, pasteboard: &NSPasteboard, _: Option<&NSString>, _: *mut *mut NSString) {
            let paths = pasteboard::files_on(pasteboard);
            if let (false, Some(requests)) = (paths.is_empty(), SERVICE_REQUESTS.get()) {
                let _ = requests.try_send(Request {
                    paths,
                    from: Asker::Service,
                });
            }
        }
    }
);

thread_local! {
    /// The service's provider, kept for as long as the app runs.
    static PROVIDER: RefCell<Option<Retained<Service>>> = const { RefCell::new(None) };
}

/// Answers Open in Monodrive under the bundle ID, `NSPortName` in the
/// Info.plist: release and dev builds can run side by side, each with its
/// own menu item. A binary outside a bundle has no Info.plist to offer it.
fn offer_service(requests: async_channel::Sender<Request>) {
    let (Some(main), Some(id)) = (MainThreadMarker::new(), own_id()) else {
        return;
    };
    let _ = SERVICE_REQUESTS.set(requests);
    // SAFETY: NSObject's plain init.
    let service: Retained<Service> = unsafe { msg_send![Service::alloc(main), init] };
    // SAFETY: `PROVIDER` keeps it alive for as long as AppKit may call it.
    unsafe { NSRegisterServicesProvider(Some(&service), &NSString::from_str(&id)) };
    PROVIDER.with_borrow_mut(|provider| *provider = Some(service));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(paths: &[(&str, bool)]) -> Vec<(PathBuf, bool)> {
        paths
            .iter()
            .map(|(path, folder)| (PathBuf::from(path), *folder))
            .collect()
    }

    fn to(folder: &str, select: &[&str], new_tab: bool) -> Move {
        Move {
            folder: folder.into(),
            select: select.iter().map(PathBuf::from).collect(),
            new_tab,
        }
    }

    #[test]
    fn show_in_finder_selects_files_here_and_opens_folders_in_tabs() {
        let asked = items(&[
            ("/a/one.txt", false),
            ("/b/docs", true),
            ("/a/two.txt", false),
            ("/c/three.txt", false),
        ]);
        assert_eq!(
            plan(asked, Asker::ShowInFinder),
            [
                to("/a", &["/a/one.txt", "/a/two.txt"], false),
                to("/b/docs", &[], true),
            ]
        );
    }

    #[test]
    fn open_in_monodrive_gives_each_folder_a_tab() {
        let asked = items(&[
            ("/a/one.txt", false),
            ("/c/three.txt", false),
            ("/b/docs", true),
        ]);
        assert_eq!(
            plan(asked, Asker::Service),
            [
                to("/a", &["/a/one.txt"], true),
                to("/c", &["/c/three.txt"], true),
                to("/b/docs", &[], true),
            ]
        );
    }

    #[test]
    fn switching_off_leaves_another_apps_choice_alone() {
        let status = |viewer, blocked| Status {
            viewer,
            blocked,
            failed: None,
        };
        let other = || Viewer::Other("Path Finder".into());
        assert_eq!(write_for(false, &status(other(), None)), None);
        assert_eq!(write_for(false, &status(Viewer::Finder, None)), None);
        assert_eq!(
            write_for(false, &status(Viewer::Monodrive, None)),
            Some(Write::Release)
        );
        assert_eq!(write_for(true, &status(other(), None)), Some(Write::Take));
        assert_eq!(
            write_for(true, &status(Viewer::Finder, Some(Blocked::DevBuild))),
            None
        );
    }
}
