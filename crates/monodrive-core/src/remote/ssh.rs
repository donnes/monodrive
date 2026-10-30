//! The system's `ssh`, run as `ssh -s host sftp` with no shell. The user's
//! config, agent, known_hosts and ProxyJump all apply, and Monodrive handles no
//! keys. `BatchMode` means ssh never prompts: what would need a prompt fails,
//! and `diagnose` reports it.

use super::{Connector, RemoteError, Source, Transport};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

pub const SSH: &str = "/usr/bin/ssh";
/// How long ssh gets to reach the host. `Limits::connect` bounds the rest.
const CONNECT_SECONDS: u64 = 15;
/// How much of what ssh says on stderr is kept: the end.
const SAID_LIMIT: usize = 16 << 10;
/// How long ssh gets to finish saying why it stopped.
const LAST_WORDS: Duration = Duration::from_secs(2);

/// The arguments that open SFTP to `host`, which `Source` has checked can't
/// read as an option. They're what sftp(1) passes, so a host's config can't
/// forward the agent or run a local command, plus `BatchMode`. Strict host key
/// checking goes on whatever the user's config says: with it off, ssh connects
/// through a changed key, and stderr would only say so afterwards.
pub fn arguments(host: &str) -> Vec<OsString> {
    [
        "-oBatchMode=yes".to_string(),
        "-oStrictHostKeyChecking=yes".into(),
        format!("-oConnectTimeout={CONNECT_SECONDS}"),
        "-oForwardAgent=no".into(),
        "-oForwardX11=no".into(),
        "-oPermitLocalCommand=no".into(),
        "-oClearAllForwardings=yes".into(),
        "-s".into(),
        "--".into(),
        host.into(),
        "sftp".into(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// Connects each source through the system's ssh.
pub fn connector() -> Connector {
    Arc::new(|source: &Source| spawn(SSH.as_ref(), &arguments(&source.host), source.host.clone()))
}

/// Runs `program` with its stdio as the transport. Its own process group, so
/// Ctrl-C in a terminal doesn't stop it, and `/` as its folder, so it never
/// keeps a volume from ejecting.
pub fn spawn(
    program: &Path,
    args: &[impl AsRef<OsStr>],
    host: String,
) -> Result<Transport, RemoteError> {
    let mut child = Command::new(program)
        .args(args)
        // ssh's messages in English, for `diagnose`.
        .env("LC_ALL", "C")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            RemoteError::Unreachable(host.clone(), format!("{}: {e}", program.display()))
        })?;
    let (Some(stdin), Some(stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RemoteError::Unreachable(
            host,
            "ssh's stdio wasn't piped".into(),
        ));
    };
    let said = Arc::new(Mutex::new(Vec::new()));
    let (done, closed) = mpsc::channel();
    {
        let said = said.clone();
        std::thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(n) = stderr.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut said = said.lock().unwrap_or_else(PoisonError::into_inner);
                said.extend_from_slice(&chunk[..n]);
                let over = said.len().saturating_sub(SAID_LIMIT);
                said.drain(..over);
            }
            let _ = done.send(());
        });
    }
    Ok(Transport {
        reader: Box::new(stdout),
        writer: Box::new(stdin),
        process: Some(Process {
            child,
            said,
            closed,
            host,
        }),
    })
}

/// The subprocess a connection started. Only its handle ever stops it.
pub(crate) struct Process {
    child: Child,
    said: Arc<Mutex<Vec<u8>>>,
    closed: Receiver<()>,
    host: String,
}

impl Process {
    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    /// What it said, once it's done talking or has had its time to.
    pub(crate) fn said_once_closed(&self) -> String {
        let _ = self.closed.recv_timeout(LAST_WORDS);
        let said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&said).into_owned()
    }

    pub(crate) fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What ssh's stderr means. `None` when it says nothing that's known, or
/// nothing at all. ssh's wording has held for decades, but a server's banner
/// comes through here too, so each match is on a whole message of ssh's own.
pub(super) fn diagnose(said: &str, host: &str) -> Option<RemoteError> {
    let host = host.to_string();
    let lines = || said.lines().map(str::trim);
    if lines().any(|line| line.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")) {
        return Some(RemoteError::HostKeyChanged(host));
    }
    if lines().any(|line| line == "Host key verification failed.") {
        return Some(RemoteError::HostKeyUnknown(host));
    }
    if let Some(denied) = lines().find_map(|line| {
        let at = line.find("Permission denied (")?;
        (at == 0 || line[..at].ends_with(": ")).then_some(&line[at..])
    }) {
        return Some(
            if denied.contains("password") || denied.contains("keyboard-interactive") {
                RemoteError::InteractiveAuth(host)
            } else {
                RemoteError::AuthFailed(host, denied.to_string())
            },
        );
    }
    if lines().any(|line| line.starts_with("Too many authentication failures")) {
        return Some(RemoteError::AuthFailed(
            host,
            "Too many authentication failures.".into(),
        ));
    }
    if lines().any(|line| line.contains("subsystem request failed")) {
        return Some(RemoteError::NoSftp(host));
    }
    let unreachable = [
        "Could not resolve hostname",
        "Connection refused",
        "Operation timed out",
        "Connection timed out",
        "No route to host",
        "Network is unreachable",
        "Host is down",
        "kex_exchange_identification",
        "Connection closed by",
        "Connection reset by",
    ];
    lines()
        .find(|line| unreachable.iter().any(|known| line.contains(known)))
        .map(|line| RemoteError::Unreachable(host, line.to_string()))
}
