//! Nothing here reaches the network: sending talks to a loopback peer, and
//! receiving is driven with the events LocalSend's server sends, since its
//! server listens on every address and a firewall asks about each new test
//! binary that does.

use super::receive::Inbox;
use super::*;
use localsend::http::dto_v2::{PrepareUploadRequestDtoV2, PrepareUploadResponseDtoV2};
use localsend::http::server::PeerIp;
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2, SessionEndReasonV2};
use localsend::model::transfer::FileDto;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

/// Long enough for anything here to happen; reaching it is a hang.
const HANG: std::time::Duration = std::time::Duration::from_secs(10);

#[test]
fn an_identity_is_made_once_and_kept_from_others() {
    let dir = tempfile::tempdir().unwrap();
    let made = Identity::load(dir.path(), "Mac".into()).unwrap();
    let again = Identity::load(dir.path(), "Mac".into()).unwrap();
    assert_eq!(made.fingerprint, again.fingerprint);
    assert_eq!(made.fingerprint.len(), 64);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.path().join("identity.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

fn register(alias: &str) -> localsend::http::dto_v2::RegisterDtoV2 {
    localsend::http::dto_v2::RegisterDtoV2 {
        alias: alias.into(),
        version: "2.2".into(),
        device_model: Some("Pixel".into()),
        device_type: None,
        fingerprint: "PEER".into(),
        port: PORT,
        protocol: localsend::model::discovery::ProtocolType::Https,
        download: false,
    }
}

fn dto(id: &str, name: &str, size: u64) -> FileDto {
    FileDto {
        id: id.into(),
        file_name: name.into(),
        size,
        file_type: "application/octet-stream".into(),
        sha256: None,
        preview: None,
        metadata: None,
    }
}

/// An inbox saving to `folder`, and what it says.
fn inbox(folder: PathBuf) -> (Arc<Inbox>, mpsc::Receiver<Event>) {
    let (tell, told) = mpsc::channel();
    let tell = Mutex::new(tell);
    let inbox = Inbox::new("ME".into(), folder, move |event| {
        let _ = tell.lock().unwrap().send(event);
    });
    (inbox, told)
}

fn offer(inbox: &Arc<Inbox>, files: &[FileDto]) -> oneshot::Receiver<PrepareUploadDecisionV2> {
    let (decision_tx, decision) = oneshot::channel();
    inbox.handle(ServerEventV2::PrepareUpload {
        session_id: "session-1".into(),
        ip: PeerIp {
            ip: "192.168.1.20".parse().unwrap(),
            scope_id: None,
        },
        info: register("Pixel 8"),
        cert_fingerprint: Some("PEER".into()),
        files: files.iter().map(|f| (f.id.clone(), f.clone())).collect(),
        decision_tx,
    });
    decision
}

/// The next event that isn't a device being seen or progress.
fn next(told: &mpsc::Receiver<Event>) -> Event {
    loop {
        match told.recv_timeout(HANG).expect("an event") {
            Event::Seen(_) | Event::Progress { .. } => {}
            event => return event,
        }
    }
}

/// Uploads `bytes` as the server would: asks where file `id` goes, writes
/// it there, and reports how that went.
fn upload(inbox: &Arc<Inbox>, id: &str, bytes: Option<&[u8]>) {
    let (target_tx, target) = oneshot::channel();
    inbox.handle(ServerEventV2::FileUpload {
        session_id: "session-1".into(),
        file_id: id.into(),
        file: dto(id, "", 0),
        target_tx,
    });
    let Ok(FileUploadTarget::Path {
        path, result_tx, ..
    }) = target.blocking_recv()
    else {
        panic!("the inbox gives the server a path");
    };
    let result = match bytes {
        Some(bytes) => std::fs::write(&path, bytes).map_err(|e| e.to_string()),
        None => Err("Connection reset".into()),
    };
    result_tx.send(result).unwrap();
}

#[test]
fn an_accepted_offer_lands_under_free_names_and_never_leaves_its_folder() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    std::fs::write(folder.join("notes.txt"), "mine").unwrap();
    std::fs::create_dir(folder.join("Photos")).unwrap();
    let (inbox, told) = inbox(folder.clone());
    let decision = offer(
        &inbox,
        &[
            dto("1", "notes.txt", 5),
            dto("2", "Photos/a.jpg", 1),
            dto("3", "Photos/2026/b.jpg", 1),
            dto("4", "../../up.txt", 2),
        ],
    );
    let Event::Offer(offered) = next(&told) else {
        panic!("a person is asked first");
    };
    assert_eq!(offered.from, "Pixel 8");
    assert_eq!(offered.bytes, 9);
    assert_eq!(offered.files[0], ("../../up.txt".into(), 2));

    inbox.answer(&offered.id, true);
    let Ok(PrepareUploadDecisionV2::Accept(ids)) = decision.blocking_recv() else {
        panic!("accepting takes the files");
    };
    assert_eq!(ids.len(), 4);
    assert!(matches!(
        next(&told),
        Event::Started {
            files: 4,
            bytes: 9,
            ..
        }
    ));

    upload(&inbox, "1", Some(b"yours"));
    upload(&inbox, "2", Some(b"a"));
    upload(&inbox, "3", Some(b"b"));
    upload(&inbox, "4", Some(b"up"));
    inbox.handle(ServerEventV2::SessionEnd {
        session_id: "session-1".into(),
        reason: SessionEndReasonV2::Finished,
    });
    let Event::Ended { ended, .. } = next(&told) else {
        panic!("the transfer ends");
    };

    assert_eq!(std::fs::read(folder.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(std::fs::read(folder.join("notes 2.txt")).unwrap(), b"yours");
    assert_eq!(std::fs::read(folder.join("Photos 2/a.jpg")).unwrap(), b"a");
    assert_eq!(
        std::fs::read(folder.join("Photos 2/2026/b.jpg")).unwrap(),
        b"b"
    );
    assert_eq!(std::fs::read(folder.join("up.txt")).unwrap(), b"up");
    assert!(!dir.path().parent().unwrap().join("up.txt").exists());
    let mut received = ended.received.clone();
    received.sort();
    assert_eq!(
        received,
        [
            folder.join("Photos 2"),
            folder.join("notes 2.txt"),
            folder.join("up.txt")
        ]
    );
    assert!(ended.failed.is_empty() && !ended.cancelled);
    let hidden: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.starts_with('.').then_some(name)
        })
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");
    assert!(quarantined(&folder.join("Photos 2/a.jpg")));
}

fn quarantined(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut value = [0u8; 128];
    // SAFETY: both strings are NUL-terminated and the buffer is its length.
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    let value = &value[..usize::try_from(read).unwrap_or(0)];
    value.starts_with(b"0081;") && value.ends_with(b";Monodrive;")
}

#[test]
fn a_declined_offer_takes_nothing_and_a_failed_file_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let (inbox, told) = inbox(dir.path().to_path_buf());
    let decision = offer(&inbox, &[dto("1", "a.txt", 1)]);
    let Event::Offer(offered) = next(&told) else {
        panic!("a person is asked first");
    };
    inbox.answer(&offered.id, false);
    assert!(matches!(
        decision.blocking_recv(),
        Ok(PrepareUploadDecisionV2::Decline)
    ));

    let decision = offer(&inbox, &[dto("1", "a.txt", 1), dto("2", "b.txt", 1)]);
    let Event::Offer(offered) = next(&told) else {
        panic!("a person is asked again");
    };
    drop(decision);
    inbox.answer(&offered.id, true);
    assert!(matches!(next(&told), Event::Started { .. }));
    assert!(matches!(
        next(&told),
        Event::Ended {
            ended: Ended {
                cancelled: true,
                ..
            },
            ..
        }
    ));

    let decision = offer(&inbox, &[dto("1", "a.txt", 1), dto("2", "b.txt", 1)]);
    let Event::Offer(offered) = next(&told) else {
        panic!("a person is asked again");
    };
    inbox.answer(&offered.id, true);
    assert!(decision.blocking_recv().is_ok());
    assert!(matches!(next(&told), Event::Started { .. }));
    upload(&inbox, "1", Some(b"a"));
    upload(&inbox, "2", None);
    inbox.handle(ServerEventV2::SessionEnd {
        session_id: "session-1".into(),
        reason: SessionEndReasonV2::Finished,
    });
    let Event::Ended { ended, .. } = next(&told) else {
        panic!("the transfer ends");
    };
    assert_eq!(ended.received, [dir.path().join("a.txt")]);
    assert_eq!(
        ended.failed,
        [("b.txt".to_string(), "Connection reset".to_string())]
    );
    assert!(!dir.path().join("b.txt").exists());
}

