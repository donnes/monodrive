use super::tests::{untrash, untrash_unfinished};
use super::*;
use crate::remote::fixture::{self, frame, status, version};
use crate::remote::{Connector, RemoteError, Source, Transport};
use std::collections::VecDeque;
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

/// Enough chunks that a test can stop a transfer partway.
const BIG: usize = 8 << 20;

/// An owner's jobs against `/usr/libexec/sftp-server` over pipes, with a temp
/// folder for each side. The queue says how the next connections go; once it's
/// empty, each is a plain new server.
struct Bench {
    _dir: tempfile::TempDir,
    root: PathBuf,
    server: PathBuf,
    local: PathBuf,
    shared: Arc<jobs::Shared>,
    next: Arc<Mutex<VecDeque<Next>>>,
}

enum Next {
    Lost(usize),
    Cancel(usize),
    Refused(RemoteError),
    Given(Transport),
}

fn bench() -> Bench {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (server, local) = (root.join("server"), root.join("local"));
    std::fs::create_dir(&server).unwrap();
    std::fs::create_dir(&local).unwrap();
    let store = places::Store {
        places: root.join("places.json"),
        recents: root.join("recents.json"),
    };
    store.add_remote("box.example", Some("box"), None).unwrap();
    let next: Arc<Mutex<VecDeque<Next>>> = Arc::default();
    let queue = next.clone();
    let shared_slot: Arc<OnceLock<Arc<jobs::Shared>>> = Arc::default();
    let slot = shared_slot.clone();
    let connector: Connector = Arc::new(move |_: &Source| {
        let next = queue.lock().unwrap().pop_front();
        Ok(match next {
            None => fixture::transport(),
            Some(Next::Lost(after)) => fixture::killed_after(after).transport,
            Some(Next::Cancel(after)) => {
                let slot = slot.clone();
                fixture::start(Some(fixture::Trip {
                    after,
                    then: Box::new(move || {
                        let shared: &Arc<jobs::Shared> = slot.get().unwrap();
                        let mut engine = shared.lock();
                        let going = engine
                            .jobs()
                            .iter()
                            .find(|job| job.state.active())
                            .map(|job| job.id);
                        let _ = engine.cancel(going.unwrap());
                    }),
                }))
                .transport
            }
            Some(Next::Refused(error)) => return Err(error),
            Some(Next::Given(transport)) => transport,
        })
    });
    let engine = Engine::open(paths(&root)).unwrap();
    let shared = jobs::Shared::with_connector(engine, None, connector);
    assert!(shared_slot.set(shared.clone()).is_ok());
    Bench {
        _dir: dir,
        root,
        server,
        local,
        shared,
        next,
    }
}

fn paths(dir: &Path) -> Paths {
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    Paths {
        state,
        settings: dir.join("settings.json"),
        trash: dir.join("Trash"),
    }
}

fn remote(path: &Path) -> RemotePath {
    RemotePath::new("box", path.to_str().unwrap()).unwrap()
}

impl Bench {
    fn queue(&self, request: Request) -> u64 {
        let submission = Submission {
            id: format!("transfer-{}", self.shared.lock().jobs().len()),
            origin: Origin::Window,
            request,
        };
        self.shared.lock().submit(submission).unwrap().op
    }

    fn status(&self, op: u64) -> Job {
        let jobs = self.shared.lock().jobs();
        jobs.into_iter().find(|job| job.id == op).unwrap()
    }

    fn run(&self, request: Request) -> Job {
        let op = self.queue(request);
        self.work(op)
    }

    fn resume(&self, job: u64, conflict: Option<Conflict>) -> Job {
        let submission = Submission {
            id: format!(
                "resume-{job}-{}",
                self.shared.lock().jobs().len() + self.seq()
            ),
            origin: Origin::Window,
            request: Request::Retry { job, conflict },
        };
        assert_eq!(self.shared.lock().submit(submission).unwrap().op, job);
        self.work(job)
    }

    /// Runs the job to its end here, as a job thread would.
    fn work(&self, op: u64) -> Job {
        jobs::run(&self.shared, op);
        self.shared.remote.release_idle();
        self.status(op)
    }

