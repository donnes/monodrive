//! What the kernel and file system charge per call, with no Monodrive code:
//! the floor every Monodrive number sits on, and what a Mac and a PC compare
//! on. Only `libc`, so it builds and runs on Linux too.
//!
//! The fixtures are the suite's own files, so it deletes them with `unlink`,
//! which is also one of the operations it measures.

use crate::format::{Cache, Measured};
use crate::measure;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Files per operation, split evenly among each count of threads.
const FILES: usize = 4_800;
const THREADS: [usize; 6] = [1, 2, 4, 8, 12, 24];
const FILE_BYTES: usize = 1_024;
const SPAWNS: usize = 20;
const FRESH_SPAWNS: usize = 10;
const FLUSHES: usize = 100;
/// The generated `node_modules` that whole-tree clones copy.
#[cfg(target_os = "macos")]
pub const TREE: &str = "node_modules";

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

fn check(result: libc::c_int, what: &str, path: &Path) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        Err(io::Error::new(
            error.kind(),
            format!("{what} {}: {error}", path.display()),
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Create,
    Stat,
    Read,
    Symlink,
    HardLink,
    Clone,
    Copy,
    Delete,
}

impl Op {
    const ALL: [Op; 8] = [
        Op::Create,
        Op::Stat,
        Op::Read,
        Op::Symlink,
        Op::HardLink,
        Op::Clone,
        Op::Copy,
        Op::Delete,
    ];

    fn id(self) -> &'static str {
        match self {
            Op::Create => "create",
            Op::Stat => "stat",
            Op::Read => "read",
            Op::Symlink => "symlink",
            Op::HardLink => "hard-link",
            Op::Clone => "clone",
            Op::Copy => "copy",
            Op::Delete => "delete",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Op::Create => "Create and write a 1 KB file",
            Op::Stat => "lstat",
            Op::Read => "Open, read and close",
            Op::Symlink => "Make a symlink",
            Op::HardLink => "Make a hard link",
            Op::Clone => "Clone a file",
            Op::Copy => "Copy a file by read and write",
            // Each file and the four items made from it.
            Op::Delete => "Delete an item",
        }
    }

    /// Items one pass touches: delete removes the file and its four copies.
    fn items(self) -> usize {
        if self == Op::Delete { FILES * 5 } else { FILES }
    }

    fn run(self, dir: &Path, n: usize, data: &[u8]) -> io::Result<()> {
        let path = |stem: &str| dir.join(format!("{stem}{n}"));
        let file = path("f");
        match self {
            Op::Create => {
                let c = c_path(&file)?;
                // SAFETY: a NUL-terminated path; the fd is closed below.
                let fd = unsafe {
                    libc::open(
                        c.as_ptr(),
                        libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY,
                        0o644,
                    )
                };
                if fd < 0 {
                    return check(-1, "create", &file);
                }
                let wrote = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
                unsafe { libc::close(fd) };
                if wrote != data.len() as isize {
                    return check(-1, "write", &file);
                }
                Ok(())
            }
            Op::Stat => {
                let c = c_path(&file)?;
                // SAFETY: an all-zero stat is valid and lstat writes only into it.
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                check(unsafe { libc::lstat(c.as_ptr(), &mut st) }, "lstat", &file)
            }
            Op::Read => {
                let c = c_path(&file)?;
                let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY) };
                if fd < 0 {
                    return check(-1, "open", &file);
                }
                let mut buf = [0u8; FILE_BYTES];
                let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                unsafe { libc::close(fd) };
                if read != FILE_BYTES as isize {
                    return check(-1, "read", &file);
                }
                Ok(())
            }
            Op::Symlink => {
                let target = CString::new(format!("f{n}")).map_err(io::Error::other)?;
                let link = path("s");
                let c = c_path(&link)?;
                check(
                    unsafe { libc::symlink(target.as_ptr(), c.as_ptr()) },
                    "symlink",
                    &link,
                )
            }
            Op::HardLink => {
                let link = path("h");
                let (from, to) = (c_path(&file)?, c_path(&link)?);
                check(
                    unsafe { libc::link(from.as_ptr(), to.as_ptr()) },
                    "link",
                    &link,
                )
            }
            Op::Clone => clone_file(&file, &path("c")),
            Op::Copy => {
                let to = path("p");
                let (from, dest) = (c_path(&file)?, c_path(&to)?);
                let input = unsafe { libc::open(from.as_ptr(), libc::O_RDONLY) };
                if input < 0 {
                    return check(-1, "open", &file);
                }
                let output = unsafe {
                    libc::open(
                        dest.as_ptr(),
                        libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY,
                        0o644,
                    )
                };
                if output < 0 {
                    unsafe { libc::close(input) };
                    return check(-1, "create", &to);
                }
                let mut buf = [0u8; FILE_BYTES];
                let read = unsafe { libc::read(input, buf.as_mut_ptr().cast(), buf.len()) };
                let wrote =
                    unsafe { libc::write(output, buf.as_ptr().cast(), read.max(0) as usize) };
                unsafe {
                    libc::close(input);
                    libc::close(output);
                }
                if read != FILE_BYTES as isize || wrote != read {
                    return check(-1, "copy", &to);
                }
                Ok(())
            }
            Op::Delete => {
                for stem in ["f", "s", "h", "c", "p"] {
                    let item = path(stem);
                    let c = c_path(&item)?;
                    check(unsafe { libc::unlink(c.as_ptr()) }, "unlink", &item)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn clone_file(from: &Path, to: &Path) -> io::Result<()> {
    let (a, b) = (c_path(from)?, c_path(to)?);
    check(
        unsafe { libc::clonefile(a.as_ptr(), b.as_ptr(), crate::suites::CLONE_NOFOLLOW) },
        "clonefile",
        to,
    )
}

#[cfg(target_os = "linux")]
fn clone_file(from: &Path, to: &Path) -> io::Result<()> {
    let input = std::fs::File::open(from)?;
    let output = std::fs::File::create_new(to)?;
    use std::os::fd::AsRawFd;
    // FICLONE: the whole file as a reflink, where the file system has them.
    const FICLONE: libc::c_ulong = 0x4004_9409;
    check(
        unsafe { libc::ioctl(output.as_raw_fd(), FICLONE, input.as_raw_fd()) },
        "FICLONE",
        to,
    )
}

/// Runs `op` on every file, split among `threads`, each in its own folder.
fn spread(op: Op, dirs: &[PathBuf], data: &[u8]) -> io::Result<()> {
    let per = FILES / dirs.len();
    std::thread::scope(|scope| {
        let workers: Vec<_> = dirs
            .iter()
            .map(|dir| scope.spawn(move || (0..per).try_for_each(|n| op.run(dir, n, data))))
            .collect();
        workers.into_iter().try_for_each(|worker| {
            worker
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("a worker panicked")))
        })
    })
}

fn measured(
    bench: String,
    title: String,
    group: &str,
    items: usize,
    sample: measure::Sample,
    check: String,
) -> Measured {
    Measured {
        bench,
        title,
        group: group.to_string(),
        variant: String::new(),
        items: Some(items as u64),
        cache: Cache::Warm,
        sample,
        check,
        peak_bytes: None,
    }
}

fn file_ops(scratch: &Path, out: &mut Vec<Measured>) -> io::Result<()> {
    let data: Vec<u8> = (0..FILE_BYTES).map(|i| i as u8).collect();
    for threads in THREADS {
        let root = scratch.join(format!("threads-{threads}"));
        std::fs::create_dir(&root)?;
        let dirs: Vec<PathBuf> = (0..threads).map(|i| root.join(format!("w{i}"))).collect();
        for dir in &dirs {
            std::fs::create_dir(dir)?;
        }
        for op in Op::ALL {
            let (sample, done) = measure::here(|| spread(op, &dirs, &data));
            if let Err(error) = done {
                // Clones fail where the file system has none; say so and go on.
                if op == Op::Clone && error.raw_os_error() == Some(libc::ENOTSUP) {
                    return Err(io::Error::other(format!(
                        "this volume can't clone files ({error}); run the platform suite on APFS or btrfs"
                    )));
                }
                return Err(error);
            }
            let left = count(&root)?;
            let expected = if op == Op::Delete {
                0
            } else {
                FILES * op_rank(op)
            };
            if left != expected {
                return Err(io::Error::other(format!(
                    "{}: {left} items after {}, not {expected}",
                    root.display(),
                    op.id()
                )));
            }
            out.push(measured(
                format!("{}/{threads}", op.id()),
                if threads == 1 {
                    "1 thread".into()
                } else {
                    format!("{threads} threads")
                },
                op.title(),
                op.items(),
                sample,
                format!("{left} items left"),
            ));
        }
        for dir in &dirs {
            check(unsafe { libc::rmdir(c_path(dir)?.as_ptr()) }, "rmdir", dir)?;
        }
        check(
            unsafe { libc::rmdir(c_path(&root)?.as_ptr()) },
            "rmdir",
            &root,
        )?;
    }
    Ok(())
}

/// Items each file has made by the end of `op`: the file, a symlink, a hard
/// link, a clone and a copy, in that order.
fn op_rank(op: Op) -> usize {
    match op {
        Op::Create | Op::Stat | Op::Read => 1,
        Op::Symlink => 2,
        Op::HardLink => 3,
        Op::Clone => 4,
        Op::Copy | Op::Delete => 5,
    }
}

fn count(root: &Path) -> io::Result<usize> {
    let mut n = 0;
    for dir in std::fs::read_dir(root)? {
        n += std::fs::read_dir(dir?.path())?.count();
    }
    Ok(n)
}

fn true_binary() -> &'static Path {
    if Path::new("/usr/bin/true").exists() {
        Path::new("/usr/bin/true")
    } else {
        Path::new("/bin/true")
    }
}

