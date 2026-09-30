//! Search results as a folder tree: one row per folder, files shown when
//! the folder is expanded. The cursor follows its row when results get
//! re-sorted as new ones arrive.

use std::collections::{BTreeMap, HashMap, HashSet};

use seekr_proto::search::{SearchFile, SearchResponse};

pub type FolderId = usize;

pub struct Folder {
    pub username: String,
    pub path: String,
    pub files: Vec<SearchFile>,
    pub slot_free: bool,
    pub avg_speed: u32,
    pub queue_length: u32,
}

/// Audio format filter, cycled with `f` in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormatFilter {
    #[default]
    All,
    Flac,
    Lossless,
    Mp3_320,
    Mp3,
    M4a,
}

impl FormatFilter {
    const CYCLE: [Self; 6] = [
        Self::All,
        Self::Flac,
        Self::Lossless,
        Self::Mp3_320,
        Self::Mp3,
        Self::M4a,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all formats",
            Self::Flac => "FLAC",
            Self::Lossless => "lossless",
            Self::Mp3_320 => "MP3 320",
            Self::Mp3 => "MP3",
            Self::M4a => "M4A/AAC",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::CYCLE.iter().position(|f| *f == self).unwrap();
        Self::CYCLE[(i + 1) % Self::CYCLE.len()]
    }

    pub fn prev(self) -> Self {
        let i = Self::CYCLE.iter().position(|f| *f == self).unwrap();
        Self::CYCLE[(i + Self::CYCLE.len() - 1) % Self::CYCLE.len()]
    }

    /// Whether an audio file passes. Non-audio files (covers, lyrics)
    /// are never filtered out, so they come along with folder downloads.
    pub fn matches(self, f: &SearchFile) -> bool {
        let ext = extension(f);
        match self {
            Self::All => true,
            Self::Flac => ext == "flac",
            Self::Lossless => LOSSLESS.contains(&ext.as_str()),
            Self::Mp3_320 => ext == "mp3" && f.bitrate().is_some_and(|b| b >= 320),
            Self::Mp3 => ext == "mp3",
            Self::M4a => matches!(ext.as_str(), "m4a" | "aac"),
        }
    }
}

impl Folder {
    /// Indices of the files shown under `filter`: matching audio files
    /// plus every non-audio file.
    pub fn visible_files(&self, filter: FormatFilter) -> Vec<usize> {
        (0..self.files.len())
            .filter(|&i| {
                let f = &self.files[i];
                !is_audio(f) || filter.matches(f)
            })
            .collect()
    }

    /// Whether the folder has any audio file passing `filter`.
    fn passes(&self, filter: FormatFilter) -> bool {
        filter == FormatFilter::All || self.files.iter().any(|f| is_audio(f) && filter.matches(f))
    }

    pub fn total_size(&self, filter: FormatFilter) -> u64 {
        self.visible_files(filter)
            .into_iter()
            .map(|i| self.files[i].size)
            .sum()
    }

    /// Short quality summary of the dominant audio format among the files
    /// passing `filter`, e.g. `FLAC 16/44.1` or `MP3 320`.
    pub fn quality(&self, filter: FormatFilter) -> String {
        let mut by_ext: HashMap<String, Vec<&SearchFile>> = HashMap::new();
        for f in self
            .files
            .iter()
            .filter(|f| is_audio(f) && filter.matches(f))
        {
            by_ext.entry(extension(f)).or_default().push(f);
        }
        let Some((ext, files)) = by_ext.into_iter().max_by_key(|(_, v)| v.len()) else {
            return String::new();
        };
        let label = ext.to_uppercase();
        if let Some(f) = files.iter().find(|f| f.sample_rate().is_some()) {
            let rate = f.sample_rate().unwrap() as f64 / 1000.0;
            return match f.bit_depth() {
                Some(depth) => format!("{label} {depth}/{rate}"),
                None => format!("{label} {rate}kHz"),
            };
        }
        match files.iter().filter_map(|f| f.bitrate()).min() {
            Some(min) => format!("{label} {min}"),
            None => label,
        }
    }
}

const AUDIO: &[&str] = &[
    "flac", "mp3", "ogg", "opus", "m4a", "aac", "wav", "aiff", "aif", "alac", "ape", "wv", "wma",
];

const LOSSLESS: &[&str] = &["flac", "wav", "aiff", "aif", "alac", "ape", "wv"];

fn extension(f: &SearchFile) -> String {
    f.basename()
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default()
}

