//! The user's saved commands: list_commands and run_command. Only the user
//! saves them; agents run what's saved, never a shell string.

use super::{Call, Preview, local};
use crate::mcp::contract::{JobView, Planned, Reply, ToolError, argv, shown};
use crate::mcp::server::Server;
use crate::user_commands::{self, Cwd, Definition, Sources};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListCommands {}

#[derive(Serialize, JsonSchema)]
pub struct Command {
    pub name: String,
    pub program: String,
    /// As saved: {paths} or {path} stands for the paths given.
    pub args: Vec<String>,
    /// files, folders or both.
    pub takes: String,
    /// Where it runs.
    pub cwd: String,
    pub timeout_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Commands {
    pub commands: Vec<Command>,
}

impl Call for ListCommands {
    type Out = Commands;

    fn run(self, server: &mut Server) -> Result<Reply<Commands>, ToolError> {
        let saved = server.places.commands()?;
        let text: String = saved.iter().map(command_line).collect();
        let commands = saved
            .iter()
            .map(|command| Command {
                name: command.name.clone(),
                program: shown(&command.program),
                args: command.args.clone(),
                takes: takes(command).into(),
                cwd: cwd(command),
                timeout_seconds: command.timeout_seconds,
            })
            .collect();
        Ok(Reply::new(text, Commands { commands }))
    }
}

fn takes(command: &Definition) -> &'static str {
    match command.sources {
        Sources::Files => "files",
        Sources::Folders => "folders",
        Sources::Both => "files and folders",
    }
}

fn cwd(command: &Definition) -> String {
    match &command.cwd {
        Cwd::Selection => "the selection's folder".to_string(),
        Cwd::Home => "home".to_string(),
        Cwd::Folder(dir) => dir.display().to_string(),
    }
}

/// A saved command on a line: its name, the program and arguments as
/// saved, what it takes and where it runs.
fn command_line(command: &Definition) -> String {
    let template = argv(
        std::iter::once(command.program.as_os_str())
            .chain(command.args.iter().map(std::ffi::OsStr::new)),
    );
    format!(
        "{}: {template} on {}, in {}, timeout {}s\n",
        command.name,
        takes(command),
        cwd(command),
        command.timeout_seconds
    )
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunCommand {
    /// A saved command's name.
    name: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    dry_run: bool,
    /// A dry run's token: runs exactly its plan.
    confirm: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Invocation {
    pub cwd: String,
    pub argv: Vec<String>,
}

impl Call for RunCommand {
    type Out = Planned<Vec<Invocation>, JobView>;

    fn run(self, server: &mut Server) -> Result<Reply<Self::Out>, ToolError> {
        let (name, paths, token) = match &self.confirm {
            Some(token) => {
                let Preview::Command { name, paths } = server.kept(token, None)? else {
                    return Err(ToolError::invalid("That token is another tool's plan."));
                };
                (name, paths, Some(token.clone()))
            }
            None => {
                let name = self
                    .name
                    .ok_or_else(|| ToolError::invalid("Give the name of a saved command."))?;
                (name, local(&self.paths, "run_command")?, None)
            }
        };
        if self.dry_run {
            let command = server.places.command(&name)?;
            let plan = user_commands::preview(&command, &paths)?;
            let invocations: Vec<Invocation> = plan
                .invocations
                .iter()
                .map(|invocation| Invocation {
                    cwd: shown(&invocation.cwd),
                    argv: invocation
                        .argv
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect(),
                })
                .collect();
            let times = match plan.invocations.len() {
                1 => "once".to_string(),
                n => format!("{n} times, each"),
            };
            let mut text = format!(
                "{} would run {times} for up to {}s. It isn't undoable.\n",
                plan.name, plan.timeout_seconds
            );
            for invocation in &plan.invocations {
                let _ = writeln!(text, "cwd {}", invocation.cwd.display());
                let _ = writeln!(
                    text,
                    "argv {}",
                    argv(invocation.argv.iter().map(|a| a.as_os_str()))
                );
            }
            if !plan.env.is_empty() {
                let env = serde_json::to_string(&plan.env).unwrap_or_default();
                let _ = writeln!(text, "env adds {env}");
            }
            let token = server.keep(Preview::Command { name, paths });
            let _ = writeln!(text, "token: {token}");
            return Ok(Reply::new(text, invocations).planned(Some(token)));
        }
        let origin = server.origin();
        let run = server.ops.run_command(origin, &name, paths)?;
        if let Some(token) = &token {
            server.spent(token, None);
        }
        let started = server
            .ops
            .runs()?
            .into_iter()
            .find(|started| started.id == run)
            .map(|started| JobView::of_run(&started))
            .ok_or_else(|| ToolError::from(format!("No run {run}.")))?;
        let text = format!(
            "{}Commands aren't undoable: Monodrive can't reverse what one changes.\n",
            started.text()
        );
        Ok(Reply::new(text, started).ran())
    }
}
