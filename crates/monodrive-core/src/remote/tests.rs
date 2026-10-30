use super::*;
use crate::remote::fixture::{self, frame, request, status, string, version};
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

fn source() -> Source {
    Source::new("box", "box.example", None).unwrap()
}

fn limits() -> Limits {
    Limits {
        connect: Duration::from_secs(10),
        request: Duration::from_secs(10),
    }
}

fn fixture_connection() -> Connection {
    Connection::open(source(), fixture::transport(), limits()).unwrap()
}

/// A temp folder by its real path, which the server reports.
fn folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap();
    (dir, real)
}

fn at(path: &Path) -> RemotePath {
    RemotePath::new("box", path.to_str().unwrap()).unwrap()
}

fn names(page: &Page) -> Vec<String> {
    let mut names: Vec<String> = page.entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    names
}

/// A connection to a hand-written server, which has answered INIT.
fn fake(serve: impl FnOnce(&mut UnixStream) + Send + 'static) -> Result<Connection, RemoteError> {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    std::thread::spawn(move || {
        if request(&mut theirs).is_some() {
            let _ = theirs.write_all(&version());
            serve(&mut theirs);
        }
    });
    let reader = ours.try_clone().unwrap();
    Connection::open(
        source(),
        Transport::new(reader, ours),
        Limits {
            connect: Duration::from_secs(10),
            request: Duration::from_millis(200),
        },
    )
}

#[test]
fn connecting_sends_only_init_and_a_listing_reads_only_that_folder() {
    let (tx, rx) = mpsc::channel();
    let connection = fake(move |server| {
        while let Some((kind, id)) = request(server) {
            let _ = tx.send(kind);
            let reply = match kind {
                11 => frame(102, Some(id), &string(b"h")),
                12 => frame(101, Some(id), &status(1)),
                _ => frame(101, Some(id), &status(0)),
            };
            let _ = server.write_all(&reply);
        }
    })
    .unwrap();
    assert!(connection.alive());
    assert!(rx.try_recv().is_err(), "connecting asked for nothing");
    let page = connection
        .list(&RemotePath::new("box", "/srv").unwrap(), 0, 10)
        .unwrap();
    assert_eq!(page, Page::default());
    let asked: Vec<u8> = rx.try_iter().collect();
    assert_eq!(asked, [11, 12, 4]);
}

#[test]
fn a_real_server_lists_unusual_names_and_pages_without_repeats() {
    let (_dir, root) = folder();
    let unusual = [
        "plain.txt",
        "with space.txt",
        "new\nline",
        "tab\there",
        "'quote\"s",
        "-rf",
        "--help",
        "ünicode-日本語",
        "$(not a command)",
        ".hidden",
        "back\\slash",
    ];
    for name in unusual {
        std::fs::write(root.join(name), name).unwrap();
    }
    std::fs::create_dir(root.join("folder")).unwrap();
    std::os::unix::fs::symlink("plain.txt", root.join("link")).unwrap();
    let connection = fixture_connection();
    let page = connection.list(&at(&root), 0, 100).unwrap();
    let mut expected: Vec<String> = unusual.iter().map(|name| (*name).to_string()).collect();
    expected.extend(["folder".to_string(), "link".to_string()]);
    expected.sort();
    assert_eq!(names(&page), expected);
    assert!(!page.more);
    assert_eq!(page.left_out, 0);
    let entry = |name: &str| page.entries.iter().find(|e| e.name == name).unwrap();
    assert_eq!(entry("folder").kind, Kind::Dir);
    assert_eq!(entry("link").kind, Kind::Link);
    assert_eq!(entry("plain.txt").kind, Kind::File);
    assert_eq!(entry("plain.txt").size, 9);
    assert!(entry("plain.txt").modified > 0);
    // One line is one path, whatever a name holds.
    assert!(!entry("new\nline").path.to_string().contains('\n'));
    assert_eq!(
        entry("new\nline").path.path(),
        root.join("new\nline").to_str().unwrap()
    );

    let mut paged = Vec::new();
    let mut offset = 0;
    loop {
        let part = connection.list(&at(&root), offset, 4).unwrap();
        assert!(part.entries.len() <= 4);
        offset += part.entries.len();
        paged.extend(part.entries.iter().map(|e| e.name.clone()));
        if !part.more {
            break;
        }
    }
    paged.sort();
    assert_eq!(paged, expected);

    let link = connection.metadata(&at(&root.join("link"))).unwrap();
    assert_eq!(link.link.as_deref(), Some("plain.txt"));
    let followed = connection.follow(&at(&root.join("link"))).unwrap();
    assert_eq!(followed.kind, Kind::File);
    assert_eq!(
        connection
            .realpath(root.join("folder/../link").to_str().unwrap())
            .unwrap()
            .path(),
        root.join("plain.txt").to_str().unwrap()
    );
    let missing = connection.metadata(&at(&root.join("nope"))).unwrap_err();
    assert!(matches!(missing, RemoteError::NotFound(_)), "{missing}");
    assert!(missing.to_string().starts_with("box:/"), "{missing}");
}

