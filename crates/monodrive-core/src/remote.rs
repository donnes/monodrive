//! Remote locations over SFTP: saved sources, connections to them, and
//! browsing. Transfers run as owner jobs, through `ops`. A remote path is a
//! `RemotePath`, a type of its own that nothing local accepts. All of this
//! blocks on the network, so the window calls it off the UI thread.
//! docs/remote.md records why.

mod sftp;
pub mod ssh;

#[cfg(test)]
pub(crate) mod fixture;
#[cfg(test)]
mod tests;

pub use crate::ops::disk::Meter;

use crate::entries::Kind;
use crate::ops;
use crate::places;
use serde::{Deserialize, Serialize};
use sftp::{Attrs, CHUNK, Handle, OPEN_NEW, OPEN_READ, Pending, Sftp, WINDOW};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{File, Permissions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::id as process_id;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many entries one listing returns at most.
pub const MOST_LISTED: usize = 100_000;
/// How much one read returns at most.
pub const MOST_READ: u32 = 1 << 20;
/// How many items a folder being copied may hold.
const MOST_IN_FOLDER: usize = 500_000;
const DEEPEST: usize = 256;
/// What a copy's unfinished item on a server is named, so nothing else can
/// ever be mistaken for one when it's removed.
const TEMP_PREFIX: &str = ".monodrive-";
/// Why the job and any later Undo can't reverse what a copy to a server did.
pub const NO_UNDO: &str = "A copy to a remote source has no undo: SFTP has no Trash.";

/// Why something remote didn't work, for the user or agent to read. The window
/// can match on it to offer the way out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteError {
    UnknownSource(String),
    BadPath(String),
    /// Authenticating needs a password or other prompt, which ssh here can't
    /// show.
    InteractiveAuth(String),
    AuthFailed(String, String),
    /// The host's key isn't the one in known_hosts. Never bypassed.
    HostKeyChanged(String),
    /// known_hosts doesn't have this host, or ssh refused its key.
    HostKeyUnknown(String),
    Unreachable(String, String),
    /// The host has no SFTP subsystem.
    NoSftp(String),
    Timeout(String),
    Disconnected(String),
    /// The server broke the protocol, which ended the connection.
    Protocol(String),
    NotFound(String),
    Denied(String),
    Failed(String),
    Unsupported(String),
    /// An item changed while it was copied or since it was last seen.
    Changed(String),
    TooBig(String),
    Cancelled,
}

impl RemoteError {
    /// The connection is the problem, not one item: a job stops, recoverably,
    /// rather than failing each of its items the same way.
    pub fn is_connection(&self) -> bool {
        matches!(
            self,
            Self::InteractiveAuth(_)
                | Self::AuthFailed(..)
                | Self::HostKeyChanged(_)
                | Self::HostKeyUnknown(_)
                | Self::Unreachable(..)
                | Self::NoSftp(_)
                | Self::Timeout(_)
                | Self::Disconnected(_)
                | Self::Protocol(_)
        )
    }

    /// The same error, saying which item it's about.
    fn at(self, what: &impl fmt::Display) -> Self {
        let named = |why: String| format!("{what}: {why}");
        match self {
            Self::NotFound(why) => Self::NotFound(named(why)),
            Self::Denied(why) => Self::Denied(named(why)),
            Self::Failed(why) => Self::Failed(named(why)),
            Self::Unsupported(why) => Self::Unsupported(named(why)),
            other => other,
        }
    }
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSource(id) => write!(f, "{id} isn't a saved remote source."),
            Self::BadPath(why)
            | Self::Disconnected(why)
            | Self::Timeout(why)
            | Self::Protocol(why)
            | Self::NotFound(why)
            | Self::Denied(why)
            | Self::Failed(why)
            | Self::Unsupported(why)
            | Self::Changed(why)
            | Self::TooBig(why) => write!(f, "{why}"),
            Self::InteractiveAuth(host) => write!(
                f,
                "{host}: interactive authentication required. Monodrive never prompts; add a key \
                to ssh-agent, or connect once with ssh in a terminal."
            ),
            Self::AuthFailed(host, said) => write!(
                f,
                "{host}: authentication failed: {said} A key with a passphrase needs to be in \
                ssh-agent."
            ),
            Self::HostKeyChanged(host) => write!(
                f,
                "{host}: its host key changed since known_hosts recorded it, so Monodrive didn't \
                connect. This can mean someone is intercepting the connection. Check with the \
                server's owner, then fix known_hosts in a terminal."
            ),
            Self::HostKeyUnknown(host) => write!(
                f,
                "{host}: ssh couldn't verify its host key, so Monodrive didn't connect. To trust \
                this server, connect once with ssh in a terminal so it's in known_hosts."
            ),
            Self::Unreachable(host, said) => write!(f, "{host}: unreachable: {said}"),
            Self::NoSftp(host) => write!(f, "{host}: SFTP isn't enabled on this server."),
            Self::Cancelled => write!(f, "Cancelled."),
        }
    }
}

impl From<RemoteError> for String {
    fn from(error: RemoteError) -> Self {
        error.to_string()
    }
}

/// A saved server. Forgetting one changes nothing on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    /// A host, `user@host` or an alias in the user's ssh config.
    pub host: String,
    /// Where browsing starts; the server's default folder without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
}

impl Source {
    /// A source as it can be saved. `id` is what remote paths are qualified
    /// with, so it holds no `:`.
    pub fn new(id: &str, host: &str, start: Option<&str>) -> Result<Self, String> {
        check_id(id)?;
        check_host(host)?;
        let start = start
            .filter(|start| !start.is_empty())
            .map(|start| Ok::<_, String>(RemotePath::new(id, start)?.path))
            .transpose()?;
        Ok(Self {
            id: id.into(),
            host: host.into(),
            start,
        })
    }

