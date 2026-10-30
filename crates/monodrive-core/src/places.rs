//! Places: the sidebar's favorite folders, the saved and recent searches in
//! the search field's menu, the registered Git repositories, the user's saved commands, and the files recently opened
//! through Monodrive. They're
//! Monodrive's own lists, kept beside settings.json rather than in it, because
//! the window and every MCP server change them and only the window writes
//! settings.json.

use crate::ops;
use crate::remote::{self, Source};
use crate::search::{Query, Scope};
use crate::user_commands::Definition;
use crate::volumes::{self, Volume, VolumeId};
use crate::worktrees::Registration;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many recently opened files are kept.
pub const RECENTS: usize = 50;
/// How many recent searches are kept.
pub const RECENT_SEARCHES: usize = 10;

/// Where the lists are saved.
#[derive(Clone, Debug)]
pub struct Store {
    /// `places.json`: the favorites, saved searches, registered repositories,
    /// saved commands, remote sources and saved servers.
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

impl Place {
    /// Where it is now, if it can be opened.
    pub fn at(&self) -> Option<&Path> {
        match &self.availability {
            Availability::Available(path) => Some(path),
            Availability::Unavailable(_) => None,
        }
    }
}

/// A search kept by name. Saving one never runs it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    /// As `Query` prints it.
    pub query: String,
    #[serde(default)]
    pub scope: Scope,
}

/// A search the window ran and showed results for. Agents' searches aren't
/// kept, so no tool reads these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSearch {
    /// As `Search::stored` prints it.
    pub query: String,
    #[serde(default)]
    pub scope: Scope,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// `None` until the first change, which keeps the defaults until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    favorites: Option<Vec<Favorite>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    searches: Vec<SavedSearch>,
    /// Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recent_searches: Vec<RecentSearch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repositories: Vec<Registration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    commands: Vec<Definition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remotes: Vec<Source>,
    /// Connect to Server's addresses, never with a password.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    servers: Vec<String>,
}

/// `path` if there's a folder there now, or why not. Reads the disk.
pub fn folder_at(path: PathBuf) -> Availability {
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Availability::Available(path),
        Ok(_) => Availability::Unavailable(format!("{} isn't a folder.", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Availability::Unavailable(format!("{} no longer exists.", path.display()))
        }
        Err(e) => Availability::Unavailable(format!("{}: {e}", path.display())),
    }
}

impl Favorite {
    /// The folder at `path` as it's on `volumes` now.
    pub fn of(path: &Path, volumes: &[Volume]) -> Self {
        Self {
            volume: volumes::holding(volumes, path)
                .filter(|volume| !volume.startup)
                .and_then(|volume| {
                    Some(Pinned {
                        id: volume.id.clone()?,
                        name: volume.name.clone(),
                        mount: volume.mount.clone(),
                    })
                }),
            path: path.into(),
        }
    }

    pub fn resolve(&self, volumes: &[Volume]) -> Availability {
        match self.located(volumes) {
            Ok(path) => folder_at(path),
            Err(why) => Availability::Unavailable(why),
        }
    }

    /// Where it is on `volumes` now, by volume identity, or why it can't be
    /// told. Touches no disk, so a privacy check can come before anything does.
    pub fn located(&self, volumes: &[Volume]) -> Result<PathBuf, String> {
        let Some(pinned) = &self.volume else {
            return Ok(self.path.clone());
        };
        let Some(volume) = volumes
            .iter()
            .find(|volume| volume.id.as_ref() == Some(&pinned.id))
        else {
            return Err(format!("{} isn't connected.", pinned.name));
        };
        match self.path.strip_prefix(&pinned.mount) {
            Ok(inside) if inside.as_os_str().is_empty() => Ok(volume.mount.clone()),
            Ok(inside) => Ok(volume.mount.join(inside)),
            Err(_) => Err(format!("{} isn't on {}.", self.path.display(), pinned.name)),
        }
    }

