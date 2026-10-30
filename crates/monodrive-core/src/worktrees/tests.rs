use super::*;
use std::os::unix::fs::PermissionsExt;

/// Runs the fixture's own git in `dir` and returns what it printed.
pub(crate) fn fixture(dir: &Path, args: &[&str]) -> String {
    let out = fixture_output(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// With no configuration but the repository's, and nothing from the
/// environment, such as `GIT_DIR`, that could reach another repository.
fn fixture_output(dir: &Path, args: &[&str]) -> std::process::Output {
    let date = "2026-01-01T00:00:00Z";
    Command::new(Git::find().unwrap().program)
        .current_dir(dir)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Monodrive")
        .env("GIT_AUTHOR_EMAIL", "test@monodrive.invalid")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_NAME", "Monodrive")
        .env("GIT_COMMITTER_EMAIL", "test@monodrive.invalid")
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap()
}

/// A repository at `dir` on `main` with one commit of `a`. Its own config
/// ignores the user's global ignores, which would change what's untracked.
pub(crate) fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    fixture(dir, &["init", "-q", "-b", "main"]);
    fixture(dir, &["config", "core.excludesFile", "/dev/null"]);
    commit(dir, "a", "one\n");
}

pub(crate) fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    fixture(dir, &["add", file]);
    fixture(dir, &["commit", "-q", "-m", file]);
}

pub(crate) fn real_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap();
    (dir, real)
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn found() -> Git {
    Git::find().unwrap()
}

#[test]
fn every_way_into_a_repository_lists_its_worktrees_once_with_their_states() {
    let (_dir, root) = real_tempdir();
    let main = root.join("répo ✓");
    repo(&main);
    let head = fixture(&main, &["rev-parse", "HEAD"]).trim().to_owned();
    let at = |name: &str| root.join(name);
    let add = |args: &[&str]| {
        let mut all = vec!["worktree", "add", "-q"];
        all.extend(args);
        fixture(&main, &all)
    };
    add(&["-b", "feature", "../wt feature"]);
    add(&["--detach", "../detached"]);
    add(&["-b", "locked", "../locked"]);
    fixture(
        &main,
        &["worktree", "lock", "--reason", "on a\ndrive", "../locked"],
    );
    add(&["-b", "nl", "../new\nline"]);
    add(&["-b", "gone", "../gone"]);
    std::fs::rename(at("gone"), at("moved away")).unwrap();
    add(&["-b", "stale", "../stale"]);
    std::fs::rename(at("stale/.git"), at("stale/dotgit")).unwrap();
    add(&["-b", "closed", "../closed"]);
    add(&["-b", "replaced", "../replaced"]);
    std::fs::rename(at("replaced"), at("replaced before")).unwrap();
    repo(&at("replaced"));
    std::fs::create_dir_all(main.join("src/deep")).unwrap();
    std::os::unix::fs::symlink(&main, at("alias")).unwrap();
    chmod(&at("closed"), 0o000);

    let git = found();
    let inventory = list(&git, &main).unwrap();
    for input in [
        main.join("src/deep"),
        main.join(".git"),
        at("wt feature"),
        at("detached"),
        at("alias"),
    ] {
        assert_eq!(list(&git, &input), Ok(inventory.clone()), "{input:?}");
    }
    chmod(&at("closed"), 0o755);

    assert_eq!(inventory.common_dir, main.join(".git"));
    let worktree = |name: &str| {
        let path = at(name);
        let found = inventory.worktrees.iter().find(|w| w.path == path);
        found
            .unwrap_or_else(|| panic!("{path:?} in {inventory:#?}"))
            .clone()
    };
    let linked = |branch: Option<&str>| Worktree {
        path: PathBuf::new(),
        head: Some(head.clone()),
        branch: branch.map(String::from),
        main: false,
        bare: false,
        locked: None,
        prunable: None,
        state: State::Ok,
    };
    let path = |name: &str, w: Worktree| Worktree {
        path: at(name),
        ..w
    };
    assert_eq!(inventory.worktrees.len(), 9);
    assert_eq!(
        inventory.worktrees[0],
        Worktree {
            main: true,
            ..path("répo ✓", linked(Some("main")))
        }
    );
    let gone_reason = Some("gitdir file points to non-existent location".to_owned());
    for (name, expected) in [
        ("wt feature", linked(Some("feature"))),
        ("detached", linked(None)),
        (
            "locked",
            Worktree {
                locked: Some("on a\ndrive".into()),
                ..linked(Some("locked"))
            },
        ),
        ("new\nline", linked(Some("nl"))),
        (
            "gone",
            Worktree {
                prunable: gone_reason.clone(),
                state: State::Missing,
                ..linked(Some("gone"))
            },
        ),
        (
            "stale",
            Worktree {
                prunable: gone_reason.clone(),
                state: State::Stale,
                ..linked(Some("stale"))
            },
        ),
        // Another repository where the worktree was: Git sees a .git there.
        (
            "replaced",
            Worktree {
                state: State::Replaced,
                ..linked(Some("replaced"))
            },
        ),
    ] {
        assert_eq!(worktree(name), path(name, expected));
    }
    // Git calls a folder it can't look into prunable; it's only unreadable.
    let closed = worktree("closed");
    assert!(
        matches!(&closed.state, State::Unreadable(why) if why.contains("Permission denied")),
        "{closed:?}"
    );
}

