//! Search results as a folder tree: one row per folder, files shown when
//! the folder is expanded. The cursor follows its row when results get
//! re-sorted as new ones arrive.

use std::collections::{BTreeMap, HashSet};

use crabseek_proto::search::{SearchFile, SearchResponse};

use crate::search::{extension, format_kbps, has_ext, is_lossy, kbps};

pub type FolderId = usize;

#[derive(Clone)]
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
        match self {
            Self::All => true,
            Self::Flac => has_ext(f, &["flac"]),
            // ALAC hides in `.m4a`; `is_lossy` tells it from AAC.
            Self::Lossless => has_ext(f, LOSSLESS) || (has_ext(f, &["m4a"]) && !is_lossy(f)),
            Self::Mp3_320 => has_ext(f, &["mp3"]) && f.bitrate().is_some_and(|b| b >= 320),
            Self::Mp3 => has_ext(f, &["mp3"]),
            Self::M4a => has_ext(f, &["m4a", "aac"]),
        }
    }
}

impl Folder {
    /// Indices of the files shown under `filter`: matching audio files
    /// plus every non-audio file.
    pub fn visible_files(&self, filter: FormatFilter) -> Vec<usize> {
        (0..self.files.len())
            .filter(|&i| shows(filter, &self.files[i]))
            .collect()
    }

    pub fn visible_count(&self, filter: FormatFilter) -> usize {
        self.files.iter().filter(|f| shows(filter, f)).count()
    }

    /// Whether the folder has any audio file passing `filter`.
    fn passes(&self, filter: FormatFilter) -> bool {
        filter == FormatFilter::All || self.files.iter().any(|f| is_audio(f) && filter.matches(f))
    }

    pub fn total_size(&self, filter: FormatFilter) -> u64 {
        self.files
            .iter()
            .filter(|f| shows(filter, f))
            .map(|f| f.size)
            .sum()
    }

    /// Short quality summary of the dominant audio format among the files
    /// passing `filter`, e.g. `FLAC 16/44.1` or `MP3 320`.
    pub fn quality(&self, filter: FormatFilter) -> String {
        // Few extensions per folder, so a list beats a map.
        let mut by_ext: Vec<(&str, Vec<&SearchFile>)> = Vec::new();
        for f in self
            .files
            .iter()
            .filter(|f| is_audio(f) && filter.matches(f))
        {
            let ext = extension(f);
            match by_ext.iter_mut().find(|(e, _)| e.eq_ignore_ascii_case(ext)) {
                Some((_, files)) => files.push(f),
                None => by_ext.push((ext, vec![f])),
            }
        }
        let Some((ext, files)) = by_ext.into_iter().max_by_key(|(_, v)| v.len()) else {
            return String::new();
        };
        let label = ext.to_uppercase();
        let rates: Vec<(u32, bool)> = files.iter().filter_map(|f| kbps(f)).collect();
        let estimated = rates.iter().any(|(_, e)| *e);
        let lossless = files
            .iter()
            .find(|f| !is_lossy(f) && f.sample_rate().is_some());
        if let Some(f) = lossless {
            let rate = f.sample_rate().unwrap() as f64 / 1000.0;
            let mut out = match f.bit_depth() {
                Some(depth) => format!("{label} {depth}/{rate}"),
                None => format!("{label} {rate}kHz"),
            };
            // Lossless bitrates vary per track, so the average says most.
            if !rates.is_empty() {
                let avg =
                    rates.iter().map(|(k, _)| u64::from(*k)).sum::<u64>() / rates.len() as u64;
                out.push(' ');
                out.push_str(&format_kbps((avg as u32, estimated)));
            }
            return out;
        }
        // The lowest bitrate is the honest summary of a lossy folder.
        match rates.iter().map(|(k, _)| *k).min() {
            Some(min) => format!("{label} {}", format_kbps((min, estimated))),
            None => label,
        }
    }
}

const AUDIO: &[&str] = &[
    "flac", "mp3", "ogg", "opus", "m4a", "aac", "wav", "aiff", "aif", "alac", "ape", "wv", "wma",
];

const LOSSLESS: &[&str] = &["flac", "wav", "aiff", "aif", "alac", "ape", "wv"];

pub fn is_audio(f: &SearchFile) -> bool {
    has_ext(f, AUDIO)
}

/// Whether `f` is listed under `filter`: matching audio, and every
/// non-audio file.
fn shows(filter: FormatFilter, f: &SearchFile) -> bool {
    !is_audio(f) || filter.matches(f)
}

