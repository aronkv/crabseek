//! Uploads for the actor.
//!
//! Flow: peer sends `QueueUpload` → we queue it → when a slot is free we
//! send `TransferRequest` (upload) → the peer accepts with
//! `TransferResponse` → we open an `F` connection (direct or via the
//! server), send the token, read the peer's `FileOffset`, and stream the
//! file from there. The downloader closes the connection when done.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crabseek_proto::ConnectionType;
use crabseek_proto::peer::{PeerMsg, TransferDirection};
use crabseek_proto::server::ServerRequest;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::AbortHandle;

use super::{Actor, Event, Internal, Purpose};

pub type UploadId = u64;

/// How long a peer may take to answer our `TransferRequest`.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the downloader may take to send `FileOffset`.
const OFFSET_TIMEOUT: Duration = Duration::from_secs(30);
/// How long we wait for the downloader to close after the last byte.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the downloader may take nothing before the upload fails, so a
/// peer that stops reading does not hold a slot forever.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const NOT_SHARED: &str = "File not shared.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadState {
    Queued,
    /// We offered the file and wait for the peer or the connection.
    Starting,
    Transferring {
        sent: u64,
        size: u64,
    },
    Completed,
    Failed {
        reason: String,
    },
}

impl UploadState {
    fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed { .. })
    }
}

pub(super) struct Upload {
    username: String,
    filename: String,
    local: PathBuf,
    size: u64,
    state: UploadState,
    /// The token of our `TransferRequest`, once sent.
    token: Option<u32>,
    task: Option<AbortHandle>,
}

impl Upload {
    pub(super) fn is_completed(&self) -> bool {
        self.state == UploadState::Completed
    }

    fn occupies_slot(&self) -> bool {
        matches!(
            self.state,
            UploadState::Starting | UploadState::Transferring { .. }
        )
    }
}

impl Actor {
    fn emit_upload(&self, id: UploadId) {
        if let Some(u) = self.uploads.get(&id) {
            self.emit(Event::Upload {
                id,
                username: u.username.clone(),
                filename: u.filename.clone(),
                state: u.state.clone(),
            });
        }
    }

    fn set_upload_state(&mut self, id: UploadId, state: UploadState) {
        if let Some(u) = self.uploads.get_mut(&id) {
            u.state = state;
            self.emit_upload(id);
        }
    }

    pub(super) fn upload_slot_free(&self) -> bool {
        self.uploads.values().filter(|u| u.occupies_slot()).count() < self.upload_slots
    }

    pub(super) fn upload_queue_len(&self) -> usize {
        self.uploads
            .values()
            .filter(|u| u.state == UploadState::Queued)
            .count()
    }