fn spawns(scratch: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    let binary = true_binary();
    let run = |path: &Path| -> Result<(), String> {
        match Command::new(path).status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("{}: {status}", path.display())),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    };
    let (sample, done) = measure::here(|| (0..SPAWNS).try_for_each(|_| run(binary)));
    done?;
    out.push(measured(
        "spawn/same".into(),
        format!("Launch {} again", binary.display()),
        "Launch a process",
        SPAWNS,
        sample,
        format!("{SPAWNS} exits of 0"),
    ));
    // A binary at a path never run before: macOS checks a new executable's
    // signature on its first launch.
    let fresh: Vec<PathBuf> = (0..FRESH_SPAWNS)
        .map(|i| scratch.join(format!("fresh-{i}")))
        .collect();
    for path in &fresh {
        std::fs::copy(binary, path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let (sample, done) = measure::here(|| fresh.iter().try_for_each(|path| run(path)));
    done?;
    for path in &fresh {
        check(
            unsafe { libc::unlink(c_path(path).map_err(|e| e.to_string())?.as_ptr()) },
            "unlink",
            path,
        )
        .map_err(|e| e.to_string())?;
    }
    out.push(measured(
        "spawn/fresh".into(),
        "Launch a copy of it written just before".into(),
        "Launch a process",
        FRESH_SPAWNS,
        sample,
        format!("{FRESH_SPAWNS} exits of 0"),
    ));
    Ok(())
}

#[derive(Clone, Copy)]
enum Flush {
    Fsync,
    #[cfg(target_os = "macos")]
    Full,
    #[cfg(target_os = "macos")]
    Barrier,
    #[cfg(target_os = "linux")]
    Data,
}

impl Flush {
    fn all() -> Vec<Flush> {
        #[cfg(target_os = "macos")]
        return vec![Flush::Fsync, Flush::Barrier, Flush::Full];
        #[cfg(target_os = "linux")]
        return vec![Flush::Fsync, Flush::Data];
    }

    fn name(self) -> &'static str {
        match self {
            Flush::Fsync => "fsync",
            #[cfg(target_os = "macos")]
            Flush::Full => "F_FULLFSYNC",
            #[cfg(target_os = "macos")]
            Flush::Barrier => "F_BARRIERFSYNC",
            #[cfg(target_os = "linux")]
            Flush::Data => "fdatasync",
        }
    }

    fn run(self, fd: libc::c_int) -> libc::c_int {
        // SAFETY: an open file descriptor.
        unsafe {
            match self {
                Flush::Fsync => libc::fsync(fd),
                #[cfg(target_os = "macos")]
                Flush::Full => libc::fcntl(fd, libc::F_FULLFSYNC),
                #[cfg(target_os = "macos")]
                Flush::Barrier => libc::fcntl(fd, libc::F_BARRIERFSYNC),
                #[cfg(target_os = "linux")]
                Flush::Data => libc::fdatasync(fd),
            }
        }
    }
}