fn cmp_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
        .then_with(|| a.cmp(b))
}

/// Free slot first, then short queues, then fast users; a user's folders
/// stay together.
fn display_order(a: &Folder, b: &Folder) -> std::cmp::Ordering {
    b.slot_free
        .cmp(&a.slot_free)
        .then(a.queue_length.cmp(&b.queue_length))
        .then(b.avg_speed.cmp(&a.avg_speed))
        .then_with(|| cmp_ignore_case(&a.username, &b.username))
        .then_with(|| cmp_ignore_case(&a.path, &b.path))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Folder(FolderId),
    File(FolderId, usize),
}

#[derive(Default, Clone)]
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
    file_count: usize,
    /// Files listed under the current filter, kept up to date as folders
    /// arrive and recounted when the filter changes.
    shown_files: usize,
}

impl Results {
    /// Empty results that keep using `filter`, so a new search or browse
    /// does not reset the format the user picked.
    pub fn with_filter(filter: FormatFilter) -> Self {
        Self {
            filter,
            ..Self::default()
        }
    }

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
            let id = self.folders.len() - 1;
            // Insert in place instead of re-sorting everything per response.
            let folders = &self.folders;
            let pos = self
                .order
                .partition_point(|&other| display_order(&folders[other], &folders[id]).is_lt());
            self.order.insert(pos, id);
            self.file_count += folders[id].files.len();
            self.shown_files += self.shown_in(&self.folders[id]);
        }
        self.rows_dirty = true;
        if self.selected.is_none() {
            self.selected = Some(Row::Folder(self.order[0]));
        }
    }

    pub fn folder(&self, id: FolderId) -> &Folder {
        &self.folders[id]
    }

    pub fn file_count(&self) -> usize {
        self.file_count
    }

    pub fn folder_count(&self) -> usize {
        self.folders.len()
    }

    /// Files in the folders shown under the current filter, counting only
    /// the ones the filter lets through.
    pub fn shown_file_count(&self) -> usize {
        self.shown_files
    }

    /// Files `folder` contributes to the list under the current filter.
    fn shown_in(&self, folder: &Folder) -> usize {
        match self.filter {
            FormatFilter::All => folder.files.len(),
            filter if folder.passes(filter) => folder.visible_count(filter),
            _ => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    pub fn filter(&self) -> FormatFilter {
        self.filter
    }

    pub fn set_filter(&mut self, filter: FormatFilter) {
        self.filter = filter;
        self.shown_files = self.folders.iter().map(|f| self.shown_in(f)).sum();
        self.rows_dirty = true;
        // Move the cursor off a hidden row now, not at the next draw, so
        // keys act on what will be shown.
        self.rows();
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
            if !self.selected.is_some_and(|s| self.rows.contains(&s)) {
                self.selected = self.nearest_visible();
            }
        }
        &self.rows
    }

    /// Where the cursor goes when the filter hid its row: the folder of a
    /// hidden file, else the next shown folder below, else the one above.
    fn nearest_visible(&self) -> Option<Row> {
        let Some(Row::Folder(id) | Row::File(id, _)) = self.selected else {
            return self.rows.first().copied();
        };
        let pos = self.order.iter().position(|&o| o == id)?;
        let shown = |&&o: &&FolderId| self.folders[o].passes(self.filter);
        self.order[pos..]
            .iter()
            .find(shown)
            .or_else(|| self.order[..pos].iter().rev().find(shown))
            .map(|&o| Row::Folder(o))
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

    /// `G` with a count: jump to row `index` (clamped).
    pub fn move_to_index(&mut self, index: usize) {
        let rows = self.rows();
        if let Some(&row) = rows.get(index.min(rows.len().saturating_sub(1))) {
            self.selected = Some(row);
        }
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
        assert_eq!(r.folder(1).quality(FormatFilter::All), "MP3 256k");
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
        assert_eq!(r.folder(1).quality(FormatFilter::Mp3_320), "MP3 320k");

        r.set_filter(FormatFilter::M4a);
        assert_eq!(r.visible_folders(), 1);
        assert_eq!(r.selected_index(), Some(0));

        r.set_filter(FormatFilter::Mp3);
        assert_eq!(r.visible_folders(), 2);
        assert_eq!(FormatFilter::All.prev(), FormatFilter::M4a);
        assert_eq!(FormatFilter::M4a.next(), FormatFilter::All);
    }

    #[test]
    fn shown_file_count_follows_filter() {
        let mut r = Results::default();
        r.add(response(
            "u",
            true,
            1,
            vec![
                file("a\\1.flac", vec![]),
                file("a\\1.mp3", vec![(0, 320)]),
                file("a\\cover.jpg", vec![]),
                file("b\\1.mp3", vec![(0, 128)]),
            ],
        ));
        assert_eq!(r.shown_file_count(), 4);
        r.set_filter(FormatFilter::Flac);
        // The FLAC and the cover; folder b has no FLAC at all.
        assert_eq!(r.shown_file_count(), 2);
        assert_eq!(r.file_count(), 4);
        assert_eq!(r.folder_count(), 2);
    }

    #[test]
    fn cursor_stays_near_when_its_folder_is_filtered_out() {
        let mut r = Results::default();
        r.add(response(
            "u",
            true,
            1,
            vec![
                file("a\\1.flac", vec![]),
                file("a\\2.mp3", vec![]),
                file("b\\1.mp3", vec![]),
                file("c\\1.mp3", vec![]),
                file("d\\1.flac", vec![]),
                file("e\\1.mp3", vec![]),
            ],
        ));
        r.move_by(2); // c, an MP3 folder
        r.set_filter(FormatFilter::Flac);
        // Not back to the top (a), but on to the next FLAC folder (d).
        assert_eq!(r.selected_index(), Some(1));
        assert_eq!(
            r.selection_files(),
            [("u".to_owned(), "d\\1.flac".to_owned())]
        );

        r.set_filter(FormatFilter::All);
        r.move_to_end(); // e
        r.set_filter(FormatFilter::Flac);
        // Nothing below e passes, so the closest one above.
        assert_eq!(r.selection_files()[0].1, "d\\1.flac");

        // A hidden file row moves to its own folder when that still shows.
        r.set_filter(FormatFilter::All);
        r.move_to_start();
        r.toggle(); // expand a: a, 1.flac, 2.mp3
        r.move_by(2);
        r.set_filter(FormatFilter::Flac);
        assert_eq!(r.selected_index(), Some(0));
        assert_eq!(r.selection_files().len(), 1);
    }

    #[test]
    fn folders_sort_case_insensitively() {
        let mut r = Results::default();
        r.add(response(
            "u",
            true,
            1,
            vec![
                file("Zebra\\1.mp3", vec![]),
                file("abba\\1.mp3", vec![]),
                file("Metallica\\1.mp3", vec![]),
            ],
        ));
        let paths: Vec<String> = r
            .rows()
            .to_vec()
            .iter()
            .map(|row| match row {
                Row::Folder(id) => r.folder(*id).path.clone(),
                Row::File(..) => unreachable!(),
            })
            .collect();
        assert_eq!(paths, ["abba", "Metallica", "Zebra"]);
    }

    #[test]
    fn alac_counts_as_lossless() {
        // 3:20 at ~1600 kbps: ALAC, not AAC.
        let alac = SearchFile {
            filename: "a\\1.m4a".into(),
            size: 40_000_000,
            extension: String::new(),
            attributes: vec![(1, 200), (4, 44100), (5, 16)],
        };
        let aac = SearchFile {
            size: 6_400_000,
            ..alac.clone()
        };
        assert!(FormatFilter::Lossless.matches(&alac));
        assert!(!FormatFilter::Lossless.matches(&aac));
        assert!(FormatFilter::M4a.matches(&aac));
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// Not a real benchmark; run with `--ignored --nocapture` to see how
    /// long a large search takes to ingest and draw rows for.
    #[test]
    #[ignore]
    fn ingest_large_search() {
        let started = std::time::Instant::now();
        let mut r = Results::default();
        for user in 0..700 {
            let files = (0..80)
                .map(|i| SearchFile {
                    filename: format!(
                        "Music\\Artist {}\\Album {}\\{i:02} - Track.flac",
                        user % 50,
                        i / 12
                    ),
                    size: 30_000_000,
                    extension: "flac".into(),
                    attributes: vec![(1, 200), (4, 44100), (5, 16)],
                })
                .collect();
            r.add(SearchResponse {
                username: format!("user{user}"),
                token: 1,
                files,
                slot_free: user % 3 != 0,
                avg_speed: (user * 7919 % 10_000_000) as u32,
                queue_length: (user % 5) as u32,
                private_files: vec![],
            });
            // The UI asks for the rows after every burst.
            r.rows();
            r.visible_folders();
            r.file_count();
        }
        println!(
            "ingested 700 responses / 56000 files in {:?}",
            started.elapsed()
        );
    }
}
