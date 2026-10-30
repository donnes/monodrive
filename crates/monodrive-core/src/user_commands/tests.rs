use super::*;
use crate::{ops, places};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// A temp folder and its real path, so `pwd -P` matches it.
fn real_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap();
    (dir, real)
}

/// A shell script fixture. Each is written for the test that runs it.
pub(crate) fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A FIFO fixtures block on. Dropping it opens it both ways, which frees
/// any fixture still waiting, so a failed test leaves no process behind that
/// nothing can reach once its temp folder is gone.
pub(crate) struct Fifo(PathBuf);

impl std::ops::Deref for Fifo {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Fifo {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fifo {
    fn drop(&mut self) {
        let open = |read| {
            std::fs::OpenOptions::new()
                .read(read)
                .write(!read)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.0)
        };
        let _both = (open(true), open(false));
    }
}

pub(crate) fn fifo(path: &Path) -> Fifo {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid C string.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    Fifo(path.into())
}

/// Whether anything has the FIFO open, or is waiting to, for reading. The
/// fixtures block reading one nobody writes, so this says whether any of
/// them is left.
fn has_reader(fifo: &Path) -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fifo)
        .is_ok()
}

fn definition(program: &Path, args: &[&str]) -> Definition {
    Definition {
        name: "Test".into(),
        program: program.into(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        cwd: Cwd::Selection,
        sources: Sources::Both,
        env: BTreeMap::new(),
        timeout_seconds: 60,
        output_limit: 64 << 10,
    }
}

/// Starts `plan` as the owner does, returning the registry and the socket
/// a run's end wakes.
fn start(plan: Plan) -> (Arc<Runs>, UnixStream, u64) {
    let (woken, wake) = UnixStream::pair().unwrap();
    let runs = Runs::new(Some(wake));
    let id = runs.start("test".into(), Origin::Window, plan).unwrap();
    (runs, woken, id)
}

fn ended(runs: &Runs, mut woken: UnixStream, id: u64) -> Run {
    woken.read_exact(&mut [0]).unwrap();
    runs.list().into_iter().find(|run| run.id == id).unwrap()
}

fn run(plan: Plan) -> Run {
    let (runs, woken, id) = start(plan);
    ended(&runs, woken, id)
}

/// Splits what an argv-printing fixture printed, an argument and a NUL
/// each.
fn printed(run: &Run) -> Vec<String> {
    assert_eq!(run.output.left_out(), 0);
    let text = format!("{}{}", run.output.head, run.output.tail);
    let mut args: Vec<String> = text.split('\0').map(String::from).collect();
    assert_eq!(args.pop().as_deref(), Some(""));
    args
}

const ODD_NAMES: [&str; 10] = [
    "with space.txt",
    "new\nline.txt",
    "quote's \"double\".txt",
    "$(touch pwned).txt",
    "`touch pwned`.txt",
    "-n",
    "--help",
    "ünïcödé 日本.txt",
    "semi;colon & pipe | gt >.txt",
    "*glob?.txt",
];

#[test]
fn odd_names_reach_the_program_byte_for_byte_as_their_own_arguments() {
    let (_dir, dir) = real_tempdir();
    let files = dir.join("files");
    std::fs::create_dir(&files).unwrap();
    let paths: Vec<PathBuf> = ODD_NAMES.iter().map(|name| files.join(name)).collect();
    for path in &paths {
        std::fs::write(path, "").unwrap();
    }
    let expected: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let argv = script(&dir, "argv", r#"printf '%s\0' "$@""#);

    // {paths}: one invocation, every path its own argument, after the rest.
    let plan = preview(&definition(&argv, &["--", PATHS]), &paths).unwrap();
    assert_eq!(plan.invocations.len(), 1);
    assert_eq!(plan.invocations[0].cwd, files);
    let mut want = vec!["--".to_string()];
    want.extend(expected.iter().cloned());
    let done = run(plan);
    assert_eq!(done.state, RunState::Completed, "{done:?}");
    assert_eq!(printed(&done), want);

    // A shell as the program still gets them as $1 and on, never as code.
    let script_text = r#"printf '%s\0' "$@""#;
    let shell = definition("/bin/sh".as_ref(), &["-c", script_text, "sh", PATHS]);
    assert_eq!(printed(&run(preview(&shell, &paths).unwrap())), expected);

    // {path}: once per path, in order, each with only its own.
    let plan = preview(&definition(&argv, &[PATH]), &paths).unwrap();
    assert_eq!(plan.invocations.len(), paths.len());
    for (invocation, path) in plan.invocations.iter().zip(&paths) {
        let want: Vec<OsString> = vec![argv.clone().into(), path.clone().into()];
        assert_eq!(invocation.argv, want);
    }
    let done = run(plan);
    assert_eq!(done.progress.len(), paths.len());
    assert_eq!(printed(&done), expected);

    let listed: Vec<_> = std::fs::read_dir(&files).unwrap().collect();
    assert_eq!(listed.len(), paths.len(), "nothing ran a name as code");
    assert!(!dir.join("pwned").exists());
}

#[test]
fn preview_finds_the_program_checks_the_selection_and_picks_the_folder() {
    let (_dir, dir) = real_tempdir();
    let (a, b) = (dir.join("a"), dir.join("b"));
    for folder in [&a, &b] {
        std::fs::create_dir(folder).unwrap();
        std::fs::write(folder.join("f.txt"), "").unwrap();
    }
    let (fa, fb) = (a.join("f.txt"), b.join("f.txt"));
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pwd = script(&bin, "pwd-tool", "pwd -P");

    let mut each = definition(&pwd, &[PATH]);
    let plan = preview(&each, &[fa.clone(), fb.clone()]).unwrap();
    let cwds: Vec<&Path> = plan.invocations.iter().map(|i| i.cwd.as_path()).collect();
    assert_eq!(cwds, [a.as_path(), b.as_path()]);
    let done = run(plan);
    assert_eq!(
        done.output.head,
        format!("{}\n{}\n", a.display(), b.display())
    );

    let all = definition(&pwd, &[PATHS]);
    let refused = preview(&all, &[fa.clone(), fb.clone()]).unwrap_err();
    assert!(refused.contains("more than one folder"), "{refused}");
    let fixed = Definition {
        cwd: Cwd::Folder(b.clone()),
        ..all.clone()
    };
    let done = run(preview(&fixed, std::slice::from_ref(&fa)).unwrap());
    assert_eq!(done.output.head, format!("{}\n", b.display()));
    let home = Definition {
        cwd: Cwd::Home,
        ..definition(&pwd, &[])
    };
    let plan = preview(&home, &[]).unwrap();
    let home_dir = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert_eq!(plan.invocations[0].cwd, home_dir);
    assert_eq!(plan.invocations[0].argv, [OsString::from(&pwd)]);

    // A name is looked up on the definition's PATH, or Monodrive's.
    each.program = "pwd-tool".into();
    let missing = preview(&each, std::slice::from_ref(&fa)).unwrap_err();
    assert!(missing.starts_with("pwd-tool isn't on PATH"), "{missing}");
    each.env.insert("PATH".into(), bin.display().to_string());
    let found = preview(&each, std::slice::from_ref(&fa)).unwrap();
    assert_eq!(found.invocations[0].argv[0], OsString::from(&pwd));

    let files_only = Definition {
        sources: Sources::Files,
        ..all.clone()
    };
    let folders_only = Definition {
        sources: Sources::Folders,
        ..all.clone()
    };
    let plain = dir.join("plain");
    std::fs::write(&plain, "not a program").unwrap();
    for (result, error) in [
        (
            preview(&files_only, std::slice::from_ref(&a)),
            "is a folder",
        ),
        (
            preview(&folders_only, std::slice::from_ref(&fa)),
            "isn't a folder",
        ),
        (
            preview(&all, &["a/f.txt".into()]),
            "a/f.txt: give the absolute path",
        ),
        (
            preview(&all, &["sftp://host/f.txt".into()]),
            "sftp://host/f.txt: give the absolute path",
        ),
        (preview(&all, &[a.join("gone")]), "doesn't exist"),
        (preview(&all, &[]), "Select something"),
        (
            preview(&definition(&plain, &[PATHS]), std::slice::from_ref(&fa)),
            "isn't an executable file",
        ),
        (
            preview(
                &Definition {
                    cwd: Cwd::Folder(dir.join("gone")),
                    ..all.clone()
                },
                std::slice::from_ref(&fa),
            ),
            "doesn't exist",
        ),
    ] {
        let got = result.unwrap_err();
        assert!(got.contains(error), "{got}");
    }
}

#[test]
fn definitions_are_checked_when_saved() {
    let sh = Path::new("/bin/sh");
    let with = |change: &dyn Fn(&mut Definition)| {
        let mut definition = definition(sh, &[PATHS]);
        change(&mut definition);
        definition.validated()
    };
    let saved = with(&|d| d.name = "  Spaced  ".into()).unwrap();
    assert_eq!(saved.name, "Spaced");
    for allowed in [
        definition(sh, &["-c", r#"for f; do echo "$f"; done"#, "sh", PATHS]),
        definition(sh, &["--", PATHS]),
        definition(sh, &[PATH, "-n", PATH]),
        definition("tar".as_ref(), &["-c", "-f", "out.tar", PATHS]),
    ] {
        allowed.validated().unwrap();
    }
    let args = |args: &'static [&'static str]| {
        move |d: &mut Definition| d.args = args.iter().map(|a| a.to_string()).collect()
    };
    let program = |program: &'static str| move |d: &mut Definition| d.program = program.into();
    let code = "would run a selected name as code";
    type Change<'a> = &'a dyn Fn(&mut Definition);
    let cases: [(Change, &str); 16] = [
        (&|d| d.name = " \n ".into(), "Name the command"),
        (&program(""), "give a program to run"),
        (&program("bin/tool"), "give the program's absolute path"),
        (&args(&["--in={path}"]), "must be whole arguments"),
        (&args(&[PATHS, PATHS]), "use {paths} once"),
        (&args(&[PATH, PATHS]), "not both"),
        (&args(&["-c", PATH]), code),
        (&args(&["-ec", PATHS]), code),
        (
            &|d| {
                d.program = "python3".into();
                d.args = vec!["-c".into(), PATH.into()];
            },
            code,
        ),
        (
            &|d| {
                d.program = "/usr/bin/osascript".into();
                d.args = vec!["-e".into(), PATHS.into()];
            },
            code,
        ),
        (
            &|d| d.cwd = Cwd::Folder("rel".into()),
            "give the folder's absolute path",
        ),
        (
            &|d| {
                d.env.insert("1X".into(), String::new());
            },
            "isn't an environment variable name",
        ),
        (
            &|d| {
                d.env.insert("GITHUB_TOKEN".into(), "x".into());
            },
            "looks like a secret",
        ),
        (&|d| d.timeout_seconds = 0, "give a timeout"),
        (&|d| d.output_limit = 10, "give an output limit"),
        (&args(&["a\0b"]), "NUL"),
    ];
    for (change, error) in cases {
        let got = with(change).unwrap_err();
        assert!(got.contains(error), "{got}");
    }
}

#[test]
fn definitions_persist_in_places_json_and_concurrent_saves_all_survive() {
    let (_dir, dir) = real_tempdir();
    let store = |dir: &Path| places::Store {
        places: dir.join("state/places.json"),
        recents: dir.join("state/recents.json"),
    };
    let first = store(&dir);
    assert_eq!(first.commands(), Ok(Vec::new()));
    let optimize = Definition {
        name: "Optimize".into(),
        cwd: Cwd::Folder("/tmp".into()),
        sources: Sources::Files,
        ..definition("oxipng".as_ref(), &["-o", "4", PATHS])
    };
    first.save_command(optimize.clone()).unwrap();
    first
        .save_command(Definition {
            name: "Open".into(),
            ..definition("/usr/bin/open".as_ref(), &[PATHS])
        })
        .unwrap();
    // Saving under a name replaces that command where it is.
    let edited = Definition {
        timeout_seconds: 5,
        ..optimize.clone()
    };
    first.save_command(edited.clone()).unwrap();
    let names = |store: &places::Store| -> Vec<String> {
        store
            .commands()
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect()
    };
    assert_eq!(names(&first), ["Optimize", "Open"]);
    assert_eq!(first.command("Optimize"), Ok(edited));
    let json = std::fs::read_to_string(&first.places).unwrap();
    assert!(json.contains("\"folder\": \"/tmp\""), "{json}");
    assert!(json.contains("\"sources\": \"files\""), "{json}");

    let bad = definition("".as_ref(), &[]);
    assert!(first.save_command(bad).is_err());
    // Remote sources don't exist yet, so nothing can ask for them.
    let remote = r#"{"name": "R", "program": "/bin/echo", "sources": "remote"}"#;
    let refused = serde_json::from_str::<Definition>(remote).unwrap_err();
    assert!(refused.to_string().contains("remote"), "{refused}");
    first.remove_command("Open").unwrap();
    assert_eq!(
        first.remove_command("Open"),
        Err("Open isn't a saved command.".into())
    );
    assert_eq!(
        first.command("Open"),
        Err("Open isn't a saved command.".into())
    );

    // Each thread stands in for a process: its own Store, the same file.
    std::thread::scope(|scope| {
        for t in 0..4 {
            let (store, dir) = (store(&dir), &dir);
            scope.spawn(move || {
                for n in 0..5 {
                    let command = Definition {
                        name: format!("c{t}-{n}"),
                        ..definition("/bin/echo".as_ref(), &[PATHS])
                    };
                    store.save_command(command).unwrap();
                    store.pin(dir, None, &[]).unwrap();
                }
            });
        }
    });
    assert_eq!(first.commands().unwrap().len(), 21);
    let favorites = first.places(&[]).unwrap();
    assert!(favorites.iter().any(|place| place.favorite.path == dir));
}

#[test]
fn cancel_stops_the_whole_group_after_the_grace_and_nothing_else() {
    let (_dir, dir) = real_tempdir();
    let (ready, hold, nearby) = (
        fifo(&dir.join("ready")),
        fifo(&dir.join("hold")),
        fifo(&dir.join("nearby")),
    );
    // A process of someone else's, in its own group, that must outlive the
    // cancel and exit 7 once released.
    let mut bystander = Command::new("/bin/sh")
        .args(["-c", r#"read x < "$1"; exit 7"#, "sh"])
        .arg(nearby.as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    // The grandchild ignores SIGTERM, so only SIGKILL after the grace ends
    // it.
    let grandchild = script(
        &dir,
        "grandchild",
        &format!(
            "trap '' TERM\necho up > '{}'\nread x < '{}'",
            ready.display(),
            hold.display()
        ),
    );
    let child = script(
        &dir,
        "child",
        &format!(
            "'{}' &\nread x < '{}'",
            grandchild.display(),
            hold.display()
        ),
    );
    let (one, two) = (dir.join("one"), dir.join("two"));
    for file in [&one, &two] {
        std::fs::write(file, "").unwrap();
    }
    let plan = preview(&definition(&child, &[PATH]), &[one, two]).unwrap();
    let (runs, woken, id) = start(plan);
    assert_eq!(std::fs::read_to_string(&ready).unwrap(), "up\n");
    assert!(runs.busy());
    runs.cancel(id).unwrap();
    let done = ended(&runs, woken, id);
    assert_eq!(done.state, RunState::Cancelled);
    assert_eq!(done.progress.len(), 1, "the second invocation never starts");
    assert_eq!(done.progress[0].ended, Some(Ended::Cancelled));
    assert!(!has_reader(&hold), "the child and grandchild are gone");
    assert!(!runs.busy());
    assert_eq!(runs.cancel(id), Err(format!("Run {id} has already ended.")));

    std::fs::write(&nearby, "go\n").unwrap();
    assert_eq!(bystander.wait().unwrap().code(), Some(7));
}

#[test]
fn what_a_command_leaves_running_in_its_group_stops_when_it_exits() {
    let (_dir, dir) = real_tempdir();
    let (ready, hold) = (fifo(&dir.join("ready")), fifo(&dir.join("hold")));
    let grandchild = script(
        &dir,
        "grandchild",
        &format!(
            "echo up > '{}'\nread x < '{}'",
            ready.display(),
            hold.display()
        ),
    );
    let child = script(
        &dir,
        "child",
        &format!(
            "'{}' &\nread x < '{}'\necho exiting",
            grandchild.display(),
            ready.display()
        ),
    );
    let home = Definition {
        cwd: Cwd::Home,
        ..definition(&child, &[])
    };
    let done = run(preview(&home, &[]).unwrap());
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(done.progress[0].ended, Some(Ended::Exited(0)));
    assert_eq!(done.output.head, "exiting\n");
    assert!(!has_reader(&hold), "the grandchild is gone");
}

#[test]
fn a_timeout_stops_its_invocation_and_the_others_still_run() {
    let (_dir, dir) = real_tempdir();
    let hold = fifo(&dir.join("hold"));
    let tool = script(
        &dir,
        "tool",
        &format!(
            "case \"$1\" in\n*slow) read x < '{}';;\n*bad) exit 3;;\n*signal) kill -KILL $$;;\nesac\necho \"done $1\"",
            hold.display()
        ),
    );
    let paths: Vec<PathBuf> = ["slow", "bad", "signal", "fast"]
        .iter()
        .map(|name| dir.join(name))
        .collect();
    for path in &paths {
        std::fs::write(path, "").unwrap();
    }
    let quick = Definition {
        timeout_seconds: 1,
        ..definition(&tool, &[PATH])
    };
    let done = run(preview(&quick, &paths).unwrap());
    let ends: Vec<Option<Ended>> = done.progress.iter().map(|p| p.ended.clone()).collect();
    assert_eq!(
        ends,
        [
            Some(Ended::TimedOut),
            Some(Ended::Exited(3)),
            Some(Ended::Signaled(libc::SIGKILL)),
            Some(Ended::Exited(0)),
        ]
    );
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.output.head, format!("done {}\n", paths[3].display()));
    assert!(!has_reader(&hold));
}

#[test]
fn output_keeps_its_start_and_end_within_the_limit() {
    let (_dir, dir) = real_tempdir();
    let flood = script(
        &dir,
        "flood",
        "printf START\ni=0\nwhile [ $i -lt 2000 ]; do printf '%0500d' 0; i=$((i+1)); done\nprintf END >&2",
    );
    let small = Definition {
        output_limit: 4 << 10,
        cwd: Cwd::Home,
        ..definition(&flood, &[])
    };
    let done = run(preview(&small, &[]).unwrap());
    let output = &done.output;
    assert_eq!(output.total, 1_000_008);
    assert_eq!(output.kept, 4 << 10);
    assert_eq!(output.left_out(), 1_000_008 - (4 << 10));
    assert_eq!((output.head.len(), output.tail.len()), (2 << 10, 2 << 10));
    assert!(
        output.head.starts_with("START000"),
        "{}",
        &output.head[..10]
    );
    assert!(
        output.tail.ends_with("000END"),
        "errors come in the same stream"
    );
}

#[test]
fn a_program_that_cant_start_says_why_and_the_run_fails() {
    let (_dir, dir) = real_tempdir();
    let orphan = dir.join("orphan");
    std::fs::write(&orphan, "#!/nonexistent/interpreter\n").unwrap();
    std::fs::set_permissions(&orphan, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = Definition {
        cwd: Cwd::Home,
        ..definition(&orphan, &[])
    };
    let done = run(preview(&home, &[]).unwrap());
    assert_eq!(done.state, RunState::Failed);
    let Some(Ended::Failed(why)) = &done.progress[0].ended else {
        panic!("{done:?}");
    };
    assert!(why.starts_with("Couldn't run"), "{why}");
}

#[test]
fn a_resent_request_returns_its_run_instead_of_starting_another() {
    let (_dir, dir) = real_tempdir();
    let ok = script(&dir, "ok", "echo once");
    let plan = preview(
        &Definition {
            cwd: Cwd::Home,
            ..definition(&ok, &[])
        },
        &[],
    )
    .unwrap();
    let (runs, woken, id) = start(plan.clone());
    assert_eq!(runs.start("test".into(), Origin::Window, plan), Ok(id));
    let done = ended(&runs, woken, id);
    assert_eq!(runs.list().len(), 1);
    assert_eq!(done.output.head, "once\n");
    assert_eq!(runs.cancel(7), Err("No run 7.".into()));
}

#[test]
fn the_owner_runs_only_saved_commands_and_agents_only_when_allowed() {
    let (_dir, dir) = real_tempdir();
    let paths = ops::Paths {
        state: dir.join("state"),
        settings: dir.join("settings.json"),
        trash: dir.join("Trash"),
    };
    let store = places::Store {
        places: dir.join("places.json"),
        recents: dir.join("recents.json"),
    };
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
    store
        .save_command(Definition {
            name: "Wait".into(),
            ..definition(&wait, &[PATHS])
        })
        .unwrap();
    let file = dir.join("a b.txt");
    std::fs::write(&file, "").unwrap();

    let owner = ops::Owner::bind(paths.clone()).unwrap();
    let serving = std::thread::spawn(move || owner.serve());
    let mut client = ops::Client::new(paths, None);
    let agent = Origin::Agent {
        client: Some("test".into()),
    };
    assert_eq!(
        client.run_command(Origin::Window, "Nope", vec![file.clone()]),
        Err("Nope isn't a saved command.".into())
    );
    std::fs::write(
        dir.join("settings.json"),
        r#"{"agents_change_files": false}"#,
    )
    .unwrap();
    let refused = client
        .run_command(agent.clone(), "Wait", vec![file.clone()])
        .unwrap_err();
    assert!(
        refused.starts_with("Agents can't run commands"),
        "{refused}"
    );
    std::fs::write(dir.join("settings.json"), "{}").unwrap();
    let id = client
        .run_command(agent, "Wait", vec![file.clone()])
        .unwrap();
    assert_eq!(std::fs::read_to_string(&ready).unwrap(), "up\n");
    let runs = client.runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Running);
    let want: Vec<OsString> = vec![wait.clone().into(), file.clone().into()];
    assert_eq!(runs[0].plan.invocations[0].argv, want);
    client.cancel_run(id).unwrap();
    // The owner stays until its runs end, then exits with its last client.
    drop(client);
    serving.join().unwrap().unwrap();
    assert!(!has_reader(&hold));
}
