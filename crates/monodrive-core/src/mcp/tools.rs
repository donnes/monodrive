//! Every tool: its name, profile, hints and description, and the module
//! whose `Call` decodes its arguments, calls one service and replies.

mod changes;
mod commands;
mod edits;
mod files;
mod git;
mod jobs;
mod places;
mod searches;
mod space;
mod updates;

use super::contract::{Code, Hints, Profile, Reply, ToolError, input_schema, output_schema};
use super::server::Server;
use crate::expand_home;
use crate::ops::{organize, rename};
use crate::storage::Cleanup;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::PathBuf;

/// A tool's arguments, which run it.
trait Call: DeserializeOwned + JsonSchema {
    type Out: Serialize + JsonSchema;
    fn run(self, server: &mut Server) -> Result<Reply<Self::Out>, ToolError>;
}

/// A tool agents can call, as `tools/list` describes it and Settings lists it.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub profile: Profile,
    pub hints: Hints,
    input: fn() -> Value,
    output: fn() -> Value,
    call: fn(&mut Server, Value) -> Result<Answer, ToolError>,
}

pub(super) struct Answer {
    text: String,
    data: Value,
    image: Option<Vec<u8>>,
}

impl Tool {
    pub fn input_schema(&self) -> Value {
        (self.input)()
    }

    pub fn output_schema(&self) -> Value {
        (self.output)()
    }

    pub(super) fn run(&self, server: &mut Server, arguments: Value) -> Result<Answer, ToolError> {
        (self.call)(server, arguments)
    }
}

const fn tool<C: Call>(
    name: &'static str,
    profile: Profile,
    hints: Hints,
    description: &'static str,
) -> Tool {
    Tool {
        name,
        description,
        profile,
        hints,
        input: input_schema::<C>,
        output: output_schema::<C::Out>,
        call: call::<C>,
    }
}

/// Arguments that don't decode are the agent's to fix, so they're a tool
/// error it reads, not a protocol error.
fn call<C: Call>(server: &mut Server, arguments: Value) -> Result<Answer, ToolError> {
    let args: C =
        serde_json::from_value(arguments).map_err(|e| ToolError::invalid(e.to_string()))?;
    let reply = args.run(server)?;
    let data = serde_json::to_value(reply.data)
        .map_err(|e| ToolError::new(Code::Internal, e.to_string()))?;
    Ok(Answer {
        text: reply.text,
        data,
        image: reply.image,
    })
}

/// `tools/list`'s entries for `profile`.
pub(super) fn list(profile: Profile) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|tool| profile.offers(tool.profile))
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema(),
                "outputSchema": tool.output_schema(),
                "annotations": tool.hints.to_json(),
            })
        })
        .collect()
}

/// A `tools/call` result: the terse text, the typed result, and whether it
/// failed.
pub(super) fn answer(answered: Result<Answer, ToolError>) -> Value {
    match answered {
        Ok(answer) => {
            let text = if answer.text.is_empty() {
                "No results.".to_string()
            } else {
                answer.text
            };
            let content = match answer.image {
                Some(jpeg) => {
                    use base64::Engine as _;
                    json!([{
                        "type": "image",
                        "data": base64::engine::general_purpose::STANDARD.encode(jpeg),
                        "mimeType": "image/jpeg",
                    }])
                }
                None => json!([{"type": "text", "text": text}]),
            };
            json!({"content": content, "structuredContent": answer.data, "isError": false})
        }
        Err(e) => json!({
            "content": [{"type": "text", "text": e.text()}],
            "structuredContent": {"error": e},
            "isError": true,
        }),
    }
}

/// A plan a dry run showed, kept for the confirm that runs exactly it.
#[derive(Clone)]
pub(super) enum Preview {
    Rename {
        paths: Vec<PathBuf>,
        rule: rename::Rule,
        fingerprint: rename::Fingerprint,
        skip_problems: bool,
    },
    Cleanup(Cleanup),
    Organize(Vec<organize::Group>),
    Command {
        name: String,
        paths: Vec<PathBuf>,
    },
    Worktree(crate::ops::Request),
}

/// Where a path argument points.
enum Where {
    Local(PathBuf),
    /// `id:/path` on a saved SSH server, or `id:` for where it starts.
    Remote(String),
    /// A path inside a ZIP: the archive, then the folder in it.
    Archive {
        archive: PathBuf,
        inside: String,
    },
}

