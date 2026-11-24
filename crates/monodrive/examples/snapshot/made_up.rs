//! The made-up Mac behind every picture: its apps, drives, places and onboarding.

use super::*;

/// Volumes that aren't there, each showing something the sidebar can show.
pub(super) struct MadeUp;

pub(super) fn system_app(name: &str) -> MacApp {
    MacApp {
        name: name.into(),
        bundle_id: None,
        path: format!("/System/Applications/{name}.app").into(),
    }
}

/// Real facts about the snapshot's own folder; a total and apps that are the
/// same on every Mac.
impl Native for MadeUp {
    fn info(&self, path: &Path) -> Result<Info, String> {
        info::info(path)
    }

    fn needs_consent(&self, _: &Path) -> bool {
        false
    }

    fn known_size(&self, _: &Path) -> Option<FolderSize> {
        None
    }

    fn measure(&self, _: PathBuf, done: SizeDone) -> Box<dyn Send> {
        done(Ok(FolderSize {
            bytes: 1_284_000_000,
            complete: true,
            ..FolderSize::default()
        }));
        Box::new(())
    }

    fn invalidate(&self, _: &Path) {}

    fn forget_sizes(&self) {}

    fn can_open(&self, _: &Path) -> Vec<MacApp> {
        ["Notes", "Preview", "TextEdit"].map(system_app).to_vec()
    }

    fn default_for(&self, _: &Path) -> Option<MacApp> {
        Some(system_app("Preview"))
    }

    fn open_with(&self, _: &[PathBuf], _: &Path) -> Result<(), String> {
        Ok(())
    }

    fn make_default(&self, _: &Path, _: &Path) -> Result<(), String> {
        Ok(())
    }

    fn choose_app(&self, done: Box<dyn FnOnce(Option<PathBuf>)>) {
        done(None);
    }

    fn installed(&self, _: &str) -> bool {
        true
    }

    /// Every download runs, except a folder's, which fails as a lost
    /// connection would.
    fn download(&self, path: &Path) -> Result<Asked, String> {
        match path.is_dir() {
            true => Err("The Internet connection appears to be offline.".into()),
            false => Ok(Asked::Requested),
        }
    }

    fn remove_download(&self, _: &Path) -> Result<Asked, String> {
        Ok(Asked::Requested)
    }

    fn download_progress(&self, _: &Path) -> Progress {
        Progress::Running
    }
}

impl Drives for MadeUp {
    fn list(&self) -> Vec<Volume> {
        const GB: u64 = 1_000_000_000;
        let volume = |name: &str, total: u64, free: u64| Volume {
            name: name.into(),
            mount: Path::new("/Volumes").join(name),
            disk: None,
            id: Some(VolumeId::Uuid(name.into())),
            total: total * GB,
            free: free * GB,
            read_only: false,
            network: false,
            ejectable: true,
            removable: true,
            startup: false,
            system: None,
        };
        vec![
            Volume {
                mount: "/".into(),
                ejectable: false,
                removable: false,
                startup: true,
                ..volume("Macintosh HD", 494, 178)
            },
            volume("Backup", 2000, 340),
            Volume {
                read_only: true,
                ..volume("Installer", 14, 0)
            },
            Volume {
                network: true,
                ejectable: false,
                removable: false,
                id: Some(VolumeId::Url("smb://nas.local/media".into())),
                ..volume("media", 8000, 5100)
            },
        ]
    }

    fn eject(&self, _: &Path, _: &mut ops::Client) -> Result<String, String> {
        Err("Nothing here ejects.".into())
    }

    fn mount(&self, address: &str, _: &Arc<Cancel>) -> Result<PathBuf, String> {
        Err(format!("{address}: the server didn't answer."))
    }

    fn open(&self, _: &Path, _: &gpui::App) {}

    fn needs_consent(&self, _: &Path) -> bool {
        false
    }

