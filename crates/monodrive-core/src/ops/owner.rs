//! The owner process and its clients. One owner per user runs every
//! operation; the window and each `monodrive mcp` reach it over a Unix
//! socket, one JSON line each way, in a folder only this user can open.

use super::jobs::{self, Shared};
use super::{Engine, JOURNAL, Job, Origin, Outcome, Paths, Request, Submission};
use serde::{Deserialize, Serialize};
use std::fs::{File, Permissions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SOCKET: &str = "owner.sock";
/// Held by the running owner, so there's only ever one.
const LOCK: &str = "owner.lock";
/// Held by a client while it starts an owner, so two don't race to.
const STARTING: &str = "starting.lock";
/// How long a new owner waits for the client that started it.
const FIRST_CLIENT_MS: i32 = 30_000;
/// A client that stops partway through a line is dropped after this.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// What a client asks the owner, one per line.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Message {
    /// Answered with a `Result<Outcome, String>`.
    Submit(Submission),
    /// Answered with a `Result<Vec<Job>, String>`.
    Jobs,
}

pub struct Owner {
    shared: Arc<Shared>,
    /// Readable when a job ends.
    woken: UnixStream,
    listener: UnixListener,
    socket: PathBuf,
    _lock: File,
}

impl Owner {
    /// Takes ownership of `paths.state`, settles what the last owner left
    /// unfinished, and listens. Fails while another owner has it.
    pub fn bind(paths: Paths) -> Result<Self, String> {
        let dir = paths.state.clone();
        private_dir(&dir)?;
        let lock = lock(&dir.join(LOCK), libc::LOCK_EX | libc::LOCK_NB).map_err(|e| {
            if e.kind() == io::ErrorKind::WouldBlock {
                format!("Another Monodrive owner is running for {}.", dir.display())
            } else {
                format!("{}: {e}", dir.join(LOCK).display())
            }
        })?;
        let engine = Engine::open(paths)?;
        let socket = dir.join(SOCKET);
        let fail = |e: io::Error| format!("{}: {e}", socket.display());
        if socket.exists() {
            // Left by an owner that crashed; with the lock held, nobody
            // listens on it.
            #[expect(
                clippy::disallowed_methods,
                reason = "the socket is the owner's own, not the user's file"
            )]
            std::fs::remove_file(&socket).map_err(fail)?;
        }
        let listener = UnixListener::bind(&socket).map_err(fail)?;
        listener.set_nonblocking(true).map_err(fail)?;
        let (woken, wake) = UnixStream::pair().map_err(|e| e.to_string())?;
        woken.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self {
            shared: Shared::new(engine, Some(wake)),
            woken,
            listener,
            socket,
            _lock: lock,
        })
    }

    /// Answers clients until the last one leaves and no job is going, or
    /// none comes. Requests are answered one at a time; copies and moves run
    /// as jobs on their own threads, so status keeps coming meanwhile.
    pub fn serve(mut self) -> io::Result<()> {
        let mut clients: Vec<BufReader<UnixStream>> = Vec::new();
        let mut served = false;
        loop {
            let busy = self.shared.lock().busy();
            let timeout = match (clients.is_empty(), served, busy) {
                (true, true, false) => return Ok(()),
                (true, false, false) => FIRST_CLIENT_MS,
                _ => -1,
            };
            let mut fds: Vec<libc::pollfd> = [self.listener.as_raw_fd(), self.woken.as_raw_fd()]
                .into_iter()
                .chain(clients.iter().map(|client| client.get_ref().as_raw_fd()))
                .map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            // SAFETY: `fds` holds `fds.len()` initialized pollfds.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if ready < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if ready == 0 {
                return Ok(());
            }
            if fds[1].revents != 0 {
                let mut drained = [0; 64];
                while io::Read::read(&mut self.woken, &mut drained).is_ok_and(|n| n > 0) {}
            }
            // From the end, so dropping a client keeps the others' places.
            for at in (0..clients.len()).rev() {
                if fds[at + 2].revents != 0 && !self.answer(&mut clients[at]) {
                    clients.swap_remove(at);
                }
            }
            if fds[0].revents != 0 {
                loop {
                    match self.listener.accept() {
                        Ok((stream, _)) => {
                            if let Some(client) = accepted(stream) {
                                clients.push(client);
                                served = true;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }

    /// Reads one request and replies. False once the client has gone.
    fn answer(&mut self, client: &mut BufReader<UnixStream>) -> bool {
        let mut line = String::new();
        match client.read_line(&mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        let reply = match serde_json::from_str::<Message>(&line) {
            Ok(Message::Submit(submission)) => {
                let outcome = self.shared.lock().submit(submission);
                jobs::start(&self.shared);
                serde_json::to_string(&outcome)
            }
            Ok(Message::Jobs) => serde_json::to_string(&Ok::<_, String>(self.shared.lock().jobs())),
            Err(e) => serde_json::to_string(&Err::<Outcome, _>(format!(
                "Monodrive couldn't read the request: {e}"
            ))),
        };
        let Ok(mut reply) = reply else {
            return false;
        };
        reply.push('\n');
        client.get_mut().write_all(reply.as_bytes()).is_ok()
    }
}

impl Drop for Owner {
    /// Before the lock goes, so the next owner never finds a live socket.
    fn drop(&mut self) {
        #[expect(
            clippy::disallowed_methods,
            reason = "the socket is the owner's own, not the user's file"
        )]
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// A connection from this user, ready to read; `None` from anyone else.
fn accepted(stream: UnixStream) -> Option<BufReader<UnixStream>> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: the socket is open for the call and both pointers are valid.
    let known = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0;
    if !known || uid != euid() {
        return None;
    }
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    Some(BufReader::new(stream))
}

fn euid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

/// Creates `dir` if needed and makes it this user's alone.
fn private_dir(dir: &Path) -> Result<(), String> {
    let fail = |e: io::Error| format!("{}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(fail)?;
    if std::fs::metadata(dir).map_err(fail)?.uid() != euid() {
        return Err(format!("{}: belongs to another user.", dir.display()));
    }
    std::fs::set_permissions(dir, Permissions::from_mode(0o700)).map_err(fail)
}

/// Opens `path` and flocks it; the lock lasts as long as the file is open.
fn lock(path: &Path, operation: libc::c_int) -> io::Result<File> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    // SAFETY: the file is open for the call.
    if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
        Ok(file)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `monodrive ops`: the owner, started by the first client that needs it.
/// Prints `ready` once clients can connect, or why it can't run.
pub fn run_owner() -> i32 {
    let owner = Paths::user()
        .ok_or_else(|| "No home folder.".to_string())
        .and_then(Owner::bind);
    let mut out = io::stdout();
    match owner {
        Err(e) => {
            let _ = writeln!(out, "{e}");
            1
        }
        Ok(owner) => {
            let _ = writeln!(out, "ready");
            let _ = out.flush();
            match owner.serve() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("monodrive ops: {e}");
                    1
                }
            }
        }
    }
}

/// A connection to the owner, opened on the first request and kept, which
/// keeps the owner running.
pub struct Client {
    paths: Paths,
    /// The Monodrive binary, to start an owner for `Paths::user` when none
    /// is running. Without it the client only connects.
    exe: Option<PathBuf>,
    conn: Option<BufReader<UnixStream>>,
    /// Makes request ids unique across processes.
    prefix: String,
    sent: u64,
}

impl Client {
    pub fn new(paths: Paths, exe: Option<PathBuf>) -> Self {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        Self {
            paths,
            exe,
            conn: None,
            prefix: format!("{}-{started}", std::process::id()),
            sent: 0,
        }
    }

    /// Runs `request` through the owner, starting one if needed. An error
    /// means nothing changed, or that the owner couldn't be reached.
    pub fn submit(&mut self, origin: Origin, request: Request) -> Result<Outcome, String> {
        self.sent += 1;
        let submission = Submission {
            id: format!("{}-{}", self.prefix, self.sent),
            origin,
            request,
        };
        // A lost connection may have lost the reply after the work was
        // done. The retry carries the same id, so the owner answers it from
        // its journal instead of running it again.
        self.ask(&Message::Submit(submission))
    }

    /// What Empty Trash would erase now. Read here, not by the owner, so it
    /// needs no connection.
    pub fn trash_items(&self) -> Result<Vec<super::TrashItem>, String> {
        super::trash_items(&self.paths.trash)
    }

    /// Every job still going and the latest finished, newest first.
    pub fn jobs(&mut self) -> Result<Vec<Job>, String> {
        self.ask(&Message::Jobs)
    }

    /// Calls `changed` each time the owner's journal changes; see `Watch`.
    pub fn watch(&self, changed: impl Fn() + Send + 'static) -> io::Result<Watch> {
        Watch::start(&self.paths.state, changed)
    }

    /// Sends `message`, again on a new connection if the first is lost.
    fn ask<T: for<'de> Deserialize<'de>>(&mut self, message: &Message) -> Result<T, String> {
        let mut line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        line.push('\n');
        let reply = self
            .send(&line)
            .or_else(|_| {
                self.conn = None;
                self.send(&line)
            })
            .map_err(|e| format!("Monodrive's file operations aren't reachable: {e}"))?;
        serde_json::from_str::<Result<T, String>>(&reply)
            .map_err(|e| format!("Monodrive's file operations replied oddly: {e}"))?
    }

    fn send(&mut self, line: &str) -> io::Result<String> {
        let conn = match &mut self.conn {
            Some(conn) => conn,
            slot @ None => slot.insert(BufReader::new(connect(&self.paths, self.exe.as_deref())?)),
        };
        conn.get_mut().write_all(line.as_bytes())?;
        let mut reply = String::new();
        if conn.read_line(&mut reply)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the owner stopped",
            ));
        }
        Ok(reply)
    }
}

