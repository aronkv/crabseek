//! Receiving a file over an `F` connection.
//!
//! The uploader opens the connection and sends `FileTransferInit` (a bare
//! `uint32` token, no length prefix). We answer with `FileOffset` (a bare
//! `uint64`), then raw file data follows. The downloader closes the
//! connection once all bytes have arrived.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;

/// How long the uploader may take to send `FileTransferInit`.
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Abort if no data arrives for this long.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

pub async fn read_transfer_init(stream: &mut TcpStream) -> io::Result<u32> {
    tokio::time::timeout(INIT_TIMEOUT, stream.read_u32_le())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no FileTransferInit"))?
}

/// The longest file or folder name (in bytes) on common filesystems.
const NAME_MAX: usize = 255;
/// Room a file name leaves free for `.<tag>.part` and ` (n)`.
const SUFFIX_ROOM: usize = 24;

/// Where a remote file ends up: `<dir>/<remote parent folder>/<file name>`.
/// Path components are sanitized so a peer cannot escape `dir`, and
/// shortened so the file and its `.part` can be created.
pub fn local_path(dir: &Path, remote: &str) -> PathBuf {
    let mut parts = remote.rsplit(['\\', '/']);
    let name = shorten(
        sanitize(parts.next().unwrap_or(remote)),
        NAME_MAX - SUFFIX_ROOM,
    );
    match parts.next().map(|f| shorten(sanitize(f), NAME_MAX)) {
        Some(folder) if !folder.is_empty() => dir.join(folder).join(name),
        _ => dir.join(name),
    }
}

fn sanitize(part: &str) -> String {
    let cleaned: String = part
        .chars()
        .map(|c| {
            if c == '/' || c == '\0' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    match cleaned.trim() {
        "" | "." | ".." => "_".to_owned(),
        s => s.to_owned(),
    }
}

/// Cuts `name` down to `max` bytes at a character boundary, keeping a
/// short extension.
fn shorten(name: String, max: usize) -> String {
    if name.len() <= max {
        return name;
    }
    let ext = name
        .rfind('.')
        .filter(|&i| i > 0 && name.len() - i <= 16)
        .map_or("", |i| &name[i..]);
    let mut cut = max - ext.len();
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{ext}", &name[..cut])
}

/// Moves `part` to `path`, or to `name (1).ext`, `name (2).ext`, ... when
/// that is taken. A hard link fails instead of replacing a file, so two
/// downloads that finish together cannot overwrite each other.
async fn finish(part: &Path, path: PathBuf) -> io::Result<PathBuf> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 0.. {
        let candidate = if i == 0 {
            path.clone()
        } else {
            path.with_file_name(format!("{stem} ({i}){ext}"))
        };
        match fs::hard_link(part, &candidate).await {
            Ok(()) => {
                let _ = fs::remove_file(part).await;
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            // No hard links here (FAT, some network shares): rename after
            // a check, which can still race with another download.
            Err(_) if !fs::try_exists(&candidate).await.unwrap_or(true) => {
                fs::rename(part, &candidate).await?;
                return Ok(candidate);
            }
            Err(_) => {}
        }
    }
    unreachable!()
}

/// Identifies where a download comes from. Two users can share a file
/// under the same folder and name, which maps both to one local path; the
/// tag keeps their partial data apart.
pub fn source_tag(username: &str, remote: &str) -> u32 {
    // FNV-1a rather than `DefaultHasher`, whose output may change between
    // Rust versions and would orphan the partial files of a restart.
    username
        .bytes()
        .chain([0])
        .chain(remote.bytes())
        .fold(0x811c_9dc5, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        })
}

fn part_path(path: &Path, tag: u32) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{tag:08x}.part"));
    path.with_file_name(name)
}