#[test]
fn an_accepted_message_is_its_text_and_writes_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let (inbox, told) = inbox(dir.path().to_path_buf());
    let message = FileDto {
        file_type: "text/plain".into(),
        preview: Some("the wifi password".into()),
        ..dto("1", "6d1c.txt", 17)
    };
    let decision = offer(&inbox, &[message]);
    let Event::Offer(offered) = next(&told) else {
        panic!("a person is asked first");
    };
    assert_eq!(offered.text.as_deref(), Some("the wifi password"));

    inbox.answer(&offered.id, true);
    let Ok(PrepareUploadDecisionV2::Accept(ids)) = decision.blocking_recv() else {
        panic!("accepting a message answers it");
    };
    assert!(ids.is_empty(), "nothing is uploaded");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn messages_are_kept_newest_first_privately_until_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("localsend/messages.json");
    let messages = Messages::at(path.clone());
    assert_eq!(messages.list().unwrap(), []);
    messages.add("Pixel".into(), "one".into(), 10).unwrap();
    // A clock that went back still lists the newest first.
    let kept = messages.add("iPad".into(), "two".into(), 5).unwrap();
    assert_eq!(
        kept.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        ["two", "one"]
    );
    assert_eq!(messages.list().unwrap(), kept);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    let kept = messages.forget(kept[1].at).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].from, "iPad");
    assert_eq!(messages.clear().unwrap(), []);
    assert_eq!(messages.list().unwrap(), []);
}

