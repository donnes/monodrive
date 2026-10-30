//! The coding agents Settings connects to `monodrive mcp`: how to add the
//! server to each one, by hand or for the user, and whether it's there.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The name agents know the server by.
const NAME: &str = "monodrive";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Cursor,
    OpenCode,
    Amp,
    Pi,
    Gemini,
    Grok,
    Antigravity,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Add,
    Remove,
}

/// How a person adds the server by hand.
pub enum Setup {
    /// A command to run in a terminal.
    Command(String),
    /// JSON to merge into a config file.
    Config { file: &'static str, json: String },
}

/// Where a harness keeps its servers, as a path under the home folder.
enum Registry {
    /// An object of servers under `key` in a JSON file. Monodrive edits it
    /// itself, unless `cli` owns the file: Claude Code rewrites
    /// `~/.claude.json` all the time, so only its CLI may change it.
    Json {
        file: &'static str,
        key: &'static str,
        cli: Option<&'static str>,
    },
    /// `[mcp_servers.<name>]` tables in a TOML file, which `cli` edits.
    Toml {
        file: &'static str,
        cli: &'static str,
    },
}

impl Harness {
    pub const ALL: [Harness; 9] = [
        Harness::ClaudeCode,
        Harness::Codex,
        Harness::Cursor,
        Harness::OpenCode,
        Harness::Amp,
        Harness::Pi,
        Harness::Gemini,
        Harness::Grok,
        Harness::Antigravity,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Codex => "Codex",
            Harness::Cursor => "Cursor",
            Harness::OpenCode => "OpenCode",
            Harness::Amp => "Amp",
            Harness::Pi => "Pi",
            Harness::Gemini => "Gemini CLI",
            Harness::Grok => "Grok",
            Harness::Antigravity => "Antigravity",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "agent-claude",
            Harness::Codex => "agent-codex",
            Harness::Cursor => "agent-cursor",
            Harness::OpenCode => "agent-opencode",
            Harness::Amp => "agent-amp",
            Harness::Pi => "agent-pi",
            Harness::Gemini => "agent-gemini",
            Harness::Grok => "agent-grok",
            Harness::Antigravity => "agent-antigravity",
        }
    }

    /// How to add the server that `exe mcp` runs by hand.
    pub fn setup(self, exe: &str) -> Setup {
        let q = shell_quote(exe);
        // The entry on one line, so the snippet stays short in Settings.
        let snippet = |file, key: &str| Setup::Config {
            file,
            json: format!(
                "{{\n  \"{key}\": {{\n    \"{NAME}\": {}\n  }}\n}}",
                self.entry(exe)
            ),
        };
        match self {
            Harness::ClaudeCode | Harness::Codex | Harness::Grok => {
                let cli = self.cli().unwrap_or_default();
                let line = std::iter::once(cli.to_string())
                    .chain(self.cli_args(Action::Add, exe))
                    .map(|word| shell_quote(&word))
                    .collect::<Vec<_>>()
                    .join(" ");
                Setup::Command(line)
            }
            Harness::Cursor => snippet("~/.cursor/mcp.json", "mcpServers"),
            Harness::OpenCode => {
                Setup::Command(format!("opencode mcp add --global {NAME} -- {q} mcp"))
            }
            Harness::Amp => Setup::Command(format!("amp mcp add {NAME} -- {q} mcp")),
            Harness::Pi => snippet("~/.pi/agent/mcp.json", "mcpServers"),
            Harness::Gemini => {
                Setup::Command(format!("gemini mcp add --scope user {NAME} {q} mcp"))
            }
            Harness::Antigravity => snippet("~/.gemini/config/mcp_config.json", "mcpServers"),
        }
    }

    /// What the Add button does, in a line.
    pub fn adds(self) -> String {
        match self.registry() {
            Registry::Json { cli: Some(cli), .. } | Registry::Toml { cli, .. } => {
                format!("Runs {cli} mcp add for you. Remove takes it out again.")
            }
            Registry::Json { file, .. } => {
                format!("Adds monodrive to ~/{file} for you. Remove takes it out again.")
            }
        }
    }

