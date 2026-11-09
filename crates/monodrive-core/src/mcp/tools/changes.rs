//! Changing files through the operation owner: trash, put_back, undo,
//! redo, rename, create_folder, create_file, duplicate, copy, move, the
//! archive jobs, and asking the cloud to download or free items.

use super::{Call, Preview, Where, local, local_one, locate};
use crate::cloud::{self, Asked};
use crate::mcp::contract::{
    Change, Changed, Code, JobView, Method, Planned, Reply, ToolError, shown,
};
use crate::mcp::server::Server;
use crate::ops::{self, Conflict, Request, rename};
use crate::remote::{self, RemotePath};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trash {
    paths: Vec<String>,
    /// Your id for this change: sending it again returns the first result
    /// instead of changing anything twice.
    request_id: Option<String>,
}

impl Call for Trash {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        let paths = local(&self.paths, "trash")?;
        server.change(Request::Trash { paths }, self.request_id.as_deref())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PutBack {
    /// Where the items are in the Trash.
    paths: Vec<String>,
    request_id: Option<String>,
}

impl Call for PutBack {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        let paths = local(&self.paths, "put_back")?;
        server.change(Request::PutBack { paths }, self.request_id.as_deref())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Undo {
    request_id: Option<String>,
}

impl Call for Undo {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        server.change(Request::Undo, self.request_id.as_deref())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Redo {
    request_id: Option<String>,
}

impl Call for Redo {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        server.change(Request::Redo, self.request_id.as_deref())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    #[serde(default)]
    paths: Vec<String>,
    /// The new name, not a path, for one item.
    name: Option<String>,
    /// How to name many items.
    rule: Option<rename::Rule>,
    /// Rename the rest when some names are invalid or collide.
    #[serde(default)]
    skip_problems: bool,
    #[serde(default)]
    dry_run: bool,
    /// A dry run's token: runs exactly its plan.
    confirm: Option<String>,
    request_id: Option<String>,
}

/// One row of a rename's plan.
#[derive(Serialize, JsonSchema)]
pub struct Row {
    pub path: String,
    pub name: String,
    /// ok, unchanged, invalid or collision.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

impl Call for Rename {
    type Out = Planned<Vec<Row>>;