    fn cloud(&self) -> Vec<CloudFolder> {
        let root = Path::new("/Users/you/Library");
        vec![
            CloudFolder {
                name: "iCloud Drive".into(),
                path: root.join("Mobile Documents/com~apple~CloudDocs"),
            },
            CloudFolder {
                name: "Dropbox".into(),
                path: root.join("CloudStorage/Dropbox"),
            },
            CloudFolder {
                name: "GoogleDrive".into(),
                path: root.join("CloudStorage/GoogleDrive-you@example.com"),
            },
        ]
    }

    /// iCloud Drive says what's left; the File Provider apps don't.
    fn cloud_free(&self, folder: &CloudFolder) -> Option<Quota> {
        (folder.name == "iCloud Drive").then_some(Quota::Free(38_400_000_000))
    }

    fn find_files(
        &self,
        _: &OnceLock<Result<Index, String>>,
        query: &str,
        _: usize,
    ) -> Result<Vec<Entry>, String> {
        Ok(["Reports", "report-final.pdf", "report-draft.pdf"]
            .into_iter()
            .filter(|name| name.to_lowercase().contains(&query.to_lowercase()))
            .map(|name| Entry {
                path: Path::new("/Users/you/Documents").join(name),
                name: name.into(),
                kind: if name.contains('.') {
                    Kind::File
                } else {
                    Kind::Dir
                },
                size: 0,
                modified: 0,
                availability: Availability::Unknown,
            })
            .collect())
    }

    fn search<'a>(
        &self,
        _: &dyn Fn() -> Result<&'a Index, String>,
        search: &Search,
        _: usize,
    ) -> Result<Found, String> {
        let pattern = search.text.clone();
        let hit = |name: &str, kind: &str, lines: &[(usize, &str)]| Hit {
            entry: Entry {
                path: Path::new("/Users/you/Work/monodrive").join(kind).join(name),
                name: name.into(),
                kind: Kind::File,
                size: 18_400,
                modified: 1_790_000_000,
                availability: Availability::Unknown,
            },
            lines: if search.looks == Looks::Contents {
                lines
                    .iter()
                    .map(|(line, text)| LineMatch {
                        line: *line,
                        text: (*text).into(),
                        matches: text
                            .match_indices(pattern.as_str())
                            .map(|(at, found)| at..at + found.len())
                            .collect(),
                    })
                    .collect()
            } else {
                Vec::new()
            },
        };
        Ok(Found {
            hits: vec![
                hit(
                    "pane.rs",
                    "src",
                    &[
                        (118, "    // todo: keep the anchor when the tab moves"),
                        (402, "        let todo = self.pending.take();"),
                        (
                            977,
                            "    /// A long line whose match is far along the line, so the start goes: todo",
                        ),
                    ],
                ),
                hit(
                    "notes.md",
                    "docs",
                    &[(12, "- todo: ask about the Trash on network drives")],
                ),
                hit(
                    "release",
                    "scripts",
                    &[
                        (41, "# todo sign the image"),
                        (88, "echo \"todo: notarize\""),
                    ],
                ),
            ],
            complete: false,
        })
    }
}

/// A Mac for onboarding to ask about: no real permission, index or agent is
/// looked at.
pub(super) struct MadeUpSetup {
    granted: bool,
    index_lacks: bool,
}

impl Setup for MadeUpSetup {
    fn full_disk_access(&self) -> bool {
        self.granted
    }

    fn index_lacks_access(&self) -> bool {
        self.index_lacks
    }

    fn saved_index(&self) -> bool {
        false
    }

    fn start_index(&self) -> Result<Index, String> {
        Err("Nothing here starts.".into())
    }

    fn open_privacy_settings(&self, _: &gpui::App) {}

    fn app(&self) -> PathBuf {
        "/Applications/Monodrive.app".into()
    }

    fn relaunch(&self, _: &mut gpui::App) {}

    fn agents(&self) -> (Vec<Harness>, Vec<Harness>) {
        (
            vec![Harness::ClaudeCode, Harness::Codex, Harness::Cursor],
            if self.granted {
                vec![Harness::Codex]
            } else {
                Vec::new()
            },
        )
    }

    fn apply_agent(&self, _: Harness, _: agents::Action) -> Result<(), String> {
        Ok(())
    }