    /// What to do once the server is added.
    pub fn then(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Run /mcp in Claude Code to check that monodrive is connected.",
            Harness::Codex => "Start a new Codex session. /mcp lists monodrive and its tools.",
            Harness::Cursor => {
                "Restart Cursor, then check that monodrive is on in its MCP settings."
            }
            Harness::OpenCode => "Run opencode mcp list to check that monodrive connects.",
            Harness::Amp => "Start a new Amp thread to load monodrive.",
            Harness::Pi => "Needs Pi 0.99 or later. Run /reload in Pi to load monodrive.",
            Harness::Gemini => "Run /mcp in Gemini CLI to check that monodrive is connected.",
            Harness::Grok => "Run grok mcp doctor monodrive to check that it connects.",
            Harness::Antigravity => {
                "Check that monodrive is listed under Manage MCP Servers in the agent panel."
            }
        }
    }

    /// Whether the harness looks installed under `home`: its config, or the
    /// folder for it, is there. Reads the disk.
    fn present_in(self, home: &Path) -> bool {
        let (Registry::Json { file, .. } | Registry::Toml { file, .. }) = self.registry();
        let config = home.join(file);
        config.exists()
            || config
                .parent()
                .is_some_and(|dir| dir != home && dir.exists())
    }

    fn registry(self) -> Registry {
        let json = |file, key| Registry::Json {
            file,
            key,
            cli: None,
        };
        match self {
            Harness::ClaudeCode => Registry::Json {
                file: ".claude.json",
                key: "mcpServers",
                cli: Some("claude"),
            },
            Harness::Codex => Registry::Toml {
                file: ".codex/config.toml",
                cli: "codex",
            },
            Harness::Cursor => json(".cursor/mcp.json", "mcpServers"),
            Harness::OpenCode => json(".config/opencode/opencode.json", "mcp"),
            Harness::Amp => json(".config/amp/settings.json", "amp.mcpServers"),
            Harness::Pi => json(".pi/agent/mcp.json", "mcpServers"),
            Harness::Gemini => json(".gemini/settings.json", "mcpServers"),
            Harness::Grok => Registry::Toml {
                file: ".grok/config.toml",
                cli: "grok",
            },
            Harness::Antigravity => json(".gemini/config/mcp_config.json", "mcpServers"),
        }
    }

    fn cli(self) -> Option<&'static str> {
        match self.registry() {
            Registry::Json { cli, .. } => cli,
            Registry::Toml { cli, .. } => Some(cli),
        }
    }

    fn cli_args(self, action: Action, exe: &str) -> Vec<String> {
        let scope: &[&str] = match self {
            Harness::ClaudeCode => &["--scope", "user"],
            _ => &[],
        };
        let verb = match action {
            Action::Add => "add",
            Action::Remove => "remove",
        };
        let mut args: Vec<String> = ["mcp", verb]
            .iter()
            .chain(scope)
            .chain(&[NAME])
            .map(|word| word.to_string())
            .collect();
        if action == Action::Add {
            args.extend(["--".into(), exe.into(), "mcp".into()]);
        }
        args
    }

    /// The server's entry in a JSON config.
    fn entry(self, exe: &str) -> Value {
        match self {
            Harness::OpenCode => json!({ "type": "local", "command": [exe, "mcp"] }),
            _ => json!({ "command": exe, "args": ["mcp"] }),
        }
    }

    /// Adds the server that `exe mcp` runs to the harness's user config under
    /// `home`, or removes it. Blocks on the disk and on the harness's CLI.
    pub fn apply(self, action: Action, exe: &str, home: &Path) -> Result<(), String> {
        match self.registry() {
            Registry::Json { cli: Some(cli), .. } | Registry::Toml { cli, .. } => {
                run(cli, &self.cli_args(action, exe))
            }
            Registry::Json { file, key, .. } => edit_json(
                &home.join(file),
                key,
                (action == Action::Add).then(|| self.entry(exe)),
            ),
        }
    }

    /// Whether the harness's user config under `home` has the server. Reads
    /// the disk; a missing or unreadable config counts as not added.
    fn added_in(self, home: &Path) -> bool {
        match self.registry() {
            Registry::Json { file, key, .. } => std::fs::read_to_string(home.join(file))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .is_some_and(|config| config.get(key).and_then(|s| s.get(NAME)).is_some()),
            Registry::Toml { file, .. } => {
                let table = format!("[mcp_servers.{NAME}]");
                std::fs::read_to_string(home.join(file))
                    .is_ok_and(|text| text.lines().any(|line| line.trim() == table))
            }
        }
    }
}

/// Runs a harness's CLI through the user's login shell: an app opened from
/// the Dock gets launchd's PATH, not the one where agent CLIs live.
fn run(cli: &str, args: &[String]) -> Result<(), String> {
    let line = std::iter::once(cli)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let out = Command::new(shell)
        .args(["-ilc", &line])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run {cli}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    if out.status.code() == Some(127) {
        return Err(format!(
            "{cli} isn't installed, or isn't on your shell's PATH."
        ));
    }
    let said = [&out.stderr, &out.stdout]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .last()
        .unwrap_or_else(|| format!("exit status {}", out.status));
    Err(format!("{cli} failed: {said}"))
}