#[test]
fn a_bare_repository_comes_first_and_its_worktrees_list_it_the_same_way() {
    let (_dir, root) = real_tempdir();
    repo(&root.join("source"));
    fixture(&root, &["clone", "-q", "--bare", "source", "bare.git"]);
    fixture(
        &root.join("bare.git"),
        &["worktree", "add", "-q", "../checkout", "main"],
    );
    let git = found();
    let inventory = list(&git, &root.join("checkout")).unwrap();
    assert_eq!(list(&git, &root.join("bare.git")), Ok(inventory.clone()));
    assert_eq!(inventory.common_dir, root.join("bare.git"));
    let bare = &inventory.worktrees[0];
    assert_eq!(
        (&bare.path, bare.main, bare.bare, &bare.head, &bare.branch),
        (&root.join("bare.git"), true, true, &None, &None)
    );
    let checkout = &inventory.worktrees[1];
    assert_eq!(
        (&checkout.path, checkout.main, checkout.branch.as_deref()),
        (&root.join("checkout"), false, Some("main"))
    );
    assert_eq!(checkout.state, State::Ok);
}

#[test]
fn folders_git_cant_answer_for_are_errors_that_say_why() {
    let (_dir, root) = real_tempdir();
    let (plain, closed, file) = (root.join("plain"), root.join("closed"), root.join("file"));
    for dir in [&plain, &closed] {
        std::fs::create_dir(dir).unwrap();
    }
    std::fs::write(&file, "").unwrap();
    chmod(&closed, 0o000);
    let git = found();
    let errors = [&plain, &closed, &file, &root.join("missing")].map(|dir| list(&git, dir));
    chmod(&closed, 0o755);
    let [plain_error, closed_error, file_error, missing_error] = errors.map(Result::unwrap_err);
    assert_eq!(
        plain_error,
        format!("{} isn't in a Git repository.", plain.display())
    );
    assert!(closed_error.contains("Permission denied"), "{closed_error}");
    assert_eq!(file_error, format!("{} isn't a folder.", file.display()));
    assert!(missing_error.ends_with("doesn't exist."), "{missing_error}");

    // Before its first commit a repository has a branch and no HEAD.
    git_init_only(&root.join("new"));
    let new = list(&git, &root.join("new")).unwrap();
    assert_eq!(new.worktrees.len(), 1);
    assert_eq!(
        (
            new.worktrees[0].head.as_ref(),
            new.worktrees[0].branch.as_deref()
        ),
        (None, Some("main"))
    );
    assert_eq!(status(&git, &root.join("new")).unwrap().head, None);
}

fn git_init_only(dir: &Path) {
    std::fs::create_dir(dir).unwrap();
    fixture(dir, &["init", "-q", "-b", "main"]);
}