    fn seq(&self) -> usize {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn download(&self, names: &[&str], conflict: Conflict) -> Request {
        Request::Download {
            paths: names
                .iter()
                .map(|name| remote(&self.server.join(name)))
                .collect(),
            into: self.local.clone(),
            conflict,
        }
    }

    fn upload(&self, names: &[&str], conflict: Conflict) -> Request {
        Request::Upload {
            paths: names.iter().map(|name| self.local.join(name)).collect(),
            into: remote(&self.server),
            conflict,
        }
    }

    fn on_server(&self) -> Vec<String> {
        listing(&self.server)
    }

    fn here(&self) -> Vec<String> {
        listing(&self.local)
    }
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|item| item.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn first_failure(job: &Job) -> &str {
    job.tasks
        .iter()
        .find_map(|task| match &task.done {
            Some(Done::Failed(why)) => Some(why.as_str()),
            _ => None,
        })
        .expect("a failed task")
}

#[test]
fn a_download_copies_files_folders_and_links_and_undo_trashes_the_copy() {
    let b = bench();
    let bytes: Vec<u8> = (0..BIG).map(|n| (n % 251) as u8).collect();
    std::fs::write(b.server.join("big.bin"), &bytes).unwrap();
    let folder = b.server.join("folder");
    std::fs::create_dir_all(folder.join("deep/er")).unwrap();
    std::fs::write(folder.join("a.txt"), "a").unwrap();
    std::fs::write(folder.join("deep/er/b.txt"), "b").unwrap();
    std::fs::write(folder.join("empty"), "").unwrap();
    std::os::unix::fs::symlink("a.txt", folder.join("link")).unwrap();
    let before = std::fs::metadata(b.server.join("big.bin")).unwrap();

    let job = b.run(b.download(&["big.bin", "folder"], Conflict::Stop));
    assert_eq!(job.state, State::Completed, "{job:?}");
    assert_eq!(job.title, "Download");
    assert_eq!(job.transfer(), Some(Transfer::Download));
    assert_eq!(job.into, b.local);
    assert!(job.undoable);
    assert_eq!(job.bytes, job.total_bytes);
    assert_eq!(job.total_bytes, BIG as u64 + 2);
    // The server's side prints as a label, which no local service takes.
    assert!(job.tasks.iter().all(|task| !task.source.is_absolute()));
    assert!(job.tasks[0].source.to_str().unwrap().starts_with("box:/"));

    let copy = b.local.join("big.bin");
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&copy)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        before
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    let copied = b.local.join("folder");
    assert_eq!(std::fs::read(copied.join("deep/er/b.txt")).unwrap(), b"b");
    assert_eq!(std::fs::read(copied.join("empty")).unwrap(), b"");
    assert_eq!(
        std::fs::read_link(copied.join("link")).unwrap(),
        PathBuf::from("a.txt")
    );
    assert_eq!(b.here(), ["big.bin", "folder"]);

    let outcome = b
        .shared
        .lock()
        .submit(Submission {
            id: "undo".into(),
            origin: Origin::Window,
            request: Request::Undo,
        })
        .unwrap();
    assert_eq!(outcome.title, "Undo Download");
    assert_eq!(b.here(), Vec::<String>::new());
    for item in outcome.items {
        untrash(&item.result.unwrap(), &b.root);
    }
    // And the server still has it all.
    assert_eq!(b.on_server(), ["big.bin", "folder"]);
}

#[test]
fn a_downloads_conflicts_act_on_this_macs_side() {
    let b = bench();
    std::fs::write(b.server.join("a.txt"), "new").unwrap();
    std::fs::write(b.local.join("a.txt"), "old").unwrap();

    let stopped = b.run(b.download(&["a.txt"], Conflict::Stop));
    assert_eq!(stopped.state, State::Waiting);
    assert!(
        first_failure(&stopped).contains("already exists"),
        "{stopped:?}"
    );
    assert_eq!(std::fs::read(b.local.join("a.txt")).unwrap(), b"old");

    let kept = b.resume(stopped.id, Some(Conflict::KeepBoth));
    assert_eq!(kept.state, State::Completed, "{kept:?}");
    assert_eq!(b.here(), ["a 2.txt", "a.txt"]);
    assert_eq!(std::fs::read(b.local.join("a 2.txt")).unwrap(), b"new");
    assert_eq!(std::fs::read(b.local.join("a.txt")).unwrap(), b"old");

    let skipped = b.run(b.download(&["a.txt"], Conflict::Skip));
    assert_eq!(skipped.state, State::Completed);
    assert!(matches!(skipped.tasks[0].done, Some(Done::Skipped(_))));
    assert_eq!(std::fs::read(b.local.join("a.txt")).unwrap(), b"old");

    let replaced = b.run(b.download(&["a.txt"], Conflict::Replace));
    assert_eq!(replaced.state, State::Completed, "{replaced:?}");
    assert_eq!(std::fs::read(b.local.join("a.txt")).unwrap(), b"new");
    // Undo brings back what it replaced, from the Trash.
    let undone = b
        .shared
        .lock()
        .submit(Submission {
            id: "undo-replace".into(),
            origin: Origin::Window,
            request: Request::Undo,
        })
        .unwrap();
    assert_eq!(std::fs::read(b.local.join("a.txt")).unwrap(), b"old");
    let copy_in_trash = undone.items[0].result.clone().unwrap();
    assert_eq!(std::fs::read(&copy_in_trash).unwrap(), b"new");
    untrash(&copy_in_trash, &b.root);
}

#[test]
fn an_upload_copies_with_conflicts_on_the_servers_side_and_has_no_undo() {
    let b = bench();
    let bytes: Vec<u8> = (0..BIG).map(|n| (n % 241) as u8).collect();
    std::fs::write(b.local.join("big.bin"), &bytes).unwrap();
    let folder = b.local.join("folder");
    std::fs::create_dir_all(folder.join("deep")).unwrap();
    std::fs::write(folder.join("deep/b.txt"), "b").unwrap();
    std::os::unix::fs::symlink("deep/b.txt", folder.join("link")).unwrap();
    std::os::unix::fs::symlink("big.bin", b.local.join("top link")).unwrap();

    let job = b.run(b.upload(&["big.bin", "folder", "top link"], Conflict::Stop));
    assert_eq!(job.state, State::Completed, "{job:?}");
    assert_eq!(job.transfer(), Some(Transfer::Upload));
    assert!(!job.undoable, "nothing here can be reversed");
    assert_eq!(job.bytes, BIG as u64 + 1);
    assert!(!job.into.is_absolute());
    assert_eq!(job.into.to_str().unwrap(), remote(&b.server).to_string());
    assert_eq!(std::fs::read(b.server.join("big.bin")).unwrap(), bytes);
    assert_eq!(
        std::fs::read(b.server.join("folder/deep/b.txt")).unwrap(),
        b"b"
    );
    assert_eq!(
        std::fs::read_link(b.server.join("folder/link")).unwrap(),
        PathBuf::from("deep/b.txt")
    );
    assert_eq!(
        std::fs::read_link(b.server.join("top link")).unwrap(),
        PathBuf::from("big.bin")
    );
    assert_eq!(b.on_server(), ["big.bin", "folder", "top link"]);
    let refused = b.shared.lock().submit(Submission {
        id: "undo-upload".into(),
        origin: Origin::Window,
        request: Request::Undo,
    });
    assert_eq!(refused.err().as_deref(), Some("Nothing to undo."));

    std::fs::write(b.local.join("a.txt"), "new").unwrap();
    std::fs::write(b.server.join("a.txt"), "old").unwrap();
    let stopped = b.run(b.upload(&["a.txt"], Conflict::Stop));
    assert_eq!(stopped.state, State::Waiting);
    assert_eq!(std::fs::read(b.server.join("a.txt")).unwrap(), b"old");

    let skipped = b.run(b.upload(&["a.txt"], Conflict::Skip));
    assert!(matches!(skipped.tasks[0].done, Some(Done::Skipped(_))));
    assert_eq!(std::fs::read(b.server.join("a.txt")).unwrap(), b"old");

    let kept = b.resume(stopped.id, Some(Conflict::KeepBoth));
    assert_eq!(kept.state, State::Completed, "{kept:?}");
    assert_eq!(std::fs::read(b.server.join("a 2.txt")).unwrap(), b"new");
    assert_eq!(std::fs::read(b.server.join("a.txt")).unwrap(), b"old");

    let replaced = b.run(b.upload(&["a.txt"], Conflict::Replace));
    assert_eq!(replaced.state, State::Completed, "{replaced:?}");
    assert_eq!(std::fs::read(b.server.join("a.txt")).unwrap(), b"new");

    // A replace is for good, so it takes only a file's place.
    std::fs::create_dir(b.server.join("taken")).unwrap();
    std::fs::write(b.server.join("taken/kept"), "kept").unwrap();
    std::fs::write(b.local.join("taken"), "x").unwrap();
    let refused = b.run(b.upload(&["taken"], Conflict::Replace));
    assert_eq!(refused.state, State::Failed);
    assert_eq!(std::fs::read(b.server.join("taken/kept")).unwrap(), b"kept");
    assert!(
        !b.on_server()
            .iter()
            .any(|name| name.starts_with(".monodrive"))
    );
}

#[test]
fn cancelling_an_upload_removes_only_its_own_temp() {
    let b = bench();
    std::fs::write(b.local.join("big.bin"), vec![1; BIG]).unwrap();
    std::fs::write(b.server.join("keep.txt"), "keep").unwrap();
    // Named as a temp of another job's, which this one must never touch.
    std::fs::write(b.server.join(".monodrive-999-0-feed"), "theirs").unwrap();
    b.next.lock().unwrap().push_back(Next::Cancel(2000));

    let job = b.run(b.upload(&["big.bin"], Conflict::Stop));
    assert_eq!(job.state, State::Cancelled, "{job:?}");
    let why = first_failure(&job);
    assert!(why.contains("cancelled partway"), "{why}");
    assert!(why.contains("removed from the server"), "{why}");
    assert_eq!(b.on_server(), [".monodrive-999-0-feed", "keep.txt"]);
    assert_eq!(std::fs::read(b.server.join("keep.txt")).unwrap(), b"keep");
    assert_eq!(b.here(), ["big.bin"]);
}

#[test]
fn cancelling_a_folder_upload_removes_all_of_its_temp_and_nothing_else() {
    let b = bench();
    let folder = b.local.join("folder");
    std::fs::create_dir_all(folder.join("deep")).unwrap();
    std::fs::write(folder.join("deep/a"), "a").unwrap();
    std::os::unix::fs::symlink("/etc/hosts", folder.join("link")).unwrap();
    std::fs::write(folder.join("z"), vec![1; BIG]).unwrap();
    std::fs::write(b.server.join("keep.txt"), "keep").unwrap();
    b.next.lock().unwrap().push_back(Next::Cancel(2500));

    let job = b.run(b.upload(&["folder"], Conflict::Stop));
    assert_eq!(job.state, State::Cancelled, "{job:?}");
    assert_eq!(b.on_server(), ["keep.txt"]);
    // A removal doesn't follow a link it made.
    assert!(Path::new("/etc/hosts").exists());
}

#[test]
fn cancelling_a_download_sends_its_partial_copy_to_the_trash_and_leaves_the_server() {
    let b = bench();
    std::fs::write(b.server.join("big.bin"), vec![1; BIG]).unwrap();
    std::fs::write(b.local.join("keep.txt"), "keep").unwrap();
    b.next.lock().unwrap().push_back(Next::Cancel(256 << 10));

    let job = b.run(b.download(&["big.bin"], Conflict::Stop));
    assert_eq!(job.state, State::Cancelled, "{job:?}");
    let why = first_failure(&job);
    assert!(why.contains("went to the Trash"), "{why}");
    untrash_unfinished(why, &b.root);
    assert_eq!(b.here(), ["keep.txt"]);
    assert_eq!(b.on_server(), ["big.bin"]);
    assert!(!job.undoable);
}

#[test]
fn losing_the_connection_interrupts_an_upload_and_continuing_cleans_up_and_starts_over() {
    let b = bench();
    let bytes = vec![7; BIG];
    std::fs::write(b.local.join("big.bin"), &bytes).unwrap();
    std::fs::write(b.local.join("next.txt"), "next").unwrap();
    std::fs::write(b.server.join("keep.txt"), "keep").unwrap();
    std::fs::write(b.server.join(".monodrive-999-0-feed"), "theirs").unwrap();
    b.next.lock().unwrap().push_back(Next::Lost(2000));

    let job = b.run(b.upload(&["big.bin", "next.txt"], Conflict::Stop));
    assert_eq!(job.state, State::Interrupted, "{job:?}");
    let why = first_failure(&job);
    assert!(why.contains("continuing the job removes it"), "{why}");
    assert!(job.tasks[1].done.is_none(), "the rest wait to be continued");
    let left: Vec<String> = b
        .on_server()
        .into_iter()
        .filter(|name| name.starts_with(".monodrive-1-0-"))
        .collect();
    assert_eq!(
        left.len(),
        1,
        "the partial copy stays until the job can reach it"
    );
    assert!(std::fs::metadata(b.server.join(&left[0])).unwrap().len() < BIG as u64);
    assert!(!b.server.join("big.bin").exists());

    let done = b.resume(job.id, None);
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(std::fs::read(b.server.join("big.bin")).unwrap(), bytes);
    assert_eq!(std::fs::read(b.server.join("next.txt")).unwrap(), b"next");
    assert_eq!(
        b.on_server(),
        [".monodrive-999-0-feed", "big.bin", "keep.txt", "next.txt"]
    );
}

#[test]
fn continuing_an_upload_refuses_a_source_that_changed_and_still_removes_its_temp() {
    let b = bench();
    std::fs::write(b.local.join("big.bin"), vec![7; BIG]).unwrap();
    b.next.lock().unwrap().push_back(Next::Lost(2000));
    let job = b.run(b.upload(&["big.bin"], Conflict::Stop));
    assert_eq!(job.state, State::Interrupted, "{job:?}");
    assert_eq!(b.on_server().len(), 1);

    std::fs::write(b.local.join("big.bin"), vec![8; 100]).unwrap();
    let refused = b.resume(job.id, None);
    assert_eq!(refused.state, State::Failed, "{refused:?}");
    assert!(first_failure(&refused).contains("changed since the job started"));
    assert_eq!(b.on_server(), Vec::<String>::new());
}

#[test]
fn continuing_an_upload_revalidates_the_destination() {
    let b = bench();
    std::fs::write(b.local.join("big.bin"), vec![7; BIG]).unwrap();
    b.next.lock().unwrap().push_back(Next::Lost(2000));
    let job = b.run(b.upload(&["big.bin"], Conflict::Stop));
    assert_eq!(job.state, State::Interrupted, "{job:?}");

    // Something else took the name meanwhile: nothing goes over it blindly.
    std::fs::write(b.server.join("big.bin"), "someone else's").unwrap();
    let stopped = b.resume(job.id, None);
    assert_eq!(stopped.state, State::Waiting, "{stopped:?}");
    assert_eq!(b.on_server(), ["big.bin"]);
    assert_eq!(
        std::fs::read(b.server.join("big.bin")).unwrap(),
        b"someone else's"
    );
}

#[test]
fn losing_the_connection_interrupts_a_download_and_continuing_checks_the_source() {
    let b = bench();
    let bytes = vec![3; BIG];
    std::fs::write(b.server.join("big.bin"), &bytes).unwrap();
    b.next.lock().unwrap().push_back(Next::Lost(256 << 10));
    let job = b.run(b.download(&["big.bin"], Conflict::Stop));
    assert_eq!(job.state, State::Interrupted, "{job:?}");
    untrash_unfinished(first_failure(&job), &b.root);
    assert_eq!(b.here(), Vec::<String>::new());

    let done = b.resume(job.id, None);
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(std::fs::read(b.local.join("big.bin")).unwrap(), bytes);

    // Lost again on another job, whose source then changes.
    std::fs::write(b.server.join("other.bin"), &bytes).unwrap();
    b.next.lock().unwrap().push_back(Next::Lost(256 << 10));
    let job = b.run(b.download(&["other.bin"], Conflict::Stop));
    assert_eq!(job.state, State::Interrupted, "{job:?}");
    untrash_unfinished(first_failure(&job), &b.root);
    std::fs::write(b.server.join("other.bin"), "edited").unwrap();
    let refused = b.resume(job.id, None);
    assert_eq!(refused.state, State::Failed, "{refused:?}");
    assert!(first_failure(&refused).contains("changed since the job started"));
    assert!(!b.local.join("other.bin").exists());
}

#[test]
fn a_connection_that_cant_be_made_interrupts_the_job_with_the_reason() {
    let b = bench();
    std::fs::write(b.server.join("a.txt"), "a").unwrap();
    std::fs::write(b.server.join("b.txt"), "b").unwrap();
    for (error, says) in [
        (
            RemoteError::HostKeyChanged("box".into()),
            "host key changed",
        ),
        (
            RemoteError::InteractiveAuth("box".into()),
            "interactive authentication required",
        ),
    ] {
        b.next.lock().unwrap().push_back(Next::Refused(error));
        let job = b.run(b.download(&["a.txt", "b.txt"], Conflict::Skip));
        assert_eq!(job.state, State::Interrupted);
        assert!(first_failure(&job).contains(says), "{job:?}");
        assert!(job.tasks[1].done.is_none());
        assert_eq!(b.here(), Vec::<String>::new());
    }
    let done = b.resume(1, None);
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(b.here(), ["a.txt", "b.txt"]);
}

#[test]
fn a_crash_leaves_an_uploads_temp_named_and_continuing_removes_only_that() {
    let b = bench();
    std::fs::write(b.local.join("a.txt"), "a").unwrap();
    let temp = b.server.join(".monodrive-1-0-abc");
    std::fs::write(&temp, "partial").unwrap();
    std::fs::write(b.server.join("keep.txt"), "keep").unwrap();
    let op = b.queue(b.upload(&["a.txt"], Conflict::Stop));
    let work = jobs::Work {
        source: b.local.join("a.txt"),
        left: Some(remote(&temp)),
        made: true,
        ..jobs::Work::default()
    };
    b.shared.record(op, 0, &work).unwrap();
    drop(b.shared.lock());

    // The next owner settles it from the journal.
    let engine = Engine::open(paths(&b.root)).unwrap();
    let job = engine.jobs().into_iter().find(|job| job.id == op).unwrap();
    assert_eq!(job.state, State::Interrupted);
    let why = first_failure(&job);
    assert!(why.contains("continuing the job removes it"), "{why}");
    assert!(temp.exists());

    let shared = jobs::Shared::with_connector(
        engine,
        None,
        Arc::new(|_: &Source| Ok(fixture::transport())),
    );
    let submission = Submission {
        id: "after-crash".into(),
        origin: Origin::Window,
        request: Request::Retry {
            job: op,
            conflict: None,
        },
    };
    shared.lock().submit(submission).unwrap();
    jobs::run(&shared, op);
    let job = shared
        .lock()
        .jobs()
        .into_iter()
        .find(|job| job.id == op)
        .unwrap();
    assert_eq!(job.state, State::Completed, "{job:?}");
    assert_eq!(b.on_server(), ["a.txt", "keep.txt"]);
}

#[test]
fn a_discard_refuses_any_name_that_isnt_a_temps() {
    let b = bench();
    std::fs::write(b.server.join("precious"), "x").unwrap();
    let connection = b.shared.remote.connect("box").unwrap();
    let refused = connection.discard(&remote(&b.server.join("precious")));
    assert!(
        matches!(refused, Err(RemoteError::BadPath(_))),
        "{refused:?}"
    );
    assert_eq!(b.on_server(), ["precious"]);
}

#[test]
fn agents_copy_only_while_the_setting_lets_them_and_a_forgotten_source_fails() {
    let b = bench();
    std::fs::write(b.local.join("a.txt"), "a").unwrap();
    std::fs::write(
        b.root.join("settings.json"),
        r#"{"agents_change_files": false}"#,
    )
    .unwrap();
    let submit = |id: &str, request| {
        b.shared.lock().submit(Submission {
            id: id.into(),
            origin: Origin::Agent { client: None },
            request,
        })
    };
    let refused = submit("up", b.upload(&["a.txt"], Conflict::Stop)).unwrap_err();
    assert!(refused.contains("Agents can't change files"), "{refused}");
    let refused = submit("down", b.download(&["a.txt"], Conflict::Stop)).unwrap_err();
    assert!(refused.contains("Agents can't change files"), "{refused}");
    assert_eq!(b.on_server(), Vec::<String>::new());

    std::fs::write(b.root.join("settings.json"), "{}").unwrap();
    let op = submit("up 2", b.upload(&["a.txt"], Conflict::Stop))
        .unwrap()
        .op;
    jobs::run(&b.shared, op);
    assert_eq!(b.on_server(), ["a.txt"]);

    let gone = Request::Upload {
        paths: vec![b.local.join("a.txt")],
        into: RemotePath::new("gone", "/srv").unwrap(),
        conflict: Conflict::Stop,
    };
    let job = b.run(gone);
    assert_eq!(job.state, State::Failed, "{job:?}");
    assert!(first_failure(&job).contains("gone isn't a saved remote source"));
}

#[test]
fn a_link_in_a_folder_stays_a_link_and_nothing_is_written_through_it() {
    let b = bench();
    let outside = b.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let folder = b.server.join("folder");
    std::fs::create_dir(&folder).unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("escape")).unwrap();
    std::fs::write(folder.join("file"), "x").unwrap();
    let job = b.run(b.download(&["folder"], Conflict::Stop));
    assert_eq!(job.state, State::Completed, "{job:?}");
    assert_eq!(
        std::fs::read_link(b.local.join("folder/escape")).unwrap(),
        outside
    );
    assert_eq!(listing(&outside), Vec::<String>::new());
}

