//! Hands Show in Finder back if Monodrive is deleted while it has it: to the
//! app that had it before, if that's still installed, or else to Finder.
//! Nothing of Monodrive's runs then, and a key naming a missing app stops
//! Show in Finder in every app. A launchd agent checks when the folder the app
//! is in changes and at each login. It runs only system tools, so it outlives
//! the app, and it removes itself once it has run. Its plist also keeps who
//! had the key before, for switching off in Settings.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run by `sh -c` with the seconds to wait, the app, its bundle ID, the
/// agent's plist and its label, and the app that had the key before in
/// `PREVIOUS_VIEWER` and `PREVIOUS_APP`. It changes the key only while it
/// still names this app: someone may have chosen Path Finder since.
const SCRIPT: &str = r#"sleep "$1"
[ -e "$2" ] && exit 0
if [ "$(defaults read -g NSFileViewer 2>/dev/null)" = "$3" ]; then
  if [ -n "$PREVIOUS_VIEWER" ] && [ -e "$PREVIOUS_APP" ]; then
    defaults write -g NSFileViewer -string "$PREVIOUS_VIEWER"
  else
    defaults delete -g NSFileViewer
  fi
fi
rm -f "$4"
launchctl bootout "gui/$(id -u)/$5"
"#;

/// Seconds from the folder changing to looking for the app. The updater
/// swaps the app in one rename; this covers a person replacing it by hand.
const WAIT: &str = "10";

/// The app that had Show in Finder before Monodrive took it, and where it
/// was then. It gets the key back only while it's still there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Previous {
    pub(super) id: String,
    pub(super) app: PathBuf,
}

const PREVIOUS_VIEWER: &str = "PREVIOUS_VIEWER";
const PREVIOUS_APP: &str = "PREVIOUS_APP";

pub(super) struct Guard {
    pub(super) id: String,
    label: String,
    app: PathBuf,
    plist: PathBuf,
}

impl Guard {
    pub(super) fn new(id: String, app: PathBuf, home: &Path) -> Self {
        let label = format!("{id}.show-in-finder");
        let plist = home.join(format!("Library/LaunchAgents/{label}.plist"));
        Self {
            id,
            label,
            app,
            plist,
        }
    }

    /// Writes the agent and loads it, replacing one left from before.
    pub(super) fn install(&self, previous: Option<&Previous>) -> Result<(), String> {
        let failed = |why: std::io::Error| format!("Couldn't add Monodrive's login item: {why}");
        if let Some(folder) = self.plist.parent() {
            std::fs::create_dir_all(folder).map_err(failed)?;
        }
        std::fs::write(&self.plist, self.plist_text(previous)).map_err(failed)?;
        self.unload();
        let loaded = Command::new("/bin/launchctl")
            .arg("bootstrap")
            .arg(domain())
            .arg(&self.plist)
            .output()
            .map_err(failed)?;
        if loaded.status.success() {
            Ok(())
        } else {
            let why = String::from_utf8_lossy(&loaded.stderr);
            Err(format!(
                "Couldn't add Monodrive's login item: {}",
                why.trim()
            ))
        }
    }

    /// Unloads the agent and deletes it, if it's there.
    pub(super) fn remove(&self) {
        self.unload();
        #[expect(
            clippy::disallowed_methods,
            reason = "Monodrive's own launchd agent, which it wrote, not one of the user's files."
        )]
        let _ = std::fs::remove_file(&self.plist);
    }

    /// Who had the key before Monodrive took it, as the agent's plist keeps it.
    pub(super) fn previous(&self) -> Option<Previous> {
        let read = |key: &str| {
            let out = Command::new("/usr/bin/plutil")
                .args(["-extract", &format!("EnvironmentVariables.{key}"), "raw"])
                .args(["-o", "-"])
                .arg(&self.plist)
                .output()
                .ok()?;
            // `raw` ends the value with a newline of its own.
            let value = String::from_utf8(out.stdout).ok()?;
            out.status
                .success()
                .then(|| value.strip_suffix('\n').unwrap_or(&value).to_string())
        };
        Some(Previous {
            id: read(PREVIOUS_VIEWER)?,
            app: read(PREVIOUS_APP)?.into(),
        })
    }

    fn unload(&self) {
        let _ = Command::new("/bin/launchctl")
            .arg("bootout")
            .arg(format!("{}/{}", domain(), self.label))
            .output();
    }

    fn plist_text(&self, previous: Option<&Previous>) -> String {
        let text = |path: &Path| xml(&path.to_string_lossy());
        let environment = previous.map_or_else(String::new, |previous| {
            format!(
                "<key>EnvironmentVariables</key><dict><key>{PREVIOUS_VIEWER}</key><string>{}</string><key>{PREVIOUS_APP}</key><string>{}</string></dict>\n",
                xml(&previous.id),
                text(&previous.app),
            )
        });
        let folder = self.app.parent().unwrap_or(&self.app);
        let arguments = [
            "/bin/sh".to_string(),
            "-c".into(),
            xml(SCRIPT),
            "monodrive-show-in-finder".into(),
            WAIT.into(),
            text(&self.app),
            xml(&self.id),
            text(&self.plist),
            xml(&self.label),
        ]
        .map(|argument| format!("<string>{argument}</string>"))
        .join("");
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>AssociatedBundleIdentifiers</key><array><string>{id}</string></array>
<key>ProgramArguments</key><array>{arguments}</array>
<key>WatchPaths</key><array><string>{folder}</string></array>
<key>RunAtLoad</key><true/>
{environment}</dict></plist>
"#,
            label = xml(&self.label),
            id = xml(&self.id),
            folder = text(folder),
        )
    }
}

