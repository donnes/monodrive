//! Places: the sidebar's favorite folders and the files recently opened
//! through Monodrive. They're Monodrive's own lists, kept beside
//! settings.json rather than in it, because the window and every MCP server
//! change them and only the window writes settings.json.

use crate::volumes::{self, Volume, VolumeId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many recently opened files are kept.
pub const RECENTS: usize = 50;

/// Where the lists are saved.
#[derive(Clone, Debug)]
pub struct Store {
    /// `places.json`: the favorites, and later saved servers.
    pub places: PathBuf,
    /// `recents.json`, newest first.
    pub recents: PathBuf,
}

/// A pinned folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    /// Where it was when it was pinned.
    pub path: PathBuf,
    /// The volume it was on, unless that was the startup disk, which is
    /// always there and whose UUID a reinstall changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<Pinned>,
}

/// The volume a favorite was pinned on, so it's found again by identity
/// wherever that volume mounts, and never on another of the same name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pinned {
    pub id: VolumeId,
    pub name: String,
    /// Where it was mounted then.
    pub mount: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Where the folder is now, on a remounted volume maybe not where it
    /// was pinned.
    Available(PathBuf),
    /// Why it can't be opened: its volume isn't connected, or it's gone.
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub favorite: Favorite,
    pub availability: Availability,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// `None` until the first change, which keeps the defaults until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    favorites: Option<Vec<Favorite>>,
}

impl Favorite {
    pub fn resolve(&self, volumes: &[Volume]) -> Availability {
        let path = match &self.volume {
            None => self.path.clone(),
            Some(pinned) => {
                let Some(volume) = volumes
                    .iter()
                    .find(|volume| volume.id.as_ref() == Some(&pinned.id))
                else {
                    return Availability::Unavailable(format!("{} isn't connected.", pinned.name));
                };
                match self.path.strip_prefix(&pinned.mount) {
                    Ok(inside) if inside.as_os_str().is_empty() => volume.mount.clone(),
                    Ok(inside) => volume.mount.join(inside),
                    Err(_) => {
                        return Availability::Unavailable(format!(
                            "{} isn't on {}.",
                            self.path.display(),
                            pinned.name
                        ));
                    }
                }
            }
        };
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => Availability::Available(path),
            Ok(_) => Availability::Unavailable(format!("{} isn't a folder.", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Availability::Unavailable(format!("{} no longer exists.", path.display()))
            }
            Err(e) => Availability::Unavailable(format!("{}: {e}", path.display())),
        }
    }

    /// Whether this is the folder at `path`, as pinned or where it is now.
    fn is(&self, path: &Path, volumes: &[Volume]) -> bool {
        self.path == path || self.resolve(volumes) == Availability::Available(path.into())
    }
}

impl Store {
    /// Monodrive's own, in its support folder.
    pub fn user() -> Option<Self> {
        let support = crate::support_dir()?;
        Some(Self {
            places: support.join("places.json"),
            recents: support.join("recents.json"),
        })
    }

    /// The favorites in order, each with where it is now or why it can't
    /// be opened. Reads the disk for each.
    pub fn places(&self, volumes: &[Volume]) -> Result<Vec<Place>, String> {
        Ok(self
            .favorites()?
            .into_iter()
            .map(|favorite| Place {
                availability: favorite.resolve(volumes),
                favorite,
            })
            .collect())
    }

    /// Pins the folder at `path` at position `at`, or last. A folder that's
    /// already pinned moves to `at`, or stays where it is without one.
    pub fn pin(&self, path: &Path, at: Option<usize>, volumes: &[Volume]) -> Result<(), String> {
        if !path.is_absolute() {
            return Err(format!("{}: give an absolute path.", path.display()));
        }
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(format!("{} isn't a folder.", path.display())),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        let mut favorites = self.favorites()?;
        let favorite = match favorites.iter().position(|f| f.is(path, volumes)) {
            Some(_) if at.is_none() => return Ok(()),
            Some(ix) => favorites.remove(ix),
            None => Favorite {
                path: path.into(),
                volume: volumes::holding(volumes, path)
                    .filter(|volume| !volume.startup)
                    .and_then(|volume| {
                        Some(Pinned {
                            id: volume.id.clone()?,
                            name: volume.name.clone(),
                            mount: volume.mount.clone(),
                        })
                    }),
            },
        };
        let at = at.unwrap_or(favorites.len()).min(favorites.len());
        favorites.insert(at, favorite);
        self.save_favorites(favorites)
    }

