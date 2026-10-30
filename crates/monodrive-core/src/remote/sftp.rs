//! A client for SFTP version 3 over any `Read` and `Write` pair: only the
//! requests Monodrive uses. A reader thread hands replies to whichever request
//! waits for each, and a writer thread sends, so a hung server fails a request
//! by its timeout instead of blocking the caller in `write`. Everything a
//! server sends is untrusted: frames and replies are bounded and checked, and
//! one bad reply ends the connection.

use super::ssh::{self, Process};
use super::{RemoteError, Transport};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The longest frame either side sends, as in OpenSSH.
pub(super) const MAX_PACKET: usize = 256 << 10;
/// How much one read or write asks for.
pub(super) const CHUNK: u32 = 32 << 10;
/// How many reads or writes of one file are on their way at once.
pub(super) const WINDOW: usize = 32;
/// How many requests of any kind are on their way at once on a connection.
const MAX_REQUESTS: usize = 64;
const MAX_HANDLE: usize = 256;
/// How often a wait for a reply looks for a cancel.
const CANCEL_TICK: Duration = Duration::from_millis(100);

const INIT: u8 = 1;
const VERSION: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const READLINK: u8 = 19;
const SYMLINK: u8 = 20;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;
const EXTENDED: u8 = 200;

const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;
const FX_NO_SUCH_FILE: u32 = 2;
const FX_PERMISSION_DENIED: u32 = 3;
const FX_OP_UNSUPPORTED: u32 = 8;

const ATTR_SIZE: u32 = 1;
const ATTR_UIDGID: u32 = 2;
const ATTR_PERMISSIONS: u32 = 4;
const ATTR_ACMODTIME: u32 = 8;
const ATTR_EXTENDED: u32 = 0x8000_0000;

pub(super) const OPEN_READ: u32 = 1;
pub(super) const OPEN_NEW: u32 = 2 | 8 | 0x20;

const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// What the server says about an item. Anything it left out is `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Attrs {
    pub(crate) size: Option<u64>,
    pub(crate) permissions: Option<u32>,
    pub(crate) modified: Option<u32>,
}

/// One name a directory read returned. `None` when it isn't UTF-8.
pub(super) struct Name {
    pub(super) name: Option<String>,
    pub(super) attrs: Attrs,
}

pub(super) struct Handle(Vec<u8>);

struct Reply {
    kind: u8,
    body: Vec<u8>,
}

#[derive(Default)]
struct Going {
    waiting: HashMap<u32, Sender<Result<Reply, RemoteError>>>,
    lost: Option<RemoteError>,
    next: u32,
}

struct Shared {
    going: Mutex<Going>,
    room: Condvar,
    process: Mutex<Option<Process>>,
}

impl Shared {
    fn going(&self) -> MutexGuard<'_, Going> {
        self.going.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn process(&self) -> MutexGuard<'_, Option<Process>> {
        self.process.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the connection with `error`, kept for every request after. Kills
    /// the subprocess this connection started, by its handle.
    fn lose(&self, error: RemoteError) {
        {
            let mut going = self.going();
            let error = going.lost.get_or_insert(error).clone();
            for (_, waiting) in going.waiting.drain() {
                let _ = waiting.send(Err(error.clone()));
            }
        }
        self.room.notify_all();
        if let Some(process) = self.process().take() {
            process.kill();
        }
    }

    /// The server's side closed: what ssh said about it, if it was ssh.
    fn ended(&self, plain: RemoteError) {
        let error = self
            .process()
            .as_mut()
            .and_then(|process| ssh::diagnose(&process.said_once_closed(), process.host()))
            .unwrap_or(plain);
        self.lose(error);
    }

    fn deliver(&self, id: u32, reply: Reply) {
        // A request that gave up, as a cancelled transfer does, gets nothing.
        let mut going = self.going();
        if let Some(waiting) = going.waiting.remove(&id) {
            let _ = waiting.send(Ok(reply));
            drop(going);
            self.room.notify_one();
        }
    }
}

pub(super) struct Sftp {
    shared: Arc<Shared>,
    sends: Sender<Vec<u8>>,
    timeout: Duration,
    can_replace: bool,
}

impl Drop for Sftp {
    fn drop(&mut self) {
        self.close();
    }
}