#[test]
fn reads_are_bounded_and_ranges_come_back_exact() {
    let (_dir, root) = folder();
    let bytes: Vec<u8> = (0..3_000_000u32).map(|n| (n % 251) as u8).collect();
    let file = root.join("big");
    std::fs::write(&file, &bytes).unwrap();
    let connection = fixture_connection();
    let (data, size) = connection.read(&at(&file), 100, 50).unwrap();
    assert_eq!((data.as_slice(), size), (&bytes[100..150], 3_000_000));
    let (data, _) = connection.read(&at(&file), 0, u32::MAX).unwrap();
    assert_eq!(data.as_slice(), &bytes[..MOST_READ as usize]);
    let (data, _) = connection.read(&at(&file), 2_999_990, 100).unwrap();
    assert_eq!(data.as_slice(), &bytes[2_999_990..]);
    let (data, _) = connection.read(&at(&file), 9_000_000, 100).unwrap();
    assert!(data.is_empty());
    let refused = connection.read(&at(&root), 0, 10).unwrap_err();
    assert!(refused.to_string().contains("not a file"), "{refused}");
}

#[test]
fn renames_never_replace_and_folders_need_their_parent() {
    let (_dir, root) = folder();
    std::fs::write(root.join("a"), "a").unwrap();
    std::fs::write(root.join("b"), "b").unwrap();
    let connection = fixture_connection();
    let refused = connection
        .rename(&at(&root.join("a")), &at(&root.join("b")))
        .unwrap_err();
    assert!(refused.to_string().contains("already exists"), "{refused}");
    assert_eq!(std::fs::read(root.join("b")).unwrap(), b"b");
    connection
        .rename(&at(&root.join("a")), &at(&root.join("c")))
        .unwrap();
    assert_eq!(std::fs::read(root.join("c")).unwrap(), b"a");
    connection.mkdir(&at(&root.join("made"))).unwrap();
    assert!(root.join("made").is_dir());
    assert!(connection.mkdir(&at(&root.join("no/parent"))).is_err());
}

#[test]
fn another_sources_path_is_refused_and_a_remote_path_is_never_a_local_one() {
    let (_dir, root) = folder();
    let elsewhere = RemotePath::new("other", root.to_str().unwrap()).unwrap();
    let connection = fixture_connection();
    let refused = connection.list(&elsewhere, 0, 10).unwrap_err();
    assert!(refused.to_string().contains("isn't on box"), "{refused}");
    assert!(connection.metadata(&elsewhere).is_err());
    assert!(connection.read(&elsewhere, 0, 1).is_err());
    assert!(connection.mkdir(&elsewhere.join("x").unwrap()).is_err());
    assert!(
        connection
            .rename(&at(&root), &elsewhere.join("x").unwrap())
            .is_err()
    );
    // Printed, it's something no local service takes for a path of its own.
    let printed = PathBuf::from(at(&root).to_string());
    assert!(!printed.is_absolute());
    assert!(!printed.starts_with("/"));
}