    /// Whether this favorite is the folder at `path` now. Its saved path
    /// alone isn't enough: another drive may be mounted there.
    fn is_at(&self, path: &Path, volumes: &[Volume]) -> bool {
        self.resolve(volumes) == Availability::Available(path.into())
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

    /// Pins the folder at `path` at position `at`, or last, and returns
    /// where it really is. A folder that's already pinned moves to `at`, or
    /// stays where it is without one. One saved at `path` with no folder
    /// there now moves to `at` all the same.
    pub fn pin(
        &self,
        path: &Path,
        at: Option<usize>,
        volumes: &[Volume],
    ) -> Result<PathBuf, String> {
        if !path.is_absolute() {
            return Err(format!("{}: give an absolute path.", path.display()));
        }
        if let Some(at) = at
            && !path.is_dir()
            && let Some(saved) = self.favorites()?.into_iter().find(|f| f.path == path)
        {
            self.move_favorite(&saved, at)?;
            return Ok(path.into());
        }
        // Where the folder really is, so a link to a folder on another
        // volume pins that volume.
        let path = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !path.is_dir() {
            return Err(format!("{} isn't a folder.", path.display()));
        }
        locked(&self.places, || {
            let mut favorites = self.favorites()?;
            let favorite = match favorites.iter().position(|f| f.is_at(&path, volumes)) {
                Some(_) if at.is_none() => return Ok(path),
                Some(ix) => favorites.remove(ix),
                None => Favorite::of(&path, volumes),
            };
            let at = at.unwrap_or(favorites.len()).min(favorites.len());
            favorites.insert(at, favorite);
            self.save_favorites(favorites)?;
            Ok(path)
        })
    }

    /// Pins the folders at `paths`, in order, before what's at position `at`
    /// in the list as it is now, as a line drawn between two rows shows. One
    /// already pinned moves, and leaving from above `at` takes one off it.
    /// All of them or none.
    pub fn pin_before(
        &self,
        paths: &[PathBuf],
        at: usize,
        volumes: &[Volume],
    ) -> Result<(), String> {
        let mut folders = Vec::with_capacity(paths.len());
        for path in paths {
            let real =
                std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
            if !real.is_dir() {
                return Err(format!("{} isn't a folder.", real.display()));
            }
            folders.push(real);
        }
        locked(&self.places, || {
            let mut favorites = self.favorites()?;
            let mut at = at.min(favorites.len());
            let mut pinned = Vec::with_capacity(folders.len());
            for folder in &folders {
                pinned.push(
                    match favorites.iter().position(|f| f.is_at(folder, volumes)) {
                        Some(ix) => {
                            if ix < at {
                                at -= 1;
                            }
                            favorites.remove(ix)
                        }
                        None => Favorite::of(folder, volumes),
                    },
                );
            }
            favorites.splice(at..at, pinned);
            self.save_favorites(favorites)
        })
    }

    /// Moves this saved favorite to position `at`, whether it's available or
    /// not: a folder at its path now may be another drive's.
    pub fn move_favorite(&self, favorite: &Favorite, at: usize) -> Result<(), String> {
        locked(&self.places, || {
            let mut favorites = self.favorites()?;
            let ix = favorites
                .iter()
                .position(|f| f == favorite)
                .ok_or_else(|| format!("{} isn't in Favorites.", favorite.path.display()))?;
            let favorite = favorites.remove(ix);
            favorites.insert(at.min(favorites.len()), favorite);
            self.save_favorites(favorites)
        })
    }

    /// Forgets the favorite that's the folder at `path` now, or else one
    /// saved at `path`, so an unavailable one can be forgotten too. The
    /// folder itself stays where it is.
    pub fn unpin(&self, path: &Path, volumes: &[Volume]) -> Result<(), String> {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
        locked(&self.places, || {
            let mut favorites = self.favorites()?;
            let ix = favorites
                .iter()
                .position(|f| f.is_at(&real, volumes))
                .or_else(|| favorites.iter().position(|f| f.path == path))
                .ok_or_else(|| format!("{} isn't in Favorites.", path.display()))?;
            favorites.remove(ix);
            self.save_favorites(favorites)
        })
    }

    /// The saved searches in their order.
    pub fn searches(&self) -> Result<Vec<SavedSearch>, String> {
        Ok(self.saved()?.searches)
    }

    /// Saves `search` at position `at`, or last, and returns it as saved,
    /// its query reprinted. One already saved under its name is replaced
    /// where it is, or moved to `at`.
    pub fn save_search(
        &self,
        search: SavedSearch,
        at: Option<usize>,
    ) -> Result<SavedSearch, String> {
        let name = named(&search.name)?;
        let query = Query::parse(&search.query)?.to_string();
        if query.is_empty() {
            return Err(format!("{name}: type something to search for."));
        }
        if let Scope::Folder(folder) = &search.scope
            && !folder.is_absolute()
        {
            return Err(format!("{}: give an absolute path.", folder.display()));
        }
        let search = SavedSearch {
            name,
            query,
            scope: search.scope,
        };
        self.change_searches(|searches| {
            match searches.iter().position(|s| s.name == search.name) {
                Some(ix) if at.is_none() => searches[ix] = search.clone(),
                found => {
                    if let Some(ix) = found {
                        searches.remove(ix);
                    }
                    let at = at.unwrap_or(searches.len()).min(searches.len());
                    searches.insert(at, search.clone());
                }
            }
            Ok(())
        })?;
        Ok(search)
    }

    pub fn rename_search(&self, name: &str, to: &str) -> Result<(), String> {
        let to = named(to)?;
        self.change_searches(|searches| {
            if to != name && searches.iter().any(|s| s.name == to) {
                return Err(format!("{to} is already a saved search."));
            }
            let search = searches
                .iter_mut()
                .find(|s| s.name == name)
                .ok_or_else(|| format!("{name} isn't a saved search."))?;
            search.name = to;
            Ok(())
        })
    }

    pub fn remove_search(&self, name: &str) -> Result<(), String> {
        self.change_searches(|searches| {
            let ix = searches
                .iter()
                .position(|s| s.name == name)
                .ok_or_else(|| format!("{name} isn't a saved search."))?;
            searches.remove(ix);
            Ok(())
        })
    }

    /// The searches run lately, newest first.
    pub fn recent_searches(&self) -> Result<Vec<RecentSearch>, String> {
        Ok(self.saved()?.recent_searches)
    }

    /// Records that `search` ran, keeping the newest `RECENT_SEARCHES`.
    pub fn searched(&self, search: RecentSearch) -> Result<(), String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let recent = &mut saved.recent_searches;
            recent.retain(|r| *r != search);
            recent.insert(0, search);
            recent.truncate(RECENT_SEARCHES);
            write(&self.places, &saved)
        })
    }

    pub fn clear_recent_searches(&self) -> Result<(), String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            saved.recent_searches.clear();
            write(&self.places, &saved)
        })
    }

    fn change_searches(
        &self,
        change: impl FnOnce(&mut Vec<SavedSearch>) -> Result<(), String>,
    ) -> Result<(), String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            change(&mut saved.searches)?;
            write(&self.places, &saved)
        })
    }

    /// The registered repositories in the order they were registered.
    pub fn repositories(&self) -> Result<Vec<Registration>, String> {
        Ok(self.saved()?.repositories)
    }

    /// Adds `repo`, or updates the one with its common Git folder where it
    /// is. `worktrees::register` finds what to pass.
    pub fn register_repository(&self, repo: Registration) -> Result<(), String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let repos = &mut saved.repositories;
            match repos.iter_mut().find(|r| r.common_dir == repo.common_dir) {
                Some(known) => *known = repo,
                None => repos.push(repo),
            }
            write(&self.places, &saved)
        })
    }

    /// Forgets the repository shown at `path`, or whose common Git folder
    /// it is, and returns it. Only the bookmark goes; Git and the files are
    /// untouched.
    pub fn forget_repository(&self, path: &Path) -> Result<Registration, String> {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let repos = &mut saved.repositories;
            let ix = repos
                .iter()
                .position(|r| {
                    [&real, path]
                        .iter()
                        .any(|p| r.path == **p || r.common_dir == **p)
                })
                .ok_or_else(|| format!("{} isn't a registered repository.", path.display()))?;
            let repo = repos.remove(ix);
            write(&self.places, &saved)?;
            Ok(repo)
        })
    }

    /// The saved commands in the order they were saved.
    pub fn commands(&self) -> Result<Vec<Definition>, String> {
        Ok(self.saved()?.commands)
    }

    pub fn command(&self, name: &str) -> Result<Definition, String> {
        self.commands()?
            .into_iter()
            .find(|command| command.name == name)
            .ok_or_else(|| format!("{name} isn't a saved command."))
    }

    /// Validates `command` and saves it, replacing the one saved under its
    /// name where it is, and returns it as saved. Running it is separate.
    pub fn save_command(&self, command: Definition) -> Result<Definition, String> {
        let command = command.validated()?;
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let commands = &mut saved.commands;
            match commands.iter_mut().find(|known| known.name == command.name) {
                Some(known) => *known = command.clone(),
                None => commands.push(command.clone()),
            }
            write(&self.places, &saved)
        })?;
        Ok(command)
    }

    pub fn remove_command(&self, name: &str) -> Result<(), String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let ix = saved
                .commands
                .iter()
                .position(|command| command.name == name)
                .ok_or_else(|| format!("{name} isn't a saved command."))?;
            saved.commands.remove(ix);
            write(&self.places, &saved)
        })
    }

    /// The saved remote sources in the order they were added.
    pub fn remotes(&self) -> Result<Vec<Source>, String> {
        Ok(self.saved()?.remotes)
    }

    pub fn remote(&self, id: &str) -> Result<Source, remote::RemoteError> {
        self.remotes()
            .map_err(remote::RemoteError::Failed)?
            .into_iter()
            .find(|source| source.id == id)
            .ok_or_else(|| remote::RemoteError::UnknownSource(id.into()))
    }

    /// Saves a source for `host` and returns it, without connecting. The same
    /// host and start already saved is returned as it is; a new one gets `id`,
    /// which must be free, or a free one made from the host.
    pub fn add_remote(
        &self,
        host: &str,
        id: Option<&str>,
        start: Option<&str>,
    ) -> Result<Source, String> {
        // Only to check the host and start: its id comes next.
        let checked = Source::new("new", host, start)?;
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let same =
                |source: &&Source| source.host == checked.host && source.start == checked.start;
            if let Some(known) = saved.remotes.iter().find(same)
                && id.is_none_or(|id| id == known.id)
            {
                return Ok(known.clone());
            }
            let taken = |id: &str| saved.remotes.iter().any(|source| source.id == id);
            let id = match id {
                Some(id) if taken(id) => return Err(format!("{id} is already a remote source.")),
                Some(id) => id.to_string(),
                None => {
                    let base = remote::id_for(host);
                    std::iter::once(base.clone())
                        .chain((2..).map(|n| format!("{base}-{n}")))
                        .find(|id| !taken(id))
                        .unwrap_or(base)
                }
            };
            let source = Source::new(&id, host, start)?;
            saved.remotes.push(source.clone());
            write(&self.places, &saved)?;
            Ok(source)
        })
    }

    /// Forgets the source `id` and returns it. Nothing on the server changes.
    pub fn forget_remote(&self, id: &str) -> Result<Source, String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let ix = saved
                .remotes
                .iter()
                .position(|source| source.id == id)
                .ok_or_else(|| format!("{id} isn't a saved remote source."))?;
            let source = saved.remotes.remove(ix);
            write(&self.places, &saved)?;
            Ok(source)
        })
    }

    /// Connect to Server's saved addresses in the order they were saved.
    pub fn servers(&self) -> Result<Vec<String>, String> {
        Ok(self.saved()?.servers)
    }

    /// Keeps `address`, as `servers::address` wrote it, unless it's saved.
    pub fn save_server(&self, address: &str) -> Result<(), String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            if saved.servers.iter().any(|known| known == address) {
                return Ok(());
            }
            saved.servers.push(address.into());
            write(&self.places, &saved)
        })
    }

    /// Forgets the saved `address`. A share mounted from it stays mounted.
    pub fn forget_server(&self, address: &str) -> Result<(), String> {
        locked(&self.places, || {
            let mut saved = self.saved()?;
            let ix = saved
                .servers
                .iter()
                .position(|known| known == address)
                .ok_or_else(|| format!("{address} isn't a saved server."))?;
            saved.servers.remove(ix);
            write(&self.places, &saved)
        })
    }

    /// Calls `changed` on its own thread each time a list here changes,
    /// whoever changed it. Other files in the folder count too, so read the
    /// lists again to see. Dropping it stops the thread.
    pub fn watch(&self, changed: impl Fn() + Send + 'static) -> std::io::Result<ops::Watch> {
        let name = self.places.file_name().unwrap_or_default().to_owned();
        let folder = self.places.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(folder)?;
        ops::Watch::start(folder, &name, changed)
    }

    /// Files opened through Monodrive, newest first. A file that doesn't
    /// read is an error, so `opened` never replaces it.
    pub fn recents(&self) -> Result<Vec<PathBuf>, String> {
        match std::fs::read(&self.recents) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{}: {e}", self.recents.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{}: {e}", self.recents.display())),
        }
    }

    /// Records that the file at `path` was opened, keeping the newest
    /// `RECENTS`.
    pub fn opened(&self, path: &Path) -> Result<(), String> {
        locked(&self.recents, || {
            let mut recents = self.recents()?;
            recents.retain(|recent| recent != path);
            recents.insert(0, path.into());
            recents.truncate(RECENTS);
            write(&self.recents, &recents)
        })
    }

    /// Empties the list, even one that doesn't read.
    pub fn clear_recents(&self) -> Result<(), String> {
        locked(&self.recents, || {
            write(&self.recents, &Vec::<PathBuf>::new())
        })
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

/// A saved search's name, trimmed.
fn named(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Name the search.".into());
    }
    Ok(name.into())
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

/// Runs a read, change and write of `file` holding its lock file, which
/// the window and every MCP server share, so no change is lost to another.
fn locked<T>(file: &Path, change: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let lock = file.with_extension("lock");
    let fail = |e: std::io::Error| format!("{}: {e}", lock.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let _held = ops::lock(&lock, libc::LOCK_EX).map_err(fail)?;
    change()
}

/// Writes `value` as JSON under a name of this writer's own, then renames
/// it into place, so a crash never leaves half a file.
fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let json = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}-{n}.tmp", std::process::id()));
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
            disk: None,
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

    /// A temp folder and its real path: pins record where a folder really
    /// is, and macOS's temp folder is behind a link.
    fn real_tempdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        (dir, real)
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
        let (_dir, dir) = real_tempdir();
        let (store, root) = (
            store(&dir),
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

        let (work, notes) = (dir.join("Work"), dir.join("notes.md"));
        std::fs::create_dir(&work).unwrap();
        std::fs::write(&notes, "").unwrap();
        assert_eq!(store.pin(&work, None, &root), Ok(work.clone()));
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
        let (_dir, dir) = real_tempdir();
        let store = store(&dir);
        let (first, second) = (dir.join("Backup"), dir.join("Backup 1"));
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
    fn an_unavailable_favorite_still_moves_and_a_saved_server_is_forgotten() {
        let (_dir, dir) = real_tempdir();
        let store = store(&dir);
        let root = [volume("Macintosh HD", "/".as_ref(), Some("R"))];
        let gone = dir.join("Gone");
        std::fs::create_dir(&gone).unwrap();
        store.pin(&gone, None, &root).unwrap();
        std::fs::rename(&gone, dir.join("Moved")).unwrap();
        assert_eq!(store.pin(&gone, Some(0), &root), Ok(gone.clone()));
        assert_eq!(paths(&store, &root)[0], gone);
        // With nowhere to go, it's still not a folder to pin.
        assert!(store.pin(&gone, None, &root).is_err());
        let saved = store.places(&root).unwrap().remove(0).favorite;
        store.move_favorite(&saved, 2).unwrap();
        assert_eq!(paths(&store, &root)[2], gone);

        store.save_server("smb://nas/media").unwrap();
        store.save_server("smb://nas/media").unwrap();
        store.save_server("smb://nas/photos").unwrap();
        store.forget_server("smb://nas/media").unwrap();
        assert_eq!(store.servers(), Ok(vec!["smb://nas/photos".into()]));
        assert!(store.forget_server("smb://nas/media").is_err());
        // Saving servers leaves the favorites as they were.
        assert_eq!(paths(&store, &root)[2], gone);
    }

    #[test]
    fn pinning_before_a_position_counts_what_leaves_from_above_it() {
        let (_dir, real) = real_tempdir();
        let store = store(&real.join("support"));
        let [a, b, c, d, e] = ["a", "b", "c", "d", "e"].map(|name| {
            let path = real.join(name);
            std::fs::create_dir(&path).unwrap();
            path
        });
        for path in [&a, &b, &c, &d] {
            store.pin(path, Some(99), &[]).unwrap();
        }
        let shown = |store: &Store| paths(store, &[])[..].to_vec();
        let start = shown(&store).len() - 4;
        let tail = |store: &Store| shown(store)[start..].to_vec();
        // A to the line between C and D.
        store
            .pin_before(std::slice::from_ref(&a), start + 3, &[])
            .unwrap();
        assert_eq!(tail(&store), [b.clone(), c.clone(), a.clone(), d.clone()]);
        // Downward by one line stays put, and upward needs no adjusting.
        store
            .pin_before(std::slice::from_ref(&c), start + 2, &[])
            .unwrap();
        assert_eq!(tail(&store), [b.clone(), c.clone(), a.clone(), d.clone()]);
        store
            .pin_before(std::slice::from_ref(&d), start, &[])
            .unwrap();
        assert_eq!(tail(&store), [d.clone(), b.clone(), c.clone(), a.clone()]);
        // Several at once, one from each side, and a new one among them.
        store
            .pin_before(&[a.clone(), e.clone(), d.clone()], start + 2, &[])
            .unwrap();
        assert_eq!(tail(&store)[..3], [b.clone(), a.clone(), e.clone()]);
        assert_eq!(tail(&store)[3..], [d.clone(), c.clone()]);
        // Past the end is the end.
        store.pin_before(std::slice::from_ref(&b), 99, &[]).unwrap();
        assert_eq!(tail(&store).last(), Some(&b));
        // One that can't be pinned leaves the list as it was.
        let before = shown(&store);
        let file = real.join("file");
        std::fs::write(&file, "").unwrap();
        assert!(store.pin_before(&[a.clone(), file], 0, &[]).is_err());
        assert_eq!(shown(&store), before);
    }

    #[test]
    fn a_watch_hears_another_client_pin_and_stops_when_dropped() {
        let (_dir, dir) = real_tempdir();
        let window = store(&dir);
        let (heard, hear) = std::sync::mpsc::channel();
        let watch = window
            .watch(move || {
                let _ = heard.send(());
            })
            .unwrap();
        let agent = store(&dir);
        agent.pin(&dir, None, &[]).unwrap();
        hear.recv().unwrap();
        drop(watch);
        // The thread let go of its sender.
        while hear.recv().is_ok() {}
    }

    #[test]
    fn recent_searches_keep_the_newest_once_each_and_clearing_leaves_saved_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let recent = |n: usize| RecentSearch {
            query: format!("q{n}"),
            scope: Scope::Everywhere,
        };
        let saved = SavedSearch {
            name: "Todo".into(),
            query: "contains:todo".into(),
            scope: Scope::Everywhere,
        };
        assert_eq!(store.save_search(saved.clone(), None), Ok(saved.clone()));
        for n in 0..RECENT_SEARCHES + 2 {
            store.searched(recent(n)).unwrap();
        }
        store.searched(recent(5)).unwrap();
        let recents = store.recent_searches().unwrap();
        assert_eq!(recents.len(), RECENT_SEARCHES);
        assert_eq!(recents[..2], [recent(5), recent(RECENT_SEARCHES + 1)]);
        assert_eq!(recents.iter().filter(|r| **r == recent(5)).count(), 1);
        // The same words in another folder are another search.
        let here = RecentSearch {
            scope: Scope::Folder("/tmp".into()),
            ..recent(5)
        };
        store.searched(here.clone()).unwrap();
        assert_eq!(store.recent_searches().unwrap()[..2], [here, recent(5)]);

        store.clear_recent_searches().unwrap();
        assert_eq!(store.recent_searches(), Ok(Vec::new()));
        assert_eq!(store.searches(), Ok(vec![saved]));
    }

    #[test]
    fn recents_keep_the_newest_once_each_up_to_the_limit_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert_eq!(store.recents(), Ok(Vec::new()));
        let file = |n: usize| PathBuf::from(format!("/files/{n}.txt"));
        for n in 0..RECENTS + 10 {
            store.opened(&file(n)).unwrap();
        }
        store.opened(&file(30)).unwrap();
        let recents = store.recents().unwrap();
        assert_eq!(recents.len(), RECENTS);
        assert_eq!(recents[..2], [file(30), file(RECENTS + 9)]);
        assert_eq!(recents.iter().filter(|path| **path == file(30)).count(), 1);
        assert!(!recents.contains(&file(9)));
        store.clear_recents().unwrap();
        assert_eq!(store.recents(), Ok(Vec::new()));
    }
    #[test]
    fn concurrent_pins_and_opens_all_survive() {
        let (_dir, dir) = real_tempdir();
        let root = [volume("Macintosh HD", "/".as_ref(), Some("R"))];
        let folders: Vec<PathBuf> = (0..40).map(|n| dir.join(format!("f{n}"))).collect();
        for folder in &folders {
            std::fs::create_dir(folder).unwrap();
        }
        // Each thread stands in for a process: its own Store, the same files.
        std::thread::scope(|scope| {
            for chunk in folders.chunks(5) {
                let (store, root) = (store(&dir), &root);
                scope.spawn(move || {
                    for folder in chunk {
                        store.pin(folder, None, root).unwrap();
                        store.opened(&folder.join("a.txt")).unwrap();
                    }
                });
            }
        });
        let store = store(&dir);
        let pinned = paths(&store, &root);
        assert!(folders.iter().all(|f| pinned.contains(f)), "{pinned:?}");
        assert_eq!(store.recents().unwrap().len(), folders.len());
    }

    #[test]
    fn saved_searches_save_edit_move_rename_and_remove_beside_the_favorites() {
        let (_dir, dir) = real_tempdir();
        let (store, root) = (
            store(&dir),
            [volume("Macintosh HD", "/".as_ref(), Some("R"))],
        );
        assert_eq!(store.searches(), Ok(Vec::new()));
        let search = |name: &str, query: &str, scope: Scope| SavedSearch {
            name: name.into(),
            query: query.into(),
            scope,
        };
        let pictures = Scope::Folder("/Users/me/Pictures".into());
        let saved = store
            .save_search(
                search(" Big photos ", "kind:image  size:>1MB", pictures.clone()),
                None,
            )
            .unwrap();
        assert_eq!(
            saved,
            search("Big photos", "kind:image size:>1mb", pictures.clone())
        );
        store
            .save_search(search("Recent", "modified:7d", Scope::Everywhere), None)
            .unwrap();
        store
            .save_search(search("Code", "kind:code", Scope::Everywhere), Some(0))
            .unwrap();
        let names = |store: &Store| -> Vec<String> {
            store
                .searches()
                .unwrap()
                .into_iter()
                .map(|s| s.name)
                .collect()
        };
        assert_eq!(names(&store), ["Code", "Big photos", "Recent"]);

        // Saving under a name edits that search where it is; `at` moves it.
        store
            .save_search(search("Big photos", "kind:image size:>5mb", pictures), None)
            .unwrap();
        assert_eq!(names(&store), ["Code", "Big photos", "Recent"]);
        assert_eq!(store.searches().unwrap()[1].query, "kind:image size:>5mb");
        let recent = store.searches().unwrap().remove(2);
        store.save_search(recent, Some(0)).unwrap();
        assert_eq!(names(&store), ["Recent", "Code", "Big photos"]);

        store.rename_search("Code", "Source").unwrap();
        assert_eq!(names(&store), ["Recent", "Source", "Big photos"]);
        for (result, error) in [
            (
                store.rename_search("Source", "Recent"),
                "Recent is already a saved search.",
            ),
            (
                store.rename_search("Code", "Other"),
                "Code isn't a saved search.",
            ),
            (store.rename_search("Source", "  "), "Name the search."),
            (store.remove_search("Code"), "Code isn't a saved search."),
        ] {
            assert_eq!(result, Err(error.into()));
        }
        for (bad, error) in [
            (
                search("X", "size:big", Scope::Everywhere),
                "size:big: give a size",
            ),
            (search("X", "   ", Scope::Everywhere), "X: type something"),
            (search("", "a", Scope::Everywhere), "Name the search."),
            (
                search("X", "a", Scope::Folder("rel".into())),
                "rel: give an absolute",
            ),
        ] {
            let got = store.save_search(bad, None).unwrap_err();
            assert!(got.starts_with(error), "{got}");
        }

        // Favorites and searches share places.json and keep each other.
        let work = dir.join("Work");
        std::fs::create_dir(&work).unwrap();
        store.pin(&work, None, &root).unwrap();
        store.remove_search("Recent").unwrap();
        assert_eq!(paths(&store, &root).last(), Some(&work));
        assert_eq!(names(&store), ["Source", "Big photos"]);
        let json = std::fs::read_to_string(&store.places).unwrap();
        assert!(
            json.contains("\"folder\": \"/Users/me/Pictures\""),
            "{json}"
        );
        assert!(json.contains("\"scope\": \"everywhere\""), "{json}");

        // A list that doesn't read is never replaced by a change.
        std::fs::write(&store.places, "{").unwrap();
        assert!(store.searches().is_err());
        assert!(store.remove_search("Source").is_err());
        assert_eq!(std::fs::read_to_string(&store.places).unwrap(), "{");
    }

    #[test]
    fn concurrent_saved_searches_and_pins_all_survive() {
        let (_dir, dir) = real_tempdir();
        let root = [volume("Macintosh HD", "/".as_ref(), Some("R"))];
        let folders: Vec<PathBuf> = (0..20).map(|n| dir.join(format!("f{n}"))).collect();
        for folder in &folders {
            std::fs::create_dir(folder).unwrap();
        }
        std::thread::scope(|scope| {
            for (t, chunk) in folders.chunks(4).enumerate() {
                let (store, root) = (store(&dir), &root);
                scope.spawn(move || {
                    for (n, folder) in chunk.iter().enumerate() {
                        let search = SavedSearch {
                            name: format!("s{t}-{n}"),
                            query: format!("ext:rs in:{}", folder.display()),
                            scope: Scope::Everywhere,
                        };
                        store.save_search(search, None).unwrap();
                        store.pin(folder, None, root).unwrap();
                    }
                });
            }
        });
        let store = store(&dir);
        assert_eq!(store.searches().unwrap().len(), folders.len());
        let pinned = paths(&store, &root);
        assert!(folders.iter().all(|f| pinned.contains(f)), "{pinned:?}");
    }

    #[test]
    fn a_link_pins_the_folder_it_leads_to_on_that_folders_volume() {
        let (_dir, dir) = real_tempdir();
        let store = store(&dir);
        let (first, second) = (dir.join("Backup"), dir.join("Backup 1"));
        for mount in [&first, &second] {
            std::fs::create_dir_all(mount.join("Photos")).unwrap();
        }
        let link = dir.join("Photos link");
        std::os::unix::fs::symlink(first.join("Photos"), &link).unwrap();
        let root = volume("Macintosh HD", "/".as_ref(), Some("R"));
        let mounted = [root.clone(), volume("Backup", &first, Some("U1"))];
        store.pin(&link, Some(0), &mounted).unwrap();
        let remounted = [root, volume("Backup", &second, Some("U1"))];
        let place = store.places(&remounted).unwrap().remove(0);
        assert_eq!(place.favorite.path, first.join("Photos"));
        assert_eq!(
            place.availability,
            Availability::Available(second.join("Photos"))
        );
    }

    #[test]
    fn a_recents_file_that_doesnt_read_is_never_overwritten() {
        let (_dir, dir) = real_tempdir();
        let store = store(&dir);
        std::fs::create_dir_all(store.recents.parent().unwrap()).unwrap();
        std::fs::write(&store.recents, "[").unwrap();
        assert!(store.opened("/files/a.txt".as_ref()).is_err());
        assert!(store.recents().is_err());
        assert_eq!(std::fs::read_to_string(&store.recents).unwrap(), "[");
    }

    #[test]
    fn another_drive_at_a_missing_drives_path_pins_its_own_folder() {
        let (_dir, dir) = real_tempdir();
        let store = store(&dir);
        let mount = dir.join("Backup");
        std::fs::create_dir_all(mount.join("Photos")).unwrap();
        let photos = mount.join("Photos");
        let root = volume("Macintosh HD", "/".as_ref(), Some("R"));
        store
            .pin(
                &photos,
                Some(0),
                &[root.clone(), volume("Backup", &mount, Some("A"))],
            )
            .unwrap();
        // Drive A is gone and drive B mounts where it was.
        let other = [root, volume("Backup", &mount, Some("B"))];
        store.pin(&photos, Some(0), &other).unwrap();
        let places = store.places(&other).unwrap();
        let ids: Vec<_> = places[..2]
            .iter()
            .map(|place| place.favorite.volume.as_ref().map(|v| v.id.clone()))
            .collect();
        assert_eq!(
            ids,
            [
                Some(VolumeId::Uuid("B".into())),
                Some(VolumeId::Uuid("A".into()))
            ]
        );
        assert_eq!(
            places[0].availability,
            Availability::Available(photos.clone())
        );
        assert!(matches!(
            places[1].availability,
            Availability::Unavailable(_)
        ));
        // Unpinning takes the folder that's there first, then A's by its
        // saved path.
        store.unpin(&photos, &other).unwrap();
        let left = store.places(&other).unwrap();
        assert!(matches!(left[0].availability, Availability::Unavailable(_)));
        store.unpin(&photos, &other).unwrap();
        assert_eq!(store.places(&other).unwrap().len(), 5);
    }
}
