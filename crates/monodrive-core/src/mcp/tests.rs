//! Contract tests: each drives `Server::handle` with JSON-RPC over temp
//! folders and looks only at the replies, so they hold however the
//! services underneath change.

use super::contract::{self, Profile};
use super::server::Server;
use super::tools::TOOLS;
use crate::ops::{self, Origin};
use crate::places;
use crate::remote::{self, Connections, RemoteError, Source};
use crate::user_commands::{self, Cwd, Definition, Ended, Run, Sources};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

type Serving = JoinHandle<std::io::Result<()>>;

fn places(dir: &Path) -> places::Store {
    places::Store {
        places: dir.join("places.json"),
        recents: dir.join("recents.json"),
    }
}

fn paths(dir: &Path) -> ops::Paths {
    ops::Paths {
        state: dir.join("state"),
        settings: dir.join("settings.json"),
        trash: dir.join("Trash"),
    }
}

/// A server whose changes go nowhere: read-only tools never connect.
fn server() -> Server {
    let nowhere = Path::new("/nonexistent/monodrive-test");
    Server::new(
        ops::Client::new(paths(nowhere), None),
        places(nowhere),
        Profile::All,
    )
}

/// An owner on its own thread, and a server that talks to it. Drop the
/// server, then join the owner: it runs its jobs to the end first.
fn owned(dir: &Path) -> (Serving, Server) {
    let owner = ops::Owner::bind(paths(dir)).unwrap();
    let serving = std::thread::spawn(move || owner.serve());
    let server = Server::new(
        ops::Client::new(paths(dir), None),
        places(dir),
        Profile::All,
    );
    (serving, server)
}

fn stop(serving: Serving, server: Server) {
    drop(server);
    serving.join().unwrap().unwrap();
}

/// A `tools/call`'s whole reply.
fn rpc(server: &mut Server, name: &str, arguments: Value) -> Value {
    server
        .handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
        .expect("a request gets a reply")
}

/// A `tools/call`'s result.
fn call(server: &mut Server, name: &str, arguments: Value) -> Value {
    let reply = rpc(server, name, arguments);
    assert!(reply.get("error").is_none(), "{reply}");
    reply["result"].clone()
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap_or_default()
}

fn failed(result: &Value) -> bool {
    result["isError"] == true
}

/// The error code of a failed call.
fn code(result: &Value) -> &str {
    assert!(failed(result), "{result}");
    result["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
}

fn data(result: &Value) -> &Value {
    assert!(!failed(result), "{result}");
    &result["structuredContent"]
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

fn settings(dir: &Path, json: &str) {
    std::fs::write(dir.join("settings.json"), json).unwrap();
}

fn listed(server: &mut Server) -> Vec<Value> {
    let reply = server
        .handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .unwrap();
    reply["result"]["tools"].as_array().unwrap().clone()
}

#[test]
fn initialize_reports_the_contract_version() {
    let reply = server()
        .handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "test"}},
        }))
        .unwrap();
    let result = &reply["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(
        result["capabilities"]["experimental"]["monodrive/contract"]["version"],
        contract::VERSION
    );
}

#[test]
fn notifications_get_no_reply() {
    let reply = server().handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    assert_eq!(reply, None);
}

#[test]
fn the_core_profile_lists_file_tools_only_and_stays_cheap() {
    let core_paths = Path::new("/nonexistent/monodrive-test");
    let mut core = Server::new(
        ops::Client::new(paths(core_paths), None),
        places(core_paths),
        Profile::Core,
    );
    let tools = listed(&mut core);
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        names.contains(&"copy") && names.contains(&"job_status"),
        "{names:?}"
    );
    assert!(
        !names.contains(&"pin") && !names.contains(&"run_command"),
        "{names:?}"
    );
    // What a model is given: names, descriptions and input schemas.
    let given: usize = tools
        .iter()
        .map(|tool| {
            tool["name"].to_string().len()
                + tool["description"].to_string().len()
                + tool["inputSchema"].to_string().len()
        })
        .sum();
    assert!(
        given < 16 << 10,
        "core tools/list gives a model {given} bytes"
    );
    // A tool outside the profile is as unknown as one that doesn't exist.
    for name in ["pin", "rm_rf"] {
        assert_eq!(rpc(&mut core, name, json!({}))["error"]["code"], -32602);
    }
    assert_eq!(listed(&mut server()).len(), TOOLS.len());
}

#[test]
fn every_tool_has_strict_inputs_open_outputs_and_hints() {
    for tool in &TOOLS {
        let input = tool.input_schema();
        assert_eq!(input["type"], "object", "{}", tool.name);
        assert_eq!(input["additionalProperties"], false, "{}", tool.name);
        let output = tool.output_schema();
        assert_eq!(output["type"], "object", "{}", tool.name);
        assert!(output.get("required").is_none(), "{}", tool.name);
        assert!(
            !output
                .to_string()
                .contains("\"additionalProperties\":false"),
            "{}",
            tool.name
        );
        assert!(output["properties"]["error"].is_object(), "{}", tool.name);
        if tool.hints.read_only {
            assert!(!tool.hints.destructive, "{}", tool.name);
        }
    }
}

#[test]
fn bad_arguments_are_tool_errors_the_agent_can_fix() {
    let mut server = server();
    for (name, arguments) in [
        ("stat", json!({"paths": "/tmp"})),
        ("folder_size", json!({})),
        (
            "folder_size",
            json!({"path": "/tmp", "timeout_seconds": -1}),
        ),
        (
            "copy",
            json!({"paths": ["/a"], "into": "/b", "conflict": "merge"}),
        ),
        (
            "rename",
            json!({"paths": ["/a"], "rule": {"type": "regex", "find": "x"}}),
        ),
        ("list_dir", json!({"path": "/tmp", "show_hiden": true})),
        ("cancel_job", json!({})),
    ] {
        let result = call(&mut server, name, arguments);
        assert_eq!(code(&result), "invalid_argument", "{name}: {result}");
        assert!(
            text(&result).ends_with("(error: invalid_argument)"),
            "{result}"
        );
    }
    // MCP lets a call without parameters leave out `arguments`.
    let bare = server
        .handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "index_status"}}))
        .unwrap();
    assert_eq!(bare["result"]["isError"], false, "{bare}");
    assert!(server.started_index().is_none());
}

#[test]
fn a_bad_filter_is_a_tool_error_that_never_starts_the_index() {
    let mut server = server();
    for (name, arguments, error) in [
        (
            "find_files",
            json!({"query": "cat kind:bogus"}),
            "kind:bogus: kinds are image, video",
        ),
        (
            "grep",
            json!({"pattern": "TODO", "filters": "size:big"}),
            "size:big: give a size",
        ),
    ] {
        let result = call(&mut server, name, arguments);
        assert_eq!(code(&result), "invalid_argument");
        assert!(text(&result).starts_with(error), "{result}");
    }
    assert!(server.started_index().is_none());
}

#[test]
fn index_status_reports_without_starting_the_index() {
    let mut server = server();
    let result = call(&mut server, "index_status", json!({}));
    assert!(
        text(&result).starts_with("Saved index") || text(&result).starts_with("No index yet"),
        "{result}"
    );
    assert_eq!(data(&result)["state"], "not_started");
    assert!(server.started_index().is_none());
}