#[test]
fn remote_paths_keep_to_one_shape() {
    let path = RemotePath::parse("box:/a//b/./c/").unwrap();
    assert_eq!(path.source(), "box");
    assert_eq!(path.path(), "/a/b/c");
    assert_eq!(path.to_string(), "box:/a/b/c");
    assert_eq!(path.name(), Some("c"));
    assert_eq!(path.parent().unwrap().to_string(), "box:/a/b");
    let root = RemotePath::parse("box:/").unwrap();
    assert_eq!((root.name(), root.parent()), (None, None));
    assert_eq!(root.join("a").unwrap().to_string(), "box:/a");
    for bad in [
        "box:/a/../b",
        "box:relative",
        "/local",
        "bad id:/a",
        "box:/a\0b",
        ":/a",
    ] {
        assert!(RemotePath::parse(bad).is_err(), "{bad}");
    }
    for bad in ["", ".", "..", "a/b", "/a", "a\0"] {
        assert!(path.join(bad).is_err(), "{bad}");
    }
    // What's read back from a journal or a client obeys the same rules.
    let read: Result<RemotePath, _> =
        serde_json::from_str(r#"{"source":"box","path":"/a/../../etc"}"#);
    assert!(read.is_err());
    let read: RemotePath = serde_json::from_str(r#"{"source":"box","path":"/a//b"}"#).unwrap();
    assert_eq!(read.path(), "/a/b");
}

#[test]
fn malformed_servers_end_the_connection_with_an_error_instead_of_hanging_or_growing() {
    // Each answers its first request after INIT with these bytes, then waits.
    type Reply = fn(u32) -> Vec<u8>;
    let replies: [(&str, Reply); 8] = [
        ("over the frame limit", |_| {
            (sftp::MAX_PACKET as u32 + 1).to_be_bytes().to_vec()
        }),
        ("the largest length there is", |_| vec![0xff; 4]),
        ("an empty frame", |_| vec![0; 4]),
        ("too short for an id", |_| frame(105, None, b"")),
        ("a string past the frame's end", |id| {
            let mut body = u32::MAX.to_be_bytes().to_vec();
            body.push(0);
            frame(102, Some(id), &body)
        }),
        ("attrs cut short", |id| {
            frame(105, Some(id), &1u32.to_be_bytes())
        }),
        ("the wrong kind of reply", |id| {
            frame(102, Some(id), &string(b"h"))
        }),
        ("a status of success where attrs belong", |id| {
            frame(101, Some(id), &status(0))
        }),
    ];
    for (what, reply) in replies {
        let connection = fake(move |server| {
            if let Some((_, id)) = request(server) {
                let _ = server.write_all(&reply(id));
            }
            let _ = request(server);
        })
        .unwrap();
        let error = connection.follow(&at("/a".as_ref())).unwrap_err();
        assert!(error.is_connection(), "{what}: {error:?}");
        assert!(
            matches!(error, RemoteError::Protocol(_)),
            "{what}: {error:?}"
        );
        assert!(!connection.alive(), "{what}");
        assert_eq!(connection.follow(&at("/a".as_ref())).unwrap_err(), error);
    }
}

#[test]
fn a_reply_cut_short_or_a_silent_server_ends_the_connection() {
    let connection = fake(|server| {
        let _ = request(server);
        let _ = server.write_all(&[0, 0, 0, 100, 105, 0, 0]);
    })
    .unwrap();
    let error = connection.follow(&at("/a".as_ref())).unwrap_err();
    assert!(matches!(error, RemoteError::Disconnected(_)), "{error:?}");

    let connection = fake(|server| while request(server).is_some() {}).unwrap();
    let error = connection.follow(&at("/a".as_ref())).unwrap_err();
    assert!(matches!(error, RemoteError::Timeout(_)), "{error:?}");
    assert!(!connection.alive());
}

#[test]
fn a_reply_for_nobody_is_ignored_and_more_data_than_asked_is_refused() {
    let connection = fake(|server| {
        let (_, id) = request(server).unwrap();
        let _ = server.write_all(&frame(105, Some(id + 1000), &0u32.to_be_bytes()));
        let _ = server.write_all(&frame(105, Some(id), &0u32.to_be_bytes()));
        // The read: open, read, close.
        while let Some((kind, id)) = request(server) {
            let reply = match kind {
                3 => frame(102, Some(id), &string(b"h")),
                5 => frame(103, Some(id), &string(&[0; 11])),
                17 => {
                    let mut body = (1u32 | 4).to_be_bytes().to_vec();
                    body.extend_from_slice(&100u64.to_be_bytes());
                    body.extend_from_slice(&0o100_644u32.to_be_bytes());
                    frame(105, Some(id), &body)
                }
                _ => frame(101, Some(id), &status(0)),
            };
            let _ = server.write_all(&reply);
        }
    })
    .unwrap();
    assert_eq!(
        connection.follow(&at("/a".as_ref())).unwrap().kind,
        Kind::Other
    );
    let error = connection.read(&at("/a".as_ref()), 0, 10).unwrap_err();
    assert!(error.to_string().contains("more data"), "{error}");
    assert!(!connection.alive());
}

#[test]
fn text_before_the_protocol_says_what_it_probably_is() {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    std::thread::spawn(move || {
        let _ = request(&mut theirs);
        let _ = theirs.write_all(b"Welcome to my server\n");
        let _ = request(&mut theirs);
    });
    let reader = ours.try_clone().unwrap();
    let Err(error) = Connection::open(source(), Transport::new(reader, ours), limits()) else {
        panic!("that isn't SFTP");
    };
    assert!(matches!(error, RemoteError::Protocol(_)), "{error:?}");
    assert!(error.to_string().contains("shell startup file"), "{error}");
}

#[test]
fn an_old_version_and_a_silent_start_are_refused() {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    std::thread::spawn(move || {
        let _ = request(&mut theirs);
        let _ = theirs.write_all(&frame(2, None, &2u32.to_be_bytes()));
        let _ = request(&mut theirs);
    });
    let reader = ours.try_clone().unwrap();
    let Err(error) = Connection::open(source(), Transport::new(reader, ours), limits()) else {
        panic!("too old");
    };
    assert!(error.to_string().contains("SFTP 2"), "{error}");

    let (ours, mut theirs) = UnixStream::pair().unwrap();
    std::thread::spawn(move || while request(&mut theirs).is_some() {});
    let reader = ours.try_clone().unwrap();
    let quick = Limits {
        connect: Duration::from_millis(50),
        request: Duration::from_millis(50),
    };
    let Err(error) = Connection::open(source(), Transport::new(reader, ours), quick) else {
        panic!("it never answered");
    };
    assert!(matches!(error, RemoteError::Timeout(_)), "{error:?}");
}

#[test]
fn names_that_arent_plain_are_left_out_and_counted() {
    let connection = fake(|server| {
        let mut read = false;
        while let Some((kind, id)) = request(server) {
            let reply = match kind {
                11 => frame(102, Some(id), &string(b"h")),
                12 if !read => {
                    read = true;
                    let names: [&[u8]; 7] = [
                        b".",
                        b"..",
                        b"../../etc/passwd",
                        b"sub/dir",
                        b"nul\0name",
                        b"caf\xe9",
                        b"fine",
                    ];
                    let mut body = (names.len() as u32).to_be_bytes().to_vec();
                    for name in names {
                        body.extend_from_slice(&string(name));
                        body.extend_from_slice(&string(b"longname"));
                        body.extend_from_slice(&0u32.to_be_bytes());
                    }
                    frame(104, Some(id), &body)
                }
                12 => frame(101, Some(id), &status(1)),
                _ => frame(101, Some(id), &status(0)),
            };
            let _ = server.write_all(&reply);
        }
    })
    .unwrap();
    let page = connection.list(&at("/srv".as_ref()), 0, 10).unwrap();
    assert_eq!(names(&page), ["fine"]);
    assert_eq!(page.left_out, 4);
}

#[test]
fn the_ssh_command_line_cant_be_changed_by_a_host() {
    let args = ssh::arguments("nas");
    let args: Vec<&str> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(
        args,
        [
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "-oConnectTimeout=15",
            "-oForwardAgent=no",
            "-oForwardX11=no",
            "-oPermitLocalCommand=no",
            "-oClearAllForwardings=yes",
            "-s",
            "--",
            "nas",
            "sftp"
        ]
    );
    for fine in [
        "nas",
        "me@nas.local",
        "192.168.1.2",
        "fe80::1%en0",
        "my_box-2",
    ] {
        assert!(Source::new("a", fine, None).is_ok(), "{fine}");
    }
    for bad in [
        "",
        "-oProxyCommand=touch /tmp/x",
        "-p",
        "a b",
        "a;b",
        "a$(b)",
        "a`b`",
        "ssh://nas",
        "a\nb",
    ] {
        assert!(Source::new("a", bad, None).is_err(), "{bad}");
    }
    for bad in ["", "a:b", "a/b", "a b", "-".repeat(65).as_str()] {
        assert!(Source::new(bad, "nas", None).is_err(), "{bad}");
    }
    assert!(Source::new("a", "nas", Some("relative")).is_err());
    assert_eq!(
        Source::new("a", "nas", Some("/srv/"))
            .unwrap()
            .start
            .as_deref(),
        Some("/srv")
    );
    let caps = source().capabilities();
    assert!(caps.list && caps.read && caps.write && caps.mkdir && caps.rename && caps.metadata);
    assert!(!caps.trash && !caps.delete);
}

#[test]
fn what_ssh_says_is_told_apart() {
    let changed = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
        @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
        @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
        Offending ED25519 key in /Users/me/.ssh/known_hosts:3\n\
        Host key for nas has changed and you have requested strict checking.\n\
        Host key verification failed.\n";
    let cases: [(&str, RemoteError); 10] = [
        (changed, RemoteError::HostKeyChanged("nas".into())),
        (
            "No ED25519 host key is known for nas and you have requested strict checking.\n\
            Host key verification failed.\n",
            RemoteError::HostKeyUnknown("nas".into()),
        ),
        (
            "me@nas: Permission denied (publickey,password,keyboard-interactive).\n",
            RemoteError::InteractiveAuth("nas".into()),
        ),
        (
            "me@nas: Permission denied (publickey).\n",
            RemoteError::AuthFailed("nas".into(), "Permission denied (publickey).".into()),
        ),
        (
            "ssh: Could not resolve hostname nas: nodename nor servname provided\n",
            RemoteError::Unreachable(
                "nas".into(),
                "ssh: Could not resolve hostname nas: nodename nor servname provided".into(),
            ),
        ),
        (
            "ssh: connect to host nas port 22: Connection refused\n",
            RemoteError::Unreachable(
                "nas".into(),
                "ssh: connect to host nas port 22: Connection refused".into(),
            ),
        ),
        (
            "ssh: connect to host nas port 22: Operation timed out\n",
            RemoteError::Unreachable(
                "nas".into(),
                "ssh: connect to host nas port 22: Operation timed out".into(),
            ),
        ),
        (
            "subsystem request failed on channel 0\n",
            RemoteError::NoSftp("nas".into()),
        ),
        (
            "Welcome, your access is logged.\nme@nas: Permission denied (publickey).\n",
            RemoteError::AuthFailed("nas".into(), "Permission denied (publickey).".into()),
        ),
        (
            "Too many authentication failures\n",
            RemoteError::AuthFailed("nas".into(), "Too many authentication failures.".into()),
        ),
    ];
    for (said, expected) in cases {
        assert_eq!(ssh::diagnose(said, "nas"), Some(expected), "{said}");
    }
    // A banner's words aren't ssh's message, and silence isn't one.
    assert_eq!(ssh::diagnose("", "nas"), None);
    assert_eq!(
        ssh::diagnose("Remember: Permission denied means ask IT.\n", "nas"),
        None
    );
    let error = RemoteError::InteractiveAuth("nas".into());
    assert!(
        error
            .to_string()
            .contains("interactive authentication required")
    );
    assert!(error.is_connection());
    let error = RemoteError::HostKeyChanged("nas".into());
    assert!(error.to_string().contains("didn't connect"));
    let error = RemoteError::HostKeyUnknown("nas".into()).to_string();
    assert!(
        error.contains("connect once with ssh in a terminal so it's in known_hosts"),
        "{error}"
    );
}

#[test]
fn a_program_that_fails_as_ssh_does_reports_why_and_is_reaped() {
    for (said, check) in [
        (
            "me@nas: Permission denied (publickey,password).",
            (|e: &RemoteError| matches!(e, RemoteError::InteractiveAuth(_)))
                as fn(&RemoteError) -> bool,
        ),
        (
            "@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @",
            |e| matches!(e, RemoteError::HostKeyChanged(_)),
        ),
        ("said nothing known", |e| {
            matches!(e, RemoteError::Disconnected(_))
        }),
    ] {
        let script = format!("echo '{said}' >&2; exit 255");
        let transport = ssh::spawn(
            "/bin/sh".as_ref(),
            &["-c".to_string(), script],
            "nas".to_string(),
        )
        .unwrap();
        let Err(error) = Connection::open(source(), transport, limits()) else {
            panic!("it exited");
        };
        assert!(check(&error), "{said}: {error:?}");
    }
}

#[test]
fn a_missing_program_is_reported_not_a_panic() {
    let error = ssh::spawn(
        "/nonexistent/ssh".as_ref(),
        &["x".to_string()],
        "nas".to_string(),
    )
    .err()
    .unwrap();
    assert!(matches!(error, RemoteError::Unreachable(..)), "{error:?}");
}

fn store(dir: &Path) -> places::Store {
    places::Store {
        places: dir.join("places.json"),
        recents: dir.join("recents.json"),
    }
}

#[test]
fn sources_save_beside_the_other_lists_and_forget_without_a_trace() {
    let (_dir, dir) = folder();
    let store = store(&dir);
    assert_eq!(store.remotes(), Ok(Vec::new()));
    let nas = store
        .add_remote("me@NAS.local", None, Some("/srv"))
        .unwrap();
    assert_eq!(
        (nas.id.as_str(), nas.host.as_str()),
        ("nas.local", "me@NAS.local")
    );
    assert_eq!(
        store
            .add_remote("me@NAS.local", None, Some("/srv"))
            .unwrap(),
        nas
    );
    let other = store.add_remote("them@NAS.local", None, None).unwrap();
    assert_eq!(other.id, "nas.local-2");
    let named = store.add_remote("box", Some("work"), None).unwrap();
    assert_eq!(named.id, "work");
    let taken = store.add_remote("box2", Some("work"), None).unwrap_err();
    assert!(taken.contains("already"), "{taken}");
    assert!(store.add_remote("-oProxyCommand=x", None, None).is_err());
    assert_eq!(store.remote("work"), Ok(named));
    assert_eq!(
        store.remote("gone"),
        Err(RemoteError::UnknownSource("gone".into()))
    );

    let work = dir.join("Work");
    std::fs::create_dir(&work).unwrap();
    let root = [];
    store.pin(&work, None, &root).unwrap();
    store.forget_remote("nas.local").unwrap();
    assert!(store.forget_remote("nas.local").is_err());
    assert_eq!(store.remotes().unwrap().len(), 2);
    let json = std::fs::read_to_string(&store.places).unwrap();
    assert!(
        json.contains("\"remotes\"") && json.contains("Work"),
        "{json}"
    );

    // A list that doesn't read is never replaced by a change.
    std::fs::write(&store.places, "{").unwrap();
    assert!(store.add_remote("box3", None, None).is_err());
    assert_eq!(std::fs::read_to_string(&store.places).unwrap(), "{");
}

#[test]
fn connections_open_once_per_source_on_demand_and_again_after_disconnecting() {
    let (_dir, dir) = folder();
    let store = store(&dir);
    store.add_remote("box.example", Some("box"), None).unwrap();
    store
        .add_remote("other.example", Some("other"), None)
        .unwrap();
    store
        .add_remote("refused.example", Some("refused"), None)
        .unwrap();
    let opened = Arc::new(AtomicUsize::new(0));
    let counted = opened.clone();
    let connections = Connections::new(
        store.clone(),
        Arc::new(move |source: &Source| {
            if source.id == "refused" {
                return Err(RemoteError::HostKeyChanged(source.host.clone()));
            }
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(fixture::transport())
        }),
    );
    assert_eq!(opened.load(Ordering::Relaxed), 0);
    assert!(!connections.is_connected("box"));

    let held: Vec<Arc<Connection>> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| connections.connect("box").unwrap()))
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert_eq!(opened.load(Ordering::Relaxed), 1);
    assert!(held.iter().all(|one| Arc::ptr_eq(one, &held[0])));
    assert!(connections.is_connected("box"));

    connections.connect("other").unwrap();
    assert_eq!(opened.load(Ordering::Relaxed), 2);

    assert!(connections.disconnect("box"));
    assert!(
        !held[0].alive(),
        "a disconnect ends it for everyone holding it"
    );
    assert!(!connections.is_connected("box"));
    assert!(!connections.disconnect("box"));
    connections.connect("box").unwrap();
    assert_eq!(opened.load(Ordering::Relaxed), 3);

    // Refused, it reports why, and it's not kept.
    assert_eq!(
        connections.connect("refused").err(),
        Some(RemoteError::HostKeyChanged("refused.example".into()))
    );
    assert!(!connections.is_connected("refused"));

    // A forgotten source can't be connected to, connected or not.
    store.forget_remote("other").unwrap();
    assert!(connections.disconnect("other"));
    assert!(matches!(
        connections.connect("other"),
        Err(RemoteError::UnknownSource(_))
    ));

    // Nobody else holds it, so an idle owner lets it go.
    drop(held);
    connections.release_idle();
    assert!(!connections.is_connected("box"));
}