/// The user's GUI session, where their launch agents run.
fn domain() -> String {
    // SAFETY: getuid has no preconditions and can't fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_plist_reads_back_with_awkward_paths() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("A & <B>");
        let guard = Guard::new(
            "dev.donnes.monodrive".into(),
            apps.join("Monodrive.app"),
            dir.path(),
        );
        std::fs::create_dir_all(guard.plist.parent().unwrap()).unwrap();
        std::fs::write(&guard.plist, guard.plist_text(None)).unwrap();
        assert_eq!(guard.previous(), None);
        let previous = Previous {
            id: "com.example.a&b".into(),
            app: apps.join("Path <Finder>.app"),
        };
        std::fs::write(&guard.plist, guard.plist_text(Some(&previous))).unwrap();
        assert_eq!(guard.previous(), Some(previous));
        let read = |key: &str| {
            let out = Command::new("/usr/bin/plutil")
                .args(["-extract", key, "raw", "-o", "-"])
                .arg(&guard.plist)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            // `raw` ends the value with a newline of its own.
            let value = String::from_utf8(out.stdout).unwrap();
            value.strip_suffix('\n').unwrap_or(&value).to_string()
        };
        assert_eq!(read("WatchPaths.0"), apps.to_string_lossy());
        assert_eq!(read("ProgramArguments.2"), SCRIPT);
    }

    /// Runs the script with `defaults` and `launchctl` stood in for, so it
    /// reads `key` and logs what it would change.
    fn run(dir: &Path, app: &Path, key: &str, plist: &Path, previous: Option<&Previous>) -> String {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        stub(
            "defaults",
            r#"if [ "$1" = read ]; then printf %s "$KEY"; else echo "defaults $*" >> "$LOG"; fi"#,
        );
        stub("launchctl", r#"echo "launchctl $*" >> "$LOG""#);
        let log = dir.join("log");
        std::fs::write(&log, "").unwrap();
        let ran = Command::new("/bin/sh")
            .args(["-c", SCRIPT, "guard", "0"])
            .args([
                app,
                Path::new("dev.donnes.monodrive"),
                plist,
                Path::new("label"),
            ])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("KEY", key)
            .env("LOG", &log)
            .env(PREVIOUS_VIEWER, previous.map_or("", |p| p.id.as_str()))
            .env(
                PREVIOUS_APP,
                previous.map_or(Path::new(""), |p| p.app.as_path()),
            )
            .status()
            .unwrap();
        assert!(ran.success());
        std::fs::read_to_string(log).unwrap()
    }

    #[test]
    fn the_agent_clears_only_our_key_and_only_once_the_app_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (app, gone) = (
            dir.path().join("Monodrive.app"),
            dir.path().join("Gone.app"),
        );
        let plist = dir.path().join("agent.plist");
        std::fs::create_dir(&app).unwrap();
        let unload = format!("launchctl bootout {}/label\n", domain());

        std::fs::write(&plist, "").unwrap();
        assert_eq!(
            run(dir.path(), &app, "dev.donnes.monodrive", &plist, None),
            ""
        );
        assert!(plist.exists());

        assert_eq!(
            run(dir.path(), &gone, "dev.donnes.monodrive", &plist, None),
            format!("defaults delete -g NSFileViewer\n{unload}")
        );
        assert!(!plist.exists());

        std::fs::write(&plist, "").unwrap();
        assert_eq!(
            run(dir.path(), &gone, "com.cocoatech.PathFinder", &plist, None),
            unload
        );
        assert!(!plist.exists());
    }

    #[test]
    fn the_agent_hands_the_key_back_to_the_app_that_had_it_while_it_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let (gone, plist) = (dir.path().join("Gone.app"), dir.path().join("agent.plist"));
        let path_finder = dir.path().join("Path Finder.app");
        std::fs::create_dir(&path_finder).unwrap();
        let previous = Previous {
            id: "com.cocoatech.PathFinder".into(),
            app: path_finder,
        };
        let unload = format!("launchctl bootout {}/label\n", domain());

        std::fs::write(&plist, "").unwrap();
        assert_eq!(
            run(
                dir.path(),
                &gone,
                "dev.donnes.monodrive",
                &plist,
                Some(&previous)
            ),
            format!("defaults write -g NSFileViewer -string com.cocoatech.PathFinder\n{unload}")
        );

        let uninstalled = Previous {
            app: dir.path().join("Deleted.app"),
            ..previous
        };
        std::fs::write(&plist, "").unwrap();
        assert_eq!(
            run(
                dir.path(),
                &gone,
                "dev.donnes.monodrive",
                &plist,
                Some(&uninstalled)
            ),
            format!("defaults delete -g NSFileViewer\n{unload}")
        );
    }
}
