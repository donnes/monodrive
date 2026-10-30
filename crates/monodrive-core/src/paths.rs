//! Typed paths: where one leads, what it could complete to, and how to show
//! it short. Go to Folder uses these, so a path pasted from an agent's
//! output opens the right folder.

use crate::entries::{self, Kind, Sort};
use crate::expand_home;
use std::path::{Path, PathBuf};

/// Where a typed path leads: a folder to show, and the entry to select in it
/// when the path named a file.
#[derive(Debug, PartialEq, Eq)]
pub struct Destination {
    pub folder: PathBuf,
    pub select: Option<PathBuf>,
}

/// Resolves `input` as pasted from a terminal or an agent: `~` allowed,
/// surrounding whitespace and quotes ignored, shell escapes (`My\ Folder`)
/// undone, and a relative path taken from `base`. Reads the disk.
pub fn destination(input: &str, base: &Path) -> Result<Destination, String> {
    let trimmed = input.trim().trim_matches(|c| c == '\'' || c == '"');
    if trimmed.is_empty() {
        return Err("Type a path.".into());
    }
    let absolute = |typed: &str| tidy(&base.join(expand_home(typed)));
    let as_typed = absolute(trimmed);
    let path = if as_typed.exists() || !trimmed.contains('\\') {
        as_typed
    } else {
        absolute(&unescape(trimmed))
    };
    let meta = std::fs::metadata(&path).map_err(|_| {
        let name = path.file_name().map_or_else(
            || trimmed.to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        format!("\"{name}\" doesn't exist.")
    })?;
    if meta.is_dir() {
        return Ok(Destination {
            folder: path,
            select: None,
        });
    }
    let folder = path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{trimmed} has no folder."))?;
    Ok(Destination {
        folder,
        select: Some(path),
    })
}

/// `path` without `.` and `..` steps, worked out from the names alone, so
/// links keep the names people know (/tmp stays /tmp).
fn tidy(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `My\ Folder` as `My Folder`: a backslash keeps the next character.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' {
            chars.next().unwrap_or(c)
        } else {
            c
        });
    }
    out
}

/// Paths `input` could be completing: the entries of the folder it's in
/// whose names start with what follows its last `/`, folders first. Reads
/// the disk.
pub fn completions(input: &str, show_hidden: bool, limit: usize) -> Vec<PathBuf> {
    let typed = input.trim();
    let (folder, prefix) = match typed.rfind('/') {
        Some(at) => (&typed[..=at], &typed[at + 1..]),
        None if typed.starts_with('~') => (typed, ""),
        None => return Vec::new(),
    };
    // A hidden name shows once its dot is typed, as in a shell.
    let show_hidden = show_hidden || prefix.starts_with('.');
    let Ok(listing) = entries::list_dir(&expand_home(folder), show_hidden, Sort::default()) else {
        return Vec::new();
    };
    let prefix = prefix.to_lowercase();
    listing
        .into_iter()
        .filter(|entry| entry.name.to_lowercase().starts_with(&prefix))
        .take(limit)
        .map(|entry| {
            // A link to a folder (/tmp, /var) completes like one; only the
            // few shown are checked.
            let folder =
                entry.kind == Kind::Dir || (entry.kind == Kind::Link && entry.path.is_dir());
            let mut path = entry.path;
            if folder {
                // Ready for the next name.
                path.push("");
            }
            path
        })
        .collect()
}

/// `path` with the home folder as `~`, the way people type it.
pub fn abbreviate(path: &Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match shown.strip_prefix(&home) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
            _ => shown,
        },
        _ => shown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_opens_and_a_file_opens_its_folder_selected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.md");
        std::fs::write(&file, "x").unwrap();
        let shown = dir.path().display().to_string();
        let elsewhere = Path::new("/");
        assert_eq!(
            destination(&format!("  '{shown}'  "), elsewhere).unwrap(),
            Destination {
                folder: dir.path().into(),
                select: None
            }
        );
        assert_eq!(
            destination(&file.display().to_string(), elsewhere).unwrap(),
            Destination {
                folder: dir.path().into(),
                select: Some(file.clone())
            }
        );
        // Relative to the folder on screen, as an agent's output often is.
        assert_eq!(
            destination("notes.md", dir.path()).unwrap(),
            Destination {
                folder: dir.path().into(),
                select: Some(file)
            }
        );
        let spaced = dir.path().join("My Folder");
        std::fs::create_dir(&spaced).unwrap();
        assert_eq!(
            destination(&format!("{shown}/My\\ Folder"), elsewhere)
                .unwrap()
                .folder,
            spaced
        );
        assert_eq!(
            destination(&format!("{shown}/My Folder/../notes.md"), elsewhere)
                .unwrap()
                .folder,
            dir.path()
        );
        assert!(destination(&format!("{shown}/missing"), elsewhere).is_err());
        assert!(destination("   ", elsewhere).is_err());
    }

    #[test]
    fn completions_follow_the_last_name_typed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Work")).unwrap();
        std::fs::write(dir.path().join("worklog.txt"), "x").unwrap();
        std::fs::write(dir.path().join(".wrc"), "x").unwrap();
        std::fs::write(dir.path().join("other"), "x").unwrap();
        let base = dir.path().display().to_string();
        let names = |input: &str| -> Vec<String> {
            completions(input, false, 10)
                .iter()
                .map(|p| p.display().to_string().replace(&base, ""))
                .collect()
        };
        assert_eq!(names(&format!("{base}/wo")), ["/Work/", "/worklog.txt"]);
        assert_eq!(names(&format!("{base}/.w")), ["/.wrc"]);
        assert_eq!(names("no-slash"), Vec::<String>::new());
    }
}