#[test]
fn list_dir_lists_folders_first_a_page_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.toml"), "x").unwrap();
    std::fs::write(dir.path().join("b.txt"), "").unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let mut server = server();
    let result = call(&mut server, "list_dir", json!({"path": dir.path()}));
    assert_eq!(
        text(&result).lines().collect::<Vec<_>>(),
        ["src/", "a.toml", "b.txt"]
    );
    let items = &data(&result)["items"];
    assert_eq!(items[0]["kind"], "folder");
    assert_eq!(items[1]["size"], 1);
    assert_eq!(data(&result)["truncated"], false);

    let first = call(
        &mut server,
        "list_dir",
        json!({"path": dir.path(), "limit": 2}),
    );
    assert_eq!(data(&first)["truncated"], true);
    let cursor = data(&first)["next_cursor"].clone();
    let rest = call(
        &mut server,
        "list_dir",
        json!({"path": dir.path(), "limit": 2, "cursor": cursor}),
    );
    assert_eq!(text(&rest), "b.txt\n");
    assert!(data(&rest).get("next_cursor").is_none());
    assert!(server.started_index().is_none());
}

#[test]
fn stat_describes_each_path_and_fails_only_when_none_is_there() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (file, folder, link) = (
        dir.path().join("notes.txt"),
        dir.path().join("docs"),
        dir.path().join("shortcut"),
    );
    std::fs::write(&file, "hello").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), "0123456789").unwrap();
    std::os::unix::fs::symlink("docs", &link).unwrap();
    let missing = dir.path().join("missing");
    let mut server = server();

    let result = call(&mut server, "stat", json!({"paths": [file, link, missing]}));
    let blocks: Vec<Vec<&str>> = text(&result)
        .split("\n\n")
        .map(|block| block.lines().collect())
        .collect();
    let notes = &blocks[0];
    assert_eq!(
        notes[..2],
        [shown(&file), "kind: Plain Text Document".into()]
    );
    assert!(notes[2].starts_with("size: 5 bytes ("), "{notes:?}");
    assert_eq!(notes[3], format!("where: {}", shown(dir.path())));
    assert!(notes[5].starts_with("created: 20") && notes[5].ends_with('Z'));
    assert!(
        notes[7].starts_with("permissions: 644, owner "),
        "{notes:?}"
    );
    assert_eq!(blocks[1][0], format!("{}@", shown(&link)));
    assert_eq!(blocks[1].last(), Some(&"link to: docs"));
    assert_eq!(
        blocks[2],
        [format!(
            "{}: No such file or directory (os error 2)",
            shown(&missing)
        )]
    );
    let items = &data(&result)["items"];
    assert_eq!(items[0]["size"], 5);
    assert_eq!(items[0]["mode"], "644");
    assert_eq!(items[1]["link"], "docs");
    assert!(items[2]["error"].is_string());
    assert_eq!(
        contract::utc(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_314_588)),
        "2026-10-29T22:56:28Z"
    );
    // Any date the attribute holds prints, however odd.
    crate::info::tests::record_opened(&file, i64::MIN, 0);
    let result = call(&mut server, "stat", json!({"paths": [file]}));
    assert!(
        text(&result)
            .lines()
            .any(|line| line == "opened: -9223372036854775808 seconds from 1970-01-01T00:00:00Z"),
        "{result}"
    );
    let result = call(&mut server, "stat", json!({"paths": [missing]}));
    assert_eq!(code(&result), "not_found");

    let result = call(&mut server, "folder_size", json!({"path": folder}));
    assert_eq!(data(&result)["bytes"], 10);
    assert_eq!(data(&result)["partial"], false);
    let result = call(
        &mut server,
        "folder_size",
        json!({"path": folder, "timeout_seconds": 0}),
    );
    assert_eq!(data(&result)["partial"], true);
    assert!(
        text(&result).ends_with("pass a larger timeout_seconds)\n"),
        "{result}"
    );
}

#[test]
fn read_file_reads_a_range_and_says_when_its_binary() {
    let dir = tempfile::tempdir().unwrap();
    let (text_file, binary) = (dir.path().join("a.txt"), dir.path().join("b"));
    std::fs::write(&text_file, "hello local").unwrap();
    std::fs::write(&binary, [0, 1, 2]).unwrap();
    let mut server = server();
    let result = call(
        &mut server,
        "read_file",
        json!({"path": text_file, "offset": 6, "limit": 3}),
    );
    assert_eq!(
        text(&result),
        format!("{}: bytes 6 to 9 of 11\nloc", shown(&text_file))
    );
    assert_eq!(data(&result)["next_offset"], 9);
    let result = call(&mut server, "read_file", json!({"path": binary}));
    assert!(data(&result).get("text").is_none());
    assert!(text(&result).ends_with("(binary, not shown; copy takes it whole)\n"));
}

#[test]
fn thumbnail_replies_with_an_image_or_says_why_not() {
    use base64::Engine as _;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut server = server();
    let result = call(
        &mut server,
        "thumbnail",
        json!({"path": fixtures.join("rotated.mp4"), "side": 64}),
    );
    let content = &result["content"][0];
    assert_eq!(content["type"], "image", "{result}");
    assert_eq!(content["mimeType"], "image/jpeg");
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(content["data"].as_str().unwrap())
        .unwrap();
    assert!(jpeg.starts_with(&[0xff, 0xd8]));
    let result = call(
        &mut server,
        "thumbnail",
        json!({"path": fixtures.join("clip.mkv")}),
    );
    assert!(failed(&result));
    assert_eq!(result["content"][0]["type"], "text");
}

#[test]
fn placeholders_say_so_and_a_local_file_needs_no_download() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "hello").unwrap();
    let mut server = server();
    let result = call(&mut server, "download", json!({"paths": [file]}));
    assert_eq!(
        text(&result),
        format!("{}: already downloaded\n", file.display())
    );
    assert_eq!(data(&result)["items"][0]["state"], "unneeded");
    let result = call(&mut server, "remove_download", json!({"paths": [file]}));
    assert!(
        text(&result).starts_with(&format!(
            "{} isn't in iCloud Drive or a File Provider folder.\n",
            file.display()
        )),
        "{result}"
    );
    assert!(file.is_file());
}

