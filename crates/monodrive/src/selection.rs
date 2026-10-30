//! What's selected in a listing. It holds paths, not rows, so it outlives
//! sorting, refreshes and live updates; the anchor is where a range grows
//! from, and the cursor is where the keys move from. Views map clicks and
//! keys onto it, so the list and the grid select alike.

use monodrive_core::entries::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Selection {
    /// Each selected entry's listed size, so the total needs no lookup.
    paths: HashMap<PathBuf, u64>,
    /// What a rectangle being dragged covers, on top of `paths` until the
    /// drag ends, so each move costs what it covers rather than everything
    /// already selected.
    swept: HashMap<PathBuf, u64>,
    anchor: Option<PathBuf>,
    cursor: Option<PathBuf>,
}

impl Selection {
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains_key(path) || self.swept.contains_key(path)
    }

    /// What a drag adds beyond what was already selected.
    fn swept_only(&self) -> impl Iterator<Item = (&PathBuf, &u64)> {
        self.swept
            .iter()
            .filter(|(path, _)| !self.paths.contains_key(*path))
    }

    pub fn len(&self) -> usize {
        self.paths.len() + self.swept_only().count()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.swept.is_empty()
    }

    /// The selected files' sizes added up; folders count none.
    pub fn size(&self) -> u64 {
        self.paths.values().sum::<u64>() + self.swept_only().map(|(_, size)| size).sum::<u64>()
    }

    /// What's selected, if that's one entry.
    pub fn only(&self) -> Option<&Path> {
        if self.len() != 1 {
            return None;
        }
        self.paths
            .keys()
            .chain(self.swept.keys())
            .next()
            .map(PathBuf::as_path)
    }

    pub fn cursor(&self) -> Option<&Path> {
        self.cursor.as_deref()
    }

    /// The cursor's row in `entries`.
    pub fn cursor_ix(&self, entries: &[Entry]) -> Option<usize> {
        let cursor = self.cursor.as_deref()?;
        entries.iter().position(|entry| entry.path == cursor)
    }

    /// The selected entries, in the order shown.
    pub fn in_order<'a>(&self, entries: &'a [Entry]) -> Vec<&'a Entry> {
        entries.iter().filter(|e| self.contains(&e.path)).collect()
    }

    /// A click: this one alone.
    pub fn select(&mut self, entry: &Entry) {
        self.paths.clear();
        self.paths.insert(entry.path.clone(), entry.listed_size());
        self.anchor = Some(entry.path.clone());
        self.cursor = Some(entry.path.clone());
    }

    /// ⌘-click: in or out, and the next range grows from here.
    pub fn toggle(&mut self, entry: &Entry) {
        if self.paths.remove(&entry.path).is_none() {
            self.paths.insert(entry.path.clone(), entry.listed_size());
        }
        self.anchor = Some(entry.path.clone());
        self.cursor = Some(entry.path.clone());
    }

    /// ⇧-click and ⇧ with the arrows: everything shown from the anchor to
    /// row `to`, and nothing else. Without an anchor, the range starts at `to`.
    pub fn extend(&mut self, entries: &[Entry], to: usize) {
        let Some(target) = entries.get(to) else {
            return;
        };
        let from = self
            .anchor
            .as_deref()
            .and_then(|anchor| entries.iter().position(|e| e.path == anchor))
            .unwrap_or(to);
        let range = from.min(to)..=from.max(to);
        self.paths = entries[range]
            .iter()
            .map(|entry| (entry.path.clone(), entry.listed_size()))
            .collect();
        if self.anchor.is_none() {
            self.anchor = Some(target.path.clone());
        }
        self.cursor = Some(target.path.clone());
    }

    /// A rectangle's drag begins: on top of what's selected with ⌘ or ⇧
    /// (`additive`), else on its own.
    pub fn begin_sweep(&mut self, additive: bool) {
        if !additive {
            self.paths.clear();
        }
        self.swept.clear();
    }

    /// The rows the rectangle covers now, in order.
    pub fn sweep(&mut self, entries: &[Entry], hit: &[usize]) {
        let swept: Vec<&Entry> = hit.iter().filter_map(|ix| entries.get(*ix)).collect();
        self.swept = swept
            .iter()
            .map(|entry| (entry.path.clone(), entry.listed_size()))
            .collect();
        if let (Some(first), Some(last)) = (swept.first(), swept.last()) {
            self.anchor = Some(first.path.clone());
            self.cursor = Some(last.path.clone());
        }
    }

    /// The drag ends: what it covered stays selected.
    pub fn end_sweep(&mut self) {
        self.paths.extend(self.swept.drain());
    }

    /// A folder closes: whatever was selected inside it gives way to the
    /// folder, and the rest stays selected.
    pub fn fold(&mut self, folder: &Entry) {
        let inside = |path: &Path| path != folder.path && path.starts_with(&folder.path);
        let before = self.paths.len();
        self.paths.retain(|path, _| !inside(path));
        if self.paths.len() != before {
            self.paths.insert(folder.path.clone(), folder.listed_size());
        }
        for end in [&mut self.anchor, &mut self.cursor] {
            if end.as_deref().is_some_and(inside) {
                *end = Some(folder.path.clone());
            }
        }
    }

    /// An item moved from `from` to `to`, as by a rename, keeps its place:
    /// still selected, and still the anchor or cursor if it was.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        if let Some(size) = self.paths.remove(from) {
            self.paths.insert(to.to_path_buf(), size);
        }
        for end in [&mut self.anchor, &mut self.cursor] {
            if end.as_deref() == Some(from) {
                *end = Some(to.to_path_buf());
            }
        }
    }

    pub fn select_all(&mut self, entries: &[Entry]) {
        self.paths = entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.listed_size()))
            .collect();
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.swept.clear();
        self.anchor = None;
        self.cursor = None;
    }

    /// Drops what's no longer shown, as when files go away or a folder
    /// closes; the anchor and cursor go with them. What stays takes its
    /// size as listed now, since a refresh can change it.
    pub fn retain(&mut self, entries: &[Entry]) {
        let shown: HashMap<&Path, u64> = entries
            .iter()
            .map(|entry| (entry.path.as_path(), entry.listed_size()))
            .collect();
        let keep = |path: &PathBuf, size: &mut u64| match shown.get(path.as_path()) {
            Some(now) => {
                *size = *now;
                true
            }
            None => false,
        };
        self.paths.retain(keep);
        self.swept.retain(keep);
        let gone = |path: &Option<PathBuf>| path.as_deref().is_some_and(|p| !shown.contains_key(p));
        if gone(&self.anchor) {
            self.anchor = None;
        }
        if gone(&self.cursor) {
            self.cursor = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monodrive_core::entries::Kind;

    fn listing(names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|name| Entry {
                path: PathBuf::from("/x").join(name),
                name: (*name).into(),
                kind: Kind::File,
                size: 10,
                modified: 0,
            })
            .collect()
    }

    fn names<'a>(selection: &Selection, entries: &'a [Entry]) -> Vec<&'a str> {
        selection
            .in_order(entries)
            .into_iter()
            .map(|e| e.name.as_str())
            .collect()
    }

    #[test]
    fn clicks_toggles_and_ranges_follow_the_order_shown() {
        let entries = listing(&["a", "b", "c", "d", "e"]);
        let mut selection = Selection::default();
        selection.select(&entries[1]);
        selection.extend(&entries, 3);
        assert_eq!(names(&selection, &entries), ["b", "c", "d"]);
        // A range replaces the last one rather than adding to it.
        selection.extend(&entries, 0);
        assert_eq!(names(&selection, &entries), ["a", "b"]);
        // ⌘-click moves the anchor, so the next range grows from there.
        selection.toggle(&entries[4]);
        assert_eq!(names(&selection, &entries), ["a", "b", "e"]);
        selection.extend(&entries, 3);
        assert_eq!(names(&selection, &entries), ["d", "e"]);
        assert_eq!(selection.size(), 20);
        selection.toggle(&entries[4]);
        assert_eq!(names(&selection, &entries), ["d"]);
        // The same paths in another order: the range is the rows between.
        let mut resorted = entries.clone();
        resorted.reverse();
        selection.select(&resorted[0]);
        selection.extend(&resorted, 2);
        assert_eq!(names(&selection, &resorted), ["e", "d", "c"]);
    }

    #[test]
    fn it_outlives_a_new_order_and_lets_go_of_what_left() {
        let entries = listing(&["a", "b", "c"]);
        let mut selection = Selection::default();
        selection.select(&entries[0]);
        selection.toggle(&entries[2]);
        let mut resorted = listing(&["c", "b"]);
        // c grew on disk since it was selected.
        resorted[0].size = 99;
        selection.retain(&resorted);
        assert_eq!(names(&selection, &resorted), ["c"]);
        assert_eq!(selection.size(), 99);
        assert_eq!(selection.cursor(), Some(Path::new("/x/c")));
        resorted.retain(|e| e.name != "c");
        selection.retain(&resorted);
        assert!(selection.is_empty() && selection.cursor().is_none());
    }

    #[test]
    fn a_sweep_adds_to_its_base_and_leaves_the_rest() {
        let entries = listing(&["a", "b", "c", "d"]);
        let mut selection = Selection::default();
        selection.select(&entries[0]);
        selection.begin_sweep(true);
        selection.sweep(&entries, &[2, 3]);
        assert_eq!(names(&selection, &entries), ["a", "c", "d"]);
        assert_eq!((selection.len(), selection.size()), (3, 30));
        // Shrinking the rectangle gives rows back.
        selection.sweep(&entries, &[2]);
        assert_eq!(names(&selection, &entries), ["a", "c"]);
        selection.end_sweep();
        assert_eq!(names(&selection, &entries), ["a", "c"]);
        // Without ⌘ or ⇧ a drag starts afresh.
        selection.begin_sweep(false);
        selection.sweep(&entries, &[1]);
        selection.end_sweep();
        assert_eq!(names(&selection, &entries), ["b"]);
        selection.select_all(&entries);
        assert_eq!(selection.len(), 4);
        selection.clear();
        assert!(selection.is_empty());
    }

    #[test]
    fn closing_a_folder_trades_what_was_inside_for_the_folder() {
        let mut entries = listing(&["docs", "docs/inner.md", "a.txt"]);
        entries[0].kind = monodrive_core::entries::Kind::Dir;
        let mut selection = Selection::default();
        selection.select(&entries[1]);
        selection.toggle(&entries[2]);
        selection.fold(&entries[0]);
        entries.remove(1);
        selection.retain(&entries);
        assert_eq!(names(&selection, &entries), ["docs", "a.txt"]);
        // A folder with nothing selected inside stays as it was.
        selection.select(&entries[1]);
        selection.fold(&entries[0]);
        assert_eq!(names(&selection, &entries), ["a.txt"]);
    }
}