    /// What every SFTP source can do. SFTP has no Trash, and Monodrive offers
    /// no remote delete, since nothing could bring the item back.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            list: true,
            read: true,
            write: true,
            mkdir: true,
            rename: true,
            metadata: true,
            trash: false,
            delete: false,
        }
    }
}

fn check_id(id: &str) -> Result<(), String> {
    let fine = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    if id.is_empty() || id.len() > 64 || !id.chars().all(fine) {
        return Err(format!(
            "{id}: a source's id takes letters, digits, -, _ and . only."
        ));
    }
    Ok(())
}

/// A host that can't read as an option or carry anything for a shell or a
/// `%h` in the user's config to expand.
fn check_host(host: &str) -> Result<(), String> {
    let fine = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | ':' | '%');
    if host.is_empty() || host.len() > 255 || host.starts_with('-') || !host.chars().all(fine) {
        return Err(format!("{host}: give a host, user@host or an ssh alias."));
    }
    Ok(())
}

/// The id a source for `host` gets unless it's given one.
pub(crate) fn id_for(host: &str) -> String {
    let name = host.rsplit('@').next().unwrap_or(host);
    let id: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(60)
        .collect();
    if id.is_empty() { "remote".into() } else { id }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub list: bool,
    pub read: bool,
    pub write: bool,
    pub mkdir: bool,
    pub rename: bool,
    pub metadata: bool,
    pub trash: bool,
    pub delete: bool,
}

/// An item on a saved source. It's not a local path and doesn't turn into one:
/// nothing local takes it, and printed as `source:/path` it's not absolute, so
/// nothing that takes a path can read it as a local one.
///
/// ```compile_fail
/// let path = monodrive_core::remote::RemotePath::parse("box:/a").unwrap();
/// let _ = std::fs::metadata(&path);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Unchecked")]
pub struct RemotePath {
    source: String,
    path: String,
}

#[derive(Deserialize)]
struct Unchecked {
    source: String,
    path: String,
}

impl TryFrom<Unchecked> for RemotePath {
    type Error = RemoteError;

    fn try_from(path: Unchecked) -> Result<Self, RemoteError> {
        Self::new(&path.source, &path.path)
    }
}

impl RemotePath {
    /// `path` on `source`: absolute, no `..`, and without `.`, empty parts or
    /// a trailing slash.
    pub fn new(source: &str, path: &str) -> Result<Self, RemoteError> {
        check_id(source).map_err(RemoteError::BadPath)?;
        let bad = |why: &str| Err(RemoteError::BadPath(format!("{source}:{path}: {why}")));
        if !path.starts_with('/') {
            return bad("remote paths must be absolute.");
        }
        if path.contains('\0') {
            return bad("a path can't hold a NUL.");
        }
        let mut clean = String::with_capacity(path.len());
        for part in path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
        {
            if part == ".." {
                return bad("a remote path can't go up with ..");
            }
            clean.push('/');
            clean.push_str(part);
        }
        if clean.is_empty() {
            clean.push('/');
        }
        Ok(Self {
            source: source.into(),
            path: clean,
        })
    }

    /// `source:/path`, as it prints.
    pub fn parse(qualified: &str) -> Result<Self, RemoteError> {
        match qualified.split_once(':') {
            Some((source, path)) => Self::new(source, path),
            None => Err(RemoteError::BadPath(format!(
                "{qualified}: give a remote path as source:/path."
            ))),
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Its last part; `None` for the root.
    pub fn name(&self) -> Option<&str> {
        self.path.rsplit('/').next().filter(|name| !name.is_empty())
    }

    pub fn parent(&self) -> Option<Self> {
        self.name()?;
        let (parent, _) = self.path.rsplit_once('/')?;
        Some(Self {
            source: self.source.clone(),
            path: if parent.is_empty() {
                "/".into()
            } else {
                parent.into()
            },
        })
    }

    /// The item `name` in this folder. A server's names are never trusted as
    /// paths: one that isn't a single plain name is an error.
    pub fn join(&self, name: &str) -> Result<Self, RemoteError> {
        if !plain_name(name) {
            return Err(RemoteError::BadPath(format!(
                "{self}: {name:?} isn't a name."
            )));
        }
        let mut path = self.path.clone();
        if path != "/" {
            path.push('/');
        }
        path.push_str(name);
        Ok(Self {
            source: self.source.clone(),
            path,
        })
    }
}

fn plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// `source:/path`. A control character in a name prints escaped, so one line
/// is always one path.
impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.source)?;
        self.path.chars().try_for_each(|c| {
            if c.is_control() {
                write!(f, "{}", c.escape_default())
            } else {
                write!(f, "{c}")
            }
        })
    }
}

/// One file, folder or link on a source, as `entries::Entry` is on this Mac.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoteEntry {
    pub path: RemotePath,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    /// Seconds since the Unix epoch; 0 when the server doesn't say.
    pub modified: u64,
    pub permissions: Option<u32>,
    /// Where a link leads, read by `Connection::metadata` only.
    pub link: Option<String>,
}

impl RemoteEntry {
    fn new(path: RemotePath, attrs: Attrs) -> Self {
        Self {
            name: path.name().unwrap_or("/").to_string(),
            path,
            kind: kind_of(attrs),
            size: attrs.size.unwrap_or(0),
            modified: attrs.modified.map_or(0, u64::from),
            permissions: attrs.permissions.map(|mode| mode & 0o7777),
            link: None,
        }
    }
}

