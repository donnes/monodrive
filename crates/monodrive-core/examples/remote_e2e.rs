//! Proves the real ssh path against a throwaway sshd, and measures transfers.
//! `cargo run --release -p monodrive-core --example remote_e2e`
//!
//! Everything sits in a temp folder under `target/`: the sshd's config and host
//! key, a user key, known_hosts and an ssh config passed with `-F`, so the
//! user's own `~/.ssh` is never read. The sshd listens only on loopback, takes
//! only that key and is stopped by the handle it was started with. Nothing else
//! is connected to. A cancelled download's partial copy goes to the real Trash,
//! as any does, and this takes it back out by the path the job reports.

use monodrive_core::entries::Kind;
use monodrive_core::ops::{self, Conflict, Origin, Request, State};
use monodrive_core::places;
use monodrive_core::remote::{Connections, Connector, RemoteError, RemotePath, Source, ssh};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const MB: usize = 1 << 20;
const TRANSFER_MB: usize = 300;

/// Stops the sshd by its handle, panic or not.
struct Sshd(Child);

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "{program} {args:?}");
}

fn key(path: &Path) -> String {
    run(
        "/usr/bin/ssh-keygen",
        &[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            path.to_str().unwrap(),
        ],
    );
    let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    public
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start_sshd(dir: &Path, port: u16) -> Sshd {
    let config = dir.join("sshd_config");
    std::fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {key}\nPidFile none\n\
            AuthorizedKeysFile {authorized}\nPasswordAuthentication no\n\
            KbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\n\
            Subsystem sftp /usr/libexec/sftp-server\n",
            key = dir.join("host_key").display(),
            authorized = dir.join("authorized_keys").display(),
        ),
    )
    .unwrap();
    let mut child = Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f"])
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sshd starts");
    let stderr = child.stderr.take().unwrap();
    let mut sshd = Sshd(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            if line.contains("Server listening") {
                let _ = tx.send(());
            }
            line.clear();
        }
    });
    if rx.recv_timeout(Duration::from_secs(10)).is_err() {
        let _ = sshd.0.kill();
        panic!("sshd didn't start listening");
    }
    sshd
}

fn peak_rss_mb() -> f64 {
    // SAFETY: `usage` is a valid rusage for getrusage to fill.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    usage.ru_maxrss as f64 / MB as f64
}

/// Starts an owner on `state` with `connector` and returns a client of it.
fn owner(
    root: &Path,
    store: &places::Store,
    connector: Connector,
) -> (ops::Client, std::thread::JoinHandle<()>) {
    let paths = ops::Paths {
        state: root.join("state"),
        settings: store.places.with_file_name("settings.json"),
        trash: root.join("Trash"),
    };
    let owner = ops::Owner::bind_with(paths.clone(), connector).unwrap();
    let serving = std::thread::spawn(move || {
        let _ = owner.serve();
    });
    (ops::Client::new(paths, None), serving)
}

fn wait_for(client: &mut ops::Client, id: u64) -> ops::Job {
    let (tx, rx) = mpsc::channel();
    let _watch = client
        .watch(move || {
            let _ = tx.send(());
        })
        .unwrap();
    loop {
        let job = client
            .jobs()
            .unwrap()
            .into_iter()
            .find(|job| job.id == id)
            .unwrap();
        if !job.state.active() {
            return job;
        }
        let _ = rx.recv_timeout(Duration::from_millis(200));
    }
}

fn job(client: &mut ops::Client, request: Request) -> ops::Job {
    let id = client.submit(Origin::Window, request).unwrap().op;
    wait_for(client, id)
}

fn remote(source: &str, path: &Path) -> RemotePath {
    RemotePath::new(source, path.to_str().unwrap()).unwrap()
}

