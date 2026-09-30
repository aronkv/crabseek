//! The index of shared files: scanning, matching incoming searches, and
//! building browse responses.
//!
//! Each shared folder becomes a virtual root named after it (`Music`), and
//! peers see paths like `Music\Artist\Album\01 - Song.flac`. Audio
//! properties (bitrate, duration, sample rate, bit depth) are read with
//! lofty and cached by path, size and modification time, so rescans only
//! read new or changed files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::probe::Probe;
use seekr_proto::search::{SearchFile, attr};
use seekr_proto::shares::{SharedDirectory, SharedFileList};
use serde::{Deserialize, Serialize};

/// At most this many results per search, like Nicotine+'s default.
pub const MAX_SEARCH_RESULTS: usize = 150;

const AUDIO: &[&str] = &[
    "flac", "mp3", "ogg", "opus", "m4a", "aac", "wav", "aiff", "aif", "alac", "ape", "wv", "wma",
];
const LOSSLESS: &[&str] = &["flac", "wav", "aiff", "aif", "alac", "ape", "wv"];

#[derive(Debug, Clone)]
pub struct SharedFile {
    /// What peers see, with `\` separators.
    pub virtual_path: String,
    pub local: PathBuf,
    pub size: u64,
    pub attributes: Vec<(u32, u32)>,
    /// Lower-cased virtual path, for matching searches.
    lower: String,
}

impl SharedFile {
    fn extension(&self) -> String {
        self.local
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }

    fn folder(&self) -> &str {
        self.virtual_path
            .rsplit_once('\\')
            .map_or("", |(folder, _)| folder)
    }

    fn name(&self) -> &str {
        self.virtual_path
            .rsplit_once('\\')
            .map_or(self.virtual_path.as_str(), |(_, name)| name)
    }

    fn as_search_file(&self, full_path: bool) -> SearchFile {
        SearchFile {
            filename: if full_path {
                self.virtual_path.clone()
            } else {
                self.name().to_owned()
            },
            size: self.size,
            extension: self.extension(),
            attributes: self.attributes.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct ShareIndex {
    files: Vec<SharedFile>,
    by_path: HashMap<String, usize>,
    folders: usize,
}

/// Cached audio properties, keyed by local path.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MetadataCache {
    entries: HashMap<PathBuf, CachedMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedMeta {
    size: u64,
    modified: u64,
    attributes: Vec<(u32, u32)>,
}

impl MetadataCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)
    }
}

pub struct ScanReport {
    pub index: ShareIndex,
    pub cache: MetadataCache,
    /// Shared folders that could not be read.
    pub errors: Vec<String>,
}

impl ShareIndex {
    /// Walks the shared folders. Blocking; run it off the async runtime.
    /// Hidden files and folders are skipped.
    pub fn scan(dirs: &[PathBuf], old_cache: &MetadataCache) -> ScanReport {
        let mut index = ShareIndex::default();
        let mut cache = MetadataCache::default();
        let mut errors = Vec::new();
        let mut roots_used: HashSet<String> = HashSet::new();

        for dir in dirs {
            if !dir.is_dir() {
                errors.push(format!("{} is not a folder", dir.display()));
                continue;
            }
            let root = unique_root(dir, &mut roots_used);
            let walker = walkdir::WalkDir::new(dir)
                .follow_links(true)
                .into_iter()
                .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()));
            for entry in walker {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::debug!(%e, "skipping unreadable entry");
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let local = entry.path().to_owned();
                let Ok(relative) = local.strip_prefix(dir) else {
                    continue;
                };
                let mut virtual_path = root.clone();
                for part in relative.components() {
                    virtual_path.push('\\');
                    virtual_path.push_str(&part.as_os_str().to_string_lossy());
                }

                let size = meta.len();
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                let attributes = if is_audio(&local) {
                    let cached = old_cache
                        .entries
                        .get(&local)
                        .filter(|c| c.size == size && c.modified == modified);
                    let attributes = match cached {
                        Some(c) => c.attributes.clone(),
                        None => read_attributes(&local),
                    };
                    cache.entries.insert(
                        local.clone(),
                        CachedMeta {
                            size,
                            modified,
                            attributes: attributes.clone(),
                        },
                    );
                    attributes
                } else {
                    Vec::new()
                };

                index.push(SharedFile {
                    lower: virtual_path.to_lowercase(),
                    virtual_path,
                    local,
                    size,
                    attributes,
                });
            }
        }
        index.folders = index
            .files
            .iter()
            .map(|f| f.folder())
            .collect::<HashSet<_>>()
            .len();
        ScanReport {
            index,
            cache,
            errors,
        }
    }

    fn push(&mut self, file: SharedFile) {
        self.by_path
            .insert(file.virtual_path.clone(), self.files.len());
        self.files.push(file);
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn folder_count(&self) -> usize {
        self.folders
    }

    /// The shared file peers call `virtual_path`.
    pub fn lookup(&self, virtual_path: &str) -> Option<&SharedFile> {
        self.by_path.get(virtual_path).map(|&i| &self.files[i])
    }

    /// Files whose path contains every term of `query` and none of its
    /// `-excluded` terms, case-insensitively.
    pub fn search(&self, query: &str, max: usize) -> Vec<SearchFile> {
        let mut include = Vec::new();
        let mut exclude = Vec::new();
        for term in query.to_lowercase().split_whitespace() {
            match term.strip_prefix('-') {
                Some(t) if !t.is_empty() => exclude.push(t.to_owned()),
                Some(_) => {}
                None => include.push(term.to_owned()),
            }
        }
        // Single letters would match nearly everything.
        if !include.iter().any(|t| t.chars().count() >= 2) {
            return Vec::new();
        }
        self.files
            .iter()
            .filter(|f| {
                include.iter().all(|t| f.lower.contains(t.as_str()))
                    && !exclude.iter().any(|t| f.lower.contains(t.as_str()))
            })
            .take(max)
            .map(|f| f.as_search_file(true))
            .collect()
    }

    fn directories<'a>(&self, files: impl Iterator<Item = &'a SharedFile>) -> Vec<SharedDirectory> {
        let mut dirs: BTreeMap<&str, Vec<SearchFile>> = BTreeMap::new();
        for f in files {
            dirs.entry(f.folder())
                .or_default()
                .push(f.as_search_file(false));
        }
        dirs.into_iter()
            .map(|(path, files)| SharedDirectory {
                path: path.to_owned(),
                files,
            })
            .collect()
    }

    /// Everything we share, for `SharedFileListResponse`.
    pub fn file_list(&self) -> SharedFileList {
        SharedFileList {
            dirs: self.directories(self.files.iter()),
            private_dirs: Vec::new(),
        }
    }

    /// `folder` and its subfolders, for `FolderContentsResponse`.
    pub fn folder_contents(&self, folder: &str) -> Vec<SharedDirectory> {
        let prefix = format!("{folder}\\");
        self.directories(
            self.files
                .iter()
                .filter(|f| f.folder() == folder || f.folder().starts_with(&prefix)),
        )
    }
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_audio(path: &Path) -> bool {
    AUDIO.contains(&extension(path).as_str())
}