impl Sftp {
    /// Starts the threads and agrees on the protocol. Nothing else is sent.
    pub(super) fn start(
        transport: Transport,
        connect: Duration,
        timeout: Duration,
    ) -> Result<Self, RemoteError> {
        let Transport {
            reader,
            writer,
            process,
        } = transport;
        let shared = Arc::new(Shared {
            going: Mutex::default(),
            room: Condvar::new(),
            process: Mutex::new(process),
        });
        let (sends, sending) = mpsc::channel::<Vec<u8>>();
        {
            let shared = shared.clone();
            std::thread::spawn(move || write_loop(writer, &sending, &shared));
        }
        {
            let shared = shared.clone();
            std::thread::spawn(move || read_loop(reader, &shared));
        }
        let mut sftp = Self {
            shared,
            sends,
            timeout: connect,
            can_replace: false,
        };
        let mut init = Vec::with_capacity(9);
        init.extend_from_slice(&5u32.to_be_bytes());
        init.push(INIT);
        init.extend_from_slice(&3u32.to_be_bytes());
        let reply = sftp.register(init, None)?.wait(None)?;
        if reply.kind != VERSION {
            return Err(sftp.malformed("didn't answer with its SFTP version"));
        }
        let mut cursor = Cursor(&reply.body);
        let version = cursor.u32().map_err(|why| sftp.malformed(why))?;
        if version < 3 {
            return Err(sftp.malformed(&format!("speaks SFTP {version}, older than 3")));
        }
        while !cursor.0.is_empty() {
            let name = cursor.bytes().map_err(|why| sftp.malformed(why))?;
            cursor.bytes().map_err(|why| sftp.malformed(why))?;
            sftp.can_replace |= name == POSIX_RENAME.as_bytes();
        }
        sftp.timeout = timeout;
        Ok(sftp)
    }

    pub(super) fn alive(&self) -> bool {
        self.shared.going().lost.is_none()
    }

    /// Whether the server can put a file over another in one step.
    pub(super) fn can_replace(&self) -> bool {
        self.can_replace
    }

    pub(super) fn close(&self) {
        self.shared.lose(RemoteError::Disconnected(
            "the connection was closed".into(),
        ));
    }

    fn malformed(&self, why: &str) -> RemoteError {
        let error = RemoteError::Protocol(format!("the server {why}"));
        self.shared.lose(error.clone());
        error
    }