#[test]
fn a_save_folder_that_is_gone_is_made_again() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Downloads");
    let (inbox, told) = inbox(folder.clone());
    let decision = offer(&inbox, &[dto("1", "a.txt", 1)]);
    let Event::Offer(offered) = next(&told) else {
        panic!("a person is asked first");
    };
    inbox.answer(&offered.id, true);
    assert!(decision.blocking_recv().is_ok());
    assert!(matches!(next(&told), Event::Started { .. }));
    upload(&inbox, "1", Some(b"a"));
    inbox.handle(ServerEventV2::SessionEnd {
        session_id: "session-1".into(),
        reason: SessionEndReasonV2::Finished,
    });
    let Event::Ended { ended, .. } = next(&told) else {
        panic!("the transfer ends");
    };
    assert_eq!(ended.failed, []);
    assert_eq!(std::fs::read(folder.join("a.txt")).unwrap(), b"a");
}

#[test]
fn a_sender_that_leaves_after_the_answer_ends_the_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let (inbox, told) = inbox(dir.path().to_path_buf());
    let decision = offer(&inbox, &[dto("1", "a.txt", 1)]);
    let Event::Offer(offered) = next(&told) else {
        panic!("a person is asked first");
    };
    inbox.answer(&offered.id, true);
    assert!(decision.blocking_recv().is_ok());
    assert!(matches!(next(&told), Event::Started { .. }));
    inbox.handle(ServerEventV2::PrepareUploadAborted {
        session_id: "session-1".into(),
    });
    let Event::Ended { ended, .. } = next(&told) else {
        panic!("the transfer ends");
    };
    assert!(ended.cancelled && ended.received.is_empty());
}

#[test]
fn a_plain_http_claim_never_replaces_a_device_met_over_https() {
    let known = Device {
        alias: "Pixel 8".into(),
        fingerprint: "AB".into(),
        kind: Kind::Phone,
        model: None,
        host: "192.168.1.20".into(),
        port: PORT,
        https: true,
    };
    let impostor = Device {
        host: "192.168.1.66".into(),
        https: false,
        ..known.clone()
    };
    assert_eq!(super::merged(&known, impostor), None);
    let moved = Device {
        host: "192.168.1.21".into(),
        ..known.clone()
    };
    assert_eq!(super::merged(&known, moved.clone()), Some(moved));
    let plain = Device {
        https: false,
        ..known.clone()
    };
    let upgraded = Device {
        host: "fe80::1%4".into(),
        port: 53318,
        ..known.clone()
    };
    assert_eq!(super::merged(&plain, upgraded.clone()), Some(upgraded));
}

/// What a loopback receiver was offered and sent.
#[derive(Default)]
struct Got {
    offered: Vec<String>,
    /// Each offered file's type and preview, by name.
    shown: HashMap<String, (String, Option<String>)>,
    pin: Option<String>,
    uploads: HashMap<String, Vec<u8>>,
}