/// A virtual root name for `dir` that no other shared folder uses.
fn unique_root(dir: &Path, used: &mut HashSet<String>) -> String {
    let base = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Shared".to_owned());
    let mut name = base.clone();
    let mut n = 2;
    while !used.insert(name.clone()) {
        name = format!("{base} ({n})");
        n += 1;
    }
    name
}

/// Audio properties in the layout Nicotine+ uses: bitrate and duration for
/// lossy files, duration, sample rate and bit depth for lossless ones.
fn read_attributes(path: &Path) -> Vec<(u32, u32)> {
    let file = Probe::open(path)
        .map(|p| p.options(ParseOptions::new().read_tags(false)))
        .and_then(|p| p.read());
    let file = match file {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(path = %path.display(), %e, "no audio properties");
            return Vec::new();
        }
    };
    let props = file.properties();
    let mut attributes = Vec::new();
    let lossless = LOSSLESS.contains(&extension(path).as_str());
    if !lossless && let Some(bitrate) = props.audio_bitrate() {
        attributes.push((attr::BITRATE, bitrate));
    }
    let secs = props.duration().as_secs() as u32;
    if secs > 0 {
        attributes.push((attr::DURATION, secs));
    }
    if lossless {
        if let Some(rate) = props.sample_rate() {
            attributes.push((attr::SAMPLE_RATE, rate));
        }
        if let Some(depth) = props.bit_depth() {
            attributes.push((attr::BIT_DEPTH, depth.into()));
        }
    }
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("seekr-shares-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let music = root.join("Music");
        for (path, contents) in [
            ("Boards of Canada/Geogaddi/01 - Ready Lets Go.mp3", "x"),
            ("Boards of Canada/Geogaddi/cover.jpg", "xx"),
            ("Boards of Canada/Geogaddi/Scans/back.jpg", "xxx"),
            ("Aphex Twin/SAW/01 - Xtal.flac", "xxxx"),
            (".hidden/secret.mp3", "x"),
        ] {
            let p = music.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, contents).unwrap();
        }
        root
    }

    fn scan(root: &Path) -> ShareIndex {
        let report = ShareIndex::scan(&[root.join("Music")], &MetadataCache::default());
        assert!(report.errors.is_empty());
        report.index
    }

    #[test]
    fn scans_with_virtual_paths_and_skips_hidden() {
        let root = tree("scan");
        let index = scan(&root);
        assert_eq!(index.file_count(), 4);
        assert_eq!(index.folder_count(), 3);
        let f = index
            .lookup("Music\\Boards of Canada\\Geogaddi\\cover.jpg")
            .unwrap();
        assert_eq!(f.size, 2);
        assert!(index.lookup("Music\\.hidden\\secret.mp3").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_terms() {
        let root = tree("search");
        let index = scan(&root);
        let names = |q: &str| -> Vec<String> {
            index
                .search(q, MAX_SEARCH_RESULTS)
                .into_iter()
                .map(|f| f.filename)
                .collect()
        };
        assert_eq!(names("geogaddi ready").len(), 1);
        assert_eq!(names("BOARDS canada").len(), 3);
        assert_eq!(names("boards -jpg").len(), 1);
        assert!(names("a").is_empty());
        assert!(names("nothing here").is_empty());
        assert_eq!(index.search("boards", 2).len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browse_lists() {
        let root = tree("browse");
        let index = scan(&root);
        let list = index.file_list();
        assert_eq!(list.dirs.len(), 3);
        assert!(
            list.dirs
                .iter()
                .all(|d| d.files.iter().all(|f| !f.filename.contains('\\')))
        );
        let contents = index.folder_contents("Music\\Boards of Canada\\Geogaddi");
        assert_eq!(contents.len(), 2); // the folder and its Scans subfolder
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_root_names() {
        let mut used = HashSet::new();
        assert_eq!(unique_root(Path::new("/a/Music"), &mut used), "Music");
        assert_eq!(unique_root(Path::new("/b/Music"), &mut used), "Music (2)");
    }
}