#[test]
fn what_a_folder_holds_that_cant_be_copied_is_reported_not_dropped() {
    let b = bench();
    let folder = b.local.join("folder");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a"), "a").unwrap();
    let pipe = std::ffi::CString::new(folder.join("pipe").to_str().unwrap()).unwrap();
    // SAFETY: `pipe` is a valid C string for the call.
    assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
    let job = b.run(b.upload(&["folder"], Conflict::Stop));
    assert_eq!(job.state, State::Failed, "{job:?}");
    assert!(first_failure(&job).contains("left out 1 items"), "{job:?}");
    assert_eq!(std::fs::read(b.server.join("folder/a")).unwrap(), b"a");

    // And the same coming back: the pipe is on the server's side now.
    let pipe = std::ffi::CString::new(b.server.join("pipe").to_str().unwrap()).unwrap();
    // SAFETY: `pipe` is a valid C string for the call.
    assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
    let job = b.run(b.download(&["pipe"], Conflict::Stop));
    assert_eq!(job.state, State::Failed, "{job:?}");
    assert!(
        b.here().iter().all(|name| !name.starts_with("pipe")),
        "{job:?}"
    );
}

#[test]
fn stopping_mid_job_holds_no_flag_for_the_next_run() {
    // A cancelled job's flag is its own; continuing it runs.
    let b = bench();
    std::fs::write(b.local.join("big.bin"), vec![1; BIG]).unwrap();
    b.next.lock().unwrap().push_back(Next::Cancel(2000));
    let job = b.run(b.upload(&["big.bin"], Conflict::Stop));
    assert_eq!(job.state, State::Cancelled, "{job:?}");
    let done = b.resume(job.id, None);
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(
        std::fs::metadata(b.server.join("big.bin")).unwrap().len(),
        BIG as u64
    );
}