/// Older versions named the partial file `<file>.part` whatever its
/// source. Take such a file over once, so an interrupted download still
/// resumes after an update.
async fn adopt_legacy_part(path: &Path, part: &Path) {
    if fs::try_exists(part).await.unwrap_or(true) {
        return;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    let _ = fs::rename(path.with_file_name(name), part).await;
}

/// Receives the file into `<target>.<tag>.part`, resuming if it already
/// exists, and renames it once complete. Returns the final path.
pub async fn receive(
    mut stream: TcpStream,
    target: PathBuf,
    tag: u32,
    size: u64,
    mut progress: impl FnMut(u64),
) -> io::Result<PathBuf> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).await?;
    }
    let part = part_path(&target, tag);
    adopt_legacy_part(&target, &part).await;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .await?;
    let mut offset = file.metadata().await?.len();
    if offset > size {
        // Not the same file as before; start over.
        file.set_len(0).await?;
        offset = 0;
    }

    stream.write_u64_le(offset).await?;
    progress(offset);

    let mut file = BufWriter::with_capacity(1024 * 1024, file);
    let mut buf = vec![0; 256 * 1024];
    let mut received = offset;
    let mut last_report = Instant::now();
    while received < size {
        let want = buf.len().min((size - received) as usize);
        let n = tokio::time::timeout(STALL_TIMEOUT, stream.read(&mut buf[..want]))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "transfer stalled"))??;
        if n == 0 {
            file.flush().await?;
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("connection closed after {received} of {size} bytes"),
            ));
        }
        file.write_all(&buf[..n]).await?;
        received += n as u64;
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            progress(received);
            last_report = Instant::now();
        }
    }
    file.flush().await?;
    file.into_inner().sync_all().await?;
    progress(received);
    // Closing the connection tells the uploader we're done.
    drop(stream);

    finish(&part, target).await
}

