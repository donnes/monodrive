//! What the app reaches outside itself: `Places`, through which views reach
//! the saved places, the drives and the servers, and how each server's
//! connection is doing.

use crate::home;
use gpui::BorrowAppContext;
use gpui::{App, Global, Task};
use monodrive_core::Origin;
use monodrive_core::cloud::{self, Availability, CloudFolder, Quota};
use monodrive_core::entries::{self, Entry, Kind};
use monodrive_core::index::{Caps, Found, Index};
use monodrive_core::ops;
use monodrive_core::places::Store;
use monodrive_core::remote::{Connection, Connections, RemoteError};
use monodrive_core::search::Search;
use monodrive_core::servers::{self, Cancel, Prompt};
use monodrive_core::volumes::{self, Volume};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// What the system does for the window. Views reach it through `Places`, so
/// their tests list a made-up set of volumes and never mount or eject
/// anything real.
pub trait Drives: Send + Sync {
    /// What's mounted. Can wait on a slow share, so call it off the UI thread.
    fn list(&self) -> Vec<Volume>;
    fn eject(&self, mount: &Path, ops: &mut ops::Client) -> Result<String, String>;
    /// Waits for macOS and any prompt, so call it off the UI thread.
    fn mount(&self, address: &str, cancel: &Arc<Cancel>) -> Result<PathBuf, String>;
    /// Opens a file in its app.
    fn open(&self, path: &Path, cx: &App);
    /// Whether opening this folder could show a privacy prompt. Call it off
    /// the UI thread.
    fn needs_consent(&self, path: &Path) -> bool;
    /// Which volume holds where `path` leads, by the device its folder is on.
    /// Call it off the UI thread.
    fn device(&self, path: &Path) -> Option<u64> {
        std::fs::metadata(path).ok().map(|meta| meta.dev())
    }
    /// The cloud folders on this Mac.
    fn cloud(&self) -> Vec<CloudFolder>;
    /// What's left in `folder`'s cloud account, if its provider says. Can
    /// wait on the provider, so call it off the UI thread.
    fn cloud_free(&self, folder: &CloudFolder) -> Option<Quota>;
    /// What the index finds for `query`, once it has started. Nothing starts
    /// it here. Call it off the UI thread.
    fn find_files(
        &self,
        index: &OnceLock<Result<Index, String>>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Entry>, String>;
    /// What a search in the window finds, through `started`, which starts the
    /// index if it may. Reads the disk, so call it off the UI thread.
    fn search<'a>(
        &self,
        started: &dyn Fn() -> Result<&'a Index, String>,
        search: &Search,
        limit: usize,
    ) -> Result<Found, String>;
}

/// The real thing.
pub struct System;

impl Drives for System {
    fn list(&self) -> Vec<Volume> {
        volumes::list()
    }

    fn eject(&self, mount: &Path, ops: &mut ops::Client) -> Result<String, String> {
        ops.eject(mount, &Origin::Window)
    }

    fn mount(&self, address: &str, cancel: &Arc<Cancel>) -> Result<PathBuf, String> {
        servers::mount(address, Prompt::Allow, cancel)
    }

    fn open(&self, path: &Path, cx: &App) {
        cx.open_with_system(path);
    }

    fn needs_consent(&self, path: &Path) -> bool {
        monodrive_core::sizes::needs_consent(path)
    }

    fn cloud(&self) -> Vec<CloudFolder> {
        cloud::folders(&home())
    }

    fn cloud_free(&self, folder: &CloudFolder) -> Option<Quota> {
        cloud::free_space(folder)
    }

    fn find_files(
        &self,
        index: &OnceLock<Result<Index, String>>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Entry>, String> {
        match index.get() {
            Some(Ok(index)) => index.find(query, limit),
            _ => Ok(Vec::new()),
        }
    }

    fn search<'a>(
        &self,
        started: &dyn Fn() -> Result<&'a Index, String>,
        search: &Search,
        limit: usize,
    ) -> Result<Found, String> {
        started()?.run(search, limit, Caps::default())
    }
}

/// Where the window's places come from. `run` sets it; without it, as in
/// benchmarks, the sidebar lists nothing and reads nothing.
#[derive(Clone)]
pub struct Places {
    pub store: Store,
    /// The Trash the window shows and Empty Trash erases.
    pub trash: PathBuf,
    pub drives: Arc<dyn Drives>,
    /// The folders of apps, whose dotfiles are macOS's and not the person's.
    pub applications: Vec<PathBuf>,
    /// This app's connections to saved SFTP servers.
    pub remote: Arc<Connections>,
    /// Where files from servers come down to open, removed as the app quits.
    pub previews: PathBuf,
    /// Where the startup disk's own folders, such as Applications, are: `/`,
    /// except on a made-up Mac.
    pub root: PathBuf,
}