    /// Queues `filename` for `username` if we share it. Returns the reason
    /// to deny it otherwise.
    fn queue_upload(&mut self, username: &str, filename: &str) -> Result<(), &'static str> {
        let Some(file) = self.shares.lookup(filename) else {
            return Err(NOT_SHARED);
        };
        let already = self
            .uploads
            .values()
            .any(|u| u.username == username && u.filename == filename && !u.state.is_finished());
        if already {
            return Ok(());
        }
        let (local, size) = (file.local.clone(), file.size);
        self.next_upload_id += 1;
        let id = self.next_upload_id;
        self.uploads.insert(
            id,
            Upload {
                username: username.to_owned(),
                filename: filename.to_owned(),
                local,
                size,
                state: UploadState::Queued,
                token: None,
                task: None,
            },
        );
        self.emit_upload(id);
        Ok(())
    }

    /// Returns the message if it is not about uploads.
    pub(super) async fn on_upload_message(
        &mut self,
        username: &str,
        msg: PeerMsg,
    ) -> Option<PeerMsg> {
        match msg {
            PeerMsg::QueueUpload { filename } => {
                if let Err(reason) = self.queue_upload(username, &filename) {
                    self.reply_peer(
                        username,
                        PeerMsg::UploadDenied {
                            filename,
                            reason: reason.to_owned(),
                        },
                    );
                }
                self.start_uploads().await;
            }
            // Legacy clients ask with a download-direction TransferRequest.
            // The spec recommends queueing instead of accepting directly.
            PeerMsg::TransferRequest {
                direction: TransferDirection::Download,
                token,
                filename,
                ..
            } => {
                let reason = match self.queue_upload(username, &filename) {
                    Ok(()) => "Queued",
                    Err(reason) => reason,
                };
                self.reply_peer(
                    username,
                    PeerMsg::TransferResponse {
                        token,
                        allowed: false,
                        size: None,
                        reason: Some(reason.to_owned()),
                    },
                );
                self.start_uploads().await;
            }
            PeerMsg::PlaceInQueueRequest { filename } => {
                let place = self
                    .uploads
                    .values()
                    .filter(|u| u.state == UploadState::Queued)
                    .position(|u| u.username == username && u.filename == filename);
                if let Some(place) = place {
                    self.reply_peer(
                        username,
                        PeerMsg::PlaceInQueueResponse {
                            filename,
                            place: place as u32 + 1,
                        },
                    );
                }
            }
            PeerMsg::TransferResponse {
                token,
                allowed,
                reason,
                ..
            } => {
                let found = self.uploads.iter().find(|(_, u)| {
                    u.username == username
                        && u.token == Some(token)
                        && u.state == UploadState::Starting
                        && u.task.is_none()
                });
                // Unknown tokens are answers to offers we gave up on.
                let (&id, _) = found?;
                if allowed {
                    self.start_connect(
                        username.to_owned(),
                        ConnectionType::File,
                        Purpose::Upload(id),
                    )
                    .await;
                } else {
                    self.set_upload_state(
                        id,
                        UploadState::Failed {
                            reason: reason.unwrap_or_else(|| "refused by peer".to_owned()),
                        },
                    );
                    self.start_uploads().await;
                }
            }
            other => return Some(other),
        }
        None
    }

    fn reply_peer(&self, username: &str, msg: PeerMsg) {
        if let Some(handle) = self.peers.get(username) {
            handle.send(msg);
        }
    }

    /// Offers queued files while slots are free, one active upload per
    /// user at a time so a single user cannot take every slot.
    pub(super) async fn start_uploads(&mut self) {
        while self.upload_slot_free() {
            let busy: Vec<String> = self
                .uploads
                .values()
                .filter(|u| u.occupies_slot())
                .map(|u| u.username.clone())
                .collect();
            let next = self
                .uploads
                .iter()
                .find(|(_, u)| u.state == UploadState::Queued && !busy.contains(&u.username))
                .map(|(&id, _)| id);
            let Some(id) = next else { return };

            let token = self.tokens.next();
            let u = self.uploads.get_mut(&id).unwrap();
            u.token = Some(token);
            let msg = PeerMsg::TransferRequest {
                direction: TransferDirection::Upload,
                token,
                filename: u.filename.clone(),
                size: Some(u.size),
            };
            let username = u.username.clone();
            self.set_upload_state(id, UploadState::Starting);
            self.send_peer(username, msg).await;

            let tx = self.internal.clone();
            tokio::spawn(async move {
                tokio::time::sleep(RESPONSE_TIMEOUT).await;
                let _ = tx.send(Internal::UploadTimeout { id, token });
            });
        }
    }

    pub(super) async fn on_upload_timeout(&mut self, id: UploadId, token: u32) {
        let stuck = self.uploads.get(&id).is_some_and(|u| {
            u.token == Some(token) && u.state == UploadState::Starting && u.task.is_none()
        });
        if stuck {
            self.set_upload_state(
                id,
                UploadState::Failed {
                    reason: "peer did not respond".to_owned(),
                },
            );
            self.start_uploads().await;
        }
    }

    /// The `F` connection for upload `id` is open.
    pub(super) fn on_upload_connection(&mut self, id: UploadId, stream: TcpStream) {
        let Some(u) = self.uploads.get_mut(&id) else {
            return;
        };
        if u.state != UploadState::Starting || u.task.is_some() {
            return;
        }
        let (token, local, size) = (u.token.unwrap_or(0), u.local.clone(), u.size);
        let tx = self.internal.clone();
        let progress_tx = tx.clone();
        let task = tokio::spawn(async move {
            let result = send_file(stream, token, local, size, |sent| {
                let _ = progress_tx.send(Internal::UploadProgress { id, sent });
            })
            .await;
            let _ = tx.send(Internal::UploadDone { id, result });
        });
        u.task = Some(task.abort_handle());
        self.set_upload_state(id, UploadState::Transferring { sent: 0, size });
    }

    pub(super) fn on_upload_progress(&mut self, id: UploadId, sent: u64) {
        if let Some(u) = self.uploads.get(&id)
            && let UploadState::Transferring { size, .. } = u.state
        {
            self.set_upload_state(id, UploadState::Transferring { sent, size });
        }
    }

    pub(super) async fn on_upload_done(
        &mut self,
        id: UploadId,
        result: io::Result<(u64, Duration)>,
    ) {
        let Some(u) = self.uploads.get_mut(&id) else {
            return;
        };
        u.task = None;
        if u.state.is_finished() {
            return; // cancelled meanwhile
        }
        match result {
            Ok((sent, elapsed)) => {
                let speed = (sent as f64 / elapsed.as_secs_f64().max(0.001)) as u32;
                self.upload_speed = speed;
                self.set_upload_state(id, UploadState::Completed);
                self.send_server(ServerRequest::SendUploadSpeed { speed })
                    .await;
            }
            Err(e) => {
                let (username, filename) = (u.username.clone(), u.filename.clone());
                self.set_upload_state(
                    id,
                    UploadState::Failed {
                        reason: e.to_string(),
                    },
                );
                self.reply_peer(&username, PeerMsg::UploadFailed { filename });
            }
        }
        self.start_uploads().await;
    }

    /// We could not open the `F` connection for upload `id`.
    pub(super) async fn fail_upload_connection(&mut self, id: UploadId, reason: String) {
        if self
            .uploads
            .get(&id)
            .is_some_and(|u| !u.state.is_finished())
        {
            self.set_upload_state(
                id,
                UploadState::Failed {
                    reason: format!("could not connect: {reason}"),
                },
            );
            self.start_uploads().await;
        }
    }

    pub(super) async fn cancel_upload(&mut self, id: UploadId) {
        let Some(u) = self.uploads.get_mut(&id) else {
            return;
        };
        if u.state.is_finished() {
            return;
        }
        if let Some(task) = u.task.take() {
            task.abort();
        }
        let (username, filename) = (u.username.clone(), u.filename.clone());
        self.set_upload_state(
            id,
            UploadState::Failed {
                reason: "cancelled".to_owned(),
            },
        );
        self.reply_peer(
            &username,
            PeerMsg::UploadDenied {
                filename,
                reason: "Cancelled".to_owned(),
            },
        );
        self.start_uploads().await;
    }

    /// Forgets finished uploads, keeping the list short.
    pub(super) fn clear_finished_uploads(&mut self) {
        self.uploads.retain(|_, u| !u.state.is_finished());
    }
}

