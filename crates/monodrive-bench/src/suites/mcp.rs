//! `monodrive mcp` over stdio, as an agent reaches it: start to first reply,
//! `tools/list`, and `find_files`, `grep`, `list_dir` and `copy` round trips.
//!
//! The server runs with `HOME` set to a folder of the fixture holding the
//! generated index, so its first search loads that and crawls nothing, and
//! its owner, journal and settings live there too. Before any search it
//! refuses to go on unless the server sees that saved index, and after the
//! first one unless the index holds the generated entry count. Any failure
//! kills the server by the PID captured when it started.

use crate::format::{Cache, Measured};
use crate::measure::{self, Sample};
use crate::suites::{delete_fixture, search};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const LISTED: usize = 10_000;
const COPIED: usize = 1_000;
const GREPPED: usize = 200;

/// Where a dev build and a release build look for the index under `home`.
const INDEX_DIRS: [&str; 2] = [
    "Library/Application Support/Monodrive (Dev)/FSearch",
    "Library/Application Support/FSearch",
];

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

fn cp_clone(from: &Path, to: &Path) -> Result<(), String> {
    let status = Command::new("cp")
        .args(["-c", "-R"])
        .arg(from)
        .arg(to)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "cp -c {} {}: {status}",
            from.display(),
            to.display()
        ))
    }
}

pub fn prepare(fixture: &Path, seed: u64) -> Result<(), String> {
    let grepped = fixture.join("grepped");
    std::fs::create_dir(&grepped).map_err(io(&grepped))?;
    for i in 0..GREPPED {
        let path = grepped.join(format!("module_{i}.rs"));
        let body = format!("// Module {i}.\nfn helper_{i}() -> u32 {{ {i} }}\n").repeat(40);
        let body = if i % 20 == 0 {
            format!("{body}// TODO: needle {i}\n")
        } else {
            body
        };
        std::fs::write(&path, body).map_err(io(&path))?;
    }
    // The real folder grep reads is in the index too, as it would be.
    search::prepare_with(fixture, seed, Some(&grepped))?;
    let home = fixture.join("home");
    for dir in INDEX_DIRS {
        let at = home.join(dir);
        std::fs::create_dir_all(at.parent().unwrap_or(&home)).map_err(io(&at))?;
        cp_clone(&search::index_dir(fixture), &at)?;
    }
    let listed = fixture.join("listed");
    std::fs::create_dir(&listed).map_err(io(&listed))?;
    for i in 0..LISTED {
        let path = listed.join(format!("file {i}.txt"));
        std::fs::write(&path, i.to_le_bytes()).map_err(io(&path))?;
    }
    let copied = fixture.join("copied");
    std::fs::create_dir(&copied).map_err(io(&copied))?;
    for i in 0..COPIED {
        let path = copied.join(format!("file {i}.txt"));
        std::fs::write(&path, i.to_le_bytes()).map_err(io(&path))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct Call<'a, P: Serialize> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Serialize)]
struct ToolParams<'a, A: Serialize> {
    name: &'a str,
    arguments: A,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    result: Option<ToolReply>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

#[derive(Deserialize, Default)]
struct ToolReply {
    #[serde(default)]
    content: Vec<Text>,
    #[serde(default, rename = "isError")]
    is_error: bool,
}

#[derive(Deserialize)]
struct Text {
    #[serde(default)]
    text: String,
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    sent: u64,
}

impl Server {
    fn start(binary: &Path, home: &Path) -> Result<Self, String> {
        let mut child = Command::new(binary)
            .arg("mcp")
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", binary.display()))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err("no pipes to the server".into());
        };
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            sent: 0,
        })
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Sends one request and reads its reply line, returning the line's length too.
    fn ask<P: Serialize>(&mut self, method: &str, params: P) -> Result<(Reply, usize), String> {
        self.sent += 1;
        let mut line = serde_json::to_string(&Call {
            jsonrpc: "2.0",
            id: self.sent,
            method,
            params,
        })
        .map_err(|e| e.to_string())?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut reply = String::new();
        if self
            .stdout
            .read_line(&mut reply)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Err(format!("the server stopped answering {method}"));
        }
        let parsed: Reply = serde_json::from_str(&reply).map_err(|e| format!("{method}: {e}"))?;
        if let Some(error) = &parsed.error {
            return Err(format!("{method}: {}", error.message));
        }
        Ok((parsed, reply.len()))
    }

    /// Calls a tool and returns its text, failing on a tool error.
    fn tool<A: Serialize>(&mut self, name: &str, arguments: A) -> Result<String, String> {
        let (reply, _) = self.ask("tools/call", ToolParams { name, arguments })?;
        let result = reply.result.unwrap_or_default();
        let text: String = result.content.into_iter().map(|t| t.text).collect();
        if result.is_error {
            Err(format!("{name}: {text}"))
        } else {
            Ok(text)
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), String> {
        let line = format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\"}}\n");
        self.stdin
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())
    }

    /// Closes stdin, which ends the server, and waits for it.
    fn stop(self) -> Result<(), String> {
        let Server {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        child.wait().map(|_| ()).map_err(|e| e.to_string())
    }
}

#[derive(Serialize)]
struct Initialize {
    #[serde(rename = "protocolVersion")]
    protocol_version: &'static str,
    capabilities: Empty,
    #[serde(rename = "clientInfo")]
    client_info: ClientInfo,
}

#[derive(Serialize)]
struct Empty {}

#[derive(Serialize)]
struct ClientInfo {
    name: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
struct Query<'a> {
    query: &'a str,
}

#[derive(Serialize)]
struct PathArg<'a> {
    path: &'a Path,
}

#[derive(Serialize)]
struct Grep<'a> {
    pattern: &'a str,
    path: &'a Path,
}