#[test]
fn change_tools_reply_with_resulting_paths_and_share_undo_with_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (serving, mut server) = owned(dir.path());
    let (made, renamed) = (dir.path().join("made"), dir.path().join("renamed"));
    let result = call(&mut server, "create_folder", json!({"path": made}));
    assert_eq!(text(&result), format!("{}\n", shown(&made)));
    assert_eq!(data(&result)["items"][0]["to"], shown(&made));
    let result = call(
        &mut server,
        "rename",
        json!({"paths": [made], "name": "renamed"}),
    );
    assert_eq!(text(&result), format!("{}\n", shown(&renamed)));
    let result = call(&mut server, "undo", json!({}));
    assert_eq!(text(&result), format!("Undo Rename\n{}\n", shown(&made)));
    assert_eq!(data(&result)["title"], "Undo Rename");
    assert!(made.is_dir() && !renamed.exists());

    let result = call(&mut server, "create_file", json!({"path": "notes.md"}));
    assert_eq!(code(&result), "invalid_argument");
    assert!(text(&result).contains("absolute"), "{result}");
    let result = call(&mut server, "create_folder", json!({"path": made}));
    assert_eq!(code(&result), "name_taken");

    settings(dir.path(), r#"{"agents_change_files": false}"#);
    for (name, arguments) in [
        ("create_file", json!({"path": dir.path().join("off.md")})),
        ("eject", json!({"path": "/"})),
        ("connect", json!({"address": "smb://nas/media"})),
        ("download", json!({"paths": [&made, &renamed]})),
    ] {
        let result = call(&mut server, name, arguments);
        assert_eq!(code(&result), "agents_disabled", "{name}: {result}");
        assert_eq!(text(&result).lines().count(), 2, "{result}");
    }
    // With changes off or on, no reply repeats a password.
    for name in ["connect", "disconnect", "forget_server"] {
        let key = if name == "connect" {
            "address"
        } else {
            "server"
        };
        let result = call(&mut server, name, json!({key: "smb://me:secret@nas/media"}));
        assert!(
            text(&result).starts_with("Leave the password out"),
            "{result}"
        );
        assert!(!result.to_string().contains("secret"), "{result}");
    }
    stop(serving, server);
}

#[test]
fn a_repeated_request_id_changes_nothing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let (serving, mut server) = owned(dir.path());
    let made = dir.path().join("once");
    let once = json!({"path": made, "request_id": "test-once"});
    let first = call(&mut server, "create_folder", once.clone());
    // A lost reply's retry gets the first outcome, not "already exists".
    let again = call(&mut server, "create_folder", once);
    assert_eq!(data(&again), data(&first));
    let other = call(
        &mut server,
        "create_file",
        json!({"path": dir.path().join("x"), "request_id": "test-once"}),
    );
    assert_eq!(code(&other), "invalid_argument", "{other}");
    stop(serving, server);
}

#[test]
fn rename_dry_runs_then_runs_only_the_plan_it_showed() {
    let dir = tempfile::tempdir().unwrap();
    let (serving, mut server) = owned(dir.path());
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    for name in ["IMG_1.jpg", "IMG_2.jpg", "Trip 2.jpg", "x"] {
        std::fs::write(files.join(name), "").unwrap();
    }
    let batch = ["IMG_1.jpg", "IMG_2.jpg", "x"].map(|name| files.join(name));
    let rule = json!({"type": "format", "name": "Trip"});
    let at = |name: &str| shown(&files.join(name));
    let result = call(
        &mut server,
        "rename",
        json!({"paths": batch, "rule": rule, "dry_run": true}),
    );
    let token = data(&result)["token"].as_str().unwrap().to_owned();
    assert_eq!(
        text(&result),
        format!(
            "{} → Trip 1.jpg\n{} → Trip 2.jpg (collision: Trip 2.jpg already exists.)\n{} → Trip 3\ntoken: {token}\n",
            at("IMG_1.jpg"),
            at("IMG_2.jpg"),
            at("x"),
        )
    );
    assert_eq!(data(&result)["plan"][1]["status"], "collision");
    assert!(
        batch.iter().all(|path| path.exists()),
        "a dry run changes nothing"
    );

    let result = call(&mut server, "rename", json!({"paths": batch, "rule": rule}));
    assert!(
        text(&result).starts_with("Nothing was renamed:\n"),
        "{result}"
    );
    let result = call(
        &mut server,
        "rename",
        json!({"paths": batch, "rule": rule, "skip_problems": true, "dry_run": true}),
    );
    let token = data(&result)["token"].as_str().unwrap().to_owned();
    let result = call(&mut server, "rename", json!({"confirm": token}));
    assert_eq!(
        text(&result),
        format!("{}\n{}\n", at("Trip 1.jpg"), at("Trip 3"))
    );
    let result = call(&mut server, "rename", json!({"confirm": token}));
    assert_eq!(code(&result), "changed_since_preview", "a plan runs once");
    let result = call(&mut server, "undo", json!({}));
    assert!(text(&result).starts_with("Undo Batch Rename\n"), "{result}");
    assert!(batch.iter().all(|path| path.exists()));

    // A plan the files no longer match is refused whole.
    let result = call(
        &mut server,
        "rename",
        json!({"paths": [&batch[0]], "name": "Renamed.jpg", "dry_run": true}),
    );
    let token = data(&result)["token"].as_str().unwrap().to_owned();
    std::fs::write(&batch[0], "changed").unwrap();
    let result = call(&mut server, "rename", json!({"confirm": token}));
    assert!(failed(&result), "{result}");
    assert!(batch[0].exists());
    stop(serving, server);
}

#[test]
fn suggested_names_feed_a_names_rule_that_must_match_the_paths() {
    let dir = tempfile::tempdir().unwrap();
    let (scan, photo) = (dir.path().join("scan0012.txt"), dir.path().join("a.jpg"));
    std::fs::write(&scan, "Invoice 4471\nDue on receipt.\n").unwrap();
    std::fs::write(&photo, "x").unwrap();
    let mut server = server();
    let result = call(
        &mut server,
        "suggest_names",
        json!({"paths": [scan, photo]}),
    );
    let lines: Vec<&str> = text(&result).lines().collect();
    assert_eq!(lines[0], format!("{} → invoice-4471.txt", scan.display()));
    assert!(lines[1].contains("(no suggestion: "), "{lines:?}");
    assert_eq!(data(&result)["items"][0]["name"], "invoice-4471.txt");
    let result = call(
        &mut server,
        "rename",
        json!({"paths": [scan, photo], "rule": {"type": "names", "names": ["only one"]}, "dry_run": true}),
    );
    assert!(
        text(&result).contains("1 names were given for 2 items"),
        "{result}"
    );
}

#[test]
fn organize_dry_runs_then_runs_only_what_it_showed() {
    let dir = tempfile::tempdir().unwrap();
    let (serving, mut server) = owned(dir.path());
    let folder = dir.path().canonicalize().unwrap().join("messy");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a.png"), "a").unwrap();
    let result = call(&mut server, "organize", json!({"path": folder}));
    assert_eq!(
        code(&result),
        "invalid_argument",
        "a run needs a dry run first"
    );
    let result = call(
        &mut server,
        "organize",
        json!({"path": folder, "dry_run": true}),
    );
    assert!(text(&result).contains("Images/ ("), "{result}");
    assert!(folder.join("a.png").exists(), "a dry run changes nothing");
    let token = data(&result)["token"].as_str().unwrap().to_owned();
    let result = call(&mut server, "organize", json!({"confirm": "nope"}));
    assert_eq!(code(&result), "changed_since_preview");
    let result = call(&mut server, "organize", json!({"confirm": token}));
    assert!(!failed(&result), "{result}");
    assert!(folder.join("Images/a.png").exists());
    stop(serving, server);
}