/// Puts `entry` under `key.monodrive` in the JSON config at `path`, or takes
/// it out when there's none, keeping the rest of the file. A file Monodrive
/// can't parse, such as JSON with comments, is left alone.
fn edit_json(path: &Path, key: &str, entry: Option<Value>) -> Result<(), String> {
    let shown = path.display();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Couldn't read {shown}: {e}")),
    };
    let refuse = || format!("{shown} isn't plain JSON, so Monodrive left it alone.");
    let mut config = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(&text).map_err(|_| refuse())?
    };
    let servers = config
        .as_object_mut()
        .ok_or_else(refuse)?
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(refuse)?;
    match entry {
        Some(entry) => {
            servers.insert(NAME.into(), entry);
        }
        None => {
            servers.shift_remove(NAME);
        }
    }
    let mut out = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    out.push('\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create {shown}: {e}"))?;
    }
    // Written in place rather than renamed over, so a config that links into
    // a dotfiles repo stays a link.
    std::fs::write(path, out).map_err(|e| format!("Couldn't write {shown}: {e}"))
}

/// The harnesses whose user config has the server. Reads the disk.
pub fn added() -> Vec<Harness> {
    let Some(home) = home() else {
        return Vec::new();
    };
    Harness::ALL
        .into_iter()
        .filter(|harness| harness.added_in(&home))
        .collect()
}

/// The harnesses that look installed. Reads the disk.
pub fn detected() -> Vec<Harness> {
    let Some(home) = home() else {
        return Vec::new();
    };
    Harness::ALL
        .into_iter()
        .filter(|harness| harness.present_in(&home))
        .collect()
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// This binary, which serves MCP when run with `mcp`.
pub fn exe() -> String {
    std::env::current_exe()
        .map(|exe| exe.display().to_string())
        .unwrap_or_else(|_| NAME.into())
}

pub fn shell_quote(text: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+=:@,%".contains(c);
    if text.is_empty() || !text.chars().all(plain) {
        format!("'{}'", text.replace('\'', r"'\''"))
    } else {
        text.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, file: &str, text: &str) {
        let path = home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_the_server_in_json_and_toml_configs() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        write(
            home,
            ".claude.json",
            r#"{"projects": {}, "mcpServers": {"monodrive": {"command": "monodrive"}}}"#,
        );
        write(home, ".cursor/mcp.json", r#"{"mcpServers": {"other": {}}}"#);
        write(
            home,
            ".config/amp/settings.json",
            r#"{"amp.mcpServers": {"monodrive": {}}}"#,
        );
        write(
            home,
            ".codex/config.toml",
            "[mcp_servers.monodrive_old]\n\n  [mcp_servers.monodrive]\ncommand = \"monodrive\"\n",
        );
        write(home, ".gemini/settings.json", "{ not json");
        let added: Vec<_> = Harness::ALL
            .into_iter()
            .filter(|harness| harness.added_in(home))
            .collect();
        assert_eq!(added, [Harness::ClaudeCode, Harness::Codex, Harness::Amp]);
    }

    #[test]
    fn a_harness_is_detected_by_its_config_or_the_folder_for_it() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let detected = || -> Vec<_> {
            Harness::ALL
                .into_iter()
                .filter(|harness| harness.present_in(home))
                .collect()
        };
        // The home folder itself isn't a sign of Claude Code.
        assert_eq!(detected(), []);
        write(home, ".claude.json", "{}");
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        assert_eq!(detected(), [Harness::ClaudeCode, Harness::Cursor]);
    }

    #[test]
    fn add_and_remove_keep_the_rest_of_a_json_config() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let before = r#"{"theme": "dark", "mcpServers": {"zeta": {"command": "z"}}, "alpha": 1}"#;
        write(home, ".cursor/mcp.json", before);
        let cursor = Harness::Cursor;
        cursor.apply(Action::Add, "/bin/monodrive", home).unwrap();
        assert!(cursor.added_in(home));
        cursor
            .apply(Action::Remove, "/bin/monodrive", home)
            .unwrap();
        assert!(!cursor.added_in(home));
        let after = std::fs::read_to_string(home.join(".cursor/mcp.json")).unwrap();
        let expected = "{\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"zeta\": {\n      \"command\": \"z\"\n    }\n  },\n  \"alpha\": 1\n}\n";
        assert_eq!(after, expected);

        // A config that isn't there yet is created.
        Harness::Pi
            .apply(Action::Add, "/bin/monodrive", home)
            .unwrap();
        assert!(Harness::Pi.added_in(home));
    }

    #[test]
    fn leaves_a_json_config_with_comments_alone() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let text = "{\n  // mine\n  \"mcpServers\": {}\n}\n";
        write(home, ".gemini/settings.json", text);
        let error = Harness::Gemini
            .apply(Action::Add, "/bin/monodrive", home)
            .unwrap_err();
        assert!(error.contains("isn't plain JSON"), "{error}");
        let after = std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap();
        assert_eq!(after, text);
    }
}