pub fn is_audio(f: &SearchFile) -> bool {
    AUDIO.contains(&extension(f).as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Folder(FolderId),
    File(FolderId, usize),
}

#[derive(Default)]
pub struct Results {
    /// Indexed by `FolderId`; never reordered.
    folders: Vec<Folder>,
    /// Display order of folder ids.
    order: Vec<FolderId>,
    expanded: HashSet<FolderId>,
    rows: Vec<Row>,
    rows_dirty: bool,
    selected: Option<Row>,
    filter: FormatFilter,
    pub users: usize,
}

impl Results {
    pub fn add(&mut self, resp: SearchResponse) {
        if resp.files.is_empty() {
            return;
        }
        self.users += 1;
        let mut by_folder: BTreeMap<String, Vec<SearchFile>> = BTreeMap::new();
        for f in resp.files {
            by_folder.entry(f.folder().to_owned()).or_default().push(f);
        }
        for (path, mut files) in by_folder {
            files.sort_by(|a, b| a.filename.cmp(&b.filename));
            self.folders.push(Folder {
                username: resp.username.clone(),
                path,
                files,
                slot_free: resp.slot_free,
                avg_speed: resp.avg_speed,
                queue_length: resp.queue_length,
            });
            self.order.push(self.folders.len() - 1);
        }
        let folders = &self.folders;
        // Free slot first, then short queues, then fast users; a user's
        // folders stay together.
        self.order.sort_by(|&a, &b| {
            let (a, b) = (&folders[a], &folders[b]);
            b.slot_free
                .cmp(&a.slot_free)
                .then(a.queue_length.cmp(&b.queue_length))
                .then(b.avg_speed.cmp(&a.avg_speed))
                .then(a.username.cmp(&b.username))
                .then(a.path.cmp(&b.path))
        });
        self.rows_dirty = true;
        if self.selected.is_none() {
            self.selected = Some(Row::Folder(self.order[0]));
        }
    }

    pub fn folder(&self, id: FolderId) -> &Folder {
        &self.folders[id]
    }

    pub fn file_count(&self) -> usize {
        self.folders.iter().map(|f| f.files.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    pub fn filter(&self) -> FormatFilter {
        self.filter
    }

    pub fn set_filter(&mut self, filter: FormatFilter) {
        self.filter = filter;
        self.rows_dirty = true;
    }

    /// Folders shown under the current filter.
    pub fn visible_folders(&mut self) -> usize {
        self.rows()
            .iter()
            .filter(|r| matches!(r, Row::Folder(_)))
            .count()
    }

    pub fn is_expanded(&self, id: FolderId) -> bool {
        self.expanded.contains(&id)
    }

    pub fn rows(&mut self) -> &[Row] {
        if self.rows_dirty {
            self.rows.clear();
            for &id in &self.order {
                let folder = &self.folders[id];
                if !folder.passes(self.filter) {
                    continue;
                }
                self.rows.push(Row::Folder(id));
                if self.expanded.contains(&id) {
                    self.rows.extend(
                        folder
                            .visible_files(self.filter)
                            .into_iter()
                            .map(|i| Row::File(id, i)),
                    );
                }
            }
            self.rows_dirty = false;
            // The selected row may have been filtered out.
            if !self.selected.is_some_and(|s| self.rows.contains(&s)) {
                self.selected = self.rows.first().copied();
            }
        }
        &self.rows
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<Row> {
        self.selected
    }

    pub fn selected_index(&mut self) -> Option<usize> {
        self.rows();
        let selected = self.selected?;
        self.rows.iter().position(|r| *r == selected)
    }

    /// Moves the cursor by `delta` rows, clamped to the list.
    pub fn move_by(&mut self, delta: isize) {
        let Some(current) = self.selected_index() else {
            return;
        };
        let rows = self.rows();
        let next = current.saturating_add_signed(delta).min(rows.len() - 1);
        self.selected = Some(rows[next]);
    }

    pub fn move_to_start(&mut self) {
        self.selected = self.rows().first().copied();
    }

    pub fn move_to_end(&mut self) {
        self.selected = self.rows().last().copied();
    }

    /// Expands a collapsed folder or collapses an expanded one; on a file,
    /// collapses its folder.
    pub fn toggle(&mut self) {
        match self.selected {
            Some(Row::Folder(id)) if self.expanded.contains(&id) => self.collapse(),
            Some(Row::Folder(id)) => {
                self.expanded.insert(id);
                self.rows_dirty = true;
            }
            Some(Row::File(..)) => self.collapse(),
            None => {}
        }
    }

    pub fn expand(&mut self) {
        if let Some(Row::Folder(id)) = self.selected {
            self.expanded.insert(id);
            self.rows_dirty = true;
        }
    }

    pub fn collapse(&mut self) {
        let id = match self.selected {
            Some(Row::Folder(id) | Row::File(id, _)) => id,
            None => return,
        };
        self.expanded.remove(&id);
        self.selected = Some(Row::Folder(id));
        self.rows_dirty = true;
    }

    /// `(username, remote path)` of the files under the cursor: the file
    /// itself, or every file of a folder that passes the filter.
    pub fn selection_files(&self) -> Vec<(String, String)> {
        match self.selected {
            Some(Row::File(id, i)) => {
                let f = &self.folders[id];
                vec![(f.username.clone(), f.files[i].filename.clone())]
            }
            Some(Row::Folder(id)) => {
                let f = &self.folders[id];
                f.visible_files(self.filter)
                    .into_iter()
                    .map(|i| (f.username.clone(), f.files[i].filename.clone()))
                    .collect()
            }
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, attrs: Vec<(u32, u32)>) -> SearchFile {
        SearchFile {
            filename: path.into(),
            size: 1000,
            extension: String::new(),
            attributes: attrs,
        }
    }

    fn response(user: &str, slot_free: bool, speed: u32, files: Vec<SearchFile>) -> SearchResponse {
        SearchResponse {
            username: user.into(),
            token: 1,
            files,
            slot_free,
            avg_speed: speed,
            queue_length: 0,
            private_files: vec![],
        }
    }

    fn folder_users(r: &mut Results) -> Vec<String> {
        let rows = r.rows().to_vec();
        rows.iter()
            .filter_map(|row| match row {
                Row::Folder(id) => Some(r.folder(*id).username.clone()),
                Row::File(..) => None,
            })
            .collect()
    }

    #[test]
    fn groups_by_folder_and_sorts() {
        let mut r = Results::default();
        r.add(response("slow", true, 10, vec![file("a\\1.mp3", vec![])]));
        r.add(response("busy", false, 999, vec![file("b\\1.mp3", vec![])]));
        r.add(response(
            "fast",
            true,
            500,
            vec![
                file("x\\1.flac", vec![]),
                file("y\\2.flac", vec![]),
                file("x\\3.flac", vec![]),
            ],
        ));
        assert_eq!(folder_users(&mut r), ["fast", "fast", "slow", "busy"]);
        assert_eq!(r.users, 3);
        assert_eq!(r.file_count(), 5);
    }

    #[test]
    fn cursor_follows_its_row_when_resorted() {
        let mut r = Results::default();
        r.add(response("slow", true, 10, vec![file("a\\1.mp3", vec![])]));
        let first = r.selected();
        r.add(response("fast", true, 500, vec![file("b\\1.mp3", vec![])]));
        assert_eq!(r.selected(), first);
        assert_eq!(r.selected_index(), Some(1));
    }

    #[test]
    fn expand_navigate_collapse() {
        let mut r = Results::default();
        r.add(response(
            "u",
            true,
            1,
            vec![file("a\\1.mp3", vec![]), file("a\\2.mp3", vec![])],
        ));
        r.add(response("v", true, 0, vec![file("b\\1.mp3", vec![])]));
        assert_eq!(r.rows().len(), 2);

        r.toggle();
        assert_eq!(r.rows().len(), 4);
        r.move_by(2);
        let Some(Row::File(id, 1)) = r.selected() else {
            panic!("expected second file, got {:?}", r.selected());
        };
        assert_eq!(
            r.selection_files(),
            [("u".to_owned(), "a\\2.mp3".to_owned())]
        );

        r.collapse();
        assert_eq!(r.selected(), Some(Row::Folder(id)));
        assert_eq!(r.rows().len(), 2);
        assert_eq!(r.selection_files().len(), 2);

        r.move_by(100);
        assert_eq!(r.selected_index(), Some(1));
        r.move_by(-100);
        assert_eq!(r.selected_index(), Some(0));
    }

    #[test]
    fn quality_summary() {
        let mut r = Results::default();
        r.add(response(
            "u",
            true,
            1,
            vec![
                file("a\\1.flac", vec![(1, 100), (4, 44100), (5, 16)]),
                file("a\\2.flac", vec![(1, 100), (4, 44100), (5, 16)]),
                file("a\\cover.jpg", vec![]),
                file("b\\1.mp3", vec![(0, 320)]),
                file("b\\2.mp3", vec![(0, 256)]),
            ],
        ));
        assert_eq!(r.folder(0).quality(FormatFilter::All), "FLAC 16/44.1");
        assert_eq!(r.folder(1).quality(FormatFilter::All), "MP3 256");
    }

    #[test]
    fn format_filter() {
        let mut r = Results::default();
        r.add(response(
            "u",
            true,
            1,
            vec![
                file("both\\1.flac", vec![(4, 44100), (5, 16)]),
                file("both\\1.mp3", vec![(0, 320)]),
                file("both\\cover.jpg", vec![]),
                file("v0\\1.mp3", vec![(0, 245)]),
                file("aac\\1.m4a", vec![(0, 256)]),
            ],
        ));
        assert_eq!(r.visible_folders(), 3);

        r.set_filter(FormatFilter::Mp3_320);
        assert_eq!(r.visible_folders(), 1);
        // Only the 320 MP3 and the cover come along with the folder.
        let files: Vec<String> = r.selection_files().into_iter().map(|(_, f)| f).collect();
        assert_eq!(files, ["both\\1.mp3", "both\\cover.jpg"]);
        // Folders are created in path order: aac, both, v0.
        assert_eq!(r.folder(1).quality(FormatFilter::Mp3_320), "MP3 320");

        r.set_filter(FormatFilter::M4a);
        assert_eq!(r.visible_folders(), 1);
        assert_eq!(r.selected_index(), Some(0));

        r.set_filter(FormatFilter::Mp3);
        assert_eq!(r.visible_folders(), 2);
        assert_eq!(FormatFilter::All.prev(), FormatFilter::M4a);
        assert_eq!(FormatFilter::M4a.next(), FormatFilter::All);
    }
}