fn kind_of(attrs: Attrs) -> Kind {
    match attrs.permissions.map(|mode| mode & u32::from(libc::S_IFMT)) {
        Some(mode) if mode == u32::from(libc::S_IFDIR) => Kind::Dir,
        Some(mode) if mode == u32::from(libc::S_IFREG) => Kind::File,
        Some(mode) if mode == u32::from(libc::S_IFLNK) => Kind::Link,
        _ => Kind::Other,
    }
}

/// One stretch of a listing, in the server's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub entries: Vec<RemoteEntry>,
    /// The folder holds more after these.
    pub more: bool,
    /// Names seen so far that Monodrive can't address, because they aren't
    /// UTF-8 or aren't plain names.
    pub left_out: u64,
}

/// How long a connection waits. A request that takes longer ends it.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub connect: Duration,
    pub request: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(20),
            request: Duration::from_secs(60),
        }
    }
}

/// The two ends of an SFTP stream, and the process behind them, if any.
pub struct Transport {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    process: Option<ssh::Process>,
}

impl Transport {
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Self {
        Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            process: None,
        }
    }
}

/// Opens the stream to a source. `ssh::connector` is the real one.
pub type Connector = Arc<dyn Fn(&Source) -> Result<Transport, RemoteError> + Send + Sync>;

/// This process's connections, at most one per source, opened on demand.
pub struct Connections {
    store: places::Store,
    connector: Connector,
    limits: Limits,
    open: Mutex<HashMap<String, Slot>>,
}

/// A source's connection. Held while connecting, so a second caller waits for
/// this connection instead of making another.
type Slot = Arc<Mutex<Option<Arc<Connection>>>>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Connections {
    pub fn new(store: places::Store, connector: Connector) -> Self {
        Self {
            store,
            connector,
            limits: Limits::default(),
            open: Mutex::default(),
        }
    }

    /// The user's saved sources, through the system's ssh.
    pub fn user() -> Option<Self> {
        Some(Self::new(places::Store::user()?, ssh::connector()))
    }

    /// The connection to source `id`, opened now if it has none or lost it.
    /// Opening does nothing on the server but agree on the protocol: no scan,
    /// no listing.
    pub fn connect(&self, id: &str) -> Result<Arc<Connection>, RemoteError> {
        let slot = locked(&self.open).entry(id.into()).or_default().clone();
        let mut slot = locked(&slot);
        if let Some(open) = slot.as_ref().filter(|open| open.alive()) {
            return Ok(open.clone());
        }
        let source = self.store.remote(id)?;
        let transport = (self.connector)(&source)?;
        let open = Arc::new(Connection::open(source, transport, self.limits)?);
        *slot = Some(open.clone());
        Ok(open)
    }

    /// Ends the connection to source `id`, if it has one, and its subprocess.
    /// A job using it stops, recoverably. Nothing on the server changes.
    pub fn disconnect(&self, id: &str) -> bool {
        let slot = locked(&self.open).remove(id);
        let open = slot.and_then(|slot| locked(&slot).take());
        open.is_some_and(|open| {
            open.sftp.close();
            true
        })
    }

    pub fn is_connected(&self, id: &str) -> bool {
        let slot = locked(&self.open).get(id).cloned();
        slot.is_some_and(|slot| locked(&slot).as_ref().is_some_and(|open| open.alive()))
    }

    /// Ends every connection nothing else holds, so an owner keeps none open
    /// once its jobs are done.
    pub(crate) fn release_idle(&self) {
        let slots: Vec<Slot> = locked(&self.open).values().cloned().collect();
        for slot in slots {
            // One that's busy connecting is about to be used.
            if let Ok(mut slot) = slot.try_lock()
                && slot
                    .as_ref()
                    .is_some_and(|open| Arc::strong_count(open) == 1)
            {
                *slot = None;
            }
        }
    }
}

/// One live connection to a source. Dropping it ends it.
pub struct Connection {
    source: Source,
    sftp: Sftp,
}