    fn run(self, server: &mut Server) -> Result<Reply<Self::Out>, ToolError> {
        let request_id = self.request_id.as_deref();
        if let Some(token) = &self.confirm {
            let Preview::Rename {
                paths,
                rule,
                fingerprint,
                skip_problems,
            } = server.kept(token, self.request_id.as_deref())?
            else {
                return Err(ToolError::invalid("That token is another tool's plan."));
            };
            let request = Request::RenameMany {
                paths,
                rule,
                preview: Some(fingerprint),
                skip_problems,
            };
            let done = server.change(request, request_id)?;
            server.spent(token, self.request_id.as_deref());
            return Ok(done.ran());
        }
        let rule = match (self.name, self.rule) {
            (Some(name), None) => {
                let [path] = self.paths.as_slice() else {
                    return Err(ToolError::invalid(
                        "name renames one path; use a rule for many.",
                    ));
                };
                if let Where::Remote(qualified) = locate(path) {
                    if self.dry_run {
                        return Err(ToolError::new(
                            Code::UnsupportedOnSource,
                            "A server's rename has no dry run: it renames at once, with no undo.",
                        ));
                    }
                    return remote_rename(server, &qualified, &name).map(Reply::ran);
                }
                if !self.dry_run {
                    let path = local_one(path, "rename")?;
                    return server
                        .change(Request::Rename { path, name }, request_id)
                        .map(Reply::ran);
                }
                rename::Rule::Names { names: vec![name] }
            }
            (None, Some(rule)) => rule,
            _ => return Err(ToolError::invalid("Give name or rule, not both.")),
        };
        let paths = local(&self.paths, "rename with a rule")?;
        if !self.dry_run {
            let request = Request::RenameMany {
                paths,
                rule,
                preview: None,
                skip_problems: self.skip_problems,
            };
            return server.change(request, request_id).map(Reply::ran);
        }
        let plan = rename::Plan::new(&paths, &rule);
        let mut text = String::new();
        let rows: Vec<Row> = plan
            .rows
            .iter()
            .map(|row| {
                let (status, why) = match &row.status {
                    rename::Status::Ok => ("ok", None),
                    rename::Status::Unchanged => ("unchanged", None),
                    rename::Status::Invalid(why) => ("invalid", Some(why.clone())),
                    rename::Status::Collision(why) => ("collision", Some(why.clone())),
                };
                let note = match (&why, status) {
                    (Some(why), _) => format!(" ({status}: {why})"),
                    (None, "unchanged") => " (unchanged)".into(),
                    (None, _) => String::new(),
                };
                let _ = writeln!(text, "{} → {}{note}", row.path.display(), row.name);
                Row {
                    path: shown(&row.path),
                    name: row.name.clone(),
                    status: status.into(),
                    why,
                }
            })
            .collect();
        let token = server.keep(Preview::Rename {
            paths,
            rule,
            fingerprint: plan.fingerprint(),
            skip_problems: self.skip_problems,
        });
        let _ = writeln!(text, "token: {token}");
        Ok(Reply::new(text, rows).planned(Some(token)))
    }
}

/// SFTP renames have no undo history: they happen at once.
fn remote_rename(server: &Server, qualified: &str, name: &str) -> Result<Reply<Change>, ToolError> {
    server.can_change()?;
    let (open, at) = server.remote_at(qualified)?;
    let to = at
        .parent()
        .ok_or_else(|| ToolError::invalid(format!("{at} has no name to change.")))?
        .join(name)?;
    open.rename(&at, &to)?;
    Ok(one_change("Rename", at.to_string(), to.to_string()))
}

fn one_change(title: &str, path: String, to: String) -> Reply<Change> {
    let change = Change {
        title: title.into(),
        items: vec![Changed {
            path,
            to: Some(to),
            error: None,
            method: None,
        }],
        failed: 0,
        interrupted: false,
    };
    Reply::new(change.text(), change)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateFolder {
    path: String,
    /// Items to move into the new folder, all on its volume.
    #[serde(default)]
    with: Vec<String>,
    request_id: Option<String>,
}

impl Call for CreateFolder {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        if let Where::Remote(qualified) = locate(&self.path) {
            if !self.with.is_empty() {
                return Err(ToolError::new(
                    Code::UnsupportedOnSource,
                    "with only works on this Mac's files.",
                ));
            }
            server.can_change()?;
            let (open, at) = server.remote_at(&qualified)?;
            open.mkdir(&at)?;
            return Ok(one_change("New Folder", at.to_string(), at.to_string()));
        }
        let path = local_one(&self.path, "create_folder")?;
        let request = if self.with.is_empty() {
            Request::NewFolder { path }
        } else {
            Request::NewFolderWith {
                path,
                items: local(&self.with, "create_folder")?,
            }
        };
        server.change(request, self.request_id.as_deref())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateFile {
    path: String,
    request_id: Option<String>,
}

impl Call for CreateFile {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        let path = local_one(&self.path, "create_file")?;
        server.change(Request::NewFile { path }, self.request_id.as_deref())
    }
}

/// Refuses before anything is written when a copy of any of `paths` into
/// its folder wouldn't clone.
fn require_clone<'a>(
    pairs: impl IntoIterator<Item = (&'a Path, &'a Path)>,
) -> Result<(), ToolError> {
    for (path, into) in pairs {
        if !ops::clones(path, into) {
            return Err(ToolError::new(
                Code::CloneUnavailable,
                format!(
                    "{} can't be cloned into {}: they aren't on one APFS volume.",
                    path.display(),
                    into.display()
                ),
            )
            .at(shown(path)));
        }
    }
    Ok(())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Duplicate {
    paths: Vec<String>,
    /// Fail rather than write a full copy.
    #[serde(default)]
    require_clone: bool,
    request_id: Option<String>,
}

impl Call for Duplicate {
    type Out = Change;

    fn run(self, server: &mut Server) -> Result<Reply<Change>, ToolError> {
        let paths = local(&self.paths, "duplicate")?;
        let beside = |path: &PathBuf| path.parent().unwrap_or(path).to_path_buf();
        let folders: Vec<PathBuf> = paths.iter().map(beside).collect();
        if self.require_clone {
            require_clone(
                paths
                    .iter()
                    .map(PathBuf::as_path)
                    .zip(folders.iter().map(PathBuf::as_path)),
            )?;
        }
        let mut reply = server.change(
            Request::Duplicate {
                paths: paths.clone(),
            },
            self.request_id.as_deref(),
        )?;
        for (item, (path, folder)) in reply.data.items.iter_mut().zip(paths.iter().zip(&folders)) {
            if item.to.is_some() {
                item.method = Some(if ops::clones(path, folder) {
                    Method::Clone
                } else {
                    Method::Copy
                });
            }
        }
        Ok(reply)
    }
}

const fn no_wait() -> u64 {
    0
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Copy {
    paths: Vec<String>,
    /// A folder; id:/path to copy to a server.
    into: String,
    #[serde(default)]
    conflict: Conflict,
    /// Fail rather than write a full copy.
    #[serde(default)]
    require_clone: bool,
    /// Hold the reply until the job ends, up to 60 s.
    #[serde(default = "no_wait")]
    wait_seconds: u64,
    request_id: Option<String>,
}

impl Call for Copy {
    type Out = JobView;

    fn run(self, server: &mut Server) -> Result<Reply<JobView>, ToolError> {
        let remote = |path: &str| matches!(locate(path), Where::Remote(_));
        let to_server = remote(&self.into);
        let from_server = self.paths.iter().any(|path| remote(path));
        if (to_server || from_server) && self.require_clone {
            return Err(ToolError::new(
                Code::CloneUnavailable,
                "A copy to or from a server is always a full copy.",
            ));
        }
        let request = match (from_server, to_server) {
            (false, false) => {
                let paths = local(&self.paths, "copy")?;
                let into = local_one(&self.into, "copy")?;
                if self.require_clone {
                    require_clone(paths.iter().map(|path| (path.as_path(), into.as_path())))?;
                }
                Request::Copy {
                    paths,
                    into,
                    conflict: self.conflict,
                }
            }
            (true, false) => Request::Download {
                paths: self
                    .paths
                    .iter()
                    .map(|path| RemotePath::parse(path))
                    .collect::<Result<_, _>>()?,
                into: local_one(&self.into, "copy")?,
                conflict: self.conflict,
            },
            (false, true) => Request::Upload {
                paths: local(&self.paths, "copy")?,
                into: RemotePath::parse(&self.into)?,
                conflict: self.conflict,
            },
            (true, true) => {
                return Err(ToolError::new(
                    Code::UnsupportedOnSource,
                    "One side of a copy is this Mac: copy between a server and this Mac.",
                ));
            }
        };
        let mut reply = server.queue(request, self.request_id.as_deref(), self.wait_seconds)?;
        if to_server {
            reply.text.push_str(remote::NO_UNDO);
            reply.text.push('\n');
        }
        Ok(reply)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Move {
    paths: Vec<String>,
    into: String,
    #[serde(default)]
    conflict: Conflict,
    /// Hold the reply until the job ends, up to 60 s.
    #[serde(default = "no_wait")]
    wait_seconds: u64,
    request_id: Option<String>,
}

impl Call for Move {
    type Out = JobView;

    fn run(self, server: &mut Server) -> Result<Reply<JobView>, ToolError> {
        let request = Request::Move {
            paths: local(&self.paths, "move")?,
            into: local_one(&self.into, "move")?,
            conflict: self.conflict,
        };
        server.queue(request, self.request_id.as_deref(), self.wait_seconds)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveCreate {
    paths: Vec<String>,
    /// Hold the reply until the job ends, up to 60 s.
    #[serde(default = "no_wait")]
    wait_seconds: u64,
    request_id: Option<String>,
}

impl Call for ArchiveCreate {
    type Out = JobView;

    fn run(self, server: &mut Server) -> Result<Reply<JobView>, ToolError> {
        let request = Request::Compress {
            paths: local(&self.paths, "archive_create")?,
        };
        server.queue(request, self.request_id.as_deref(), self.wait_seconds)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveExtract {
    /// The ZIP.
    path: String,
    into: String,
    /// Entries or folders by their names in the archive; all when empty.
    #[serde(default)]
    entries: Vec<String>,
    #[serde(default)]
    conflict: Conflict,
    /// Hold the reply until the job ends, up to 60 s.
    #[serde(default = "no_wait")]
    wait_seconds: u64,
    request_id: Option<String>,
}

impl Call for ArchiveExtract {
    type Out = JobView;

    fn run(self, server: &mut Server) -> Result<Reply<JobView>, ToolError> {
        let request = Request::Extract {
            archive: local_one(&self.path, "archive_extract")?,
            entries: self.entries,
            into: local_one(&self.into, "archive_extract")?,
            conflict: self.conflict,
        };
        server.queue(request, self.request_id.as_deref(), self.wait_seconds)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Download {
    paths: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveDownload {
    paths: Vec<String>,
}

/// What the cloud was asked for one item.
#[derive(Serialize, JsonSchema)]
pub struct Asking {
    pub path: String,
    /// requested, or unneeded when it's already so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Askings {
    pub items: Vec<Asking>,
}

impl Call for Download {
    type Out = Askings;

    fn run(self, server: &mut Server) -> Result<Reply<Askings>, ToolError> {
        ask_cloud(
            server,
            &self.paths,
            cloud::download,
            "downloading; stat shows when it's done",
            "already downloaded",
        )
    }
}

impl Call for RemoveDownload {
    type Out = Askings;

    fn run(self, server: &mut Server) -> Result<Reply<Askings>, ToolError> {
        ask_cloud(
            server,
            &self.paths,
            cloud::remove_download,
            "removing the download; download brings it back",
            "already not downloaded",
        )
    }
}

/// Asks macOS to download or free each path, a line each saying what it
/// came to.
fn ask_cloud(
    server: &Server,
    paths: &[String],
    ask: fn(&Path) -> Result<Asked, String>,
    requested: &str,
    unneeded: &str,
) -> Result<Reply<Askings>, ToolError> {
    if !server.ops.agents_change_files() {
        return Err(ToolError::new(
            Code::AgentsDisabled,
            "Agents can't download or remove downloads: Let agents change files is off in Monodrive's Agents settings.",
        ));
    }
    let mut lines = Vec::new();
    let mut items = Vec::new();
    for path in local(paths, "download")? {
        let (line, item) = match ask(&path) {
            Ok(asked) => {
                let (state, said) = match asked {
                    Asked::Requested => ("requested", requested),
                    Asked::Unneeded => ("unneeded", unneeded),
                };
                (
                    format!("{}: {said}", path.display()),
                    Asking {
                        path: shown(&path),
                        state: Some(state.into()),
                        error: None,
                    },
                )
            }
            Err(e) => (
                e.clone(),
                Asking {
                    path: shown(&path),
                    state: None,
                    error: Some(e),
                },
            ),
        };
        lines.push(line);
        items.push(item);
    }
    let text = lines.join("\n") + "\n";
    if items.iter().all(|item| item.error.is_some()) {
        return Err(ToolError::from(text));
    }
    Ok(Reply::new(text, Askings { items }))
}