    /// Queues a whole frame, waiting for room among the requests on their
    /// way. `id` is `None` for the one frame without an id, INIT, whose reply
    /// comes as id 0.
    fn register(&self, frame: Vec<u8>, id: Option<u32>) -> Result<Pending<'_>, RemoteError> {
        let (tx, rx) = mpsc::channel();
        let mut going = self.shared.going();
        loop {
            if let Some(lost) = &going.lost {
                return Err(lost.clone());
            }
            if going.waiting.len() < MAX_REQUESTS {
                break;
            }
            let (next, waited) = self
                .shared
                .room
                .wait_timeout(going, self.timeout)
                .unwrap_or_else(PoisonError::into_inner);
            going = next;
            if waited.timed_out() {
                drop(going);
                return Err(self.timed_out());
            }
        }
        going.waiting.insert(id.unwrap_or(0), tx);
        drop(going);
        if self.sends.send(frame).is_err() {
            return Err(RemoteError::Disconnected("the connection closed".into()));
        }
        Ok(Pending { sftp: self, rx })
    }

    fn timed_out(&self) -> RemoteError {
        let error = RemoteError::Timeout(format!(
            "the server didn't answer within {} seconds",
            self.timeout.as_secs_f32()
        ));
        self.shared.lose(error.clone());
        error
    }

    fn send(&self, kind: u8, body: impl FnOnce(&mut Vec<u8>)) -> Result<Pending<'_>, RemoteError> {
        let id = {
            let mut going = self.shared.going();
            // 0 stays INIT's, and ids in use are skipped after a wrap.
            loop {
                going.next = going.next.wrapping_add(1);
                let id = going.next;
                if id != 0 && !going.waiting.contains_key(&id) {
                    break id;
                }
            }
        };
        let mut frame = Vec::with_capacity(64);
        frame.extend_from_slice(&[0; 4]);
        frame.push(kind);
        frame.extend_from_slice(&id.to_be_bytes());
        body(&mut frame);
        let length = frame.len() - 4;
        if length > MAX_PACKET {
            return Err(RemoteError::Failed("That path is too long.".into()));
        }
        frame[..4].copy_from_slice(&(length as u32).to_be_bytes());
        self.register(frame, Some(id))
    }

    fn ask(&self, kind: u8, body: impl FnOnce(&mut Vec<u8>)) -> Result<Reply, RemoteError> {
        self.send(kind, body)?.wait(None)
    }

    /// A reply that's only a status: fine, or the error it stands for.
    fn checked(&self, reply: &Reply) -> Result<(), RemoteError> {
        match self.status(reply)? {
            (FX_OK, _) => Ok(()),
            (code, message) => Err(status_error(code, message)),
        }
    }

    fn status(&self, reply: &Reply) -> Result<(u32, String), RemoteError> {
        if reply.kind != STATUS {
            return Err(self.malformed("answered with the wrong kind of reply"));
        }
        let mut cursor = Cursor(&reply.body);
        let code = cursor.u32().map_err(|why| self.malformed(why))?;
        // Some old servers leave the message and language out.
        let message = cursor.bytes().unwrap_or_default();
        Ok((code, String::from_utf8_lossy(message).into_owned()))
    }

    /// The error a status reply stands for, when a reply of `kind` was asked
    /// for, or `None` when it is that kind.
    fn unexpected(&self, reply: &Reply, kind: u8) -> Option<RemoteError> {
        if reply.kind == kind {
            return None;
        }
        Some(match self.status(reply) {
            Ok((FX_OK, _)) => self.malformed("answered with the wrong kind of reply"),
            Ok((code, message)) => status_error(code, message),
            Err(error) => error,
        })
    }

    fn attrs_of(&self, kind: u8, path: &str) -> Result<Attrs, RemoteError> {
        let reply = self.ask(kind, |body| put(body, path.as_bytes()))?;
        if let Some(error) = self.unexpected(&reply, ATTRS) {
            return Err(error);
        }
        Cursor(&reply.body)
            .attrs()
            .map_err(|why| self.malformed(why))
    }

    /// What's at `path`, following a link.
    pub(super) fn stat(&self, path: &str) -> Result<Attrs, RemoteError> {
        self.attrs_of(STAT, path)
    }

    /// What's at `path`, the link itself when it's one.
    pub(super) fn lstat(&self, path: &str) -> Result<Attrs, RemoteError> {
        self.attrs_of(LSTAT, path)
    }

    /// The one name a request answers with.
    fn one_name(&self, kind: u8, path: &str) -> Result<String, RemoteError> {
        let reply = self.ask(kind, |body| put(body, path.as_bytes()))?;
        if let Some(error) = self.unexpected(&reply, NAME) {
            return Err(error);
        }
        let mut cursor = Cursor(&reply.body);
        let name = cursor
            .u32()
            .and_then(|count| {
                if count == 0 {
                    return Err("answered with no name");
                }
                cursor.bytes()
            })
            .map_err(|why| self.malformed(why))?;
        String::from_utf8(name.to_vec()).map_err(|_| self.malformed("sent a name that isn't UTF-8"))
    }

    pub(super) fn realpath(&self, path: &str) -> Result<String, RemoteError> {
        self.one_name(REALPATH, path)
    }

    pub(super) fn readlink(&self, path: &str) -> Result<String, RemoteError> {
        self.one_name(READLINK, path)
    }

    fn handle(&self, reply: &Reply) -> Result<Handle, RemoteError> {
        if let Some(error) = self.unexpected(reply, HANDLE) {
            return Err(error);
        }
        let handle = Cursor(&reply.body)
            .bytes()
            .map_err(|why| self.malformed(why))?;
        if handle.len() > MAX_HANDLE {
            return Err(self.malformed("sent a handle longer than SFTP allows"));
        }
        Ok(Handle(handle.to_vec()))
    }

    pub(super) fn opendir(&self, path: &str) -> Result<Handle, RemoteError> {
        let reply = self.ask(OPENDIR, |body| put(body, path.as_bytes()))?;
        self.handle(&reply)
    }

    /// The next names in the directory, or `None` at its end.
    pub(super) fn readdir(&self, handle: &Handle) -> Result<Option<Vec<Name>>, RemoteError> {
        let reply = self.ask(READDIR, |body| put(body, &handle.0))?;
        if reply.kind == STATUS {
            return match self.status(&reply)? {
                (FX_EOF, _) => Ok(None),
                (FX_OK, _) => Err(self.malformed("answered with the wrong kind of reply")),
                (code, message) => Err(status_error(code, message)),
            };
        }
        if let Some(error) = self.unexpected(&reply, NAME) {
            return Err(error);
        }
        let mut cursor = Cursor(&reply.body);
        let count = cursor.u32().map_err(|why| self.malformed(why))?;
        // Each counted name takes room in the frame, so this can't grow past
        // what the frame holds.
        let mut names = Vec::new();
        for _ in 0..count {
            let name = cursor.bytes().map_err(|why| self.malformed(why))?;
            cursor.bytes().map_err(|why| self.malformed(why))?;
            let attrs = cursor.attrs().map_err(|why| self.malformed(why))?;
            names.push(Name {
                name: String::from_utf8(name.to_vec()).ok(),
                attrs,
            });
        }
        Ok(Some(names))
    }

    pub(super) fn close_handle(&self, handle: Handle) -> Result<(), RemoteError> {
        let reply = self.ask(CLOSE, |body| put(body, &handle.0))?;
        self.checked(&reply)
    }

    /// Opens a file; with `OPEN_NEW`, one this call creates, or fails.
    pub(super) fn open(&self, path: &str, flags: u32, mode: u32) -> Result<Handle, RemoteError> {
        let reply = self.ask(OPEN, |body| {
            put(body, path.as_bytes());
            body.extend_from_slice(&flags.to_be_bytes());
            put_mode(body, mode);
        })?;
        self.handle(&reply)
    }

    pub(super) fn send_read(
        &self,
        handle: &Handle,
        offset: u64,
        len: u32,
    ) -> Result<Pending<'_>, RemoteError> {
        self.send(READ, |body| {
            put(body, &handle.0);
            body.extend_from_slice(&offset.to_be_bytes());
            body.extend_from_slice(&len.to_be_bytes());
        })
    }

    pub(super) fn send_write(
        &self,
        handle: &Handle,
        offset: u64,
        data: &[u8],
    ) -> Result<Pending<'_>, RemoteError> {
        self.send(WRITE, |body| {
            put(body, &handle.0);
            body.extend_from_slice(&offset.to_be_bytes());
            put(body, data);
        })
    }

    /// The data a read answered with: empty at the end of the file, and
    /// never more than `len`.
    fn data(&self, reply: &Reply, len: u32) -> Result<Vec<u8>, RemoteError> {
        if reply.kind == STATUS {
            return match self.status(reply)? {
                (FX_EOF, _) => Ok(Vec::new()),
                (FX_OK, _) => Err(self.malformed("answered a read without data")),
                (code, message) => Err(status_error(code, message)),
            };
        }
        if let Some(error) = self.unexpected(reply, DATA) {
            return Err(error);
        }
        let data = Cursor(&reply.body)
            .bytes()
            .map_err(|why| self.malformed(why))?;
        if data.len() > len as usize {
            return Err(self.malformed("sent more data than it was asked for"));
        }
        Ok(data.to_vec())
    }

    pub(super) fn mkdir(&self, path: &str, mode: u32) -> Result<(), RemoteError> {
        let reply = self.ask(MKDIR, |body| {
            put(body, path.as_bytes());
            put_mode(body, mode);
        })?;
        self.checked(&reply)
    }

    /// Never replaces: SFTP 3's rename fails when `to` exists.
    pub(super) fn rename(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        let reply = self.ask(RENAME, |body| {
            put(body, from.as_bytes());
            put(body, to.as_bytes());
        })?;
        self.checked(&reply)
    }

    /// Puts `from` over the file at `to` in one step.
    pub(super) fn replace(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        let reply = self.ask(EXTENDED, |body| {
            put(body, POSIX_RENAME.as_bytes());
            put(body, from.as_bytes());
            put(body, to.as_bytes());
        })?;
        self.checked(&reply)
    }

    /// OpenSSH sends the link's target first, whatever the draft says.
    pub(super) fn symlink(&self, target: &str, link: &str) -> Result<(), RemoteError> {
        let reply = self.ask(SYMLINK, |body| {
            put(body, target.as_bytes());
            put(body, link.as_bytes());
        })?;
        self.checked(&reply)
    }

    pub(super) fn remove(&self, path: &str) -> Result<(), RemoteError> {
        let reply = self.ask(REMOVE, |body| put(body, path.as_bytes()))?;
        self.checked(&reply)
    }

    pub(super) fn rmdir(&self, path: &str) -> Result<(), RemoteError> {
        let reply = self.ask(RMDIR, |body| put(body, path.as_bytes()))?;
        self.checked(&reply)
    }
}