#[test]
fn a_connection_lost_mid_listing_reconnects_next_time() {
    let (_dir, dir) = folder();
    let store = store(&dir);
    store.add_remote("box.example", Some("box"), None).unwrap();
    let opened = Arc::new(AtomicUsize::new(0));
    let counted = opened.clone();
    let connections = Connections::new(
        store,
        Arc::new(move |_: &Source| {
            let n = counted.fetch_add(1, Ordering::Relaxed);
            Ok(if n == 0 {
                fixture::killed_after(1).transport
            } else {
                fixture::transport()
            })
        }),
    );
    let first = connections.connect("box");
    // It died on its first bytes: during the handshake or right after.
    if let Ok(first) = first {
        assert!(first.list(&at(&dir), 0, 1).is_err());
    }
    let second = connections.connect("box").unwrap();
    assert!(second.list(&at(&dir), 0, 1).is_ok());
    assert_eq!(opened.load(Ordering::Relaxed), 2);
}

fn meter<'a>(copied: &'a AtomicU64, cancel: &'a AtomicBool) -> Meter<'a> {
    Meter { copied, cancel }
}

#[test]
fn previews_download_to_a_real_read_only_path_and_clean_up_after_themselves() {
    let (_dir, dir) = folder();
    let (root, cache) = (dir.join("server"), dir.join("cache"));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("note.txt"), "hello").unwrap();
    std::fs::write(root.join("big"), vec![1; 5000]).unwrap();
    std::os::unix::fs::symlink("note.txt", root.join("link")).unwrap();
    let connection = fixture_connection();
    let previews = Previews::open(&cache, 5006).unwrap();
    let (copied, cancel) = (AtomicU64::new(0), AtomicBool::new(false));
    let meter = meter(&copied, &cancel);

    let preview = previews
        .fetch(&connection, &at(&root.join("note.txt")), &meter)
        .unwrap();
    assert_eq!(std::fs::read(&preview.path).unwrap(), b"hello");
    assert_eq!(preview.path.file_name().unwrap(), "note.txt");
    assert_eq!(copied.load(Ordering::Relaxed), 5);
    let mode = std::fs::metadata(&preview.path)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o400);
    assert!(preview.path.starts_with(&cache));

    let linked = previews
        .fetch(&connection, &at(&root.join("link")), &meter)
        .unwrap();
    assert_eq!(std::fs::read(&linked.path).unwrap(), b"hello");
    assert_eq!(linked.path.file_name().unwrap(), "link");

    // It doesn't fit beside what's held open, until that's let go.
    let big = at(&root.join("big"));
    let refused = previews.fetch(&connection, &big, &meter).err().unwrap();
    assert!(matches!(refused, RemoteError::TooBig(_)), "{refused:?}");
    let (first, second) = (preview.path.clone(), linked.path.clone());
    drop(preview);
    assert!(!first.exists());
    assert!(second.exists());
    let held = previews.fetch(&connection, &big, &meter).unwrap();
    assert_eq!(std::fs::metadata(&held.path).unwrap().len(), 5000);

    let refused = previews
        .fetch(&connection, &at(&root), &meter)
        .err()
        .unwrap();
    assert!(refused.to_string().contains("not a file"), "{refused}");

    // Cancelled, nothing of it stays.
    drop(held);
    cancel.store(true, Ordering::Relaxed);
    let stopped = previews.fetch(&connection, &big, &meter).err().unwrap();
    assert_eq!(stopped, RemoteError::Cancelled);
    let mine: Vec<PathBuf> = std::fs::read_dir(previews.inner.dir.clone())
        .unwrap()
        .map(|item| item.unwrap().path())
        .collect();
    assert_eq!(mine, [second.parent().unwrap().to_path_buf()]);

    drop(linked);
    drop(previews);
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
}