/// First listing, throughput both ways, cancel latency and peak memory.
fn measure(name: &str, connector: Connector, source: &str, store: &places::Store, dir: &Path) {
    println!("\n{name}");
    let root = dir.join(format!("measure-{name}"));
    let (far, near) = (root.join("far"), root.join("near"));
    let many = far.join("many");
    std::fs::create_dir_all(&many).unwrap();
    std::fs::create_dir_all(&near).unwrap();
    for n in 0..10_000 {
        std::fs::write(many.join(format!("file-{n:05}.txt")), "x").unwrap();
    }
    let chunk = vec![0x5a; MB];
    let big = far.join("big.bin");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&big).unwrap();
        for _ in 0..TRANSFER_MB {
            file.write_all(&chunk).unwrap();
        }
    }
    let connections = Connections::new(store.clone(), connector.clone());
    let started = Instant::now();
    let open = connections.connect(source).unwrap();
    let connected = started.elapsed();
    println!("  connect                 {connected:>10.2?}");
    let started = Instant::now();
    let page = open.list(&remote(source, &many), 0, 100_000).unwrap();
    let listed = started.elapsed();
    assert_eq!(page.entries.len(), 10_000);
    println!("  first listing of 10,000 {listed:>10.2?}");
    let started = Instant::now();
    let first = open.list(&remote(source, &many), 0, 200).unwrap();
    assert!(first.more);
    println!("  first page of 200       {:>10.2?}", started.elapsed());

    let rss_before = peak_rss_mb();
    // A Unix socket's path must be short, and `target/` may not be.
    let state = tempfile::tempdir().unwrap();
    let (mut client, serving) = owner(state.path(), store, connector);
    let started = Instant::now();
    let done = job(
        &mut client,
        Request::Download {
            paths: vec![remote(source, &big)],
            into: near.clone(),
            conflict: Conflict::Stop,
        },
    );
    let took = started.elapsed();
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(
        std::fs::metadata(near.join("big.bin")).unwrap().len(),
        (TRANSFER_MB * MB) as u64
    );
    println!(
        "  download {TRANSFER_MB} MB         {took:>10.2?}  {:.0} MB/s",
        TRANSFER_MB as f64 / took.as_secs_f64()
    );

    let into = far.join("into");
    std::fs::create_dir(&into).unwrap();
    let started = Instant::now();
    let done = job(
        &mut client,
        Request::Upload {
            paths: vec![near.join("big.bin")],
            into: remote(source, &into),
            conflict: Conflict::Stop,
        },
    );
    let took = started.elapsed();
    assert_eq!(done.state, State::Completed, "{done:?}");
    println!(
        "  upload {TRANSFER_MB} MB           {took:>10.2?}  {:.0} MB/s",
        TRANSFER_MB as f64 / took.as_secs_f64()
    );
    println!(
        "  peak memory              {:>6.0} MB (before transfers {rss_before:.0} MB)",
        peak_rss_mb()
    );

    for upload in [true, false] {
        let request = if upload {
            Request::Upload {
                paths: vec![near.join("big.bin")],
                into: remote(source, &far),
                conflict: Conflict::KeepBoth,
            }
        } else {
            Request::Download {
                paths: vec![remote(source, &big)],
                into: near.clone(),
                conflict: Conflict::KeepBoth,
            }
        };
        let id = client.submit(Origin::Window, request).unwrap().op;
        while client
            .jobs()
            .unwrap()
            .iter()
            .find(|job| job.id == id)
            .is_none_or(|job| job.bytes < (5 * MB) as u64)
        {}
        let started = Instant::now();
        client
            .submit(Origin::Window, Request::Cancel { job: id })
            .unwrap();
        let job = loop {
            let jobs = client.jobs().unwrap();
            let job = jobs.into_iter().find(|job| job.id == id).unwrap();
            if !job.state.active() {
                break job;
            }
        };
        assert_eq!(job.state, State::Cancelled, "{job:?}");
        if let Some(ops::Done::Failed(why)) = &job.tasks[0].done
            && let Some((_, trashed)) = why.split_once("went to the Trash at ")
        {
            let trashed = Path::new(trashed.trim_end_matches('.'));
            std::fs::rename(trashed, root.join("untrashed")).unwrap();
        }
        let way = if upload { "upload" } else { "download" };
        println!(
            "  cancel a running {way:<8} {:>9.2?}, with its temp removed",
            started.elapsed()
        );
    }
    let left: Vec<PathBuf> = std::fs::read_dir(&far)
        .unwrap()
        .map(|item| item.unwrap().path())
        .filter(|path| path.file_name().unwrap().to_str().unwrap().starts_with('.'))
        .collect();
    assert_eq!(left, Vec::<PathBuf>::new());
    drop(open);
    drop(connections);
    drop(client);
    let _ = serving.join();
}

