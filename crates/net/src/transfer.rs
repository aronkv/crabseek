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

/// Where a remote file ends up: `<dir>/<remote parent folder>/<file name>`.
/// Path components are sanitized so a peer cannot escape `dir`.
pub fn local_path(dir: &Path, remote: &str) -> PathBuf {
    let mut parts = remote.rsplit(['\\', '/']);
    let name = sanitize(parts.next().unwrap_or(remote));
    match parts.next().map(sanitize) {
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

/// `path` if free, otherwise `name (1).ext`, `name (2).ext`, ...
async fn unique_path(path: PathBuf) -> PathBuf {
    if !fs::try_exists(&path).await.unwrap_or(false) {
        return path;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 1.. {
        let candidate = path.with_file_name(format!("{stem} ({i}){ext}"));
        if !fs::try_exists(&candidate).await.unwrap_or(false) {
            return candidate;
        }
    }
    unreachable!()
}

fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// Receives the file into `<target>.part`, resuming if it already exists,
/// and renames it once complete. Returns the final path.
pub async fn receive(
    mut stream: TcpStream,
    target: PathBuf,
    size: u64,
    mut progress: impl FnMut(u64),
) -> io::Result<PathBuf> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).await?;
    }
    let part = part_path(&target);
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

    let target = unique_path(target).await;
    fs::rename(&part, &target).await?;
    Ok(target)
}

/// Size of a partial download, for resuming.
pub async fn partial_size(target: &Path) -> u64 {
    match File::open(part_path(target)).await {
        Ok(f) => f.metadata().await.map(|m| m.len()).unwrap_or(0),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

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
            receive(down, target.clone(), data.len() as u64, |_| {}),
            upload(up, &data)
        );
        assert_eq!(offset, 0);
        assert_eq!(result.unwrap(), target);
        assert_eq!(std::fs::read(&target).unwrap(), data);
        assert!(!part_path(&target).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn resumes_from_part_file() {
        let dir = temp_dir("resume");
        let target = dir.join("song.flac");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(part_path(&target), &data[..40_000]).unwrap();
        assert_eq!(partial_size(&target).await, 40_000);

        let (down, up) = pair().await;
        let (result, offset) = tokio::join!(
            receive(down, target.clone(), data.len() as u64, |_| {}),
            upload(up, &data)
        );
        assert_eq!(offset, 40_000);
        result.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), data);
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
        let (result, ()) = tokio::join!(receive(down, target.clone(), 5000, |_| {}), uploader);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(partial_size(&target).await, 1000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn existing_file_gets_numbered() {
        let dir = temp_dir("unique");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("song.flac"), b"old").unwrap();
        assert_eq!(
            unique_path(dir.join("song.flac")).await,
            dir.join("song (1).flac")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