    /// Forgets the favorite at `path`, as pinned or where it is now. The
    /// folder itself stays where it is.
    pub fn unpin(&self, path: &Path, volumes: &[Volume]) -> Result<(), String> {
        let mut favorites = self.favorites()?;
        let ix = favorites
            .iter()
            .position(|f| f.is(path, volumes))
            .ok_or_else(|| format!("{} isn't in Favorites.", path.display()))?;
        favorites.remove(ix);
        self.save_favorites(favorites)
    }

    /// Files opened through Monodrive, newest first.
    pub fn recents(&self) -> Vec<PathBuf> {
        std::fs::read(&self.recents)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Records that the file at `path` was opened, keeping the newest
    /// `RECENTS`.
    pub fn opened(&self, path: &Path) -> Result<(), String> {
        let mut recents = self.recents();
        recents.retain(|recent| recent != path);
        recents.insert(0, path.into());
        recents.truncate(RECENTS);
        write(&self.recents, &recents)
    }

    pub fn clear_recents(&self) -> Result<(), String> {
        write(&self.recents, &Vec::<PathBuf>::new())
    }

    /// The saved favorites, or the defaults until they're first changed. A
    /// file that doesn't read is an error, so a change never replaces a
    /// list Monodrive couldn't read.
    fn favorites(&self) -> Result<Vec<Favorite>, String> {
        Ok(self.saved()?.favorites.unwrap_or_else(defaults))
    }

    fn saved(&self) -> Result<Saved, String> {
        match std::fs::read(&self.places) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{}: {e}", self.places.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Saved::default()),
            Err(e) => Err(format!("{}: {e}", self.places.display())),
        }
    }

    fn save_favorites(&self, favorites: Vec<Favorite>) -> Result<(), String> {
        let mut saved = self.saved()?;
        saved.favorites = Some(favorites);
        write(&self.places, &saved)
    }
}

/// Today's sidebar: Home, Desktop, Documents, Downloads and Applications.
fn defaults() -> Vec<Favorite> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let in_home = ["Desktop", "Documents", "Downloads"];
    let folders = home.iter().flat_map(|home| {
        std::iter::once(home.clone()).chain(in_home.iter().map(|name| home.join(name)))
    });
    folders
        .chain([PathBuf::from("/Applications")])
        .map(|path| Favorite { path, volume: None })
        .collect()
}