fn main() {
    std::fs::create_dir_all("target").unwrap();
    let scratch = tempfile::Builder::new()
        .prefix("remote-e2e-")
        .tempdir_in("target")
        .unwrap();
    let dir = scratch.path().canonicalize().unwrap();
    let store = places::Store {
        places: dir.join("places.json"),
        recents: dir.join("recents.json"),
    };

    let host_key = key(&dir.join("host_key"));
    let user_key = key(&dir.join("user_key"));
    let other_key = key(&dir.join("other_key"));
    key(&dir.join("not_authorized"));
    std::fs::write(dir.join("authorized_keys"), format!("{user_key}\n")).unwrap();
    let port = free_port();
    let closed_port = free_port();
    let sshd = start_sshd(&dir, port);

    std::fs::write(
        dir.join("known_hosts"),
        format!("[127.0.0.1]:{port} {host_key}\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("known_hosts_changed"),
        format!("[127.0.0.1]:{port} {other_key}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("known_hosts_empty"), "").unwrap();
    let user = std::env::var("USER").unwrap();
    let host = |alias: &str, port: u16, identity: &str, known: &str, strict: &str| {
        format!(
            "Host {alias}\n  HostName 127.0.0.1\n  Port {port}\n  User {user}\n  \
            IdentityFile {dir}/{identity}\n  IdentitiesOnly yes\n  IdentityAgent none\n  \
            UserKnownHostsFile {dir}/{known}\n  GlobalKnownHostsFile /dev/null\n  \
            StrictHostKeyChecking {strict}\n  PasswordAuthentication no\n  \
            KbdInteractiveAuthentication no\n  LogLevel ERROR\n",
            dir = dir.display(),
        )
    };
    let config = dir.join("ssh_config");
    std::fs::write(
        &config,
        [
            host("fixture", port, "user_key", "known_hosts", "yes"),
            host("changed", port, "user_key", "known_hosts_changed", "yes"),
            host("permissive", port, "user_key", "known_hosts_changed", "no"),
            host("unknown", port, "user_key", "known_hosts_empty", "yes"),
            host(
                "accept-new",
                port,
                "user_key",
                "known_hosts_empty",
                "accept-new",
            ),
            host("nokey", port, "not_authorized", "known_hosts", "yes"),
            host("closed", closed_port, "user_key", "known_hosts", "yes"),
        ]
        .concat(),
    )
    .unwrap();
    for alias in [
        "fixture",
        "changed",
        "permissive",
        "unknown",
        "accept-new",
        "nokey",
        "closed",
    ] {
        store.add_remote(alias, Some(alias), None).unwrap();
    }

    let real: Connector = {
        let config = config.clone();
        Arc::new(move |source: &Source| {
            let mut args: Vec<OsString> = vec!["-F".into(), config.clone().into()];
            args.extend(ssh::arguments(&source.host));
            ssh::spawn(ssh::SSH.as_ref(), &args, source.host.clone())
        })
    };

    println!("sshd on 127.0.0.1:{port}, pid {}", sshd.0.id());
    let connections = Connections::new(store.clone(), real.clone());
    let expect = |alias: &str, check: fn(&RemoteError) -> bool| match connections.connect(alias) {
        Ok(_) => panic!("{alias} connected"),
        Err(error) => {
            assert!(check(&error), "{alias}: {error:?}");
            println!("{alias}: refused, as it should be: {error}");
        }
    };
    expect("changed", |e| matches!(e, RemoteError::HostKeyChanged(_)));
    // The user's config can't let a changed or an unknown key through.
    expect("permissive", |e| {
        matches!(e, RemoteError::HostKeyChanged(_))
    });
    expect("unknown", |e| matches!(e, RemoteError::HostKeyUnknown(_)));
    expect("accept-new", |e| {
        matches!(e, RemoteError::HostKeyUnknown(_))
    });
    expect("nokey", |e| matches!(e, RemoteError::AuthFailed(..)));
    expect("closed", |e| matches!(e, RemoteError::Unreachable(..)));

    let root = dir.join("real");
    let (far, near) = (root.join("far"), root.join("near"));
    std::fs::create_dir_all(far.join("sub")).unwrap();
    std::fs::create_dir_all(&near).unwrap();
    std::fs::write(far.join("over ssh.txt"), "through the real ssh").unwrap();
    std::fs::write(near.join("up.txt"), "up").unwrap();
    let open = connections.connect("fixture").unwrap();
    println!("connected to {} over ssh", open.source().host);
    let page = open.list(&remote("fixture", &far), 0, 10).unwrap();
    assert_eq!(page.entries.len(), 2);
    assert!(page.entries.iter().any(|entry| entry.kind == Kind::Dir));
    for entry in &page.entries {
        println!("  {} {:?} {} bytes", entry.path, entry.kind, entry.size);
    }
    let (data, size) = open
        .read(&remote("fixture", &far.join("over ssh.txt")), 8, 8)
        .unwrap();
    assert_eq!((data.as_slice(), size), (&b"the real"[..], 20));
    println!("read a range: {:?}", String::from_utf8_lossy(&data));

    let (mut client, serving) = owner(&root, &store, real.clone());
    let down = job(
        &mut client,
        Request::Download {
            paths: vec![remote("fixture", &far.join("over ssh.txt"))],
            into: near.clone(),
            conflict: Conflict::Stop,
        },
    );
    assert_eq!(down.state, State::Completed, "{down:?}");
    assert!(down.undoable);
    assert_eq!(
        std::fs::read(near.join("over ssh.txt")).unwrap(),
        b"through the real ssh"
    );
    println!("job {}: {} {}", down.id, down.title, down.state.label());
    let up = job(
        &mut client,
        Request::Upload {
            paths: vec![near.join("up.txt")],
            into: remote("fixture", &far.join("sub")),
            conflict: Conflict::Stop,
        },
    );
    assert_eq!(up.state, State::Completed, "{up:?}");
    assert!(!up.undoable);
    assert_eq!(std::fs::read(far.join("sub/up.txt")).unwrap(), b"up");
    println!("job {}: {} {}", up.id, up.title, up.state.label());
    drop(client);
    let _ = serving.join();

    assert!(connections.disconnect("fixture"));
    assert!(!open.alive());
    assert!(open.list(&remote("fixture", &far), 0, 1).is_err());
    println!("disconnected, and the ssh it started is gone");
    drop(open);

    measure(
        "over-sftp-server",
        {
            Arc::new(|source: &Source| {
                let none: [&str; 0] = [];
                ssh::spawn(
                    "/usr/libexec/sftp-server".as_ref(),
                    &none,
                    source.host.clone(),
                )
            })
        },
        "fixture",
        &store,
        &dir,
    );
    measure("over-ssh-loopback", real, "fixture", &store, &dir);

    drop(connections);
    drop(sshd);
    println!("\nsshd stopped by its handle; the scratch folder goes next");
}