/// Calls `changed` on its own thread each time the owner's journal is
/// written, made or replaced: someone asked for a change, maybe an agent
/// for a new job. The window checks its jobs then, instead of asking the
/// owner all the time. It reads nothing and holds no connection, so it never
/// keeps an owner running. Dropping it stops the thread.
pub struct Watch {
    queue: OwnedFd,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The `ident` of the event that stops the thread.
const STOP: usize = 0;

impl Watch {
    fn start(state: &Path, changed: impl Fn() + Send + 'static) -> io::Result<Self> {
        private_dir(state).map_err(io::Error::other)?;
        // SAFETY: kqueue has no preconditions; the descriptor is owned here.
        let queue = match unsafe { libc::kqueue() } {
            -1 => return Err(io::Error::last_os_error()),
            fd => unsafe { OwnedFd::from_raw_fd(fd) },
        };
        let raw = queue.as_raw_fd();
        listen(raw, STOP, libc::EVFILT_USER, 0)?;
        // The folder tells when the journal is made or replaced (a trim
        // renames a new one into place); the file when it's written.
        let folder = open_events(state)?;
        listen(
            raw,
            folder.as_raw_fd() as usize,
            libc::EVFILT_VNODE,
            libc::NOTE_WRITE,
        )?;
        let at = state.join(JOURNAL);
        let mut journal = follow(raw, &at);
        let thread = std::thread::spawn(move || {
            let _folder = folder;
            loop {
                // SAFETY: an all-zero kevent is valid, and the call writes
                // at most the one it's given.
                let mut event: libc::kevent = unsafe { std::mem::zeroed() };
                let n = unsafe {
                    libc::kevent(raw, std::ptr::null(), 0, &mut event, 1, std::ptr::null())
                };
                if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if n <= 0 || event.filter == libc::EVFILT_USER {
                    return;
                }
                let gone = libc::NOTE_DELETE | libc::NOTE_RENAME;
                let on_journal = journal
                    .as_ref()
                    .is_some_and(|fd| event.ident == fd.as_raw_fd() as usize);
                if !on_journal || event.fflags & gone != 0 {
                    journal = follow(raw, &at);
                }
                changed();
            }
        });
        Ok(Self {
            queue,
            thread: Some(thread),
        })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut stop = event(STOP, libc::EVFILT_USER, libc::NOTE_TRIGGER, 0);
        // SAFETY: the queue is open until this returns, and the change list
        // holds one event.
        unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &raw mut stop,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn event(ident: usize, filter: i16, fflags: u32, flags: u16) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Adds `ident` to the queue, reporting each change once.
fn listen(queue: RawFd, ident: usize, filter: i16, fflags: u32) -> io::Result<()> {
    let mut change = event(ident, filter, fflags, libc::EV_ADD | libc::EV_CLEAR);
    // SAFETY: the change list holds one event and nothing is read back.
    let added = unsafe {
        libc::kevent(
            queue,
            &raw mut change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if added < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Opens `path` only to hear about it, which never keeps a volume busy.
fn open_events(path: &Path) -> io::Result<OwnedFd> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: the path is a valid C string; the descriptor is owned here.
    match unsafe { libc::open(path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) } {
        -1 => Err(io::Error::last_os_error()),
        fd => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    }
}

/// Listens to the journal at `at`, if it's there yet. Closing the last one
/// took it off the queue.
fn follow(queue: RawFd, at: &Path) -> Option<OwnedFd> {
    let journal = open_events(at).ok()?;
    let fflags = libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_DELETE | libc::NOTE_RENAME;
    listen(
        queue,
        journal.as_raw_fd() as usize,
        libc::EVFILT_VNODE,
        fflags,
    )
    .ok()?;
    Some(journal)
}

/// Connects to the running owner, or starts one detached and connects.
fn connect(paths: &Paths, exe: Option<&Path>) -> io::Result<UnixStream> {
    let socket = paths.state.join(SOCKET);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    let Some(exe) = exe else {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "no owner is running",
        ));
    };
    private_dir(&paths.state).map_err(io::Error::other)?;
    let _starting = lock(&paths.state.join(STARTING), libc::LOCK_EX)?;
    // Another client may have started one while this one waited.
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    // Its own process group and `/` as its folder, so Ctrl-C in a terminal
    // doesn't stop it and it never keeps a volume from ejecting.
    let mut owner = Command::new(exe)
        .arg("ops")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let mut said = String::new();
    if let Some(out) = owner.stdout.take() {
        BufReader::new(out).read_line(&mut said)?;
    }
    // Reaped when it exits, so it never lingers as a zombie.
    std::thread::spawn(move || owner.wait());
    match said.trim() {
        "ready" => UnixStream::connect(&socket),
        "" => Err(io::Error::other("the owner didn't start")),
        why => Err(io::Error::other(why.to_string())),
    }
}
