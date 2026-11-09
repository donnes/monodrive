//! The cloud downloads the app asked macOS for, so every pane's badge, the
//! inspector and the preview show the same state. macOS doesn't say when one
//! ends, so while any run they're checked every `CHECK`; while none do,
//! nothing is.

use crate::native::native;
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Task};
use monodrive_core::cloud::{Asked, Progress};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

pub const CHECK: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    /// Why, in the provider's words or macOS's.
    Failed(String),
}

/// An item's contents arrived or left, so what lists it should list it again.
pub struct Changed(pub PathBuf);

/// Called once for each path of a request, with how it ended.
pub type Then = Rc<dyn Fn(&Path, Result<(), String>, &mut App)>;

/// Called once with how removing each download went.
pub type Removed = Box<dyn FnOnce(Vec<(PathBuf, Result<(), String>)>, &mut App)>;

#[derive(Default)]
pub struct Downloads {
    states: HashMap<PathBuf, State>,
    waiting: HashMap<PathBuf, Vec<Then>>,
    checking: Option<Task<()>>,
}

impl EventEmitter<Changed> for Downloads {}

struct Shared(Entity<Downloads>);
impl Global for Shared {}

/// The app's one list.
pub fn get(cx: &mut App) -> Entity<Downloads> {
    if let Some(shared) = cx.try_global::<Shared>() {
        return shared.0.clone();
    }
    let downloads = cx.new(|_| Downloads::default());
    cx.set_global(Shared(downloads.clone()));
    downloads
}

impl Downloads {
    pub fn state(&self, path: &Path) -> Option<&State> {
        self.states.get(path)
    }

    /// Asks macOS to download each of `paths`, off the UI thread. `then`
    /// hears each one end: at once when it's already here or refused, or
    /// when a check finds it done or failed. A path already on its way only
    /// adds `then`.
    pub fn download(&mut self, paths: Vec<PathBuf>, then: Then, cx: &mut Context<Self>) {
        let mut asked = Vec::new();
        for path in paths {
            if self.states.get(&path) == Some(&State::Running) {
                self.waiting.entry(path).or_default().push(then.clone());
            } else {
                self.states.insert(path.clone(), State::Running);
                asked.push(path);
            }
        }
        cx.notify();
        if asked.is_empty() {
            return;
        }
        let native = native(cx);
        let answers = cx.background_executor().spawn(async move {
            asked
                .into_iter()
                .map(|path| {
                    let asked = native.download(&path);
                    (path, asked)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let answers = answers.await;
            let _ = this.update(cx, |this, cx| {
                for (path, asked) in answers {
                    match asked {
                        Ok(Asked::Requested) => {
                            this.waiting.entry(path).or_default().push(then.clone());
                        }
                        Ok(Asked::Unneeded) => {
                            this.states.remove(&path);
                            cx.emit(Changed(path.clone()));
                            this.finish(&path, Ok(()), Some(&then), cx);
                        }
                        Err(why) => {
                            this.states.insert(path.clone(), State::Failed(why.clone()));
                            this.finish(&path, Err(why), Some(&then), cx);
                        }
                    }
                }
                this.keep_checking(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks macOS to remove the download of each of `paths`, off the UI
    /// thread, and calls `done` with each answer.
    pub fn remove(&mut self, paths: Vec<PathBuf>, done: Removed, cx: &mut Context<Self>) {
        for path in &paths {
            self.states.remove(path);
        }
        cx.notify();
        let native = native(cx);
        let answers = cx.background_executor().spawn(async move {
            paths
                .into_iter()
                .map(|path| {
                    let removed = native.remove_download(&path).map(|_| ());
                    (path, removed)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let answers = answers.await;
            let _ = this.update(cx, |_, cx| {
                for (path, removed) in &answers {
                    if removed.is_ok() {
                        cx.emit(Changed(path.clone()));
                    }
                }
                cx.defer(move |cx| done(answers, cx));
            });
        })
        .detach();
    }

    /// Tells `then` and everything else waiting on `path` how it ended.
    fn finish(
        &mut self,
        path: &Path,
        how: Result<(), String>,
        then: Option<&Then>,
        cx: &mut Context<Self>,
    ) {
        let waiting = self.waiting.remove(path).unwrap_or_default();
        let path = path.to_path_buf();
        for then in waiting.into_iter().chain(then.cloned()) {
            let (path, how) = (path.clone(), how.clone());
            cx.defer(move |cx| then(&path, how, cx));
        }
    }

    /// Checks what's on its way every `CHECK`, until nothing is.
    fn keep_checking(&mut self, cx: &mut Context<Self>) {
        if self.checking.is_some() || self.waiting.is_empty() {
            return;
        }
        let native = native(cx);
        self.checking = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CHECK).await;
                let Ok(paths) = this.update(cx, |this, _| {
                    this.waiting.keys().cloned().collect::<Vec<_>>()
                }) else {
                    return;
                };
                let native = native.clone();
                let progress = cx
                    .background_executor()
                    .spawn(async move {
                        paths
                            .into_iter()
                            .map(|path| {
                                let progress = native.download_progress(&path);
                                (path, progress)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                let more = this.update(cx, |this, cx| {
                    for (path, progress) in progress {
                        let how = match progress {
                            Progress::Running => continue,
                            Progress::Done => {
                                this.states.remove(&path);
                                cx.emit(Changed(path.clone()));
                                Ok(())
                            }
                            Progress::Failed(why) => {
                                this.states.insert(path.clone(), State::Failed(why.clone()));
                                Err(why)
                            }
                        };
                        this.finish(&path, how, None, cx);
                        cx.notify();
                    }
                    let more = !this.waiting.is_empty();
                    if !more {
                        this.checking = None;
                    }
                    more
                });
                if !matches!(more, Ok(true)) {
                    return;
                }
            }
        }));
    }
}