/// A receiver on loopback, over plain HTTP, that answers `prepare-upload`
/// with `status`, taking every file on 200.
fn receiver(status: u16) -> (Device, Arc<Mutex<Got>>) {
    let got = Arc::new(Mutex::new(Got::default()));
    let listener = runtime()
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = got.clone();
    runtime().spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, status, shared.clone()));
        }
    });
    let device = Device {
        alias: "Pixel 8".into(),
        fingerprint: "PEER".into(),
        kind: Kind::Phone,
        model: None,
        host: "127.0.0.1".into(),
        port,
        https: false,
    };
    (device, got)
}

async fn serve(stream: tokio::net::TcpStream, status: u16, got: Arc<Mutex<Got>>) {
    let mut stream = BufReader::new(stream);
    let mut names: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let target = line.split(' ').nth(1).unwrap_or_default().to_owned();
        let (mut length, mut chunked) = (0, false);
        loop {
            let mut header = String::new();
            stream.read_line(&mut header).await.unwrap();
            let header = header.trim_end().to_ascii_lowercase();
            if header.is_empty() {
                break;
            }
            if let Some(value) = header.strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            chunked |= header == "transfer-encoding: chunked";
        }
        let mut body = Vec::new();
        if chunked {
            loop {
                let mut size = String::new();
                stream.read_line(&mut size).await.unwrap();
                let size = usize::from_str_radix(size.trim(), 16).unwrap();
                let mut chunk = vec![0; size + 2];
                stream.read_exact(&mut chunk).await.unwrap();
                if size == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..size]);
            }
        } else {
            body.resize(length, 0);
            stream.read_exact(&mut body).await.unwrap();
        }
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let param = |name: &str| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                .map(str::to_owned)
        };
        let (code, reply) = match path {
            "/api/localsend/v2/prepare-upload" => {
                let request: PrepareUploadRequestDtoV2 = serde_json::from_slice(&body).unwrap();
                let mut got = got.lock().unwrap();
                got.pin = param("pin");
                got.offered = request
                    .files
                    .values()
                    .map(|f| f.file_name.clone())
                    .collect();
                got.offered.sort();
                got.shown = request
                    .files
                    .values()
                    .map(|f| {
                        (
                            f.file_name.clone(),
                            (f.file_type.clone(), f.preview.clone()),
                        )
                    })
                    .collect();
                names = request
                    .files
                    .iter()
                    .map(|(id, f)| (id.clone(), f.file_name.clone()))
                    .collect();
                let reply = PrepareUploadResponseDtoV2 {
                    session_id: "s".into(),
                    files: names
                        .keys()
                        .map(|id| (id.clone(), format!("t{id}")))
                        .collect(),
                };
                let reply = match status {
                    204 => String::new(),
                    _ => serde_json::to_string(&reply).unwrap(),
                };
                (status, reply)
            }
            "/api/localsend/v2/upload" => {
                let id = param("fileId").unwrap();
                assert_eq!(param("token"), Some(format!("t{id}")));
                got.lock().unwrap().uploads.insert(names[&id].clone(), body);
                (200, String::new())
            }
            _ => (200, String::new()),
        };
        let head = format!(
            "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            reply.len()
        );
        let stream = stream.get_mut();
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(reply.as_bytes()).await.unwrap();
    }
}

fn me() -> Identity {
    let dir = tempfile::tempdir().unwrap();
    Identity::load(dir.path(), "Mac".into()).unwrap()
}

#[test]
fn a_send_offers_each_file_a_folder_by_its_path_and_uploads_them() {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("Photos");
    std::fs::create_dir_all(photos.join("2026")).unwrap();
    std::fs::write(photos.join("a.jpg"), "aa").unwrap();
    std::fs::write(photos.join("2026/b.jpg"), "bbb").unwrap();
    std::fs::write(photos.join(".DS_Store"), "junk").unwrap();
    std::os::unix::fs::symlink("/etc/hosts", photos.join("link")).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "n").unwrap();
    let (device, got) = receiver(200);
    let said = Arc::new(Mutex::new(Vec::new()));
    let saying = said.clone();

    let sent = send(
        &me(),
        &device,
        &Outgoing::Files(vec![photos, dir.path().join("notes.txt")]),
        None,
        move |progress| saying.lock().unwrap().push(progress),
        &Cancel::new(),
    )
    .unwrap();

    assert_eq!(
        sent,
        Sent {
            files: 3,
            bytes: 6,
            declined: 0
        }
    );
    let got = got.lock().unwrap();
    assert_eq!(
        got.offered,
        ["Photos/2026/b.jpg", "Photos/a.jpg", "notes.txt"]
    );
    assert_eq!(got.uploads["Photos/2026/b.jpg"], b"bbb");
    assert_eq!(got.uploads["Photos/a.jpg"], b"aa");
    assert_eq!(got.uploads["notes.txt"], b"n");
    let said = said.lock().unwrap();
    assert_eq!(said.first(), Some(&Progress::Waiting));
    assert_eq!(said.last(), Some(&Progress::Sending { sent: 6, total: 6 }));
}