#[test]
fn a_cache_a_dead_process_left_is_swept_and_a_live_ones_kept() {
    let (_dir, dir) = folder();
    let cache = dir.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let dead = cache.join("1-1");
    std::fs::create_dir(&dead).unwrap();
    std::fs::write(dead.join("left"), "x").unwrap();
    std::fs::write(cache.join("1-1.lock"), "").unwrap();
    let live = Previews::open(&cache, 10).unwrap();
    assert!(!dead.exists());
    assert!(!cache.join("1-1.lock").exists());
    let other = Previews::open(&cache, 10).unwrap();
    assert!(live.inner.dir.exists());
    assert!(other.inner.dir.exists());
}

/// Attrs of a regular file of `size` bytes.
fn file_attrs(size: u64) -> Vec<u8> {
    let mut body = (1u32 | 4).to_be_bytes().to_vec();
    body.extend_from_slice(&size.to_be_bytes());
    body.extend_from_slice(&0o100_644u32.to_be_bytes());
    body
}

/// A server whose stat says `stat` bytes and whose lstat says `lstat`, and
/// that answers each read with `sent(asked)` bytes. Records the kinds it's
/// asked.
fn sizes_differ(stat: u64, lstat: u64, sent: fn(u32) -> u32) -> (Connection, mpsc::Receiver<u8>) {
    let (tx, rx) = mpsc::channel();
    let connection = fake(move |server| {
        while let Some((kind, id, body)) = fixture::packet(server) {
            let _ = tx.send(kind);
            let reply = match kind {
                17 => frame(105, Some(id), &file_attrs(stat)),
                7 => frame(105, Some(id), &file_attrs(lstat)),
                16 => {
                    let mut name = 1u32.to_be_bytes().to_vec();
                    name.extend_from_slice(&string(b"/srv/real"));
                    name.extend_from_slice(&string(b""));
                    name.extend_from_slice(&0u32.to_be_bytes());
                    frame(104, Some(id), &name)
                }
                3 => frame(102, Some(id), &string(b"h")),
                5 => {
                    // After the handle "h": a u64 offset, then the length.
                    let asked = u32::from_be_bytes(body[13..17].try_into().unwrap());
                    frame(103, Some(id), &string(&vec![7; sent(asked) as usize]))
                }
                _ => frame(101, Some(id), &status(0)),
            };
            let _ = server.write_all(&reply);
        }
    })
    .unwrap();
    (connection, rx)
}