impl Connection {
    /// Connects over `transport`, and ends it if the server hasn't agreed on
    /// the protocol within `limits.connect`.
    pub fn open(source: Source, transport: Transport, limits: Limits) -> Result<Self, RemoteError> {
        Ok(Self {
            sftp: Sftp::start(transport, limits.connect, limits.request)?,
            source,
        })
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    pub fn alive(&self) -> bool {
        self.sftp.alive()
    }

    /// The source's own path for `path`, refusing another source's.
    fn on<'a>(&self, at: &'a RemotePath) -> Result<&'a str, RemoteError> {
        if at.source != self.source.id {
            return Err(RemoteError::BadPath(format!(
                "{at} isn't on {}.",
                self.source.id
            )));
        }
        Ok(&at.path)
    }

    /// Where browsing starts: the saved start, or the server's default folder.
    pub fn start(&self) -> Result<RemotePath, RemoteError> {
        match &self.source.start {
            Some(start) => RemotePath::new(&self.source.id, start),
            None => self.realpath("."),
        }
    }

    /// `path` as the server resolves it, links followed.
    pub fn realpath(&self, path: &str) -> Result<RemotePath, RemoteError> {
        let real = self
            .sftp
            .realpath(path)
            .map_err(|e| e.at(&format!("{}:{path}", self.source.id)))?;
        RemotePath::new(&self.source.id, &real)
    }

    /// What's at `at`, the link itself when it's one, with where it leads.
    pub fn metadata(&self, at: &RemotePath) -> Result<RemoteEntry, RemoteError> {
        let path = self.on(at)?;
        let attrs = self.sftp.lstat(path).map_err(|e| e.at(at))?;
        let mut entry = RemoteEntry::new(at.clone(), attrs);
        if entry.kind == Kind::Link {
            entry.link = self.sftp.readlink(path).ok();
        }
        Ok(entry)
    }

    /// What's at `at`, following a link.
    pub fn follow(&self, at: &RemotePath) -> Result<RemoteEntry, RemoteError> {
        let attrs = self.sftp.stat(self.on(at)?).map_err(|e| e.at(at))?;
        Ok(RemoteEntry::new(at.clone(), attrs))
    }

    /// Calls `visit` with each usable entry of a folder until it says stop,
    /// and returns how many names it left out. It reads no further than that.
    fn each(
        &self,
        at: &RemotePath,
        mut visit: impl FnMut(RemoteEntry) -> Result<bool, RemoteError>,
    ) -> Result<u64, RemoteError> {
        let handle = self.sftp.opendir(self.on(at)?).map_err(|e| e.at(at))?;
        let mut left_out = 0;
        let read = 'read: loop {
            let names = match self.sftp.readdir(&handle) {
                Ok(Some(names)) => names,
                Ok(None) => break Ok(()),
                Err(e) => break Err(e.at(at)),
            };
            for name in names {
                let Some(text) = name.name else {
                    left_out += 1;
                    continue;
                };
                if text == "." || text == ".." {
                    continue;
                }
                let Ok(path) = at.join(&text) else {
                    left_out += 1;
                    continue;
                };
                match visit(RemoteEntry::new(path, name.attrs)) {
                    Ok(true) => {}
                    Ok(false) => break 'read Ok(()),
                    Err(e) => break 'read Err(e),
                }
            }
        };
        let closed = self.sftp.close_handle(handle);
        read.and(closed.map_err(|e| e.at(at)))?;
        Ok(left_out)
    }

    /// Up to `limit` entries of a folder after the first `offset`, reading no
    /// more of it than that takes. Asking for the next page reads the folder
    /// from its start again; a folder changing between pages can shift them.
    pub fn list(&self, at: &RemotePath, offset: usize, limit: usize) -> Result<Page, RemoteError> {
        let limit = limit.clamp(1, MOST_LISTED);
        let mut page = Page::default();
        let mut seen = 0;
        page.left_out = self.each(at, |entry| {
            if seen >= offset {
                if page.entries.len() == limit {
                    page.more = true;
                    return Ok(false);
                }
                page.entries.push(entry);
            }
            seen += 1;
            Ok(true)
        })?;
        Ok(page)
    }

    /// Up to `len` bytes of a file from `offset`, at most `MOST_READ`, and
    /// the file's size.
    pub fn read(
        &self,
        at: &RemotePath,
        offset: u64,
        len: u32,
    ) -> Result<(Vec<u8>, u64), RemoteError> {
        let path = self.on(at)?;
        let entry = self.follow(at)?;
        if entry.kind != Kind::File {
            return Err(RemoteError::Failed(format!("{at}: not a file.")));
        }
        let handle = self.sftp.open(path, OPEN_READ, 0).map_err(|e| e.at(at))?;
        let len = len.min(MOST_READ);
        let mut out = Vec::new();
        let read = loop {
            let want = (len - out.len() as u32).min(CHUNK);
            if want == 0 {
                break Ok(());
            }
            let at_offset = offset.saturating_add(out.len() as u64);
            match self
                .sftp
                .send_read(&handle, at_offset, want)
                .and_then(|pending| pending.read(want, None))
            {
                Ok(data) if data.is_empty() => break Ok(()),
                Ok(data) => out.extend_from_slice(&data),
                Err(e) => break Err(e.at(at)),
            }
        };
        let closed = self.sftp.close_handle(handle);
        read.and(closed.map_err(|e| e.at(at)))?;
        Ok((out, entry.size))
    }

    /// Makes the folder `at`, failing if something is there.
    pub fn mkdir(&self, at: &RemotePath) -> Result<(), RemoteError> {
        let path = self.on(at)?;
        self.sftp.mkdir(path, 0o755).map_err(|e| e.at(at))
    }

    /// Renames an item on the source, never replacing what's at `to`.
    pub fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), RemoteError> {
        let (old, new) = (self.on(from)?, self.on(to)?);
        // A server may not keep to the protocol's no-replace rule.
        match self.sftp.lstat(new) {
            Ok(_) => return Err(RemoteError::Failed(format!("{to} already exists."))),
            Err(RemoteError::NotFound(_)) => {}
            Err(e) => return Err(e.at(to)),
        }
        self.sftp.rename(old, new).map_err(|e| e.at(from))
    }

    /// Whether a file can be put over another: see `place`.
    pub(crate) fn can_replace(&self) -> bool {
        self.sftp.can_replace()
    }

    /// The unused name beside `to` for this copy's unfinished item.
    pub(crate) fn temp_in(&self, into: &RemotePath, tag: &str) -> Result<RemotePath, RemoteError> {
        into.join(&format!("{TEMP_PREFIX}{tag}"))
    }

    /// Everything under the folder or at the file `from`, without following
    /// links. Bounded, so a server can't fill memory with its listing.
    pub(crate) fn walk(&self, from: &RemoteEntry, stop: &AtomicBool) -> Result<Tree, RemoteError> {
        let mut tree = Tree {
            bytes: if from.kind == Kind::File {
                from.size
            } else {
                0
            },
            ..Tree::default()
        };
        if from.kind != Kind::Dir {
            return Ok(tree);
        }
        let mut folders = vec![(from.path.clone(), String::new(), 0)];
        while let Some((folder, inside, depth)) = folders.pop() {
            if depth > DEEPEST {
                return Err(RemoteError::TooBig(format!(
                    "{folder}: folders go deeper than Monodrive copies."
                )));
            }
            tree.left_out += self.each(&folder, |entry| {
                if stop.load(Ordering::Relaxed) {
                    return Err(RemoteError::Cancelled);
                }
                if tree.items.len() >= MOST_IN_FOLDER {
                    return Err(RemoteError::TooBig(format!(
                        "{}: holds more than {MOST_IN_FOLDER} items, more than Monodrive copies.",
                        from.path
                    )));
                }
                let name = if inside.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{inside}/{}", entry.name)
                };
                let node = match entry.kind {
                    Kind::Dir => {
                        folders.push((entry.path.clone(), name.clone(), depth + 1));
                        Node::Dir(0o755)
                    }
                    Kind::File => {
                        tree.bytes = tree.bytes.saturating_add(entry.size);
                        Node::File
                    }
                    Kind::Link => Node::Link,
                    Kind::Other => {
                        tree.left_out += 1;
                        return Ok(true);
                    }
                };
                tree.items.push((name, node));
                Ok(true)
            })?;
        }
        Ok(tree)
    }

    /// Copies the item `from` to the new local path `to`, verifying each
    /// file's size. Links stay links. Leaves what it made if it fails: the
    /// job sends that to the Trash.
    pub(crate) fn fetch(
        &self,
        from: &RemoteEntry,
        tree: &Tree,
        to: &Path,
        meter: &Meter,
    ) -> Result<(), RemoteError> {
        match from.kind {
            Kind::File => self.fetch_file(&from.path, to, u64::MAX, meter)?,
            Kind::Link => self.fetch_link(&from.path, to)?,
            Kind::Dir => {
                make_dir(to, from.permissions)?;
                for (name, node) in &tree.items {
                    if meter.cancel.load(Ordering::Relaxed) {
                        return Err(RemoteError::Cancelled);
                    }
                    let (remote, local) = (from.path.joined(name)?, to.join(name));
                    match node {
                        Node::Dir(_) => make_dir(&local, None)?,
                        Node::File => self.fetch_file(&remote, &local, u64::MAX, meter)?,
                        Node::Link => self.fetch_link(&remote, &local)?,
                    }
                }
            }
            Kind::Other => {
                return Err(RemoteError::Failed(format!(
                    "{}: isn't a file, folder or link.",
                    from.path
                )));
            }
        }
        Ok(())
    }

    fn fetch_link(&self, from: &RemotePath, to: &Path) -> Result<(), RemoteError> {
        let target = self.sftp.readlink(self.on(from)?).map_err(|e| e.at(from))?;
        std::os::unix::fs::symlink(target, to).map_err(|e| local_error(to, &e))
    }

    /// Reads ahead by `WINDOW` chunks, so distance costs latency once, not per
    /// chunk. A new file, so nothing there is followed or overwritten. A file
    /// over `most` bytes is refused before anything is written, and reads stop
    /// at the size checked here, so at most that many bytes ever arrive.
    fn fetch_file(
        &self,
        from: &RemotePath,
        to: &Path,
        most: u64,
        meter: &Meter,
    ) -> Result<(), RemoteError> {
        let path = self.on(from)?;
        let was = self.sftp.lstat(path).map_err(|e| e.at(from))?;
        let size = was.size.unwrap_or(0);
        if size > most {
            return Err(RemoteError::TooBig(format!(
                "{from}: is {size} bytes now, more than the {most} there's room for."
            )));
        }
        let mode = was.permissions.unwrap_or(0o644) & 0o777 | 0o600;
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(to)
            .map_err(|e| local_error(to, &e))?;
        let handle = self.sftp.open(path, OPEN_READ, 0).map_err(|e| e.at(from))?;
        let copied = self.read_whole(&handle, size, &mut file, to, meter);
        let closed = self.sftp.close_handle(handle);
        copied.map_err(|e| e.at(from))?;
        closed.map_err(|e| e.at(from))?;
        if meter.cancel.load(Ordering::Relaxed) {
            return Err(RemoteError::Cancelled);
        }
        let now = self.sftp.lstat(path).map_err(|e| e.at(from))?;
        if (now.size, now.modified) != (was.size, was.modified) {
            return Err(RemoteError::Changed(format!(
                "{from} changed during the copy."
            )));
        }
        let written = file.metadata().map_err(|e| local_error(to, &e))?.len();
        if written != size {
            return Err(RemoteError::Changed(format!(
                "{from}: copied {written} bytes of {size}."
            )));
        }
        if let Some(modified) = was.modified {
            let when = UNIX_EPOCH + Duration::from_secs(modified.into());
            let _ = file.set_modified(when);
        }
        Ok(())
    }

    fn read_whole(
        &self,
        handle: &Handle,
        size: u64,
        file: &mut File,
        to: &Path,
        meter: &Meter,
    ) -> Result<(), RemoteError> {
        let stop = Some(meter.cancel);
        let mut on_way: VecDeque<(u64, u32, Pending<'_>)> = VecDeque::with_capacity(WINDOW);
        let mut next = 0;
        loop {
            while on_way.len() < WINDOW && next < size {
                let len = u64::from(CHUNK).min(size - next) as u32;
                on_way.push_back((next, len, self.sftp.send_read(handle, next, len)?));
                next += u64::from(len);
            }
            let Some((offset, len, pending)) = on_way.pop_front() else {
                return Ok(());
            };
            let mut data = pending.read(len, stop)?;
            let mut have = data.len() as u32;
            // The server may answer with less than was asked. What's still
            // missing comes before the replies already on their way.
            loop {
                file.write_all(&data).map_err(|e| local_error(to, &e))?;
                meter.copied.fetch_add(data.len() as u64, Ordering::Relaxed);
                if have == len {
                    break;
                }
                let want = len - have;
                data = self
                    .sftp
                    .send_read(handle, offset + u64::from(have), want)?
                    .read(want, stop)?;
                if data.is_empty() {
                    return Err(RemoteError::Changed(
                        "The file got shorter during the copy.".into(),
                    ));
                }
                have += data.len() as u32;
            }
        }
    }

    /// Makes the new folder `at`, failing if anything is there.
    pub(crate) fn make_new_folder(&self, at: &RemotePath, mode: u32) -> Result<(), RemoteError> {
        self.sftp.mkdir(self.on(at)?, mode).map_err(|e| e.at(at))
    }

    /// Copies the local item `from` to the new remote path `to`, verifying
    /// each file's size. `tree` is what `walk_local` found there. `created` is
    /// called as soon as the server has confirmed it made `to`, which can only
    /// be for this call. Without that call, what's at `to` isn't this copy's.
    pub(crate) fn put(
        &self,
        from: &Path,
        tree: &Tree,
        to: &RemotePath,
        meter: &Meter,
        created: impl FnOnce() -> Result<(), RemoteError>,
    ) -> Result<(), RemoteError> {
        let meta = std::fs::symlink_metadata(from).map_err(|e| local_error(from, &e))?;
        if meta.is_file() {
            return self.put_file(from, to, meter, created);
        }
        if meta.is_symlink() {
            self.put_link(from, to)?;
            return created();
        }
        self.make_new_folder(to, meta.mode() & 0o777 | 0o700)?;
        created()?;
        for (name, node) in &tree.items {
            if meter.cancel.load(Ordering::Relaxed) {
                return Err(RemoteError::Cancelled);
            }
            let (local, remote) = (from.join(name), to.joined(name)?);
            match node {
                Node::Dir(mode) => self.make_new_folder(&remote, *mode)?,
                Node::File => self.put_file(&local, &remote, meter, || Ok(()))?,
                Node::Link => self.put_link(&local, &remote)?,
            }
        }
        Ok(())
    }

    fn put_link(&self, from: &Path, to: &RemotePath) -> Result<(), RemoteError> {
        let target = std::fs::read_link(from).map_err(|e| local_error(from, &e))?;
        let Some(target) = target.to_str() else {
            return Err(RemoteError::Failed(format!(
                "{}: its target isn't UTF-8.",
                from.display()
            )));
        };
        self.sftp
            .symlink(target, self.on(to)?)
            .map_err(|e| e.at(to))
    }

    /// Creates `to`, failing if anything is there, so this copy only ever
    /// writes a file it made.
    fn put_file(
        &self,
        from: &Path,
        to: &RemotePath,
        meter: &Meter,
        created: impl FnOnce() -> Result<(), RemoteError>,
    ) -> Result<(), RemoteError> {
        let path = self.on(to)?;
        let mut file = File::open(from).map_err(|e| local_error(from, &e))?;
        let was = file.metadata().map_err(|e| local_error(from, &e))?;
        let handle = self
            .sftp
            .open(path, OPEN_NEW, was.mode() & 0o777)
            .map_err(|e| e.at(to))?;
        let sent = created().and_then(|()| self.write_whole(&handle, &mut file, from, meter));
        let closed = self.sftp.close_handle(handle);
        let sent = sent.map_err(|e| e.at(to))?;
        closed.map_err(|e| e.at(to))?;
        let now = file.metadata().map_err(|e| local_error(from, &e))?;
        if (now.len(), now.modified().ok()) != (was.len(), was.modified().ok()) || sent != was.len()
        {
            return Err(RemoteError::Changed(format!(
                "{} changed during the copy.",
                from.display()
            )));
        }
        let there = self.sftp.stat(path).map_err(|e| e.at(to))?;
        if there.size != Some(sent) {
            return Err(RemoteError::Changed(format!(
                "{to}: the server has {} bytes of {sent}.",
                there.size.unwrap_or(0)
            )));
        }
        Ok(())
    }

    fn write_whole(
        &self,
        handle: &Handle,
        file: &mut File,
        from: &Path,
        meter: &Meter,
    ) -> Result<u64, RemoteError> {
        let stop = Some(meter.cancel);
        let mut on_way: VecDeque<(usize, Pending<'_>)> = VecDeque::with_capacity(WINDOW);
        let mut chunk = vec![0; CHUNK as usize];
        let mut sent = 0;
        loop {
            let n = file.read(&mut chunk).map_err(|e| local_error(from, &e))?;
            if n == 0 {
                break;
            }
            if on_way.len() == WINDOW
                && let Some((len, pending)) = on_way.pop_front()
            {
                pending.write(stop)?;
                meter.copied.fetch_add(len as u64, Ordering::Relaxed);
            }
            if meter.cancel.load(Ordering::Relaxed) {
                return Err(RemoteError::Cancelled);
            }
            on_way.push_back((n, self.sftp.send_write(handle, sent, &chunk[..n])?));
            sent += n as u64;
        }
        for (len, pending) in on_way {
            pending.write(stop)?;
            meter.copied.fetch_add(len as u64, Ordering::Relaxed);
        }
        Ok(sent)
    }

    /// Puts the finished `temp` at `to`. Without `replacing`, never over
    /// anything. With it, only over the file that's still as `replacing` says,
    /// in one step, for good: there's no Trash to put the old one in.
    pub(crate) fn place(
        &self,
        temp: &RemotePath,
        to: &RemotePath,
        replacing: Option<(u64, u64)>,
    ) -> Result<(), RemoteError> {
        let (old, new) = (self.on(temp)?, self.on(to)?);
        let Some(seen) = replacing else {
            match self.sftp.lstat(new) {
                Ok(_) => return Err(RemoteError::Changed(format!("{to} already exists."))),
                Err(RemoteError::NotFound(_)) => {}
                Err(e) => return Err(e.at(to)),
            }
            return self.sftp.rename(old, new).map_err(|e| e.at(to));
        };
        let there = self.metadata(to)?;
        if there.kind != Kind::File || (there.size, there.modified) != seen {
            return Err(RemoteError::Changed(format!(
                "{to} changed since the copy started, so it wasn't replaced."
            )));
        }
        self.sftp.replace(old, new).map_err(|e| e.at(to))
    }

    /// Removes a temp the server confirmed this copy made, and everything in
    /// it, which is only ever what the copy put there. The one way Monodrive
    /// removes anything from a server, and it refuses a name that isn't a
    /// temp's. Fine if it's gone.
    pub(crate) fn discard(&self, temp: &RemotePath) -> Result<(), RemoteError> {
        if !temp
            .name()
            .is_some_and(|name| name.starts_with(TEMP_PREFIX))
        {
            return Err(RemoteError::BadPath(format!(
                "{temp}: not an unfinished copy of Monodrive's."
            )));
        }
        match self.metadata(temp) {
            Ok(entry) => self.remove_all(&entry, 0),
            Err(RemoteError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn remove_all(&self, entry: &RemoteEntry, depth: usize) -> Result<(), RemoteError> {
        let path = self.on(&entry.path)?;
        if entry.kind != Kind::Dir {
            return self.sftp.remove(path).map_err(|e| e.at(&entry.path));
        }
        if depth > DEEPEST {
            return Err(RemoteError::TooBig(format!(
                "{}: goes deeper than Monodrive removes.",
                entry.path
            )));
        }
        let mut inside = Vec::new();
        self.each(&entry.path, |item| {
            inside.push(item);
            Ok(inside.len() <= MOST_IN_FOLDER)
        })?;
        for item in &inside {
            self.remove_all(item, depth + 1)?;
        }
        self.sftp.rmdir(path).map_err(|e| e.at(&entry.path))
    }
}

impl RemotePath {
    /// The item at the relative path `inside`, each part a name that
    /// `Connection::walk` already checked.
    fn joined(&self, inside: &str) -> Result<Self, RemoteError> {
        inside
            .split('/')
            .try_fold(self.clone(), |path, name| path.join(name))
    }
}

fn local_error(path: &Path, e: &std::io::Error) -> RemoteError {
    if e.kind() == ErrorKind::AlreadyExists {
        return RemoteError::Failed(format!(
            "{}: already exists. Names that differ only by case, or in another Unicode form, \
            are one name on this Mac.",
            path.display()
        ));
    }
    RemoteError::Failed(format!("{}: {e}", path.display()))
}

fn make_dir(path: &Path, mode: Option<u32>) -> Result<(), RemoteError> {
    std::fs::DirBuilder::new()
        .mode(mode.unwrap_or(0o755) & 0o777 | 0o700)
        .create(path)
        .map_err(|e| local_error(path, &e))
}

/// What's inside a folder being copied, by path relative to it, each folder
/// before what it holds.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    pub(crate) items: Vec<(String, Node)>,
    pub(crate) bytes: u64,
    /// Items that can't be copied: sockets, pipes and devices, and names
    /// Monodrive can't address.
    pub(crate) left_out: u64,
}

#[derive(Debug)]
pub(crate) enum Node {
    Dir(u32),
    File,
    Link,
}

/// Everything under the local item at `from`, as `Connection::walk` does on a
/// server.
pub(crate) fn walk_local(from: &Path, stop: &AtomicBool) -> Result<Tree, RemoteError> {
    let meta = std::fs::symlink_metadata(from).map_err(|e| local_error(from, &e))?;
    let mut tree = Tree::default();
    if !meta.is_dir() {
        tree.bytes = if meta.is_file() { meta.len() } else { 0 };
        return Ok(tree);
    }
    let mut folders = vec![(from.to_path_buf(), String::new(), 0)];
    while let Some((folder, inside, depth)) = folders.pop() {
        if depth > DEEPEST {
            return Err(RemoteError::TooBig(format!(
                "{}: folders go deeper than Monodrive copies.",
                folder.display()
            )));
        }
        for item in std::fs::read_dir(&folder).map_err(|e| local_error(&folder, &e))? {
            if stop.load(Ordering::Relaxed) {
                return Err(RemoteError::Cancelled);
            }
            if tree.items.len() >= MOST_IN_FOLDER {
                return Err(RemoteError::TooBig(format!(
                    "{}: holds more than {MOST_IN_FOLDER} items, more than Monodrive copies.",
                    from.display()
                )));
            }
            let item = item.map_err(|e| local_error(&folder, &e))?;
            let path = item.path();
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                tree.left_out += 1;
                continue;
            };
            let name = if inside.is_empty() {
                name
            } else {
                format!("{inside}/{name}")
            };
            let meta = std::fs::symlink_metadata(&path).map_err(|e| local_error(&path, &e))?;
            let node = if meta.is_dir() {
                folders.push((path, name.clone(), depth + 1));
                Node::Dir(meta.mode() & 0o777 | 0o700)
            } else if meta.is_file() {
                tree.bytes = tree.bytes.saturating_add(meta.len());
                Node::File
            } else if meta.is_symlink() {
                Node::Link
            } else {
                tree.left_out += 1;
                continue;
            };
            tree.items.push((name, node));
        }
    }
    Ok(tree)
}

/// Downloads of remote files for a preview or to open, each kept only while
/// its `Preview` lives, in a folder this object owns. Bounded: what doesn't fit
/// is refused. One a crash left behind goes the next time any process opens
/// the cache.
pub struct Previews {
    inner: Arc<Inner>,
    next: AtomicU64,
}

struct Inner {
    dir: PathBuf,
    lock: PathBuf,
    /// Held for as long as this cache lives, so no other process sweeps it.
    _held: File,
    limit: u64,
    used: AtomicU64,
}

/// 1 GB, for what one window or agent holds open at once.
pub const PREVIEW_LIMIT: u64 = 1 << 30;

impl Previews {
    pub fn user() -> Result<Self, String> {
        let support = crate::support_dir().ok_or("No home folder.")?;
        Self::open(&support.join("previews"), PREVIEW_LIMIT)
    }

    /// A cache of its own under `root`, removing what dead processes left.
    pub fn open(root: &Path, limit: u64) -> Result<Self, String> {
        let fail = |e: std::io::Error| format!("{}: {e}", root.display());
        std::fs::create_dir_all(root).map_err(fail)?;
        sweep(root);
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let dir = root.join(format!("{}-{started}", process_id()));
        let lock = dir.with_extension("lock");
        let held = ops::lock(&lock, libc::LOCK_EX | libc::LOCK_NB).map_err(fail)?;
        std::fs::create_dir(&dir).map_err(fail)?;
        Ok(Self {
            inner: Arc::new(Inner {
                dir,
                lock,
                _held: held,
                limit,
                used: AtomicU64::new(0),
            }),
            next: AtomicU64::new(0),
        })
    }

    /// Downloads the file at `at`, or the file a link there leads to, to a
    /// real local path, read-only, with `meter` following it. Nothing stays
    /// if it fails or is cancelled.
    pub fn fetch(
        &self,
        connection: &Connection,
        at: &RemotePath,
        meter: &Meter,
    ) -> Result<Preview, RemoteError> {
        let entry = connection.follow(at)?;
        if entry.kind != Kind::File {
            return Err(RemoteError::Failed(format!("{at}: not a file.")));
        }
        let name = at.name().unwrap_or("file");
        let used = self.inner.used.fetch_add(entry.size, Ordering::Relaxed);
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let mut preview = Preview {
            path: PathBuf::new(),
            size: entry.size,
            reserved: entry.size,
            folder: self.inner.dir.join(n.to_string()),
            inner: self.inner.clone(),
        };
        if used.saturating_add(entry.size) > self.inner.limit {
            return Err(RemoteError::TooBig(format!(
                "{at}: {} bytes doesn't fit in the preview cache's {} bytes.",
                entry.size, self.inner.limit
            )));
        }
        make_dir(&preview.folder, None)?;
        let part = preview.folder.join(".part");
        let real = connection.realpath(at.path())?;
        connection.fetch_file(&real, &part, preview.reserved, meter)?;
        let path = preview.folder.join(name);
        std::fs::set_permissions(&part, Permissions::from_mode(0o400))
            .and_then(|()| std::fs::rename(&part, &path))
            .map_err(|e| local_error(&path, &e))?;
        let meta = std::fs::metadata(&path).map_err(|e| local_error(&path, &e))?;
        preview.size = meta.len();
        preview.path = path;
        Ok(preview)
    }
}

/// A downloaded file. Dropping it removes it.
pub struct Preview {
    /// The real local path, for Quick Look or Open With.
    pub path: PathBuf,
    pub size: u64,
    /// What it holds of the cache's room, whatever it turned out to be.
    reserved: u64,
    folder: PathBuf,
    inner: Arc<Inner>,
}

impl Drop for Preview {
    fn drop(&mut self) {
        #[expect(
            clippy::disallowed_methods,
            reason = "the preview cache is Monodrive's own, not the user's files"
        )]
        let _ = std::fs::remove_dir_all(&self.folder);
        self.inner.used.fetch_sub(self.reserved, Ordering::Relaxed);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        #[expect(
            clippy::disallowed_methods,
            reason = "the preview cache is Monodrive's own, not the user's files"
        )]
        {
            let _ = std::fs::remove_dir_all(&self.dir);
            let _ = std::fs::remove_file(&self.lock);
        }
    }
}

/// Removes each cache under `root` whose lock nothing holds.
fn sweep(root: &Path) {
    let Ok(items) = std::fs::read_dir(root) else {
        return;
    };
    for lock in items.flatten().map(|item| item.path()) {
        if lock.extension().is_none_or(|ext| ext != "lock")
            || ops::lock(&lock, libc::LOCK_EX | libc::LOCK_NB).is_err()
        {
            continue;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the preview cache is Monodrive's own, not the user's files"
        )]
        {
            let _ = std::fs::remove_dir_all(lock.with_extension(""));
            let _ = std::fs::remove_file(&lock);
        }
    }
}