/// Size of a partial download, for resuming.
pub async fn partial_size(target: &Path, tag: u32) -> u64 {
    match File::open(part_path(target, tag)).await {
        Ok(f) => f.metadata().await.map(|m| m.len()).unwrap_or(0),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    const TAG: u32 = 7;

    #[test]
    fn local_paths_stay_inside_dir() {
        let dir = Path::new("/dl");
        assert_eq!(
            local_path(dir, "@@music\\Artist\\Album\\01 - Song.flac"),
            Path::new("/dl/Album/01 - Song.flac")
        );
        assert_eq!(local_path(dir, "song.mp3"), Path::new("/dl/song.mp3"));
        assert_eq!(local_path(dir, "a\\..\\..\\x"), Path::new("/dl/_/x"));
        assert_eq!(local_path(dir, "a\\.."), Path::new("/dl/a/_"));
    }

    #[test]
    fn long_names_are_shortened_to_fit() {
        let dir = Path::new("/dl");
        let long = format!("{}.flac", "x".repeat(300));
        let path = local_path(dir, &format!("Album\\{long}"));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.len() <= NAME_MAX - SUFFIX_ROOM, "{}", name.len());
        assert!(name.ends_with(".flac"));
        // Two-byte characters are never cut in half.
        let wide = format!("{}.mp3", "é".repeat(200));
        let name = local_path(dir, &wide)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(name.len() <= NAME_MAX - SUFFIX_ROOM && name.ends_with("é.mp3"));
        let folder = local_path(dir, &format!("{}\\a.flac", "d".repeat(400)));
        assert_eq!(
            folder.parent().unwrap().file_name().unwrap().len(),
            NAME_MAX
        );
        // Short names are left alone.
        assert_eq!(
            local_path(dir, "A\\song.flac"),
            Path::new("/dl/A/song.flac")
        );
    }

    #[tokio::test]
    async fn a_very_long_name_can_still_be_received() {
        let dir = temp_dir("long");
        let target = local_path(&dir, &format!("Album\\{}.flac", "x".repeat(251)));
        let data = vec![5u8; 1000];
        let (down, up) = pair().await;
        let (result, _) = tokio::join!(
            receive(down, target.clone(), TAG, 1000, |_| {}),
            upload(up, &data)
        );
        assert_eq!(std::fs::read(result.unwrap()).unwrap(), data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (a, b) = tokio::join!(TcpStream::connect(addr), listener.accept());
        (a.unwrap(), b.unwrap().0)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crabseek-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Plays the uploader: reads the offset, sends the rest of `data`.
    async fn upload(mut stream: TcpStream, data: &[u8]) -> u64 {
        let offset = stream.read_u64_le().await.unwrap();
        stream.write_all(&data[offset as usize..]).await.unwrap();
        // Wait for the downloader to close.
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.unwrap();
        offset
    }

    #[tokio::test]
    async fn receives_and_renames() {
        let dir = temp_dir("receive");
        let target = dir.join("Album").join("song.flac");
        let data: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();

        let (down, up) = pair().await;
        let (result, offset) = tokio::join!(
            receive(down, target.clone(), TAG, data.len() as u64, |_| {}),
            upload(up, &data)
        );
        assert_eq!(offset, 0);
        assert_eq!(result.unwrap(), target);
        assert_eq!(std::fs::read(&target).unwrap(), data);
        assert!(!part_path(&target, TAG).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn resumes_from_part_file() {
        let dir = temp_dir("resume");
        let target = dir.join("song.flac");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(part_path(&target, TAG), &data[..40_000]).unwrap();
        assert_eq!(partial_size(&target, TAG).await, 40_000);

        let (down, up) = pair().await;
        let (result, offset) = tokio::join!(
            receive(down, target.clone(), TAG, data.len() as u64, |_| {}),
            upload(up, &data)
        );
        assert_eq!(offset, 40_000);
        result.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_tags_tell_users_and_paths_apart() {
        let tag = source_tag("alice", "Music\\Album\\01.flac");
        assert_eq!(tag, source_tag("alice", "Music\\Album\\01.flac"));
        assert_ne!(tag, source_tag("carol", "Music\\Album\\01.flac"));
        assert_ne!(tag, source_tag("alice", "Other\\Album\\01.flac"));
        // Fixed value: a restart (or an update) has to find the same file.
        assert_eq!(source_tag("alice", "x"), 0x5444_97e7);
    }

    #[tokio::test]
    async fn sources_do_not_resume_each_others_data() {
        let dir = temp_dir("sources");
        let target = dir.join("song.flac");
        let alice: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        let carol: Vec<u8> = alice.iter().map(|b| !b).collect();

        // Alice's download is cut off part way.
        let (down, mut up) = pair().await;
        let cut = async move {
            up.read_u64_le().await.unwrap();
            up.write_all(&alice[..40_000]).await.unwrap();
        };
        let (result, ()) = tokio::join!(receive(down, target.clone(), 1, 100_000, |_| {}), cut);
        assert!(result.is_err());

        // Carol's file of the same name starts from the beginning.
        let (down, up) = pair().await;
        let (result, offset) = tokio::join!(
            receive(down, target.clone(), 2, carol.len() as u64, |_| {}),
            upload(up, &carol)
        );
        assert_eq!(offset, 0);
        assert_eq!(std::fs::read(result.unwrap()).unwrap(), carol);
        // Alice's partial data is still there for her to resume.
        assert_eq!(partial_size(&target, 1).await, 40_000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn part_file_of_an_older_version_is_resumed() {
        let dir = temp_dir("legacy");
        let target = dir.join("song.flac");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("song.flac.part"), &data[..40_000]).unwrap();

        let (down, up) = pair().await;
        let (result, offset) = tokio::join!(
            receive(down, target.clone(), TAG, data.len() as u64, |_| {}),
            upload(up, &data)
        );
        assert_eq!(offset, 40_000);
        result.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), data);
        assert!(!dir.join("song.flac.part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn early_close_keeps_part_file() {
        let dir = temp_dir("early");
        let target = dir.join("song.flac");
        let (down, mut up) = pair().await;
        let uploader = async move {
            up.read_u64_le().await.unwrap();
            up.write_all(&[1; 1000]).await.unwrap();
        };
        let (result, ()) = tokio::join!(receive(down, target.clone(), TAG, 5000, |_| {}), uploader);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(partial_size(&target, TAG).await, 1000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn existing_file_gets_numbered() {
        let dir = temp_dir("unique");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("song.flac"), b"old").unwrap();
        std::fs::write(dir.join("song.flac.part"), b"new").unwrap();
        let path = finish(&dir.join("song.flac.part"), dir.join("song.flac"))
            .await
            .unwrap();
        assert_eq!(path, dir.join("song (1).flac"));
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("song.flac")).unwrap(), b"old");
        assert!(!dir.join("song.flac.part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Both downloads of one name finish at the same moment: neither may
    /// replace the other.
    #[tokio::test]
    async fn simultaneous_finishes_keep_both_files() {
        let dir = temp_dir("race");
        std::fs::create_dir_all(&dir).unwrap();
        let mut tasks = Vec::new();
        for i in 0..8u8 {
            let part = dir.join(format!("song.flac.{i}.part"));
            std::fs::write(&part, [i]).unwrap();
            let target = dir.join("song.flac");
            tasks.push(tokio::spawn(
                async move { finish(&part, target).await.unwrap() },
            ));
        }
        let mut contents = Vec::new();
        for t in tasks {
            contents.push(std::fs::read(t.await.unwrap()).unwrap()[0]);
        }
        contents.sort_unstable();
        assert_eq!(contents, (0..8).collect::<Vec<u8>>());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