#[test]
fn edit_image_writes_beside_the_original_and_a_bad_step_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (serving, mut server) = owned(dir.path());
    let folder = dir.path().canonicalize().unwrap().join("pics");
    std::fs::create_dir(&folder).unwrap();
    let photo = folder.join("a.png");
    image::RgbImage::new(40, 20).save(&photo).unwrap();
    let result = call(
        &mut server,
        "edit_image",
        json!({"path": photo, "steps": [{"type": "rotate", "quarter_turns": 1}]}),
    );
    let edited = folder.join("a edited.png");
    assert_eq!(text(&result).trim(), shown(&edited));
    assert_eq!(image::open(&edited).unwrap().width(), 20);
    assert_eq!(image::open(&photo).unwrap().width(), 40);
    let result = call(
        &mut server,
        "edit_image",
        json!({"path": photo, "steps": [{"type": "crop", "rect": {"x": 500, "y": 500, "width": 5, "height": 5}}]}),
    );
    assert!(failed(&result));
    let names: Vec<_> = std::fs::read_dir(&folder).unwrap().flatten().collect();
    assert_eq!(names.len(), 2, "not even a hidden file stays");
    stop(serving, server);
}

#[test]
fn copy_replies_with_a_job_that_waits_continues_and_reports_how_it_copied() {
    let dir = tempfile::tempdir().unwrap();
    let (file, into) = (dir.path().join("a.txt"), dir.path().join("into"));
    std::fs::write(&file, "a").unwrap();
    std::fs::create_dir(&into).unwrap();
    let (serving, mut server) = owned(dir.path());
    let copy = json!({"paths": [file], "into": into, "wait_seconds": 60});
    let result = call(&mut server, "copy", copy.clone());
    let job = data(&result);
    assert_eq!(job["state"], "completed", "{result}");
    assert_eq!(job["kind"], "copy");
    assert_eq!(job["items"][0]["to"], shown(&into.join("a.txt")));
    // A temp folder and its copy are on one APFS volume.
    assert_eq!(job["items"][0]["method"], "clone");
    let id = job["id"].as_str().unwrap().to_owned();
    let result = call(&mut server, "job_status", json!({"job": id}));
    assert!(
        text(&result).starts_with(&format!("job {id} copy into {}: completed", into.display())),
        "{result}"
    );

    // Copied again, it waits at the name; continuing keeps its id.
    let result = call(&mut server, "copy", copy);
    assert_eq!(data(&result)["state"], "waiting", "{result}");
    let waiting = data(&result)["id"].as_str().unwrap().to_owned();
    let result = call(
        &mut server,
        "continue_job",
        json!({"job": waiting, "conflict": "keep_both", "wait_seconds": 60}),
    );
    assert_eq!(data(&result)["id"], waiting);
    assert_eq!(data(&result)["state"], "completed", "{result}");
    let result = call(&mut server, "continue_job", json!({"job": waiting}));
    assert!(
        text(&result).starts_with(&format!("Job {waiting} has nothing left to do.")),
        "{result}"
    );
    let result = call(&mut server, "job_status", json!({"job": "999"}));
    assert_eq!(code(&result), "not_found");
    stop(serving, server);
    assert_eq!(std::fs::read_to_string(into.join("a 2.txt")).unwrap(), "a");
}

#[test]
fn require_clone_refuses_a_copy_that_would_write_every_byte() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "a").unwrap();
    let mut server = server();
    // /dev is devfs, never APFS.
    let result = call(
        &mut server,
        "copy",
        json!({"paths": [file], "into": "/dev", "require_clone": true}),
    );
    assert_eq!(code(&result), "clone_unavailable", "{result}");
}

#[test]
fn archives_list_like_folders_and_create_and_extract_as_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let (proj, out) = (dir.path().join("proj"), dir.path().join("out"));
    std::fs::create_dir_all(proj.join("docs")).unwrap();
    std::fs::create_dir(&out).unwrap();
    std::fs::write(proj.join("docs/a.txt"), "hello").unwrap();
    std::fs::write(proj.join("docs/b.txt"), "").unwrap();
    let (serving, mut server) = owned(dir.path());
    let result = call(
        &mut server,
        "archive_create",
        json!({"paths": [proj], "wait_seconds": 60}),
    );
    let archive = dir.path().join("proj.zip");
    assert_eq!(data(&result)["kind"], "compress");
    assert!(
        text(&result).ends_with(&format!("{}\n", archive.display())),
        "{result}"
    );

    let result = call(&mut server, "list_dir", json!({"path": archive}));
    assert_eq!(text(&result), "proj/\n");
    let docs = archive.join("proj/docs");
    let result = call(&mut server, "list_dir", json!({"path": docs, "limit": 1}));
    assert_eq!(text(&result).lines().next(), Some("a.txt"));
    assert_eq!(data(&result)["path"], shown(&docs));
    assert_eq!(data(&result)["items"][0]["name"], "a.txt");
    assert_eq!(data(&result)["items"][0]["size"], 5);
    assert_eq!(data(&result)["truncated"], true);
    let result = call(&mut server, "stat", json!({"paths": [docs.join("a.txt")]}));
    assert_eq!(code(&result), "unsupported_on_source");

    let extract = json!({"path": archive, "into": out, "wait_seconds": 60});
    let result = call(&mut server, "archive_extract", extract.clone());
    assert_eq!(data(&result)["state"], "completed", "{result}");
    assert_eq!(
        std::fs::read_to_string(out.join("proj/docs/a.txt")).unwrap(),
        "hello"
    );
    let result = call(&mut server, "archive_extract", extract);
    assert_eq!(
        data(&result)["state"],
        "waiting",
        "conflict defaults to stop"
    );
    settings(dir.path(), r#"{"agents_change_files": false}"#);
    let result = call(&mut server, "archive_create", json!({"paths": [proj]}));
    assert_eq!(code(&result), "agents_disabled");
    stop(serving, server);
}