#[test]
fn a_send_says_what_the_receiver_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "a").unwrap();
    let identity = me();
    let refused = |status, pin: Option<&str>| {
        let (device, got) = receiver(status);
        let result = send(
            &identity,
            &device,
            &Outgoing::Files(vec![file.clone()]),
            pin,
            |_| {},
            &Cancel::new(),
        );
        (result, got.lock().unwrap().pin.clone())
    };
    assert_eq!(refused(403, None).0, Err(SendError::Declined));
    assert_eq!(refused(409, None).0, Err(SendError::Busy));
    assert_eq!(refused(401, None).0, Err(SendError::Pin { wrong: false }));
    assert_eq!(
        refused(401, Some("123456")),
        (Err(SendError::Pin { wrong: true }), Some("123456".into()))
    );
    assert_eq!(refused(429, Some("1")).0, Err(SendError::TooManyTries));
    let (device, _) = receiver(200);
    assert_eq!(
        send(
            &identity,
            &device,
            &Outgoing::Files(vec![dir.path().join("gone")]),
            None,
            |_| {},
            &Cancel::new()
        ),
        Err(SendError::NotFound(dir.path().join("gone")))
    );
}

#[test]
fn text_goes_as_a_message_that_a_receiver_reads_or_takes_as_a_file() {
    let identity = me();
    let text = "wifi: hunter2\nsee you at 6";
    let message = Outgoing::Text(text.into());
    let sent = Sent {
        files: 1,
        bytes: text.len() as u64,
        declined: 0,
    };

    let (read, got) = receiver(204);
    assert_eq!(
        send(&identity, &read, &message, None, |_| {}, &Cancel::new()),
        Ok(sent.clone())
    );
    let got = got.lock().unwrap();
    assert_eq!(
        got.shown.values().collect::<Vec<_>>(),
        [&("text/plain".to_string(), Some(text.to_string()))]
    );
    assert!(got.uploads.is_empty());

    let (saves, got) = receiver(200);
    assert_eq!(
        send(&identity, &saves, &message, None, |_| {}, &Cancel::new()),
        Ok(sent)
    );
    assert_eq!(
        got.lock().unwrap().uploads.values().collect::<Vec<_>>(),
        [&text.as_bytes().to_vec()]
    );

    let (declines, _) = receiver(403);
    assert_eq!(
        send(&identity, &declines, &message, None, |_| {}, &Cancel::new()),
        Err(SendError::Declined)
    );
    assert_eq!(
        send(
            &identity,
            &declines,
            &Outgoing::Text(String::new()),
            None,
            |_| {},
            &Cancel::new()
        ),
        Err(SendError::Nothing)
    );
}

#[test]
fn bytes_go_as_a_file_by_their_name_and_type() {
    let identity = me();
    let picture = Outgoing::Bytes {
        name: "Clipboard.png".into(),
        bytes: b"png".to_vec(),
    };

    let (saves, got) = receiver(200);
    assert_eq!(
        send(&identity, &saves, &picture, None, |_| {}, &Cancel::new()),
        Ok(Sent {
            files: 1,
            bytes: 3,
            declined: 0
        })
    );
    let got = got.lock().unwrap();
    assert_eq!(got.shown["Clipboard.png"], ("image/png".to_string(), None));
    assert_eq!(got.uploads["Clipboard.png"], b"png");

    let (takes_nothing, _) = receiver(204);
    assert_eq!(
        send(
            &identity,
            &takes_nothing,
            &picture,
            None,
            |_| {},
            &Cancel::new()
        ),
        Err(SendError::Declined)
    );
}
