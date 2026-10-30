//! What each window had open, saved as JSON in the support folder so a relaunch
//! comes back to it. Only `run` turns saving on, so tests and `monodrive <folder>`
//! leave the file as it is. A file from another version or one that doesn't read
//! means starting fresh, and saving replaces it.

use crate::preferences::{Frame, View};
use gpui::{App, Global, Task};
use monodrive_core::entries::Sort;
use monodrive_core::places::Favorite;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

const VERSION: u32 = 1;
const SAVE_DELAY: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    version: u32,
    pub windows: Vec<WindowState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub tabs: Vec<TabState>,
    pub active: usize,
    pub frame: Option<Frame>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    /// One or two.
    pub panes: Vec<PaneState>,
    pub second_focused: bool,
    pub ratio: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    pub history: Vec<StopState>,
    pub at: usize,
    pub view: View,
    pub sort: Sort,
}

/// A place in a pane's history. The folder is kept as a favorite is, by volume
/// identity, so it's never another drive's that took the path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StopState {
    pub place: Favorite,
    pub recents: bool,
}

impl Session {
    pub fn of(windows: Vec<WindowState>) -> Self {
        Self {
            version: VERSION,
            windows,
        }
    }
}

/// `bytes` as a session, with anything that can't be opened left out; `None` if
/// nothing can be.
pub fn decode(bytes: &[u8]) -> Option<Session> {
    let mut session: Session = serde_json::from_slice(bytes).ok()?;
    if session.version != VERSION {
        return None;
    }
    for window in &mut session.windows {
        for tab in &mut window.tabs {
            tab.panes.retain(|pane| !pane.history.is_empty());
            tab.panes.truncate(2);
            for pane in &mut tab.panes {
                pane.at = pane.at.min(pane.history.len() - 1);
            }
            tab.second_focused &= tab.panes.len() == 2;
            tab.ratio = if tab.ratio.is_finite() {
                tab.ratio.clamp(0.1, 0.9)
            } else {
                0.5
            };
        }
        window.tabs.retain(|tab| !tab.panes.is_empty());
        window.active = window.active.min(window.tabs.len().saturating_sub(1));
    }
    session.windows.retain(|window| !window.tabs.is_empty());
    (!session.windows.is_empty()).then_some(session)
}

pub fn load(path: &Path) -> Option<Session> {
    decode(&std::fs::read(path).ok()?)
}

/// Writes then renames, so a crash mid-write never leaves half a file.
pub fn save(path: &Path, session: &Session) {
    let Ok(json) = serde_json::to_vec_pretty(session) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

/// Where the session goes, and each open window's state as last said.
struct Store {
    path: PathBuf,
    windows: Vec<(u64, WindowState)>,
    pending: Task<()>,
}
impl Global for Store {}

impl Store {
    fn session(&self) -> Session {
        Session::of(
            self.windows
                .iter()
                .map(|(_, state)| state.clone())
                .collect(),
        )
    }
}

pub fn path() -> Option<PathBuf> {
    Some(monodrive_core::support_dir()?.join("windows.json"))
}

/// Turns saving on.
pub fn init(path: PathBuf, cx: &mut App) {
    cx.set_global(Store {
        path,
        windows: Vec::new(),
        pending: Task::ready(()),
    });
    // A save still waiting when the app quits would be lost.
    cx.on_app_quit(|cx| {
        let store = cx.global::<Store>();
        let (path, session) = (store.path.clone(), store.session());
        async move { save(&path, &session) }
    })
    .detach();
}

/// Saving is on.
pub fn enabled(cx: &App) -> bool {
    cx.has_global::<Store>()
}

/// Keeps what window `key` has open, and saves shortly after, off the UI thread.
pub fn update(key: u64, state: WindowState, cx: &mut App) {
    if !enabled(cx) {
        return;
    }
    let store = cx.global_mut::<Store>();
    match store.windows.iter_mut().find(|(window, _)| *window == key) {
        Some((_, kept)) if *kept == state => return,
        Some((_, kept)) => *kept = state,
        None => store.windows.push((key, state)),
    }
    schedule(cx);
}

/// Window `key` closed. The last window's state stays, so quitting by closing
/// it comes back to it; any other is gone for good.
pub fn forget(key: u64, cx: &mut App) {
    if !enabled(cx) || cx.windows().is_empty() {
        return;
    }
    cx.global_mut::<Store>()
        .windows
        .retain(|(window, _)| *window != key);
    schedule(cx);
}

/// Replacing the pending save drops it, so only the last change is written.
fn schedule(cx: &mut App) {
    let timer = cx.background_executor().timer(SAVE_DELAY);
    let store = cx.global::<Store>();
    let (path, session) = (store.path.clone(), store.session());
    let pending = cx.background_executor().spawn(async move {
        timer.await;
        save(&path, &session);
    });
    cx.global_mut::<Store>().pending = pending;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(at: usize, stops: usize) -> PaneState {
        PaneState {
            history: (0..stops)
                .map(|ix| StopState {
                    place: Favorite {
                        path: format!("/stop/{ix}").into(),
                        volume: None,
                    },
                    recents: false,
                })
                .collect(),
            at,
            view: View::List,
            sort: Sort::default(),
        }
    }

    #[test]
    fn a_file_that_is_old_or_broken_means_starting_fresh() {
        assert_eq!(decode(b"not json"), None);
        assert_eq!(decode(b"{}"), None);
        let tab = TabState {
            panes: vec![pane(0, 1)],
            second_focused: false,
            ratio: 0.5,
        };
        let window = WindowState {
            tabs: vec![tab],
            active: 0,
            frame: None,
        };
        let session = Session::of(vec![window]);
        let bytes = serde_json::to_vec(&session).unwrap();
        assert_eq!(decode(&bytes), Some(session.clone()));
        let older = Session {
            version: VERSION + 1,
            ..session
        };
        assert_eq!(decode(&serde_json::to_vec(&older).unwrap()), None);
    }

    #[test]
    fn what_can_not_be_opened_is_left_out_and_positions_stay_inside() {
        let window = WindowState {
            tabs: vec![
                TabState {
                    panes: vec![pane(0, 0)],
                    second_focused: false,
                    ratio: 0.5,
                },
                TabState {
                    panes: vec![pane(9, 2), pane(0, 0), pane(0, 1), pane(0, 1)],
                    second_focused: true,
                    ratio: f32::NAN,
                },
            ],
            active: 7,
            frame: None,
        };
        let bytes = serde_json::to_vec(&Session::of(vec![window])).unwrap();
        // NaN isn't JSON, so serde writes null and the whole file is refused.
        assert_eq!(decode(&bytes), None);
        let fixed = String::from_utf8(bytes).unwrap().replace("null,\"at", "");
        let fixed = fixed.replace("\"ratio\":null", "\"ratio\":2.0");
        let session = decode(fixed.as_bytes()).unwrap();
        let window = &session.windows[0];
        assert_eq!((window.tabs.len(), window.active), (1, 0));
        let tab = &window.tabs[0];
        assert_eq!((tab.panes.len(), tab.panes[0].at, tab.ratio), (2, 1, 0.9));
    }

    #[test]
    fn saving_replaces_the_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("support/windows.json");
        let session = Session::of(vec![WindowState {
            tabs: vec![TabState {
                panes: vec![pane(0, 1)],
                second_focused: false,
                ratio: 0.5,
            }],
            active: 0,
            frame: None,
        }]);
        save(&path, &session);
        assert_eq!(load(&path), Some(session));
        assert!(!path.with_extension("json.tmp").exists());
    }
}