#[derive(Serialize)]
struct Copy<'a> {
    paths: [&'a Path; 1],
    into: &'a Path,
}

#[derive(Serialize)]
struct Job {
    job: u64,
}

fn measured(bench: &str, title: &str, sample: Sample, check: String) -> Measured {
    Measured {
        bench: bench.into(),
        title: title.into(),
        group: "MCP over stdio".into(),
        variant: String::new(),
        items: None,
        cache: Cache::Warm,
        sample,
        check,
    }
}

fn lines(text: &str) -> usize {
    text.lines().filter(|line| !line.is_empty()).count()
}

fn session(
    server: &mut Server,
    fixture: &Path,
    work: &Path,
    out: &mut Vec<Measured>,
) -> Result<(), String> {
    let pid = server.pid();
    let (sample, reply) = measure::served(pid, || server.ask("tools/list", Empty {}));
    let (_, bytes) = reply?;
    out.push(measured(
        "mcp/tools-list",
        "tools/list",
        sample,
        format!("{bytes} bytes"),
    ));

    let status = server.tool("index_status", Empty {})?;
    if !status.starts_with("Saved index") {
        return Err(format!(
            "the server doesn't see the generated index, so a search would crawl: {status}"
        ));
    }
    let (sample, found) =
        measure::served(pid, || server.tool("find_files", Query { query: "report" }));
    let found = found?;
    let expected = search::expected_entries(fixture)?;
    let status = server.tool("index_status", Empty {})?;
    let indexed = crate::report::thousands(expected);
    if !status.starts_with(&format!("{indexed} files indexed")) {
        return Err(format!(
            "the server's index isn't the generated one: {status}"
        ));
    }
    out.push(measured(
        "mcp/find-files-first",
        "find_files \"report\", loading the index",
        sample,
        format!("{} results", lines(&found)),
    ));
    let (sample, found) =
        measure::served(pid, || server.tool("find_files", Query { query: "zephyr" }));
    out.push(measured(
        "mcp/find-files",
        "find_files \"zephyr\"",
        sample,
        format!("{} results", lines(&found?)),
    ));

    let listed = fixture.join("listed");
    let (sample, listing) =
        measure::served(pid, || server.tool("list_dir", PathArg { path: &listed }));
    out.push(measured(
        "mcp/list-dir",
        "list_dir of 10,000 entries",
        sample,
        format!("{} lines", lines(&listing?)),
    ));

    let grepped = fixture.join("grepped");
    let (sample, matches) = measure::served(pid, || {
        server.tool(
            "grep",
            Grep {
                pattern: "TODO: needle",
                path: &grepped,
            },
        )
    });
    out.push(measured(
        "mcp/grep",
        "grep 200 files in a folder outside the content index",
        sample,
        format!("{} lines", lines(&matches?)),
    ));

    let into = work.join("into");
    std::fs::create_dir(&into).map_err(io(&into))?;
    let copied = fixture.join("copied");
    let (sample, queued) = measure::served(pid, || {
        server.tool(
            "copy",
            Copy {
                paths: [&copied],
                into: &into,
            },
        )
    });
    let queued = queued?;
    out.push(measured(
        "mcp/copy",
        "copy of 1,000 files, until the job is queued",
        sample,
        "queued".into(),
    ));
    let job: u64 = queued
        .split_whitespace()
        .nth(1)
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| format!("copy replied {queued:?}"))?;
    // What an agent does next: ask until the job ends.
    let (mut sample_done, status) = measure::served(pid, || -> Result<String, String> {
        loop {
            let status = server.tool("job_status", Job { job })?;
            if !status.contains(": queued,") && !status.contains(": running,") {
                return Ok(status);
            }
        }
    });
    let status = status?;
    sample_done.wall_ms += sample.wall_ms;
    let copied_count = std::fs::read_dir(into.join("copied"))
        .map_err(io(&into))?
        .count();
    out.push(measured(
        "mcp/copy-done",
        "copy of 1,000 files, then job_status until it ends",
        sample_done,
        format!(
            "{}, {copied_count} files",
            status
                .split(", by")
                .next()
                .unwrap_or("")
                .rsplit(": ")
                .next()
                .unwrap_or("")
        ),
    ));
    Ok(())
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let binary: PathBuf = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("monodrive");
    if !binary.is_file() {
        return Err(format!(
            "{}: build it with cargo build --release -p monodrive",
            binary.display()
        ));
    }
    let work = fixture.join(format!("session-{}", std::process::id()));
    std::fs::create_dir(&work).map_err(io(&work))?;
    let home = work.join("home");
    // A clone: the server's index saves over itself, and its owner keeps a journal.
    cp_clone(&fixture.join("home"), &home)?;
    let short = crate::suites::ShortPath::to(&home, "home")?;
    let home = short.path();
    let mut out = Vec::new();
    let (sample, started) = measure::here(|| -> Result<Server, String> {
        let mut server = Server::start(&binary, home)?;
        let initialize = Initialize {
            protocol_version: "2025-06-18",
            capabilities: Empty {},
            client_info: ClientInfo {
                name: "monodrive-bench",
                version: "1",
            },
        };
        match server.ask("initialize", initialize) {
            Ok(_) => Ok(server),
            Err(error) => {
                let _ = server.child.kill();
                Err(error)
            }
        }
    });
    let mut server = started?;
    out.push(measured(
        "mcp/start",
        "Start the server and answer initialize",
        sample,
        "initialized".into(),
    ));
    let done = server
        .notify("notifications/initialized")
        .and_then(|()| session(&mut server, fixture, &work, &mut out));
    if done.is_err() {
        let _ = server.child.kill();
    }
    let stopped = server.stop();
    drop(short);
    delete_fixture(&work)?;
    done.and(stopped).map(|()| out)
}
