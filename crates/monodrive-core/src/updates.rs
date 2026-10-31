//! Finds newer releases on GitHub and installs one over the running app.
//! curl, codesign, spctl, hdiutil and ditto do the work, so an update trusts
//! what macOS trusts: a download only replaces the app once Apple's notary
//! and this team's signature both vouch for it.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const RELEASES: &str = "https://api.github.com/repos/donnes/monodrive/releases?per_page=100";
/// The Developer ID team that signs releases.
const SIGNED_BY_US: &str =
    "=anchor apple generic and certificate leaf[subject.OU] = \"YNNZS92R87\"";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Nightly,
}

impl Default for Channel {
    /// The channel this build came from.
    fn default() -> Self {
        if crate::VERSION.contains("-nightly") {
            Channel::Nightly
        } else {
            Channel::Stable
        }
    }
}

/// What `scripts/release-notes` wrote above the full list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes {
    /// A paragraph on the new features.
    pub highlights: Option<String>,
    /// One line a fix, or a change in notes without the sections.
    pub changes: Vec<String>,
    /// Such as "12 changes from 2 contributors".
    pub stats: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// When it was published, as RFC 3339.
    pub published: String,
    pub notes: Notes,
    /// Its page on GitHub.
    pub page: String,
    pub disk_image: String,
}

/// A release as GitHub lists it.
#[derive(Deserialize)]
struct Listed {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The releases on `channel` newer than this build, newest first. Blocks on
/// the network, so call it off the UI thread.
pub fn pending(channel: Channel) -> Result<Vec<Release>, String> {
    let listed = run(Command::new("/usr/bin/curl").args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--max-time",
        "30",
        "--header",
        "Accept: application/vnd.github+json",
        RELEASES,
    ]))?;
    let listed: Vec<Listed> = serde_json::from_slice(&listed).map_err(|e| e.to_string())?;
    let current = Version::parse(crate::VERSION).map_err(|e| e.to_string())?;
    Ok(newer(listed, channel, &current))
}

fn newer(listed: Vec<Listed>, channel: Channel, current: &Version) -> Vec<Release> {
    let mut releases: Vec<Release> = listed
        .into_iter()
        .filter(|release| !release.draft && (channel == Channel::Nightly || !release.prerelease))
        .filter_map(|release| {
            let version = Version::parse(release.tag_name.strip_prefix('v')?).ok()?;
            if version <= *current {
                return None;
            }
            let image = format!("Monodrive-{version}-arm64.dmg");
            let disk_image = release
                .assets
                .into_iter()
                .find(|asset| asset.name == image)?
                .browser_download_url;
            Some(Release {
                version,
                published: release.published_at.unwrap_or_default(),
                notes: notes(release.body.as_deref().unwrap_or_default()),
                page: release.html_url,
                disk_image,
            })
        })
        .collect();
    releases.sort_by(|a, b| b.version.cmp(&a.version));
    releases
}

/// Reads a release's body: `scripts/release-notes`' header, highlights and
/// fixes, or GitHub's generated list on an older release.
fn notes(body: &str) -> Notes {
    let summary = body.split("<details>").next().unwrap_or(body);
    let mut notes = Notes::default();
    let mut highlights = Vec::new();
    for line in summary.lines().map(str::trim) {
        if line.starts_with("## New Contributors") {
            break;
        }
        if line.starts_with("**v") {
            notes.stats = line.split_once(" · ").map(|(_, stats)| stats.to_string());
        } else if let Some(change) = line.strip_prefix("- ").or(line.strip_prefix("* ")) {
            let change = change.split(" by @").next().unwrap_or(change);
            notes.changes.push(without_pull_request(change).to_string());
        } else if !line.is_empty() && !line.starts_with('#') && !line.starts_with("**") {
            highlights.push(line);
        }
    }
    notes.highlights = (!highlights.is_empty()).then(|| highlights.join(" "));
    notes
}

/// A line without its ending " (#123)", which only means something on GitHub.
fn without_pull_request(line: &str) -> &str {
    line.rsplit_once(" (#")
        .filter(|(_, end)| {
            end.strip_suffix(')')
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map_or(line, |(line, _)| line)
}

/// The channel Settings saved. A missing or unreadable file means the default.
pub fn saved_channel() -> Channel {
    #[derive(Deserialize)]
    struct Settings {
        #[serde(default)]
        update_channel: Channel,
    }
    crate::support_dir()
        .and_then(|dir| std::fs::read(dir.join("settings.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .map(|settings| settings.update_channel)
        .unwrap_or_default()
}

/// The app bundle this process runs from, if it's a release that can update
/// itself. Debug and ad hoc builds aren't. Runs codesign, so call it off the
/// UI thread.
pub fn installed_app() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.ancestors().nth(3)?;
    (app.extension() == Some(OsStr::new("app")) && signed_by_us(app).is_ok()).then(|| app.into())
}

fn signed_by_us(path: &Path) -> Result<(), String> {
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "-R", SIGNED_BY_US])
        .arg(path))
    .map(drop)
}

/// Downloads `release` and swaps it in for `app`, which takes effect on the
/// next launch. Nothing changes unless every check passes. Blocks on the
/// network and the disk, so call it off the UI thread.
pub fn install(release: &Release, app: &Path) -> Result<(), String> {
    let work = crate::support_dir()
        .ok_or("HOME isn't set.")?
        .join("Updates");
    let staged = app.with_file_name(format!(".Monodrive-{}.app", release.version));
    clear(&work)?;
    clear(&staged)?;
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = stage(release, &work, &staged).and_then(|()| swap(&staged, app));
    // After a swap, this is the old app. Monodrive made every path here.
    let cleaned = clear(&staged).and_then(|()| clear(&work));
    result.and(cleaned)
}

fn stage(release: &Release, work: &Path, staged: &Path) -> Result<(), String> {
    let image = work.join("Monodrive.dmg");
    run(Command::new("/usr/bin/curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--proto", "=https", "--output"])
        .arg(&image)
        .arg(&release.disk_image))?;
    signed_by_us(&image)?;
    // The same check `scripts/release` ends on: Apple notarized this image.
    run(Command::new("/usr/sbin/spctl")
        .args(["--assess", "--type", "open"])
        .args(["--context", "context:primary-signature"])
        .arg(&image))?;

    let mount = work.join("mount");
    run(Command::new("/usr/bin/hdiutil")
        .args(["attach", "-quiet", "-nobrowse", "-readonly", "-noautoopen"])
        .arg("-mountpoint")
        .arg(&mount)
        .arg(&image))?;
    let copied = run(Command::new("/usr/bin/ditto")
        .arg(mount.join("Monodrive.app"))
        .arg(staged));
    let detached = run(Command::new("/usr/bin/hdiutil")
        .args(["detach", "-quiet", "-force"])
        .arg(&mount));
    copied?;
    detached?;

    signed_by_us(staged)?;
    // --version opens no window, so it doesn't start the index.
    let reported = run(Command::new(staged.join("Contents/MacOS/monodrive")).arg("--version"))?;
    let reported = String::from_utf8_lossy(&reported);
    if reported.trim() != format!("Monodrive {}", release.version) {
        return Err(format!(
            "The download reports \"{}\", expected {}.",
            reported.trim(),
            release.version
        ));
    }
    Ok(())
}