/// How a hand-written server answers a request to create the temp.
#[derive(Clone, Copy)]
enum Create {
    Collide,
    HangUp,
}

/// A server where a temp's name is always taken by a file of someone else's,
/// so exclusive creation can't succeed. Records each kind it's asked.
fn taken_names(create: Create) -> (Transport, Arc<Mutex<Vec<u8>>>) {
    let asked: Arc<Mutex<Vec<u8>>> = Arc::default();
    let recorded = asked.clone();
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    std::thread::spawn(move || {
        let _ = fixture::request(&mut theirs);
        let _ = theirs.write_all(&version());
        while let Some((kind, id, body)) = fixture::packet(&mut theirs) {
            recorded.lock().unwrap().push(kind);
            let path = String::from_utf8_lossy(body.get(4..).unwrap_or_default()).into_owned();
            let attrs = |mode: u32, size: u64| {
                let mut out = (1u32 | 4).to_be_bytes().to_vec();
                out.extend_from_slice(&size.to_be_bytes());
                out.extend_from_slice(&mode.to_be_bytes());
                frame(105, Some(id), &out)
            };
            let reply = match kind {
                3 | 14 => match create {
                    Create::Collide => frame(101, Some(id), &status(4)),
                    Create::HangUp => return,
                },
                7 | 17 if path.contains(".monodrive-") => attrs(0o100_644, 5),
                7 | 17 if path.ends_with("server") => attrs(0o040_755, 0),
                7 | 17 => frame(101, Some(id), &status(2)),
                _ => frame(101, Some(id), &status(0)),
            };
            let _ = theirs.write_all(&reply);
        }
    });
    (Transport::new(ours.try_clone().unwrap(), ours), asked)
}