/// Writes `value` as JSON, then renames it into place, so a crash never
/// leaves half a file.
fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> Store {
        Store {
            places: dir.join("state/places.json"),
            recents: dir.join("state/recents.json"),
        }
    }

    fn volume(name: &str, mount: &Path, uuid: Option<&str>) -> Volume {
        Volume {
            name: name.into(),
            mount: mount.into(),
            id: uuid.map(|uuid| VolumeId::Uuid(uuid.into())),
            total: 0,
            free: 0,
            read_only: false,
            network: false,
            ejectable: true,
            removable: false,
            startup: mount == Path::new("/"),
        }
    }

    fn paths(store: &Store, volumes: &[Volume]) -> Vec<PathBuf> {
        let places = store.places(volumes).unwrap();
        places
            .into_iter()
            .map(|place| place.favorite.path)
            .collect()
    }

    #[test]
    fn favorites_start_as_todays_sidebar_then_pin_move_and_unpin_persist() {
        let dir = tempfile::tempdir().unwrap();
        let (store, root) = (
            store(dir.path()),
            [volume("Macintosh HD", "/".as_ref(), Some("R"))],
        );
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let defaults = [
            home.clone(),
            home.join("Desktop"),
            home.join("Documents"),
            home.join("Downloads"),
            "/Applications".into(),
        ];
        assert_eq!(paths(&store, &root), defaults);

        let (work, notes) = (dir.path().join("Work"), dir.path().join("notes.md"));
        std::fs::create_dir(&work).unwrap();
        std::fs::write(&notes, "").unwrap();
        store.pin(&work, None, &root).unwrap();
        // Pinning it again leaves it where it is; `at` moves it.
        store.pin(&work, None, &root).unwrap();
        assert_eq!(paths(&store, &root).last(), Some(&work));
        store.pin(&work, Some(0), &root).unwrap();
        // On the startup disk it keeps no volume, so a new UUID can't lose it.
        let places = store.places(&root).unwrap();
        assert_eq!(
            places[0].favorite,
            Favorite {
                path: work.clone(),
                volume: None
            }
        );
        assert_eq!(
            places[0].availability,
            Availability::Available(work.clone())
        );
        assert!(
            store
                .pin(&notes, None, &root)
                .unwrap_err()
                .contains("isn't a folder")
        );
        assert!(store.pin("Work".as_ref(), None, &root).is_err());

        store.unpin(&home.join("Desktop"), &root).unwrap();
        let reread = paths(&store, &root);
        assert_eq!(reread.len(), 5);
        assert!(!reread.contains(&home.join("Desktop")));
        store.unpin(&work, &root).unwrap();
        assert!(work.is_dir(), "unpinning forgets only the bookmark");
        assert!(
            store
                .unpin(&work, &root)
                .unwrap_err()
                .contains("isn't in Favorites")
        );

        // A list that doesn't read is never replaced by a change.
        std::fs::write(&store.places, "{").unwrap();
        assert!(store.places(&root).is_err());
        assert!(store.pin(&work, None, &root).is_err());
        assert_eq!(std::fs::read_to_string(&store.places).unwrap(), "{");
    }

    #[test]
    fn a_favorite_follows_its_volume_by_identity_never_by_name_or_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let (first, second) = (dir.path().join("Backup"), dir.path().join("Backup 1"));
        for mount in [&first, &second] {
            std::fs::create_dir_all(mount.join("Photos")).unwrap();
        }
        let root = volume("Macintosh HD", "/".as_ref(), Some("R"));
        let mounted = [root.clone(), volume("Backup", &first, Some("U1"))];
        store.pin(&first.join("Photos"), Some(0), &mounted).unwrap();
        let resolved = |volumes: &[Volume]| store.places(volumes).unwrap()[0].availability.clone();
        assert_eq!(
            resolved(&mounted),
            Availability::Available(first.join("Photos"))
        );

        let remounted = [root.clone(), volume("Backup", &second, Some("U1"))];
        assert_eq!(
            resolved(&remounted),
            Availability::Available(second.join("Photos"))
        );
        let missing = [root.clone()];
        let gone = Availability::Unavailable("Backup isn't connected.".into());
        assert_eq!(resolved(&missing), gone);
        let impostor = [root.clone(), volume("Backup", &first, Some("U2"))];
        assert_eq!(resolved(&impostor), gone);

        // Found where it is now, it can be forgotten from there.
        store.unpin(&second.join("Photos"), &remounted).unwrap();
        assert_eq!(paths(&store, &remounted).len(), 5);
    }

    #[test]
    fn recents_keep_the_newest_once_each_up_to_the_limit_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert_eq!(store.recents(), Vec::<PathBuf>::new());
        let file = |n: usize| PathBuf::from(format!("/files/{n}.txt"));
        for n in 0..RECENTS + 10 {
            store.opened(&file(n)).unwrap();
        }
        store.opened(&file(30)).unwrap();
        let recents = store.recents();
        assert_eq!(recents.len(), RECENTS);
        assert_eq!(recents[..2], [file(30), file(RECENTS + 9)]);
        assert_eq!(recents.iter().filter(|path| **path == file(30)).count(), 1);
        assert!(!recents.contains(&file(9)));
        store.clear_recents().unwrap();
        assert_eq!(store.recents(), Vec::<PathBuf>::new());
    }
}
