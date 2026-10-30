//! A disposable SFTP server for tests: `/usr/libexec/sftp-server` over pipes,
//! with no ssh, network or keys, reaching only the paths a test names under its
//! temp folder.

use super::Transport;
use std::io::{self, Read};
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