#[test]
fn storage_tools_scan_find_duplicates_and_trash_chosen_copies_only_on_confirm() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (serving, mut server) = owned(&root);
    let photos = root.join("photos");
    let photo: Vec<u8> = (0..300_000u32).map(|n| (n % 253) as u8).collect();
    let mut other = photo.clone();
    other[100_000] ^= 1;
    let (a, b, c) = (
        photos.join("a/photo.jpg"),
        photos.join("b/photo.jpg"),
        photos.join("other.jpg"),
    );
    for (path, bytes) in [(&a, &photo), (&b, &photo), (&c, &other)] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let result = call(
        &mut server,
        "storage_scan",
        json!({"path": photos, "largest": 1}),
    );
    let scan = data(&result);
    assert_eq!(scan["bytes"], 900_000);
    assert_eq!(scan["files"], 3);
    assert_eq!(scan["largest"][0]["path"], shown(&c));
    assert_eq!(scan["folders_by_size"].as_array().unwrap().len(), 2);
    assert_eq!(scan["partial"], false);

    let result = call(&mut server, "duplicates", json!({"path": photos}));
    let groups = &data(&result)["groups"];
    assert_eq!(groups[0]["verified"], true);
    assert_eq!(groups[0]["paths"], json!([shown(&a), shown(&b)]));
    assert_eq!(data(&result)["wasted"], 300_000);

    let dry_run = |server: &mut Server, trash: &[&PathBuf]| {
        call(
            server,
            "cleanup_duplicates",
            json!({"path": photos, "trash": trash, "dry_run": true}),
        )
    };
    let refused = dry_run(&mut server, &[&a, &b]);
    assert!(
        text(&refused).starts_with("Keep at least one of "),
        "{refused}"
    );
    let refused = dry_run(&mut server, &[&c]);
    assert!(
        text(&refused).contains("not a verified duplicate in this scan."),
        "{refused}"
    );
    let result = dry_run(&mut server, &[&b]);
    assert_eq!(data(&result)["plan"]["keep"], json!([shown(&a)]));
    assert_eq!(data(&result)["plan"]["trash"], json!([shown(&b)]));
    let token = data(&result)["token"].as_str().unwrap().to_owned();
    assert!(b.exists());

    settings(&root, r#"{"agents_change_files": false}"#);
    let confirm = json!({"confirm": token});
    let result = call(&mut server, "cleanup_duplicates", confirm.clone());
    assert_eq!(code(&result), "agents_disabled");
    settings(&root, r#"{"agents_change_files": true}"#);
    let result = call(&mut server, "cleanup_duplicates", confirm.clone());
    assert_eq!(data(&result)["items"][0]["path"], shown(&b), "{result}");
    assert!(!b.exists() && a.exists());
    let result = call(&mut server, "cleanup_duplicates", confirm);
    assert_eq!(code(&result), "changed_since_preview");
    let result = call(&mut server, "undo", json!({}));
    assert_eq!(
        text(&result),
        format!("Undo Trash Duplicates\n{}\n", b.display())
    );
    assert_eq!(std::fs::read(&b).unwrap(), photo);
    stop(serving, server);
}

#[test]
fn overview_says_what_is_measured_and_what_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    std::fs::create_dir(home.join("Documents")).unwrap();
    std::fs::write(home.join("Documents/a"), vec![1; 4096]).unwrap();
    std::fs::create_dir(home.join("Pictures")).unwrap();
    let paths = ops::Paths {
        trash: home.join(".Trash"),
        ..paths(&home)
    };
    let mut server = Server::new(ops::Client::new(paths, None), places(&home), Profile::All);
    let timed_out = call(&mut server, "overview", json!({"timeout_seconds": 0}));
    assert!(
        text(&timed_out).contains(&format!("{}/Documents/", home.display()))
            && text(&timed_out).contains("not measured"),
        "{timed_out}"
    );
    assert_eq!(data(&timed_out)["partial"], true);
    let measured = call(&mut server, "overview", json!({}));
    assert!(text(&measured).contains("on disk"), "{measured}");
}

#[test]
fn place_tools_pin_list_and_unpin_favorites_and_eject_refuses_without_ejecting() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap();
    let (work, gone) = (real.join("Work"), real.join("Gone"));
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(&gone).unwrap();
    let mut server = server();
    server.places = places(dir.path());
    for (path, at) in [(&gone, json!(null)), (&work, json!(0))] {
        let result = call(&mut server, "pin", json!({"path": path, "at": at}));
        assert_eq!(text(&result), format!("{}\n", path.display()));
    }
    std::fs::rename(&gone, real.join("Moved")).unwrap();
    let result = call(&mut server, "list_places", json!({}));
    let lines: Vec<&str> = text(&result).lines().collect();
    assert_eq!(lines[..2], ["Favorites", &shown(&work)]);
    let places = data(&result);
    assert_eq!(places["favorites"][0]["path"], shown(&work));
    let moved = places["favorites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|favorite| favorite["path"] == shown(&gone))
        .unwrap();
    assert!(moved["unavailable"].is_string(), "{moved}");
    assert_eq!(places["volumes"][0]["path"], "/");
    assert_eq!(places["volumes"][0]["startup"], true);

    let result = call(&mut server, "unpin", json!({"path": gone}));
    assert_eq!(
        text(&result),
        format!("Unpinned {}; the folder is untouched.\n", gone.display())
    );
    server
        .places
        .save_server("smb://nas.invalid/media")
        .unwrap();
    let result = call(&mut server, "list_places", json!({}));
    assert!(
        text(&result).ends_with("Servers\nsmb://nas.invalid/media\n"),
        "{result}"
    );
    assert_eq!(data(&result)["servers"][0]["kind"], "smb");
    for (name, key, address, refusal) in [
        (
            "connect",
            "address",
            "afp://nas/media",
            "Monodrive connects to smb://",
        ),
        (
            "disconnect",
            "server",
            "smb://nas.invalid/media",
            "smb://nas.invalid/media isn't connected.",
        ),
        (
            "forget_server",
            "server",
            "smb://nas.invalid/other",
            "smb://nas.invalid/other isn't a saved server.",
        ),
    ] {
        let result = call(&mut server, name, json!({key: address}));
        assert!(text(&result).starts_with(refusal), "{result}");
    }
    let result = call(
        &mut server,
        "forget_server",
        json!({"server": "nas.invalid/media"}),
    );
    assert!(
        text(&result).starts_with("Forgot smb://nas.invalid/media"),
        "{result}"
    );
    for (path, refusal) in [
        (dir.path().to_path_buf(), " isn't a mounted volume."),
        ("/".into(), " is the startup disk."),
    ] {
        let result = call(&mut server, "eject", json!({"path": path}));
        assert!(
            text(&result).lines().next().unwrap().ends_with(refusal),
            "{result}"
        );
    }
}

#[test]
fn search_tools_save_list_rename_and_remove_without_searching() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = server();
    server.places = places(dir.path());
    let home = std::env::var("HOME").unwrap();
    let result = call(&mut server, "list_searches", json!({}));
    assert_eq!(text(&result), "No saved searches.\n");
    for (arguments, printed) in [
        (
            json!({"name": "Photos", "query": "kind:image  size:>1MB", "path": "~/Pictures"}),
            format!("Photos: kind:image size:>1mb (in {home}/Pictures)\n"),
        ),
        (
            json!({"name": "Recent", "query": "modified:<7d", "at": 0}),
            "Recent: modified:7d\n".into(),
        ),
    ] {
        let result = call(&mut server, "save_search", arguments);
        assert_eq!(text(&result), printed);
    }
    let result = call(
        &mut server,
        "rename_search",
        json!({"name": "Recent", "to": "This week"}),
    );
    assert_eq!(text(&result), "Renamed Recent to This week.\n");
    let result = call(&mut server, "list_searches", json!({}));
    assert_eq!(data(&result)["searches"][0]["name"], "This week");
    assert_eq!(
        data(&result)["searches"][1]["path"],
        format!("{home}/Pictures")
    );
    let result = call(&mut server, "remove_search", json!({"name": "Photos"}));
    assert_eq!(text(&result), "Removed the saved search Photos.\n");
    let result = call(&mut server, "remove_search", json!({"name": "Photos"}));
    assert!(
        text(&result).starts_with("Photos isn't a saved search."),
        "{result}"
    );
    assert!(server.started_index().is_none());
}