/// Swaps two paths at once, so the app is whole at every moment.
fn swap(a: &Path, b: &Path) -> Result<(), String> {
    let path = |path: &Path| CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string());
    let (a, b) = (path(a)?, path(b)?);
    // SAFETY: both are NUL-terminated strings that outlive the call.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "Only the updater's own downloads and the app it just replaced come here, none of them the user's files."
)]
fn clear(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Runs a system tool to its end and returns what it printed, or what it said
/// went wrong.
fn run(command: &mut Command) -> Result<Vec<u8>, String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let error = String::from_utf8_lossy(&output.stderr);
    Err(error.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(tag: &str, prerelease: bool) -> Listed {
        let version = tag.trim_start_matches('v');
        Listed {
            tag_name: tag.into(),
            draft: false,
            prerelease,
            body: None,
            html_url: String::new(),
            published_at: None,
            assets: vec![Asset {
                name: format!("Monodrive-{version}-arm64.dmg"),
                browser_download_url: format!("https://example.com/{version}"),
            }],
        }
    }

    fn versions(channel: Channel, current: &str) -> Vec<String> {
        let list = vec![
            listed("v0.1.0-nightly.20261031.9", true),
            listed("v0.1.0", false),
            listed("v0.1.1-nightly.20261101.10", true),
            listed("v0.1.1-nightly.20261102.11", true),
            listed("not-a-version", false),
        ];
        newer(list, channel, &Version::parse(current).unwrap())
            .into_iter()
            .map(|release| release.version.to_string())
            .collect()
    }

    #[test]
    fn stable_sees_only_newer_stables() {
        assert_eq!(versions(Channel::Stable, "0.1.0"), Vec::<String>::new());
        assert_eq!(
            versions(Channel::Stable, "0.1.0-nightly.20261031.9"),
            ["0.1.0"]
        );
    }

    #[test]
    fn nightly_sees_everything_newer_with_the_newest_first() {
        assert_eq!(
            versions(Channel::Nightly, "0.1.0"),
            ["0.1.1-nightly.20261102.11", "0.1.1-nightly.20261101.10"]
        );
    }

    #[test]
    fn nightlies_order_by_their_numbers_not_their_text() {
        let list = vec![
            listed("v0.1.1-nightly.20261102.9", true),
            listed("v0.1.1-nightly.20261102.10", true),
        ];
        let current = Version::parse("0.1.1-nightly.20261102.9").unwrap();
        let found = newer(list, Channel::Nightly, &current);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].version.to_string(), "0.1.1-nightly.20261102.10");
    }

    #[test]
    fn a_release_without_its_disk_image_or_still_a_draft_is_not_offered() {
        let mut bare = listed("v9.0.0", false);
        bare.assets.clear();
        let mut draft = listed("v9.0.1", false);
        draft.draft = true;
        assert!(newer(vec![bare, draft], Channel::Stable, &Version::new(0, 1, 0)).is_empty());
    }

    #[test]
    fn notes_read_the_summary_and_stop_before_the_full_list() {
        let body = "**v0.2.0** · 12 changes from 2 contributors\n\n## Highlights\nSearch got faster.\nTabs arrived.\n\n## Fixes\n- Eject works again (#12)\n- Sizes add up\n\n<details>\n<summary>Full changelog</summary>\n\n- feat: everything (abc)\n</details>\n";
        assert_eq!(
            notes(body),
            Notes {
                highlights: Some("Search got faster. Tabs arrived.".into()),
                changes: vec!["Eject works again".into(), "Sizes add up".into()],
                stats: Some("12 changes from 2 contributors".into()),
            }
        );
    }

    #[test]
    fn notes_keep_generated_changes_and_drop_authors_and_links() {
        let body = "## What's Changed\n* feat: a thing by @me in https://x/1\n* fix: another\n\n## New Contributors\n* @me made their first contribution\n\n**Full Changelog**: https://x";
        assert_eq!(
            notes(body),
            Notes {
                highlights: None,
                changes: vec!["feat: a thing".into(), "fix: another".into()],
                stats: None,
            }
        );
        assert_eq!(notes("**Full Changelog**: https://x"), Notes::default());
    }
}
