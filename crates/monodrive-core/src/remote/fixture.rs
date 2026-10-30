//! A disposable SFTP server for tests: `/usr/libexec/sftp-server` over pipes,
//! with no ssh, network or keys, reaching only the paths a test names under its
//! temp folder.

use super::Transport;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

const SERVER: &str = "/usr/libexec/sftp-server";

/// Calls `then` once, as soon as this many bytes have come from the server.
pub(crate) struct Trip {
    pub(crate) after: usize,
    pub(crate) then: Box<dyn FnOnce() + Send>,
}

/// The server's stdout. It stops the server by its handle when dropped, which
/// the connection's reader does at the end of the stream.
struct Stream {
    out: std::process::ChildStdout,
    child: Arc<Mutex<Child>>,
    read: usize,
    trip: Option<Trip>,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.out.read(buf)?;
        self.read += n;
        if self
            .trip
            .as_ref()
            .is_some_and(|trip| self.read >= trip.after)
            && let Some(trip) = self.trip.take()
        {
            (trip.then)();
        }
        Ok(n)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub(crate) struct Server {
    pub(crate) transport: Transport,
    pub(crate) child: Arc<Mutex<Child>>,
}

pub(crate) fn start(trip: Option<Trip>) -> Server {
    let mut child = Command::new(SERVER)
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("sftp-server is part of macOS");
    let (stdin, out) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
    let child = Arc::new(Mutex::new(child));
    let stream = Stream {
        out,
        child: child.clone(),
        read: 0,
        trip,
    };
    Server {
        transport: Transport::new(stream, stdin),
        child,
    }
}

/// A connection to a new server for each caller.
pub(crate) fn transport() -> Transport {
    start(None).transport
}

/// A trip that kills the server by the handle it was spawned with.
pub(crate) fn killed_after(after: usize) -> Server {
    let slot: Arc<Mutex<Option<Arc<Mutex<Child>>>>> = Arc::default();
    let inside = slot.clone();
    let server = start(Some(Trip {
        after,
        then: Box::new(move || {
            if let Some(child) = inside.lock().unwrap().as_ref() {
                let _ = child.lock().unwrap().kill();
            }
        }),
    }));
    *slot.lock().unwrap() = Some(server.child.clone());
    server
}

/// One frame from a hand-written server.
pub(crate) fn frame(kind: u8, id: Option<u32>, body: &[u8]) -> Vec<u8> {
    let length = 1 + if id.is_some() { 4 } else { 0 } + body.len();
    let mut out = (length as u32).to_be_bytes().to_vec();
    out.push(kind);
    if let Some(id) = id {
        out.extend_from_slice(&id.to_be_bytes());
    }
    out.extend_from_slice(body);
    out
}

pub(crate) fn string(text: &[u8]) -> Vec<u8> {
    let mut out = (text.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(text);
    out
}

pub(crate) fn status(code: u32) -> Vec<u8> {
    let mut out = code.to_be_bytes().to_vec();
    out.extend_from_slice(&string(b""));
    out.extend_from_slice(&string(b""));
    out
}

pub(crate) fn version() -> Vec<u8> {
    frame(2, None, &3u32.to_be_bytes())
}

/// Reads one request: its kind, id, and the rest of its body.
pub(crate) fn packet(stream: &mut UnixStream) -> Option<(u8, u32, Vec<u8>)> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).ok()?;
    let mut body = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut body).ok()?;
    let id = body
        .get(1..5)
        .map_or(0, |id| u32::from_be_bytes([id[0], id[1], id[2], id[3]]));
    let rest = body.get(5..).unwrap_or_default().to_vec();
    Some((body[0], id, rest))
}

/// Reads one request: its kind and id.
pub(crate) fn request(stream: &mut UnixStream) -> Option<(u8, u32)> {
    packet(stream).map(|(kind, id, _)| (kind, id))
}