    /// Taken on the granted Mac, so the snapshots show both of its states.
    fn show_in_finder(&self) -> finder::Status {
        finder::Status {
            viewer: if self.granted {
                finder::Viewer::Monodrive
            } else {
                finder::Viewer::Finder
            },
            blocked: None,
            failed: None,
        }
    }

    fn set_show_in_finder(&self, _: bool) -> finder::Status {
        self.show_in_finder()
    }
}

/// Each step of onboarding, in its own window.
pub(super) fn onboarding(cx: &mut Cx, out: &Path, base: &Preferences) -> gpui::Result<()> {
    cx.update(|cx| cx.set_global(base.clone()));
    for (name, step, granted, index_lacks, asked) in [
        ("welcome", Step::Welcome, false, false, false),
        ("checklist", Step::Checklist, false, false, false),
        ("checklist-asked", Step::Checklist, false, false, true),
        ("checklist-granted", Step::Checklist, true, false, false),
        ("checklist-reopen", Step::Checklist, true, true, false),
    ] {
        let setup = Arc::new(MadeUpSetup {
            granted,
            index_lacks,
        });
        let window = cx.open_window(size(px(640.), px(520.)), |_, cx| {
            cx.new(|cx| {
                let mut view = Onboarding::new(step, setup, cx);
                if asked {
                    view.open_settings(cx);
                }
                view
            })
        })?;
        // The logo decodes, then a second pass draws it.
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, _| window.refresh())?;
        }
        cx.run_until_parked();
        let path = out.join(format!("onboarding-{name}.png"));
        cx.capture_screenshot(window.into())?.save(&path)?;
        println!("{}", path.display());
    }
    Ok(())
}

/// A temp folder's places: the default favorites, one on a drive that isn't
/// connected, saved servers, and recents with one file gone.
pub(super) fn made_up_places(dir: &Path, trash: &Path) -> gpui::Result<Places> {
    let dir = dir.canonicalize()?;
    let store = Store {
        places: dir.join("places.json"),
        recents: dir.join("recents.json"),
    };
    let mount = dir.join("Archive");
    let photos = mount.join("Photos");
    std::fs::create_dir_all(&photos)?;
    let archive = Volume {
        name: "Archive".into(),
        mount,
        disk: None,
        id: Some(VolumeId::Uuid("Archive".into())),
        total: 0,
        free: 0,
        read_only: false,
        network: false,
        ejectable: true,
        removable: true,
        startup: false,
        system: None,
    };
    store
        .pin(&photos, None, &[archive])
        .map_err(std::io::Error::other)?;
    for address in ["smb://nas.local/media", "smb://studio.local/projects"] {
        store.save_server(address).map_err(std::io::Error::other)?;
    }
    // build-box is macOS's own sftp-server over pipes, starting in a made-up
    // folder; studio's host key has changed.
    let server = dir.join("srv");
    for folder in ["releases", "www", "logs"] {
        std::fs::create_dir_all(server.join(folder))?;
    }
    for (name, bytes) in [
        ("deploy.log", 1_400_000),
        ("backup.sh", 2_000),
        ("notes.md", 6_000),
    ] {
        std::fs::write(server.join(name), vec![b'x'; bytes])?;
    }
    let start = server.display().to_string();
    for (host, start) in [
        ("build-box", Some(start.as_str())),
        ("me@studio.local", None),
    ] {
        store
            .add_remote(host, None, start)
            .map_err(std::io::Error::other)?;
    }
    for name in ["gone.pdf", "Brief.pdf"] {
        let opened = dir.join(name);
        if name != "gone.pdf" {
            std::fs::write(&opened, "")?;
        }
        store.opened(&opened).map_err(std::io::Error::other)?;
    }
    Ok(Places {
        remote: Arc::new(Connections::new(store.clone(), local_sftp())),
        previews: dir.join("previews"),
        store,
        trash: trash.into(),
        applications: Vec::new(),
        drives: Arc::new(MadeUp),
        root: dir.join("disk"),
    })
}