/// A request on its way.
pub(super) struct Pending<'a> {
    sftp: &'a Sftp,
    rx: Receiver<Result<Reply, RemoteError>>,
}

impl Pending<'_> {
    /// Waits for the reply. A hung server ends the connection at the timeout;
    /// `stop`, when it's set, ends only the wait.
    fn wait(self, stop: Option<&AtomicBool>) -> Result<Reply, RemoteError> {
        let tick = match stop {
            Some(_) => CANCEL_TICK.min(self.sftp.timeout),
            None => self.sftp.timeout,
        };
        let mut waited = Duration::ZERO;
        loop {
            if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
                return Err(RemoteError::Cancelled);
            }
            match self.rx.recv_timeout(tick) {
                Ok(reply) => return reply,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(RemoteError::Disconnected("the connection closed".into()));
                }
                Err(RecvTimeoutError::Timeout) => {
                    waited += tick;
                    if waited >= self.sftp.timeout {
                        return Err(self.sftp.timed_out());
                    }
                }
            }
        }
    }

    pub(super) fn read(self, len: u32, stop: Option<&AtomicBool>) -> Result<Vec<u8>, RemoteError> {
        let sftp = self.sftp;
        sftp.data(&self.wait(stop)?, len)
    }

    pub(super) fn write(self, stop: Option<&AtomicBool>) -> Result<(), RemoteError> {
        let sftp = self.sftp;
        sftp.checked(&self.wait(stop)?)
    }
}