fn flushes(scratch: &Path, out: &mut Vec<Measured>) -> io::Result<()> {
    let path = scratch.join("flushed");
    let block = [7u8; 4_096];
    for flush in Flush::all() {
        let file = std::fs::File::create_new(&path)?;
        use std::os::fd::AsRawFd;
        let fd = file.as_raw_fd();
        let (sample, done) = measure::here(|| {
            (0..FLUSHES).try_for_each(|_| {
                let wrote = unsafe { libc::pwrite(fd, block.as_ptr().cast(), block.len(), 0) };
                if wrote != block.len() as isize {
                    return check(-1, "pwrite", &path);
                }
                check(flush.run(fd), flush.name(), &path)
            })
        });
        done?;
        drop(file);
        check(
            unsafe { libc::unlink(c_path(&path)?.as_ptr()) },
            "unlink",
            &path,
        )?;
        out.push(measured(
            format!("flush/{}", flush.name()),
            format!("Write 4 KB, then {}", flush.name()),
            "Write and flush",
            FLUSHES,
            sample,
            format!("{FLUSHES} flushes"),
        ));
    }
    Ok(())
}

/// One call for the whole tree, against `copyfile`'s recursive clone, which
/// walks it: what plan 25's whole-folder clones choose between.
#[cfg(target_os = "macos")]
fn tree_clones(fixture: &Path, scratch: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    let tree = fixture.join(TREE);
    let entries = crate::suites::walk_count(&tree).map_err(|e| e.to_string())?;
    let from = c_path(&tree).map_err(|e| e.to_string())?;
    for (id, title, flags) in [
        ("tree/clonefile", "clonefile on the folder", None),
        (
            "tree/copyfile",
            "copyfile with COPYFILE_CLONE and COPYFILE_RECURSIVE",
            Some(libc::COPYFILE_CLONE | libc::COPYFILE_ACL | libc::COPYFILE_RECURSIVE),
        ),
    ] {
        let to = scratch.join(id.replace('/', "-"));
        let dest = c_path(&to).map_err(|e| e.to_string())?;
        let (sample, done) = measure::here(|| unsafe {
            match flags {
                None => {
                    libc::clonefile(from.as_ptr(), dest.as_ptr(), crate::suites::CLONE_NOFOLLOW)
                }
                Some(flags) => {
                    libc::copyfile(from.as_ptr(), dest.as_ptr(), std::ptr::null_mut(), flags)
                }
            }
        });
        check(done, id, &to).map_err(|e| e.to_string())?;
        let copied = crate::suites::walk_count(&to).map_err(|e| e.to_string())?;
        crate::suites::delete_fixture(&to)?;
        if copied != entries {
            return Err(format!("{id}: {copied} entries, not {entries}"));
        }
        out.push(measured(
            id.into(),
            title.into(),
            "Clone a node_modules folder",
            entries,
            sample,
            format!("{copied} entries"),
        ));
    }
    Ok(())
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let scratch = fixture.join(format!("scratch-{}", std::process::id()));
    std::fs::create_dir(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let mut out = Vec::new();
    let done = file_ops(&scratch, &mut out)
        .map_err(|e| e.to_string())
        .and_then(|()| spawns(&scratch, &mut out))
        .and_then(|()| flushes(&scratch, &mut out).map_err(|e| e.to_string()));
    #[cfg(target_os = "macos")]
    let done = done.and_then(|()| tree_clones(fixture, &scratch, &mut out));
    crate::suites::delete_fixture(&scratch)?;
    done.map(|()| out)
}