impl Global for Places {}

impl Places {
    /// The folders that never show their hidden files, whatever the setting.
    pub(crate) fn tidy_folders(&self) -> Vec<PathBuf> {
        std::iter::once(self.trash.clone())
            .chain(self.applications.iter().cloned())
            .collect()
    }
}

pub(crate) fn label_of(path: &Path) -> String {
    if path == home() {
        return "Home".into();
    }
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub(crate) fn glyph_of(path: &Path) -> &'static str {
    if path == home().join("Downloads") {
        "arrow-down-circle"
    } else if path == Path::new("/Applications") {
        "app-window"
    } else {
        "folder-01"
    }
}

/// The rows for `recents`, newest first. One that's gone stays, marked.
pub(crate) fn recent_entries(recents: Vec<PathBuf>) -> (Vec<Entry>, HashSet<PathBuf>) {
    let mut missing = HashSet::new();
    let rows = recents
        .into_iter()
        .map(|path| {
            entries::entry_at(&path).unwrap_or_else(|_| {
                missing.insert(path.clone());
                Entry {
                    name: path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    ),
                    path,
                    kind: Kind::Other,
                    size: 0,
                    modified: 0,
                    availability: Availability::Local,
                }
            })
        })
        .collect();
    (rows, missing)
}

/// How a server's connection is doing, as this app last saw it.
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Connecting,
    Connected,
    Failed(RemoteError),
}

impl Link {
    /// One line for a sidebar row or the palette.
    pub fn short(&self) -> &'static str {
        match self {
            Link::Connecting => "Connecting…",
            Link::Connected => "Connected",
            Link::Failed(error) => short_reason(error),
        }
    }
}

/// What a failure is called where there's room for a few words.
pub fn short_reason(error: &RemoteError) -> &'static str {
    match error {
        RemoteError::HostKeyChanged(_) => "Host key changed",
        RemoteError::HostKeyUnknown(_) => "Host key not known",
        RemoteError::InteractiveAuth(_) => "Needs a key in ssh-agent",
        RemoteError::AuthFailed(..) => "Authentication failed",
        RemoteError::Unreachable(..) | RemoteError::Timeout(_) => "Didn't answer",
        RemoteError::NoSftp(_) => "No SFTP",
        RemoteError::UnknownSource(_) => "Not saved",
        _ => "Couldn't connect",
    }
}

/// Each saved server's connection in this app, by source id. A server with
/// none isn't connected.
#[derive(Default)]
pub struct Servers(pub HashMap<String, Link>);

impl Global for Servers {}

pub fn link<'a>(id: &str, cx: &'a App) -> Option<&'a Link> {
    cx.try_global::<Servers>()
        .and_then(|servers| servers.0.get(id))
}

fn set_link(id: &str, link: Option<Link>, cx: &mut App) {
    let servers = &mut cx.default_global::<Servers>().0;
    if servers.get(id) == link.as_ref() {
        return;
    }
    match link {
        Some(link) => servers.insert(id.to_owned(), link),
        None => servers.remove(id),
    };
    // Tell whoever draws it; `default_global` alone doesn't.
    cx.update_global::<Servers, _>(|_, _| {});
}

/// The connection to source `id`, opened off the UI thread if it has none,
/// with `Servers` following it.
pub fn reach(id: &str, cx: &mut App) -> Task<Result<Arc<Connection>, RemoteError>> {
    let Some(connections) = cx.try_global::<Places>().map(|p| p.remote.clone()) else {
        return Task::ready(Err(RemoteError::UnknownSource(id.into())));
    };
    if link(id, cx) != Some(&Link::Connected) {
        set_link(id, Some(Link::Connecting), cx);
    }
    let id = id.to_owned();
    let work = {
        let id = id.clone();
        cx.background_executor()
            .spawn(async move { connections.connect(&id) })
    };
    cx.spawn(async move |cx| {
        let result = work.await;
        let link = match &result {
            Ok(_) => Some(Link::Connected),
            Err(RemoteError::UnknownSource(_) | RemoteError::Cancelled) => None,
            Err(error) => Some(Link::Failed(error.clone())),
        };
        cx.update(|cx| set_link(&id, link, cx));
        result
    })
}

/// Ends the app's connection to `id`. Nothing on the server changes, and a
/// copy the owner is making keeps its own connection.
pub fn disconnect(id: &str, cx: &mut App) {
    if let Some(connections) = cx.try_global::<Places>().map(|p| p.remote.clone()) {
        let id = id.to_owned();
        cx.background_executor()
            .spawn(async move { connections.disconnect(&id) })
            .detach();
    }
    set_link(id, None, cx);
}