/// Sends `FileTransferInit`, reads the downloader's `FileOffset`, then
/// streams the file from that offset. Returns bytes sent and time taken.
async fn send_file(
    mut stream: TcpStream,
    token: u32,
    local: PathBuf,
    size: u64,
    mut progress: impl FnMut(u64),
) -> io::Result<(u64, Duration)> {
    stream.write_u32_le(token).await?;
    let offset = tokio::time::timeout(OFFSET_TIMEOUT, stream.read_u64_le())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no FileOffset from peer"))??;
    if offset > size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("peer asked for offset {offset} of a {size} byte file"),
        ));
    }

    let mut file = File::open(&local).await?;
    file.seek(io::SeekFrom::Start(offset)).await?;
    let started = Instant::now();
    let mut sent = offset;
    let mut last_report = Instant::now();
    let mut buf = vec![0; 256 * 1024];
    progress(sent);
    while sent < size {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "file got shorter while uploading",
            ));
        }
        let n = n.min((size - sent) as usize);
        write_all_or_stall(&mut stream, &buf[..n], STALL_TIMEOUT).await?;
        sent += n as u64;
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            progress(sent);
            last_report = Instant::now();
        }
    }
    stream.flush().await?;
    progress(sent);
    // The downloader closes the connection once it has everything.
    let mut rest = [0; 64];
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, stream.read(&mut rest)).await;
    Ok((sent - offset, started.elapsed()))
}

/// Like `write_all`, but fails if the peer takes nothing for `stall`.
/// Slow peers are fine as long as they keep reading.
async fn write_all_or_stall(
    w: &mut (impl AsyncWrite + Unpin),
    mut data: &[u8],
    stall: Duration,
) -> io::Result<()> {
    while !data.is_empty() {
        let n = tokio::time::timeout(stall, w.write(data))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "peer stopped reading"))??;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        data = &data[n..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn sends_from_offset() {
        let dir = std::env::temp_dir().join(format!("crabseek-upload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.flac");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 3) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let downloader = async move {
            let (mut s, _) = listener.accept().await.unwrap();
            assert_eq!(s.read_u32_le().await.unwrap(), 77);
            s.write_u64_le(50_000).await.unwrap();
            let mut got = vec![0; 150_000];
            s.read_exact(&mut got).await.unwrap();
            got
        };
        let uploader = async {
            let stream = TcpStream::connect(addr).await.unwrap();
            send_file(stream, 77, path.clone(), data.len() as u64, |_| {}).await
        };
        let (got, result) = tokio::join!(downloader, uploader);
        assert_eq!(got, data[50_000..]);
        assert_eq!(result.unwrap().0, 150_000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn stalled_reader_times_out() {
        let (mut w, mut r) = tokio::io::duplex(1024);
        let data = vec![7; 4096];
        // A reader that keeps up gets everything.
        let (written, read) = tokio::join!(
            write_all_or_stall(&mut w, &data, Duration::from_secs(5)),
            async {
                let mut got = vec![0; data.len()];
                r.read_exact(&mut got).await.map(|_| got)
            }
        );
        written.unwrap();
        assert_eq!(read.unwrap(), data);
        // One that stops reading fails the write instead of blocking it.
        let err = write_all_or_stall(&mut w, &data, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }
}