/// Local paths start with `/` or `~`; anything else with a colon names a
/// server. A local path that runs through a `.zip` file is inside it.
fn locate(path: &str) -> Where {
    let local = path.starts_with('/') || path.starts_with('~');
    if !local && path.contains(':') {
        return Where::Remote(path.to_owned());
    }
    let full = expand_home(path);
    if !local || full.symlink_metadata().is_ok() {
        return Where::Local(full);
    }
    for archive in full.ancestors().skip(1) {
        let Ok(meta) = archive.symlink_metadata() else {
            continue;
        };
        let zip = archive
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
        if meta.is_file() && zip {
            let inside = full
                .strip_prefix(archive)
                .map(|rest| rest.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Where::Archive {
                archive: archive.to_path_buf(),
                inside,
            };
        }
        break;
    }
    Where::Local(full)
}

/// Every path is local, or this tool can't do it here.
fn local(paths: &[String], tool: &str) -> Result<Vec<PathBuf>, ToolError> {
    paths
        .iter()
        .map(|path| match locate(path) {
            Where::Local(path) => Ok(path),
            Where::Remote(_) => Err(ToolError::new(
                Code::UnsupportedOnSource,
                format!("{tool} works on this Mac's files, not a server's: {path}"),
            )
            .at(path.clone())),
            Where::Archive { .. } => Err(ToolError::new(
                Code::UnsupportedOnSource,
                format!(
                    "{tool} can't reach inside a ZIP: {path}. archive_extract takes items out."
                ),
            )
            .at(path.clone())),
        })
        .collect()
}

fn local_one(path: &str, tool: &str) -> Result<PathBuf, ToolError> {
    local(&[path.to_owned()], tool).map(|mut paths| paths.remove(0))
}

const fn default_limit() -> usize {
    50
}

const fn scan_timeout() -> u64 {
    30
}

const fn short_timeout() -> u64 {
    10
}

use Profile::{All, Core};

pub const TOOLS: [Tool; 54] = [
    // Core: find and read.
    tool::<files::FindFiles>(
        "find_files",
        Core,
        Hints::READ,
        "Find files and folders by name anywhere on disk. Words are fuzzy and typo-tolerant. \
        Filters go inline: kind:image (or video, audio, document, code, archive, font, app, folder, \
        file, link), ext:rs, size:>5mb or 1mb..10mb, modified:7d, >1y, today or >=2026-01-01, \
        in:~/Work, globs such as src/**/*.ts, and !node_modules/. Repeated kind: and ext: widen; \
        other filters narrow. Best matches first.",
    ),
    tool::<files::Grep>(
        "grep",
        Core,
        Hints::READ,
        "Search inside files with the content index. Smart case. mode is literal, regex, or symbol \
        (where a name is defined). path limits it to a folder, filters picks files in find_files \
        syntax. Prints path:line: text, at most 5 lines a file.",
    ),
    tool::<files::ListDir>(
        "list_dir",
        Core,
        Hints::READ.net(),
        "List one folder: on this Mac, on a saved SSH server (id:/path, or id: where it starts), or \
        inside a ZIP (~/a.zip or ~/a.zip/docs). Folders end in /, links in @; (not downloaded) marks \
        cloud placeholders. Names sort as people read them; sort by modified or size with \
        descending: true for newest or largest first.",
    ),
    tool::<files::Stat>(
        "stat",
        Core,
        Hints::READ.net(),
        "Get Info for each path: kind, size, where, volume, dates (UTC), permissions, a link's \
        target, and whether a cloud item is downloaded. Works on id:/path too. A folder's size comes \
        from folder_size.",
    ),
    tool::<files::ReadFile>(
        "read_file",
        Core,
        Hints::READ.net(),
        "Read part of a text file, on this Mac or a saved server (id:/path): limit bytes from \
        offset, 64 KB by default, 1 MB at most. Says so for binary files.",
    ),
    tool::<files::Thumbnail>(
        "thumbnail",
        Core,
        Hints::READ,
        "A JPEG picture of a file: a video's frame, a HEIC or a PDF's first page, from the generator \
        Finder uses. side is the longer edge, 16 to 1024 pixels. Refuses cloud placeholders; .avi, \
        .mkv and .webm fail.",
    ),
    tool::<files::IndexStatus>(
        "index_status",
        Core,
        Hints::READ,
        "Whether the index is ready, how far its first build has got, and whether it can see the \
        whole disk. Never starts indexing.",
    ),
    tool::<space::FolderSize>(
        "folder_size",
        Core,
        Hints::READ,
        "Add up every file under a folder without following links. Counts hard links once and cloud \
        placeholders at their size without downloading them. Stops at timeout_seconds with partial: \
        true.",
    ),
    tool::<space::Overview>(
        "overview",
        Core,
        Hints::READ,
        "What could free space, where the user's things are, and which locations are connected, as \
        the window's Overview shows them. Measures known folders until timeout_seconds and never \
        walks the whole disk. Scan a drive with storage_scan; clean up with trash.",
    ),
    tool::<space::StorageScan>(
        "storage_scan",
        Core,
        Hints::READ,
        "What takes up space in a folder: totals, the largest files, and the folders holding the \
        most. on_disk is the blocks files take; unshared is what trashing them frees once the Trash \
        is emptied. Stops at timeout_seconds with partial: true.",
    ),
    tool::<space::Duplicates>(
        "duplicates",
        Core,
        Hints::READ,
        "Find files with identical contents in a folder. Same-size files are candidates; reading \
        them whole verifies them. Hard links are one file and cloud placeholders are never read. \
        Only verified copies can go to cleanup_duplicates.",
    ),
    // Core: change files.
    tool::<changes::Trash>(
        "trash",
        Core,
        Hints::CHANGE,
        "Move files or folders to the Trash; never deletes. One rename per item, so a big folder \
        such as node_modules goes faster than rm -rf. Prints where each went. put_back or undo \
        restores them. Only a person empties the Trash, so the space stays used until then.",
    ),
    tool::<changes::PutBack>(
        "put_back",
        Core,
        Hints::ADD,
        "Restore items Monodrive moved to the Trash, given their paths in the Trash. Prints where \
        each went back to.",
    ),
    tool::<changes::Undo>(
        "undo",
        Core,
        Hints::CHANGE,
        "Undo the last file change, whether an agent or the Monodrive window made it. Refuses if the \
        files changed since.",
    ),
    tool::<changes::Redo>(
        "redo",
        Core,
        Hints::CHANGE,
        "Redo the last undone file change. Refuses if the files changed since.",
    ),
    tool::<changes::Rename>(
        "rename",
        Core,
        Hints::CHANGE.net(),
        "Rename items where they are, as one change undo reverses: name for one path, or a rule for \
        many. dry_run: true shows old → new and a token; confirm: token runs exactly that. Never \
        replaces an item. On a server (id:/path) only name works.",
    ),
    tool::<changes::CreateFolder>(
        "create_folder",
        Core,
        Hints::ADD.net(),
        "Create a folder at path; its parent must exist. with moves those items into it, on one \
        volume, as one change. Works on id:/path, without with.",
    ),
    tool::<changes::CreateFile>(
        "create_file",
        Core,
        Hints::ADD,
        "Create an empty file at path. Its parent must exist; never replaces a file.",
    ),
    tool::<changes::Duplicate>(
        "duplicate",
        Core,
        Hints::ADD,
        "Copy files or folders beside themselves, named as Finder does (\"name copy\"), keeping \
        metadata. method says whether each cloned; require_clone refuses to write a full copy.",
    ),
    tool::<changes::Copy>(
        "copy",
        Core,
        Hints::ADD.net(),
        "Copy files or folders into a folder, keeping metadata, as a job. Either side may be a saved \
        server (id:/path); copying to one can't be undone. conflict says what to do when a name is \
        taken. method says whether each cloned; require_clone refuses to write a full copy.",
    ),
    tool::<changes::Move>(
        "move",
        Core,
        Hints::CHANGE,
        "Move files or folders into a folder, as a job. Across volumes it copies, checks the copy, \
        then trashes the source.",
    ),
    tool::<changes::ArchiveCreate>(
        "archive_create",
        Core,
        Hints::ADD,
        "Compress items from one folder into a new ZIP beside them, as a job: name.zip for one item, \
        Archive.zip for more, numbered if taken.",
    ),
    tool::<changes::ArchiveExtract>(
        "archive_extract",
        Core,
        Hints::ADD,
        "Extract a ZIP into a folder, as a job: the entries named, or everything, in a folder named \
        for the archive when it holds more than one item at its top. Refuses paths leading out, \
        encrypted entries and zip bombs. list_dir shows what it holds.",
    ),
    tool::<space::CleanupDuplicates>(
        "cleanup_duplicates",
        Core,
        Hints::CHANGE,
        "Move chosen duplicate copies to the Trash. dry_run: true with path and trash scans again, \
        verifies those copies, and returns what's kept and trashed with a token; it refuses a copy \
        that isn't verified and trashing every copy of a group. confirm: token trashes exactly that, \
        once, within 10 minutes; copies that changed since stay.",
    ),
    tool::<changes::Download>(
        "download",
        Core,
        Hints::ADD.net().idempotent(),
        "Ask iCloud Drive or the cloud app holding each path to download it, in the background. \
        copy and other tools that read contents refuse items that aren't downloaded. stat says \
        when each is done.",
    ),
    tool::<changes::RemoveDownload>(
        "remove_download",
        Core,
        Hints::CHANGE.idempotent(),
        "Remove the downloaded copy of each path and keep it in the cloud, as Finder's Remove \
        Download does. Refuses an item that isn't uploaded yet. download brings it back.",
    ),
    // Core: jobs.
    tool::<jobs::JobStatus>(
        "job_status",
        Core,
        Hints::READ,
        "A job's state and each finished item: where it went or why not. Without job, every recent \
        job. wait_seconds waits for the job to end.",
    ),
    tool::<jobs::CancelJob>(
        "cancel_job",
        Core,
        Hints::ADD.idempotent(),
        "Stop a job after the item it's on. Finished items stay; undo reverses a file job's. A \
        command's processes get SIGTERM, then SIGKILL.",
    ),
    tool::<jobs::ContinueJob>(
        "continue_job",
        Core,
        Hints::CHANGE,
        "Continue a file job that's waiting at a conflict, failed or was interrupted, optionally \
        with another conflict choice. Finished items stay; the rest run again.",
    ),
    // All: edits and organize.
    tool::<edits::EditImage>(
        "edit_image",
        All,
        Hints::CHANGE,
        "Edit a picture by steps, each applied to what the last left, in pixels from the top left. \
        Writes `name edited.ext` beside it unless replace, which trashes the original and takes its \
        name. Undo reverses either. Formats macOS can't write, such as WebP, save as PNG and refuse \
        replace. redact fills black and strips metadata.",
    ),
    tool::<edits::EditVideo>(
        "edit_video",
        All,
        Hints::CHANGE,
        "Edit a video by steps. trim and mute need no re-encode, so cuts snap to keyframes; crop and \
        rotate re-encode and can take minutes. Writes `name edited.ext` beside it unless replace, \
        which trashes the original and takes its name. Undo reverses either. .mkv and .webm fail.",
    ),
    tool::<edits::Organize>(
        "organize",
        All,
        Hints::CHANGE,
        "Sort one folder's files into folders inside it, by rules: screenshots, installers, a shared \
        name start, then kind. Folders, links and hidden files stay. dry_run: true with path shows \
        the plan and a token; confirm: token runs exactly it as one change undo reverses.",
    ),
    tool::<edits::SuggestNames>(
        "suggest_names",
        All,
        Hints::READ,
        "Suggest names for text-based files from their contents: a format's own title, a line that \
        reads as one, or the first phrase. Uses no model. Pass the names you accept to rename with a \
        names rule.",
    ),
    // All: places and servers.
    tool::<places::ListPlaces>(
        "list_places",
        All,
        Hints::READ,
        "The sidebar's favorite folders, the mounted volumes with free space, the cloud folders, \
        and the saved servers: smb:// addresses and SSH servers by id. Reads no contents and \
        connects to nothing.",
    ),
    tool::<places::Pin>(
        "pin",
        All,
        Hints::ADD.idempotent(),
        "Add a folder to the sidebar's favorites, or move one there to position at (0 is first). \
        Changes no files.",
    ),
    tool::<places::Unpin>(
        "unpin",
        All,
        Hints::CHANGE.idempotent(),
        "Remove a folder from the sidebar's favorites. The folder stays.",
    ),
    tool::<places::Eject>(
        "eject",
        All,
        Hints::CHANGE.idempotent(),
        "Eject or unmount the volume at path, as Finder does. Refuses the startup disk, one a \
        Monodrive job is using, and one macOS says is busy; never forces.",
    ),
    tool::<places::Connect>(
        "connect",
        All,
        Hints::ADD.net().idempotent(),
        "Mount an smb:// share, or say where it's mounted, and save its address. An \
        sftp://host/folder address saves an SSH server once it connects, browsed as id:/path. Uses \
        credentials macOS and ssh already have; takes no password.",
    ),
    tool::<places::Disconnect>(
        "disconnect",
        All,
        Hints::CHANGE.net().idempotent(),
        "Unmount an smb:// share, or end this session's connection to an SSH server by id. Never \
        forces.",
    ),
    tool::<places::AddServer>(
        "add_server",
        All,
        Hints::ADD,
        "Save an SSH server to browse as id:/path: a host, user@host or an alias in the user's ssh \
        config. Doesn't connect. A server that needs a password reports auth_required.",
    ),
    tool::<places::ForgetServer>(
        "forget_server",
        All,
        Hints::CHANGE.idempotent(),
        "Forget a saved smb:// address or SSH server id. A mounted share stays mounted; nothing on \
        the server changes.",
    ),
    // All: saved searches.
    tool::<searches::ListSearches>(
        "list_searches",
        All,
        Hints::READ,
        "The sidebar's saved searches, in order. Runs none of them.",
    ),
    tool::<searches::SaveSearch>(
        "save_search",
        All,
        Hints::ADD.idempotent(),
        "Save a search in find_files syntax to the sidebar without running it. A taken name is \
        replaced; at moves it (0 is first).",
    ),
    tool::<searches::RenameSearch>(
        "rename_search",
        All,
        Hints::CHANGE,
        "Rename a saved search.",
    ),
    tool::<searches::RemoveSearch>(
        "remove_search",
        All,
        Hints::CHANGE.idempotent(),
        "Remove a saved search from the sidebar. Changes no files.",
    ),
    // All: Git.
    tool::<git::ListWorktrees>(
        "list_worktrees",
        All,
        Hints::READ,
        "The Git worktrees of the repository path is in, the main one first, with branch, HEAD and \
        state. Without path, every repository on the disk with a linked worktree, found through the \
        index, and every registered one. Prunable is Git's view, never a sign a folder is safe to \
        delete. Fetches nothing.",
    ),
    tool::<git::WorktreeStatus>(
        "worktree_status",
        All,
        Hints::READ,
        "One worktree's branch, HEAD and upstream, ahead and behind when fetched, and how many paths \
        are staged, unstaged, conflicted and untracked. Fetches nothing and takes no locks.",
    ),
    tool::<git::RemoveWorktree>(
        "remove_worktree",
        All,
        Hints::CHANGE,
        "Move a linked worktree's folder to the Trash, then the folder Git keeps for it, so Git \
        forgets it; undo brings both back and the branch stays. Refuses the main worktree, a locked \
        one, one with submodules checked out, a link and one holding another worktree. When \
        uncommitted changes or commits nothing else has would go with it, it returns them and a \
        token and moves nothing; dry_run: true always does. confirm: token removes it. Pass repo \
        when the folder is gone.",
    ),
    tool::<git::ListRepos>(
        "list_repos",
        All,
        Hints::READ,
        "The Git repositories registered in Monodrive, marked when unavailable.",
    ),
    tool::<git::RegisterRepo>(
        "register_repo",
        All,
        Hints::ADD.idempotent(),
        "Register the Git repository path is in. Changes nothing in it.",
    ),
    tool::<git::ForgetRepo>(
        "forget_repo",
        All,
        Hints::CHANGE.idempotent(),
        "Forget a registered repository, by the path list_repos gives. Git and the files are \
        untouched.",
    ),
    // All: saved commands and the app.
    tool::<commands::ListCommands>(
        "list_commands",
        All,
        Hints::READ,
        "The commands the user saved in Monodrive: name, program and arguments, what they run on and \
        where. Only the user saves commands.",
    ),
    tool::<commands::RunCommand>(
        "run_command",
        All,
        Hints::CHANGE.net(),
        "Run a saved command on paths, never through a shell, as a job. {paths} becomes every path, \
        {path} runs once per path. dry_run: true shows each invocation's folder and arguments and a \
        token; confirm: token runs exactly that. Not undoable.",
    ),
    tool::<updates::CheckUpdates>(
        "check_updates",
        All,
        Hints::READ.net(),
        "Releases newer than this Monodrive, newest first. channel is stable or nightly, and defaults \
        to the one in Settings. Installs nothing.",
    ),
];