fn status_error(code: u32, message: String) -> RemoteError {
    match code {
        FX_NO_SUCH_FILE => RemoteError::NotFound(message),
        FX_PERMISSION_DENIED => RemoteError::Denied(message),
        FX_OP_UNSUPPORTED => RemoteError::Unsupported(message),
        _ => RemoteError::Failed(message),
    }
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn put_mode(out: &mut Vec<u8>, mode: u32) {
    out.extend_from_slice(&ATTR_PERMISSIONS.to_be_bytes());
    out.extend_from_slice(&mode.to_be_bytes());
}

fn write_loop(mut writer: Box<dyn Write + Send>, frames: &Receiver<Vec<u8>>, shared: &Shared) {
    while let Ok(frame) = frames.recv() {
        if writer
            .write_all(&frame)
            .and_then(|()| writer.flush())
            .is_err()
        {
            shared.ended(RemoteError::Disconnected("the connection closed".into()));
            return;
        }
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, shared: &Shared) {
    loop {
        let body = match read_frame(&mut reader) {
            Ok(body) => body,
            Err(Frame::Closed) => {
                return shared.ended(RemoteError::Disconnected("the connection closed".into()));
            }
            Err(Frame::Bad(why)) => return shared.lose(RemoteError::Protocol(why)),
        };
        let kind = body[0];
        if kind == VERSION {
            let body = body[1..].to_vec();
            shared.deliver(0, Reply { kind, body });
            continue;
        }
        let Some(id) = body.get(1..5) else {
            return shared.lose(RemoteError::Protocol(
                "the server sent a reply too short to read".into(),
            ));
        };
        let id = u32::from_be_bytes([id[0], id[1], id[2], id[3]]);
        let body = body[5..].to_vec();
        shared.deliver(id, Reply { kind, body });
    }
}

enum Frame {
    /// Closed, also partway through a frame.
    Closed,
    Bad(String),
}

/// One whole frame's body, never more than `MAX_PACKET`, and never empty.
fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>, Frame> {
    let mut length = [0; 4];
    reader.read_exact(&mut length).map_err(closed)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_PACKET {
        // A shell startup file printing into the stream reads as a huge length.
        return Err(Frame::Bad(format!(
            "the server sent a frame of {length} bytes, outside what SFTP allows. A shell \
            startup file printing text on the server does this."
        )));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(closed)?;
    Ok(body)
}

fn closed(e: std::io::Error) -> Frame {
    if matches!(
        e.kind(),
        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe
    ) {
        Frame::Closed
    } else {
        Frame::Bad(format!("reading from the server failed: {e}"))
    }
}

/// Reads fields from a reply. Any field past its end is an error.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        if self.0.len() < n {
            return Err("sent a reply cut short");
        }
        let (taken, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from(self.u32()?) << 32 | u64::from(self.u32()?))
    }

    fn bytes(&mut self) -> Result<&'a [u8], &'static str> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn attrs(&mut self) -> Result<Attrs, &'static str> {
        let flags = self.u32()?;
        let mut attrs = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            attrs.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            self.take(8)?;
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attrs.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            self.take(4)?;
            attrs.modified = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(attrs)
    }
}