#[test]
fn a_create_that_collides_removes_nothing_from_the_server() {
    let b = bench();
    std::fs::write(b.local.join("a.txt"), "a").unwrap();
    std::fs::create_dir(b.local.join("folder")).unwrap();
    for name in ["a.txt", "folder"] {
        let (transport, asked) = taken_names(Create::Collide);
        b.next.lock().unwrap().push_back(Next::Given(transport));
        let job = b.run(b.upload(&[name], Conflict::Stop));
        assert_eq!(job.state, State::Failed, "{job:?}");
        let asked = asked.lock().unwrap().clone();
        assert!(asked.contains(&3) || asked.contains(&14), "{asked:?}");
        assert!(
            !asked.contains(&13) && !asked.contains(&15),
            "removed something it didn't make: {asked:?}"
        );
    }
}

#[test]
fn a_connection_lost_while_creating_leaves_the_temp_as_unproven() {
    let b = bench();
    std::fs::write(b.local.join("a.txt"), "a").unwrap();
    let (transport, asked) = taken_names(Create::HangUp);
    b.next.lock().unwrap().push_back(Next::Given(transport));
    let job = b.run(b.upload(&["a.txt"], Conflict::Stop));
    assert_eq!(job.state, State::Interrupted, "{job:?}");
    let why = first_failure(&job);
    assert!(why.contains("can't tell whether this job made it"), "{why}");
    let asked = asked.lock().unwrap().clone();
    assert!(!asked.contains(&13) && !asked.contains(&15), "{asked:?}");

    // Continuing says so once, removes nothing, and then copies.
    let reported = b.resume(job.id, None);
    assert_eq!(reported.state, State::Failed, "{reported:?}");
    assert!(
        first_failure(&reported).contains("left it there"),
        "{reported:?}"
    );
    let done = b.resume(job.id, None);
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(b.on_server(), ["a.txt"]);
}

