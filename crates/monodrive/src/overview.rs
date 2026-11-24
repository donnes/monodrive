//! The app's side of the Overview, off the UI thread: reading it, measuring
//! what it has due for every window at once, and the storage notifications.

use crate::Operations;
use crate::SharedIndex;
use crate::lock;
use crate::notify;
use crate::system::Places;
use gpui::{App, Global, Task, prelude::*};
use monodrive_core::cloud::CloudFolder;
use monodrive_core::entries::Entry;
use monodrive_core::ops::{self, AgentChange};
use monodrive_core::overview::{self, Env, Overview};
use monodrive_core::storage::{self};
use monodrive_core::volumes::Volume;
use monodrive_core::worktrees::Git;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Folders measured at once.
const MEASURING: usize = 2;
/// How often the storage notifications look again.
const WATCH_EVERY: Duration = Duration::from_secs(3 * 60 * 60);
/// How far back the page says what agents changed, and how many.
const CHANGES_SINCE: u64 = 24 * 60 * 60;
const CHANGES_SHOWN: usize = 5;

/// Where the Overview reads and saves, for this app's places.
pub(crate) fn env_of(places: &Places) -> Env {
    Env {
        root: places.root.clone(),
        ..Env::new(places.trash.clone(), places.store.clone())
    }
}

/// What a read needs, each kept from the first read of a page: asking macOS
/// for volumes can wait on a slow share.
#[derive(Clone)]
pub(crate) struct Reader {
    env: Env,
    volumes: Arc<Vec<Volume>>,
    cloud: Arc<Vec<CloudFolder>>,
}

impl Reader {
    /// Asks macOS for the volumes and cloud folders, so call it off the UI
    /// thread.
    pub(crate) fn new(places: &Places) -> Self {
        Self {
            env: env_of(places),
            volumes: Arc::new(places.drives.list()),
            cloud: Arc::new(places.drives.cloud()),
        }
    }

    pub(crate) fn read(&self) -> Overview {
        overview::read(
            &self.env,
            &self.volumes,
            &self.cloud,
            &|_| false,
            overview::now(),
        )
    }

    /// The Overview and what agents changed lately, through `ops`, which the
    /// read lets go of so the owner can exit when the window does.
    pub(crate) fn read_with(
        &self,
        ops: Option<Arc<Mutex<ops::Client>>>,
    ) -> (Overview, Vec<AgentChange>) {
        let since = overview::now().saturating_sub(CHANGES_SINCE);
        let changes = ops.map_or_else(Vec::new, |ops| {
            lock(&ops).agent_changes(since, CHANGES_SHOWN)
        });
        (self.read(), changes)
    }

    /// Finds what the Overview suggests on the disk, and with `worktrees`
    /// checks which Git worktrees are clean.
    pub(crate) fn discover(
        &self,
        git: Option<&Git>,
        worktrees: bool,
        search: impl Fn(&str, usize) -> Result<Vec<Entry>, String>,
    ) {
        let _ = overview::discover(&self.env, git, &search);
        if let (true, Some(git)) = (worktrees, git) {
            let _ = overview::check_worktrees(&self.env, git, search);
        }
    }

    /// The clean worktrees, asked about and measured again first.
    pub(crate) fn clean_worktrees(
        &self,
        git: &Git,
        search: impl Fn(&str, usize) -> Result<Vec<Entry>, String>,
    ) -> Option<overview::Suggestion> {
        let _ = overview::check_worktrees(&self.env, git, search);
        let (_, due) = overview::clean_worktrees(&self.env, &self.volumes, overview::now());
        let progress = storage::Progress::default();
        for folder in due {
            overview::measure(&self.env, &folder, &progress, None, overview::now());
        }
        overview::clean_worktrees(&self.env, &self.volumes, overview::now()).0
    }
}

/// The owner's client, for one read.
pub(crate) fn operations(cx: &App) -> Option<Arc<Mutex<ops::Client>>> {
    cx.try_global::<Operations>().map(|ops| ops.0.clone())
}

/// Measures what the Overview has due for the whole app, so it goes on
/// whichever page is on screen, and tells each open page as each folder
/// finishes.
#[derive(Clone, Default)]
pub(crate) struct Measurer(Arc<Mutex<Measuring>>);

#[derive(Default)]
struct Measuring {
    /// Folders measured so far in this run, and how many it has, while one
    /// runs.
    run: Option<(usize, usize)>,
    /// What's due changed during the run, so another follows it.
    again: bool,
    heard: Vec<async_channel::Sender<()>>,
}

impl Global for Measurer {}