#[test]
fn worktree_tools_list_and_inspect_worktrees_and_register_and_forget_repositories() {
    use crate::worktrees::tests::{fixture, real_tempdir, repo};
    let (_dir, root) = real_tempdir();
    let main = root.join("repo");
    repo(&main);
    let (feature, detached) = (root.join("feature"), root.join("detached"));
    fixture(
        &main,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    fixture(&main, &["worktree", "add", "-q", "--detach", "../detached"]);
    fixture(
        &main,
        &["worktree", "lock", "--reason", "on a drive", "../detached"],
    );
    let head = fixture(&main, &["rev-parse", "--short=7", "HEAD"]);
    let head = head.trim();
    let mut server = server();
    server.places = places(&root.join("state"));
    let mut tool = |name: &str, path: &Path| call(&mut server, name, json!({"path": path}));

    let listed = format!(
        "{} (main, {head}, main worktree)\n{} (detached, {head}, locked: on a drive)\n{} (feature, {head})\n",
        main.display(),
        detached.display(),
        feature.display()
    );
    for path in [&main, &feature] {
        assert_eq!(text(&tool("list_worktrees", path)), listed);
    }
    let result = tool("list_worktrees", &main);
    assert_eq!(data(&result)["worktrees"][1]["locked"], "on a drive");
    let result = tool("list_worktrees", &root);
    assert!(text(&result).starts_with(&format!("{} isn't in a Git repository.", root.display())));

    std::fs::write(feature.join("a"), "changed\n").unwrap();
    std::fs::write(feature.join("new"), "").unwrap();
    let result = tool("worktree_status", &feature);
    assert_eq!(
        text(&result),
        format!("feature at {head}, no upstream\n1 unstaged, 1 untracked\n")
    );
    assert_eq!(data(&result)["untracked"], 1);

    let mut repos = || text(&call(&mut server, "list_repos", json!({}))).to_owned();
    assert_eq!(repos(), "No registered repositories.\n");
    let mut tool = |name: &str, path: &Path| call(&mut server, name, json!({"path": path}));
    assert_eq!(
        text(&tool("register_repo", &feature)),
        format!("{}\n", main.display())
    );
    let mut repos = || text(&call(&mut server, "list_repos", json!({}))).to_owned();
    assert_eq!(repos(), format!("{}\n", main.display()));
    let mut tool = |name: &str, path: &Path| call(&mut server, name, json!({"path": path}));
    assert_eq!(
        text(&tool("forget_repo", &main)),
        format!("Forgot {}; the repository is untouched.\n", main.display())
    );
    assert!(main.join(".git/HEAD").is_file());
}

#[test]
fn command_tools_list_dry_run_run_and_cancel_only_what_the_user_saved() {
    use crate::user_commands::tests::{fifo, script};
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().map(|real| (dir, real)).unwrap();
    let (_kept, dir) = dir;
    let (ready, hold) = (fifo(&dir.join("ready")), fifo(&dir.join("hold")));
    let wait = script(
        &dir,
        "wait",
        &format!(
            "echo up > '{}'\nread x < '{}'",
            ready.display(),
            hold.display()
        ),
    );
    let (spaced, dashed) = (dir.join("a \"b\".txt"), dir.join("-n"));
    for file in [&spaced, &dashed] {
        std::fs::write(file, "").unwrap();
    }
    places(&dir)
        .save_command(Definition {
            name: "Wait".into(),
            program: wait.clone(),
            args: vec!["--".into(), user_commands::PATHS.into()],
            cwd: Cwd::Selection,
            sources: Sources::Files,
            env: [("MODE".to_string(), "fast".to_string())].into(),
            timeout_seconds: 30,
            output_limit: 1 << 10,
        })
        .unwrap();
    let (serving, mut server) = owned(&dir);

    let result = call(&mut server, "list_commands", json!({}));
    let template = serde_json::to_string(&[wait.to_str().unwrap(), "--", "{paths}"]).unwrap();
    assert_eq!(
        text(&result),
        format!("Wait: {template} on files, in the selection's folder, timeout 30s\n")
    );
    let result = call(
        &mut server,
        "run_command",
        json!({"name": "Wait", "paths": [spaced, dashed], "dry_run": true}),
    );
    let args = serde_json::to_string(&[
        wait.to_str().unwrap(),
        "--",
        spaced.to_str().unwrap(),
        dashed.to_str().unwrap(),
    ])
    .unwrap();
    let token = data(&result)["token"].as_str().unwrap().to_owned();
    assert_eq!(
        text(&result),
        format!(
            "Wait would run once for up to 30s. It isn't undoable.\ncwd {}\nargv {args}\nenv adds {{\"MODE\":\"fast\"}}\ntoken: {token}\n",
            dir.display()
        )
    );
    // Agents run what the user saved; no tool saves one.
    assert_eq!(
        rpc(&mut server, "save_command", json!({}))["error"]["code"],
        -32602
    );
    for (name, arguments, error) in [
        (
            "run_command",
            json!({"name": "Nope", "paths": [spaced], "dry_run": true}),
            "Nope isn't a saved command.",
        ),
        (
            "run_command",
            json!({"name": "Wait", "paths": [dir]}),
            "is a folder",
        ),
        ("job_status", json!({"job": "cmd-9"}), "No job cmd-9."),
    ] {
        let result = call(&mut server, name, arguments);
        assert!(text(&result).contains(error), "{result}");
    }

    let result = call(&mut server, "run_command", json!({"confirm": token}));
    assert_eq!(data(&result)["id"], "cmd-1", "{result}");
    assert!(text(&result).contains("aren't undoable"), "{result}");
    assert_eq!(std::fs::read_to_string(&ready).unwrap(), "up\n");
    let result = call(&mut server, "job_status", json!({}));
    assert_eq!(
        text(&result),
        "job cmd-1 command: running, 0 of 1 items, by agent\n"
    );
    let result = call(&mut server, "cancel_job", json!({"job": "cmd-1"}));
    assert!(
        text(&result).starts_with("Cancelling job cmd-1."),
        "{result}"
    );
    stop(serving, server);
}

#[test]
fn a_commands_job_shows_each_invocations_end_and_the_outputs_two_ends() {
    let invocation = |path: &str| user_commands::Invocation {
        argv: vec!["/bin/tool".into(), path.into()],
        cwd: "/tmp".into(),
    };
    let run = Run {
        id: 4,
        origin: Origin::Window,
        state: user_commands::RunState::Cancelled,
        started: 0,
        plan: user_commands::Plan {
            name: "Tool".into(),
            invocations: vec![invocation("/a"), invocation("/b"), invocation("/c")],
            env: Default::default(),
            timeout_seconds: 60,
            output_limit: 1 << 10,
        },
        progress: vec![
            user_commands::Progress {
                ended: Some(Ended::Exited(3)),
                millis: 12,
            },
            user_commands::Progress {
                ended: Some(Ended::Cancelled),
                millis: 40,
            },
        ],
        output: user_commands::Output {
            head: "start".into(),
            tail: "end".into(),
            total: 100,
            kept: 8,
        },
    };
    assert_eq!(
        contract::JobView::of_run(&run).text(),
        "job cmd-4 command: cancelled, 2 of 3 items, by window\n\
         exit 3 after 12 ms: [\"/bin/tool\",\"/a\"]\n\
         cancelled after 40 ms: [\"/bin/tool\",\"/b\"]\n\
         not run: [\"/bin/tool\",\"/c\"]\n\
         output:\nstart\n[92 bytes left out]\nend\n"
    );
}

#[test]
fn remote_paths_browse_read_and_copy_both_ways_through_the_same_tools() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let store = places(&root);
    let connector =
        || -> remote::Connector { Arc::new(|_: &Source| Ok(remote::fixture::transport())) };
    let owner = ops::Owner::bind_with(paths(&root), connector()).unwrap();
    let serving = std::thread::spawn(move || owner.serve());
    let mut server = Server::new(
        ops::Client::new(paths(&root), None),
        store.clone(),
        Profile::All,
    );
    server.remote = Connections::new(store, connector());

    let (far, near) = (root.join("far"), root.join("near"));
    std::fs::create_dir(&far).unwrap();
    std::fs::create_dir(&near).unwrap();
    std::fs::write(far.join("a file.txt"), "hello remote").unwrap();
    std::fs::create_dir(far.join("sub")).unwrap();
    std::os::unix::fs::symlink("a file.txt", far.join("link")).unwrap();
    std::fs::write(near.join("up.txt"), "going up").unwrap();

    let result = call(
        &mut server,
        "add_server",
        json!({"host": "-oProxyCommand=x"}),
    );
    assert!(failed(&result));
    let result = call(
        &mut server,
        "add_server",
        json!({"host": "me@box", "id": "box", "path": far}),
    );
    assert_eq!(data(&result)["id"], "box");
    let result = call(&mut server, "list_places", json!({}));
    assert_eq!(data(&result)["servers"][0]["kind"], "ssh");
    assert_eq!(data(&result)["servers"][0]["connected"], false);

    // connect takes an sftp:// address as Connect to Server does, and saves
    // it as a server once it's connected.
    let result = call(
        &mut server,
        "connect",
        json!({"address": format!("sftp://me@spare{}", near.display())}),
    );
    assert_eq!(
        text(&result),
        format!(
            "spare: me@spare, starts at {}; browse it as spare:/path\n",
            near.display()
        )
    );
    assert!(server.places.remote("spare").is_ok());
    let qualified = |name: &str| format!("box:{}", far.join(name).display());
    let result = call(&mut server, "list_dir", json!({"path": "box:"}));
    let mut names: Vec<&str> = text(&result).lines().collect();
    names.sort_unstable();
    assert_eq!(names, ["a file.txt", "link@", "sub/"]);
    assert_eq!(data(&result)["path"], format!("box:{}", far.display()));
    let result = call(
        &mut server,
        "list_dir",
        json!({"path": qualified(""), "limit": 2}),
    );
    assert_eq!(data(&result)["next_cursor"], "2");

    let result = call(
        &mut server,
        "stat",
        json!({"paths": [qualified("link"), qualified("nope")]}),
    );
    assert!(text(&result).contains("link to: a file.txt"), "{result}");
    assert!(data(&result)["items"][1]["error"].is_string());
    let result = call(
        &mut server,
        "read_file",
        json!({"path": qualified("a file.txt"), "offset": 6, "limit": 3}),
    );
    assert_eq!(
        text(&result),
        format!("{}: bytes 6 to 9 of 12\nrem", qualified("a file.txt"))
    );
    let result = call(&mut server, "list_dir", json!({"path": "gone:/x"}));
    assert_eq!(code(&result), "not_found");

    let result = call(
        &mut server,
        "create_folder",
        json!({"path": qualified("made")}),
    );
    assert_eq!(text(&result), format!("{}\n", qualified("made")));
    let result = call(
        &mut server,
        "rename",
        json!({"paths": [qualified("made")], "name": "named"}),
    );
    assert_eq!(text(&result), format!("{}\n", qualified("named")));
    assert!(far.join("named").is_dir());
    for (name, arguments) in [
        ("trash", json!({"paths": [qualified("named")]})),
        ("move", json!({"paths": [qualified("named")], "into": near})),
        (
            "rename",
            json!({"paths": [qualified("named")], "rule": {"type": "names", "names": ["x"]}}),
        ),
    ] {
        assert_eq!(
            code(&call(&mut server, name, arguments)),
            "unsupported_on_source",
            "{name}"
        );
    }
    let result = call(
        &mut server,
        "copy",
        json!({"paths": [qualified("a file.txt")], "into": qualified("sub")}),
    );
    assert_eq!(code(&result), "unsupported_on_source");

    let result = call(
        &mut server,
        "copy",
        json!({"paths": [qualified("a file.txt")], "into": near, "wait_seconds": 60}),
    );
    assert_eq!(data(&result)["kind"], "download");
    assert_eq!(data(&result)["state"], "completed", "{result}");
    assert_eq!(data(&result)["items"][0]["method"], "copy");
    let result = call(
        &mut server,
        "copy",
        json!({"paths": [near.join("up.txt")], "into": qualified("sub"), "wait_seconds": 60}),
    );
    assert_eq!(data(&result)["kind"], "upload");
    assert!(text(&result).contains(remote::NO_UNDO), "{result}");
    assert_eq!(
        std::fs::read(near.join("a file.txt")).unwrap(),
        b"hello remote"
    );
    assert_eq!(std::fs::read(far.join("sub/up.txt")).unwrap(), b"going up");

    settings(&root, r#"{"agents_change_files": false}"#);
    for (name, arguments) in [
        ("add_server", json!({"host": "other"})),
        ("create_folder", json!({"path": qualified("no")})),
        (
            "copy",
            json!({"paths": [near.join("up.txt")], "into": qualified("no")}),
        ),
    ] {
        assert_eq!(
            code(&call(&mut server, name, arguments)),
            "agents_disabled",
            "{name}"
        );
    }
    assert!(!far.join("no").exists());
    settings(&root, "{}");

    let result = call(&mut server, "disconnect", json!({"server": "box"}));
    assert!(text(&result).contains("Disconnected from box"), "{result}");
    let result = call(&mut server, "forget_server", json!({"server": "box"}));
    assert_eq!(
        text(&result),
        "Forgot box; nothing on the server changed.\n"
    );
    assert!(far.join("a file.txt").exists());
    stop(serving, server);
}

#[test]
fn a_server_that_needs_a_password_says_auth_required() {
    let dir = tempfile::tempdir().unwrap();
    let store = places(dir.path());
    store.add_remote("nas", None, None).unwrap();
    let mut server = server();
    server.places = store.clone();
    server.remote = Connections::new(
        store,
        Arc::new(|source: &Source| Err(RemoteError::InteractiveAuth(source.host.clone()))),
    );
    let result = call(&mut server, "list_dir", json!({"path": "nas:/srv"}));
    assert_eq!(code(&result), "auth_required");
    assert!(
        text(&result).contains("nas: interactive authentication required"),
        "{result}"
    );
}

/// The contract as `contract/v1.json` keeps it: names, profiles, schemas,
/// hints and error codes. Descriptions and text aren't part of it.
fn live_contract() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "profile": match tool.profile { Profile::Core => "core", Profile::All => "all" },
                "inputSchema": tool.input_schema(),
                "outputSchema": tool.output_schema(),
                "annotations": tool.hints.to_json(),
            })
        })
        .collect();
    // Every code, as the schema of an error's lists them.
    let codes = contract::input_schema::<contract::Code>()["enum"].clone();
    json!({"version": contract::VERSION, "tools": tools, "error_codes": codes})
}