#[test]
fn recovery_leaves_on_the_server_what_it_cant_prove_this_job_made() {
    let b = bench();
    std::fs::write(b.local.join("a.txt"), "a").unwrap();
    let theirs = b.server.join(".monodrive-1-0-abc");
    std::fs::write(&theirs, "someone else's").unwrap();
    let op = b.queue(b.upload(&["a.txt"], Conflict::Stop));
    // Written down as about to be made, and then the owner crashed.
    let work = jobs::Work {
        source: b.local.join("a.txt"),
        left: Some(remote(&theirs)),
        ..jobs::Work::default()
    };
    b.shared.record(op, 0, &work).unwrap();

    let engine = Engine::open(paths(&b.root)).unwrap();
    let job = engine.jobs().into_iter().find(|job| job.id == op).unwrap();
    assert_eq!(job.state, State::Interrupted);
    let why = first_failure(&job);
    assert!(why.contains("can't tell whether this job made it"), "{why}");

    let shared = jobs::Shared::with_connector(
        engine,
        None,
        Arc::new(|_: &Source| Ok(fixture::transport())),
    );
    let resume = |id: &str| {
        let submission = Submission {
            id: id.into(),
            origin: Origin::Window,
            request: Request::Retry {
                job: op,
                conflict: None,
            },
        };
        shared.lock().submit(submission).unwrap();
        jobs::run(&shared, op);
        shared
            .lock()
            .jobs()
            .into_iter()
            .find(|job| job.id == op)
            .unwrap()
    };
    let reported = resume("first");
    assert_eq!(reported.state, State::Failed, "{reported:?}");
    assert!(
        first_failure(&reported).contains("left it there"),
        "{reported:?}"
    );
    assert_eq!(std::fs::read(&theirs).unwrap(), b"someone else's");
    assert_eq!(b.on_server(), [".monodrive-1-0-abc"]);

    let done = resume("second");
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(std::fs::read(&theirs).unwrap(), b"someone else's");
    assert_eq!(b.on_server(), [".monodrive-1-0-abc", "a.txt"]);
}