impl Measurer {
    fn state(&self) -> std::sync::MutexGuard<'_, Measuring> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn say(&self) {
        self.state().heard.retain(|page| page.try_send(()).is_ok());
    }

    pub(crate) fn listen(&self) -> async_channel::Receiver<()> {
        let (send, receive) = async_channel::unbounded();
        self.state().heard.push(send);
        receive
    }

    /// Folders measured and how many there are, while a run is underway.
    pub(crate) fn progress(&self) -> Option<(usize, usize)> {
        self.state().run
    }

    /// Starts a run, or asks for another once this one ends. False when one is
    /// already underway.
    pub(crate) fn begin(&self) -> bool {
        let mut state = self.state();
        if state.run.is_some() {
            state.again = true;
            return false;
        }
        state.run = Some((0, 0));
        true
    }

    pub(crate) fn set(&self, run: Option<(usize, usize)>) {
        self.state().run = run;
        self.say();
    }

    pub(crate) fn measured_one(&self) {
        if let Some((done, _)) = &mut self.state().run {
            *done += 1;
        }
        self.say();
    }

    /// Ends a run, unless what's due changed while it ran.
    pub(crate) fn end(&self) -> bool {
        let mut state = self.state();
        if std::mem::take(&mut state.again) {
            return false;
        }
        state.run = None;
        drop(state);
        self.say();
        true
    }
}

/// Measures every folder the Overview has due, `MEASURING` at a time, saving
/// each total as it finishes. When a run is already underway, another
/// follows it, for what became due since.
pub(crate) fn measure_overview(cx: &mut App) {
    let Some(places) = cx.try_global::<Places>().cloned() else {
        return;
    };
    let measurer = cx.default_global::<Measurer>().clone();
    if !measurer.begin() {
        return;
    }
    let background = cx.background_executor().clone();
    cx.background_executor()
        .spawn(async move {
            loop {
                let reader = Reader::new(&places);
                let due: VecDeque<PathBuf> = reader.read().due.into();
                measurer.set(Some((0, due.len())));
                let queue = Arc::new(Mutex::new(due));
                let workers: Vec<Task<()>> = (0..MEASURING)
                    .map(|_| {
                        let (queue, measurer, env) =
                            (queue.clone(), measurer.clone(), reader.env.clone());
                        background.spawn(async move {
                            let progress = storage::Progress::default();
                            loop {
                                let next = queue
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .pop_front();
                                let Some(folder) = next else { return };
                                overview::measure(&env, &folder, &progress, None, overview::now());
                                measurer.measured_one();
                            }
                        })
                    })
                    .collect();
                for worker in workers {
                    worker.await;
                }
                if measurer.end() {
                    return;
                }
            }
        })
        .detach();
}

/// Looks at the startup disk and the clean worktrees now and every few hours,
/// for the notifications Settings wants. Asks Git and measures only what a
/// switch that's on needs.
pub(crate) fn watch_storage(cx: &mut App) {
    cx.spawn(async |cx| {
        loop {
            let wanted = cx.update(|cx| {
                let places = cx.try_global::<Places>().cloned()?;
                let index = cx.try_global::<SharedIndex>()?.0.clone();
                let low = notify::wanted(notify::Kind::LowDisk, cx);
                let worktrees = notify::wanted(notify::Kind::Worktrees, cx);
                (low || worktrees).then_some((places, index, low, worktrees))
            });
            if let Some((places, index, low, worktrees)) = wanted {
                let found = cx
                    .background_spawn(
                        async move { look_at_storage(&places, &index, low, worktrees) },
                    )
                    .await;
                if let Some(said) = cx.update(|cx| notify::storage(found, overview::now(), cx)) {
                    cx.background_spawn(async move { said.save() }).await;
                }
            }
            cx.background_executor().timer(WATCH_EVERY).await;
        }
    })
    .detach();
}

/// What the storage notifications would say about: the startup disk if it's
/// low, and the clean worktrees, asked about and measured again first.
fn look_at_storage(
    places: &Places,
    index: &std::sync::OnceLock<Result<monodrive_core::index::Index, String>>,
    low: bool,
    worktrees: bool,
) -> notify::Storage {
    let reader = Reader::new(places);
    let clean = worktrees
        .then(|| {
            let git = Git::find().ok()?;
            reader.clean_worktrees(&git, |name: &str, limit| {
                places.drives.find_files(index, name, limit)
            })
        })
        .flatten();
    let disk = low
        .then(|| {
            let about = reader.read();
            let headline = about.headline.filter(|disk| disk.low)?;
            let trash = headline.trash.map_or(0, |trash| trash.unshared);
            let freeable = about
                .suggestions
                .iter()
                .map(|suggestion| suggestion.size.unshared)
                .sum::<u64>()
                + trash;
            Some((headline, freeable))
        })
        .flatten();
    notify::Storage {
        low: disk,
        worktrees: clean,
        said: notify::Said::read(),
        allowed: notify::allowed(),
    }
}