/// What `new` breaks of `old`, one line each. An input may gain optional
/// fields and accepted values; an output may gain anything.
fn breaks(old: &Value, new: &Value, at: &str, input: bool, out: &mut Vec<String>) {
    if old.get("type") != new.get("type") {
        out.push(format!(
            "{at}: type changed from {} to {}",
            old["type"], new["type"]
        ));
        return;
    }
    if input && old.get("default") != new.get("default") {
        out.push(format!("{at}: default changed"));
    }
    if let (true, Some(values)) = (input, old.get("enum").and_then(Value::as_array)) {
        let now = new
            .get("enum")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for value in values.iter().filter(|value| !now.contains(value)) {
            out.push(format!("{at}: input value {value} removed"));
        }
    }
    if input {
        let required = |schema: &Value| -> Vec<Value> {
            schema
                .get("required")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let before = required(old);
        for name in required(new).iter().filter(|name| !before.contains(name)) {
            out.push(format!("{at}: {name} is newly required"));
        }
    }
    if let Some(properties) = old.get("properties").and_then(Value::as_object) {
        for (name, schema) in properties {
            match new.pointer(&format!("/properties/{name}")) {
                Some(now) => breaks(schema, now, &format!("{at}.{name}"), input, out),
                None => out.push(format!("{at}.{name} removed")),
            }
        }
    }
    if let (Some(old), Some(new)) = (old.get("items"), new.get("items")) {
        breaks(old, new, &format!("{at}[]"), input, out);
    }
    // Tagged unions, such as rename's rule: each variant by its tag.
    let tag = |variant: &Value| variant.pointer("/properties/type/const").cloned();
    if let Some(variants) = old.get("oneOf").and_then(Value::as_array) {
        let now = new
            .get("oneOf")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for variant in variants {
            match now.iter().find(|candidate| tag(candidate) == tag(variant)) {
                Some(matched) => breaks(
                    variant,
                    matched,
                    &format!("{at}<{}>", tag(variant).unwrap_or_default()),
                    input,
                    out,
                ),
                None if input => out.push(format!(
                    "{at}: variant {} removed",
                    tag(variant).unwrap_or_default()
                )),
                None => {}
            }
        }
    }
}

#[test]
fn the_contract_only_grows_and_its_snapshot_keeps_up() {
    let file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("contract/v{}.json", contract::VERSION));
    let live = live_contract();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap_or_default()).unwrap_or(Value::Null);
    let mut found = Vec::new();
    if let Some(tools) = saved["tools"].as_array() {
        for old in tools {
            let name = old["name"].as_str().unwrap_or_default();
            let Some(new) = live["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
            else {
                found.push(format!(
                    "{name}: removed; ship a new tool beside it instead"
                ));
                continue;
            };
            if old["profile"] == "core" && new["profile"] != "core" {
                found.push(format!("{name}: left the core profile"));
            }
            breaks(
                &old["inputSchema"],
                &new["inputSchema"],
                &format!("{name} input"),
                true,
                &mut found,
            );
            breaks(
                &old["outputSchema"],
                &new["outputSchema"],
                &format!("{name} output"),
                false,
                &mut found,
            );
        }
        let codes = live["error_codes"].as_array().unwrap();
        for code in saved["error_codes"].as_array().into_iter().flatten() {
            if !codes.contains(code) {
                found.push(format!("error code {code} removed"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "contract v{} breaks:\n{}",
        contract::VERSION,
        found.join("\n")
    );
    if live != saved {
        let mut pretty = serde_json::to_string_pretty(&live).unwrap();
        pretty.push('\n');
        std::fs::write(&file, pretty).unwrap();
    }
}

#[test]
fn the_contract_check_catches_what_breaks_agents() {
    let old = json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "mode": {"type": "string", "enum": ["a", "b"], "default": "a"},
        },
        "required": ["path"],
    });
    let mut found = Vec::new();
    let grown = json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "mode": {"type": "string", "enum": ["a", "b", "c"], "default": "a"},
            "limit": {"type": "integer"},
        },
        "required": ["path"],
    });
    breaks(&old, &grown, "t", true, &mut found);
    assert!(found.is_empty(), "{found:?}");
    let broken = json!({
        "type": "object",
        "properties": {"mode": {"type": "integer"}, "limit": {"type": "integer"}},
        "required": ["limit"],
    });
    breaks(&old, &broken, "t", true, &mut found);
    // In whatever order the schema lists them.
    found.sort();
    assert_eq!(
        found,
        [
            "t.mode: type changed from \"string\" to \"integer\"",
            "t.path removed",
            "t: \"limit\" is newly required",
        ]
    );
}