#[test]
fn status_counts_changes_and_known_ahead_behind_without_writing_the_index() {
    let (_dir, root) = real_tempdir();
    let main = root.join("repo");
    repo(&main);
    for (file, text) in [("b", "b\n"), ("c", "c\n"), ("d", "d\n")] {
        commit(&main, file, text);
    }
    fixture(
        &main,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    fixture(
        &main,
        &["worktree", "add", "-q", "-b", "conflict", "../conflict"],
    );
    fixture(&main, &["worktree", "add", "-q", "--detach", "../clean"]);
    fixture(
        &main,
        &["branch", "-q", "--set-upstream-to=main", "feature"],
    );
    let (feature, conflict) = (root.join("feature"), root.join("conflict"));
    commit(&feature, "e", "e\n");
    commit(&main, "a", "main's\n");
    commit(&conflict, "a", "conflict's\n");
    let merge = fixture_output(&conflict, &["merge", "-q", "main"]);
    assert!(!merge.status.success(), "the merge conflicts");

    std::fs::write(feature.join("a"), "staged\n").unwrap();
    fixture(&feature, &["add", "a"]);
    std::fs::write(feature.join("a"), "staged, then changed\n").unwrap();
    fixture(&feature, &["mv", "b", "b moved"]);
    std::fs::write(feature.join("c"), "changed\n").unwrap();
    std::fs::write(feature.join("new file"), "").unwrap();
    std::fs::create_dir(feature.join("folder")).unwrap();
    for name in ["1", "2"] {
        std::fs::write(feature.join("folder").join(name), "").unwrap();
    }
    // Same contents, new times: a status that may write refreshes the index.
    std::fs::write(feature.join("d"), "d\n").unwrap();
    let index = PathBuf::from(
        fixture(
            &feature,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )
        .trim(),
    );
    let before = std::fs::read(&index).unwrap();

    let git = found();
    assert_eq!(
        status(&git, &feature),
        Ok(Status {
            head: Some(fixture(&feature, &["rev-parse", "HEAD"]).trim().into()),
            branch: Some("feature".into()),
            upstream: Some("main".into()),
            ahead_behind: Some((1, 1)),
            staged: 2,
            unstaged: 2,
            conflicted: 0,
            untracked: 2,
        })
    );
    assert_eq!(
        std::fs::read(&index).unwrap(),
        before,
        "status wrote the index"
    );

    let conflicted = status(&git, &conflict).unwrap();
    assert_eq!((conflicted.conflicted, conflicted.upstream), (1, None));
    let clean = status(&git, &root.join("clean")).unwrap();
    assert!(clean.is_clean() && clean.branch.is_none(), "{clean:?}");

    // An upstream that was never fetched has no ahead or behind.
    fixture(
        &main,
        &["remote", "add", "origin", "/nonexistent/origin.git"],
    );
    fixture(&main, &["config", "branch.feature.remote", "origin"]);
    let unfetched = status(&git, &feature).unwrap();
    assert_eq!(
        (unfetched.upstream.as_deref(), unfetched.ahead_behind),
        (Some("origin/main"), None)
    );
}

/// A fake git at `dir/name` running `script`.
fn fake(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    chmod(&path, 0o755);
    path
}

#[test]
fn a_git_that_hangs_or_floods_is_stopped_and_reaped() {
    let (_dir, root) = real_tempdir();
    let git = Git {
        program: fake(&root, "hangs", "kill -STOP $$"),
        timeout: Duration::from_millis(100),
    };
    assert_eq!(
        list(&git, &root),
        Err("git didn't finish within 100ms, so Monodrive stopped it.".into())
    );

    let git = Git {
        program: fake(&root, "floods", "echo $$ > \"$0.pid\"\nexec yes"),
        timeout: Duration::from_secs(60),
    };
    assert_eq!(
        status(&git, &root),
        Err("git printed more than 8 MB, so Monodrive stopped it.".into())
    );
    let pid: libc::pid_t = std::fs::read_to_string(root.join("floods.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only asks whether the process exists, which a
    // zombie still does.
    let exists = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!exists, "the git that flooded was killed and reaped");

    // A git that fails while something it started keeps its errors open.
    let script = "sh -c 'kill -STOP $$' >/dev/null &\necho $! > \"$0.pid\"\nexit 1";
    let git = Git {
        program: fake(&root, "fails", script),
        // The stopped child never closes git's errors, so this is spent
        // waiting; it only has to outlast starting the script.
        timeout: Duration::from_secs(1),
    };
    let failed = list(&git, &root);
    let held: libc::pid_t = std::fs::read_to_string(root.join("fails.pid"))
        .unwrap_or_else(|e| panic!("{e}: {failed:?}"))
        .trim()
        .parse()
        .unwrap();
    // SAFETY: kills the stopped process the fake started and recorded.
    unsafe { libc::kill(held, libc::SIGKILL) };
    assert_eq!(failed, Err("git failed: exit status: 1".into()));
}

#[test]
fn status_runs_no_command_the_repository_configures() {
    let (_dir, root) = real_tempdir();
    let marker = root.join("marker");
    let mark = fake(
        &root,
        "mark",
        &format!("echo \"$0 $*\" >> '{}'\ncat", marker.display()),
    );
    let hooks = root.join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    for hook in [
        "post-index-change",
        "reference-transaction",
        "post-checkout",
    ] {
        fake(
            &hooks,
            hook,
            &format!("echo {hook} >> '{}'", marker.display()),
        );
    }
    let (main, sub) = (root.join("repo"), root.join("sub"));
    repo(&sub);
    commit(&sub, ".gitattributes", "* filter=m\n");
    repo(&main);
    commit(&main, ".gitattributes", "* filter=m diff=m\n");
    let file = ["-c", "protocol.file.allow=always"];
    fixture(
        &main,
        &[&file[..], &["submodule", "add", "-q", "../sub", "sub"]].concat(),
    );
    fixture(&main, &["commit", "-q", "-m", "sub"]);
    // Set last, so no fixture command runs them.
    let (mark, hooks) = (mark.to_str().unwrap(), hooks.to_str().unwrap());
    for (key, value) in [
        ("core.fsmonitor", mark),
        ("core.hooksPath", hooks),
        ("core.untrackedCache", "true"),
        ("diff.external", mark),
        ("diff.m.textconv", mark),
        ("filter.m.smudge", mark),
    ] {
        fixture(&main, &["config", key, value]);
    }
    fixture(&main.join("sub"), &["config", "filter.m.clean", mark]);
    // Same contents, new times, so status hashes them again.
    for changed in [main.join("a"), main.join("sub/a")] {
        std::fs::write(changed, "one\n").unwrap();
    }
    let ran = || std::fs::read_to_string(&marker).unwrap_or_default();

    let git = found();
    let clean = status(&git, &main);
    assert_eq!(ran(), "");
    assert!(clean.as_ref().is_ok_and(Status::is_clean), "{clean:?}");

    for key in ["filter.m.clean", "filter.m.process"] {
        fixture(&main, &["config", key, mark]);
    }
    assert_eq!(
        status(&git, &main),
        Err("This repository's own config sets filters that run commands (m), so Monodrive skipped status.".into())
    );
    assert_eq!(ran(), "");
    assert_eq!(list(&git, &main).map(|i| i.worktrees.len()), Ok(1));
}

#[test]
fn registering_keeps_one_bookmark_per_repository_and_forgetting_only_removes_it() {
    let (_dir, root) = real_tempdir();
    let store = places::Store {
        places: root.join("state/places.json"),
        recents: root.join("state/recents.json"),
    };
    let main = root.join("repo");
    repo(&main);
    std::fs::create_dir(main.join("src")).unwrap();
    fixture(
        &main,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    std::os::unix::fs::symlink(&main, root.join("alias")).unwrap();
    fixture(&root, &["clone", "-q", "--bare", "repo", "bare.git"]);

    let git = found();
    let repo = Registration {
        common_dir: main.join(".git"),
        path: main.clone(),
    };
    for input in [
        root.join("feature"),
        main.join("src"),
        root.join("alias"),
        main.clone(),
    ] {
        assert_eq!(register(&git, &store, &input), Ok(repo.clone()));
    }
    let bare = register(&git, &store, &root.join("bare.git")).unwrap();
    assert_eq!(bare.path, root.join("bare.git"));
    assert_eq!(store.repositories(), Ok(vec![repo.clone(), bare.clone()]));
    assert!(register(&git, &store, &root.join("state")).is_err());
    assert_eq!(store.repositories().unwrap().len(), 2);

    assert_eq!(repo.resolve(), Availability::Available(main.clone()));
    std::fs::rename(root.join("bare.git"), root.join("elsewhere.git")).unwrap();
    assert_eq!(
        bare.resolve(),
        Availability::Unavailable(format!("{} no longer exists.", bare.path.display()))
    );

    assert_eq!(store.forget_repository(&bare.path), Ok(bare.clone()));
    assert_eq!(
        store.forget_repository(&root.join("alias")),
        Ok(repo.clone())
    );
    assert!(
        main.join(".git/HEAD").is_file(),
        "forgetting leaves the repository"
    );
    assert_eq!(list(&git, &main).unwrap().worktrees.len(), 2);
    assert_eq!(
        store.forget_repository(&main),
        Err(format!("{} isn't a registered repository.", main.display()))
    );
}