#[test]
fn a_preview_never_downloads_more_than_it_reserved_whatever_the_server_says_next() {
    let (_dir, dir) = folder();
    let previews = Previews::open(&dir.join("cache"), 100).unwrap();
    let (copied, cancel) = (AtomicU64::new(0), AtomicBool::new(false));
    let meter = meter(&copied, &cancel);
    let file = RemotePath::new("box", "/srv/file").unwrap();

    // Grown, or another file behind a changed link, since the stat that
    // reserved its room: refused before anything is asked for or written.
    let (grown, asked) = sizes_differ(10, 1000, |asked| asked);
    let refused = previews.fetch(&grown, &file, &meter).err().unwrap();
    assert!(matches!(refused, RemoteError::TooBig(_)), "{refused:?}");
    let asked: Vec<u8> = asked.try_iter().collect();
    assert!(!asked.contains(&3), "opened it anyway: {asked:?}");
    assert_eq!(copied.load(Ordering::Relaxed), 0);

    // A reply larger than the read asked for ends it at the reservation.
    let (lying, _) = sizes_differ(10, 10, |asked| asked + 1);
    let refused = previews.fetch(&lying, &file, &meter).err().unwrap();
    assert!(matches!(refused, RemoteError::Protocol(_)), "{refused:?}");
    assert_eq!(copied.load(Ordering::Relaxed), 0);

    // Smaller than reserved: it says what's really there, and gives back the
    // room it reserved when it goes.
    let (shrunk, _) = sizes_differ(100, 20, |asked| asked);
    let preview = previews.fetch(&shrunk, &file, &meter).unwrap();
    assert_eq!(preview.size, 20);
    assert_eq!(std::fs::metadata(&preview.path).unwrap().len(), 20);
    let refused = previews.fetch(&shrunk, &file, &meter).err().unwrap();
    assert!(matches!(refused, RemoteError::TooBig(_)), "{refused:?}");
    drop(preview);
    let again = previews.fetch(&shrunk, &file, &meter).unwrap();
    assert_eq!(again.size, 20);
    drop(again);
    assert_eq!(previews.inner.used.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read_dir(&previews.inner.dir).unwrap().count(), 0);
}