#[test]
fn remove_worktree_trashes_a_clean_worktree_and_shows_work_at_risk_before_a_token() {
    use crate::worktrees::tests::{commit, fixture, real_tempdir, repo};
    let (_dir, root) = real_tempdir();
    let (serving, mut server) = owned(&root);
    let main = root.join("repo");
    repo(&main);
    for name in ["clean", "busy"] {
        fixture(
            &main,
            &["worktree", "add", "-q", "-b", name, &format!("../{name}")],
        );
    }
    let (clean, busy) = (root.join("clean"), root.join("busy"));
    commit(&busy, "b", "b\n");
    std::fs::write(busy.join("new"), "").unwrap();

    // Nothing only it holds: the folder, then Git's registration, go at once.
    let result = call(&mut server, "remove_worktree", json!({"path": clean}));
    let trashed: Vec<&str> = text(&result).lines().collect();
    assert!(
        trashed.len() == 2 && trashed.iter().all(|path| path.contains("/.Trash/")),
        "{result}"
    );
    assert!(!clean.exists() && !main.join(".git/worktrees/clean").exists());

    let result = call(&mut server, "remove_worktree", json!({"path": busy}));
    let plan = &data(&result)["plan"];
    assert_eq!(plan["uncommitted"], 1);
    assert_eq!(plan["unpushed"], 1);
    assert_eq!(plan["branch"], "busy");
    let token = data(&result)["token"].as_str().unwrap().to_owned();
    let short = fixture(&busy, &["rev-parse", "--short", "HEAD"]);
    assert!(
        text(&result).starts_with(&format!(
            "{} holds work nothing else does.\n1 uncommitted change:\n  ? new\n\
             1 commit on busy that no other branch, tag or remote has:\n  {} b\n\
             The branch busy stays.\n",
            busy.display(),
            short.trim()
        )),
        "{result}"
    );
    assert!(
        busy.join("new").exists(),
        "work at risk moves nothing without a confirm"
    );
    let result = call(&mut server, "remove_worktree", json!({"confirm": "nope"}));
    assert_eq!(code(&result), "changed_since_preview");
    let result = call(&mut server, "remove_worktree", json!({"confirm": token}));
    assert!(!failed(&result) && !busy.exists(), "{result}");
    let result = call(&mut server, "remove_worktree", json!({"confirm": token}));
    assert_eq!(code(&result), "changed_since_preview", "a token works once");

    call(&mut server, "undo", json!({}));
    assert!(busy.join("new").exists());
    let result = call(&mut server, "remove_worktree", json!({"path": main}));
    assert!(text(&result).contains("own checkout"), "{result}");
    stop(serving, server);
}
