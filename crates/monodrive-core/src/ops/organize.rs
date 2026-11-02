//! Sorting one folder's files into folders directly inside it, by rules read
//! from names and extended attributes, without opening a file. `propose`
//! changes nothing, and the owner checks every item again when it runs.
//! Folders, links and hidden files stay put, so a repository is never taken
//! apart, and a destination is always one validated name inside the folder.

use super::disk::{self, FileId, Stamp};
use super::{Make, Plan, check_name, plan_into_new, plan_new};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// How many files must share a name's start before they count as a series.
const SERIES: usize = 3;
/// Starts that say which camera, not what the pictures are of.
const CAMERA: [&str; 6] = ["img", "dsc", "dscn", "pxl", "mvimg", "screenshot"];

/// What `propose` found: groups to move, and how many it left in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub folder: PathBuf,
    pub groups: Vec<Group>,
    /// Files no rule placed.
    pub unplaced: usize,
}

/// Items going to one folder inside the folder being sorted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub folder: PathBuf,
    /// Why, in words a person reads.
    pub reason: String,
    pub items: Vec<Member>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub path: PathBuf,
    id: FileId,
    stamp: Stamp,
}

impl Group {
    /// The destination's name.
    pub fn name(&self) -> String {
        self.folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

enum Rule {
    Screenshots,
    Installers,
    Series(String),
    Kind(&'static str, &'static str),
}

impl Rule {
    fn folder(&self) -> String {
        match self {
            Rule::Screenshots => "Screenshots".into(),
            Rule::Installers => "Installers".into(),
            Rule::Series(start) => start.clone(),
            Rule::Kind(folder, _) => (*folder).into(),
        }
    }

    fn reason(&self) -> String {
        match self {
            Rule::Screenshots => "macOS marked these as screenshots".into(),
            Rule::Installers => "Disk images and installers".into(),
            Rule::Series(start) => format!("Names starting with \"{start}\""),
            Rule::Kind(_, what) => (*what).into(),
        }
    }
}

fn kind(extension: &str) -> Option<Rule> {
    let (folder, what) = match extension {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp"
        | "svg" | "raw" | "cr2" | "nef" | "arw" | "dng" => {
            ("Images", "Pictures, by their extension")
        }
        "mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi" | "mpg" | "mpeg" | "3gp" => {
            ("Videos", "Videos, by their extension")
        }
        "mp3" | "m4a" | "aac" | "wav" | "flac" | "aiff" | "ogg" | "opus" => {
            ("Audio", "Audio, by its extension")
        }
        "pdf" | "doc" | "docx" | "pages" | "rtf" | "txt" | "md" | "odt" | "xls" | "xlsx"
        | "numbers" | "csv" | "ppt" | "pptx" | "key" => {
            ("Documents", "Documents, by their extension")
        }
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" => {
            ("Archives", "Archives, by their extension")
        }
        _ => return None,
    };
    Some(Rule::Kind(folder, what))
}

/// The start of a name like `invoice-2026-03`, up to the first digit: its
/// words, if it has any and they aren't a camera's.
fn series_start(name: &str) -> Option<String> {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let digit = stem.find(|c: char| c.is_ascii_digit())?;
    let start = stem[..digit].trim_matches(|c: char| !c.is_alphanumeric());
    let lower = start.to_lowercase();
    (start.chars().count() >= 3 && !CAMERA.contains(&lower.as_str())).then(|| {
        let mut chars = start.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    })
}

/// Whether Spotlight's attribute marks `path` a screenshot. Reading an
/// attribute opens nothing and downloads nothing.
fn is_screenshot(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut value = [0u8; 64];
    // SAFETY: both strings end in NUL, and `value` holds the length given.
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"com.apple.metadata:kMDItemIsScreenCapture".as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    read > 0
}

/// What sorting `folder` would do. Reads the folder and changes nothing.
pub fn propose(folder: &Path) -> Result<Proposal, String> {
    let listed = std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut files = Vec::new();
    let mut existing: HashMap<String, String> = HashMap::new();
    for entry in listed.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if name.starts_with('.') {
            continue;
        }
        if meta.is_dir() {
            existing.insert(name.to_lowercase(), name);
        } else if meta.is_file() {
            files.push((name, entry.path()));
        }
    }
    files.sort();
    let mut starts: HashMap<String, usize> = HashMap::new();
    for (name, _) in &files {
        if let Some(start) = series_start(name) {
            *starts.entry(start).or_default() += 1;
        }
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut unplaced = 0;
    for (name, path) in files {
        let extension = Path::new(&name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let rule = if is_screenshot(&path) {
            Some(Rule::Screenshots)
        } else if ["dmg", "pkg", "mpkg"].contains(&extension.as_str()) {
            Some(Rule::Installers)
        } else if let Some(start) = series_start(&name).filter(|s| starts[s] >= SERIES) {
            Some(Rule::Series(start))
        } else {
            kind(&extension)
        };
        let Some(rule) = rule else {
            unplaced += 1;
            continue;
        };
        let wanted = rule.folder();
        // An existing folder of that name is where they go, as it's spelled.
        let name = existing
            .get(&wanted.to_lowercase())
            .cloned()
            .unwrap_or(wanted);
        if check_name(&name).is_err() {
            unplaced += 1;
            continue;
        }
        let Ok((id, stamp)) = disk::stamp(&path) else {
            unplaced += 1;
            continue;
        };
        let destination = folder.join(&name);
        let member = Member { path, id, stamp };
        match groups.iter_mut().find(|g| g.folder == destination) {
            Some(group) => group.items.push(member),
            None => groups.push(Group {
                folder: destination,
                reason: rule.reason(),
                items: vec![member],
            }),
        }
    }
    groups.sort_by(|a, b| {
        b.items
            .len()
            .cmp(&a.items.len())
            .then(a.folder.cmp(&b.folder))
    });
    Ok(Proposal {
        folder: folder.to_path_buf(),
        groups,
        unplaced,
    })
}

/// The owner's check of one group: its folder is one name inside the folder its
/// items are in, and each item is still what was reviewed.
pub(super) fn plan_group(group: &Group) -> Vec<(PathBuf, Result<Plan, String>)> {
    let inside = group.folder.parent();
    let name_ok = group
        .folder
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| check_name(name).is_ok());
    let meta = std::fs::symlink_metadata(&group.folder);
    let mut plans = Vec::new();
    if !name_ok {
        let why = format!("{} can't be a folder here.", group.folder.display());
        return vec![(group.folder.clone(), Err(why))];
    }
    match meta {
        Ok(meta) if !meta.is_dir() => {
            let why = format!("{} isn't a folder.", group.folder.display());
            return vec![(group.folder.clone(), Err(why))];
        }
        Ok(_) => {}
        Err(_) => plans.push((group.folder.clone(), plan_new(&group.folder, Make::Folder))),
    }
    for item in &group.items {
        let plan = if item.path.parent() != inside {
            Err(format!(
                "{} isn't beside {}.",
                item.path.display(),
                group.folder.display()
            ))
        } else if disk::stamp(&item.path).ok() != Some((item.id, item.stamp)) {
            Err(format!("{} changed since the review.", item.path.display()))
        } else {
            plan_into_new(&item.path, &group.folder)
        };
        plans.push((item.path.clone(), plan));
    }
    plans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touched(names: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().canonicalize().unwrap();
        for name in names {
            std::fs::write(folder.join(name), name).unwrap();
        }
        (dir, folder)
    }

    fn sorted(proposal: &Proposal) -> Vec<(String, Vec<String>)> {
        proposal
            .groups
            .iter()
            .map(|group| {
                let names = group.items.iter().map(|item| {
                    item.path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                });
                (group.name(), names.collect())
            })
            .collect()
    }

    #[test]
    fn each_rule_places_files_and_the_rest_stay() {
        let (_dir, folder) = touched(&[
            "a.jpg",
            "IMG_1.jpg",
            "IMG_2.jpg",
            "IMG_3.jpg",
            "setup.dmg",
            "invoice-2026-01.pdf",
            "invoice-2026-02.pdf",
            "invoice-2026-03.pdf",
            "report.pdf",
            "mystery.xyz",
            ".hidden.jpg",
        ]);
        let proposal = propose(&folder).unwrap();
        assert_eq!(
            sorted(&proposal),
            [
                (
                    "Images".to_string(),
                    vec!["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg", "a.jpg"]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                ),
                (
                    "Invoice".to_string(),
                    vec![
                        "invoice-2026-01.pdf".to_string(),
                        "invoice-2026-02.pdf".to_string(),
                        "invoice-2026-03.pdf".to_string()
                    ]
                ),
                ("Documents".to_string(), vec!["report.pdf".to_string()]),
                ("Installers".to_string(), vec!["setup.dmg".to_string()]),
            ]
        );
        assert_eq!(proposal.unplaced, 1);
    }

    #[test]
    fn folders_and_links_stay_and_an_existing_folder_keeps_its_spelling() {
        let (_dir, folder) = touched(&["a.png"]);
        std::fs::create_dir(folder.join("images")).unwrap();
        std::fs::create_dir_all(folder.join("repo/.git")).unwrap();
        std::os::unix::fs::symlink(folder.join("a.png"), folder.join("link.png")).unwrap();
        let proposal = propose(&folder).unwrap();
        assert_eq!(
            sorted(&proposal),
            [("images".to_string(), vec!["a.png".to_string()])]
        );
    }

    #[test]
    fn a_screenshot_goes_by_its_mark_not_its_extension() {
        let (_dir, folder) = touched(&["shot.png"]);
        let path = CString::new(folder.join("shot.png").as_os_str().as_bytes()).unwrap();
        // SAFETY: both strings end in NUL, and the value's length is given.
        let set = unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"com.apple.metadata:kMDItemIsScreenCapture".as_ptr(),
                b"1".as_ptr().cast(),
                1,
                0,
                0,
            )
        };
        assert_eq!(set, 0);
        assert_eq!(sorted(&propose(&folder).unwrap())[0].0, "Screenshots");
    }
}
