//! Download bookkeeping for the actor.
//!
//! Flow: `QueueUpload` → peer's `TransferRequest` (we accept) → peer opens
//! an `F` connection → [`crate::transfer::receive`].

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use seekr_proto::peer::{PeerMsg, TransferDirection};
use tokio::net::TcpStream;
use tokio::task::AbortHandle;

use super::{Actor, Event, Internal};
use crate::transfer;

pub type DownloadId = u64;

/// Give up if the uploader accepted but never opens the file connection.
const FILE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);

/// What we tell peers that try to download from us until sharing exists.
const NOT_SHARED: &str = "File not shared.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    /// Waiting in the peer's upload queue; `place` once they tell us.
    Queued {
        place: Option<u32>,
    },
    Transferring {
        received: u64,
        size: u64,
    },
    Completed {
        path: PathBuf,
    },
    Failed {
        reason: String,
    },
}

impl DownloadState {
    fn is_finished(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. })
    }
}

pub(super) struct Download {
    username: String,
    filename: String,
    state: DownloadState,
    /// Set once the peer sends `TransferRequest`.
    transfer: Option<(u32, u64)>,
    task: Option<AbortHandle>,
}

impl Actor {
    pub(super) async fn start_download(
        &mut self,
        id: DownloadId,
        username: String,
        filename: String,
    ) {
        self.downloads.insert(
            id,
            Download {
                username: username.clone(),
                filename: filename.clone(),
                state: DownloadState::Queued { place: None },
                transfer: None,
                task: None,
            },
        );
        self.emit_download(id);
        self.send_peer(
            username.clone(),
            PeerMsg::QueueUpload {
                filename: filename.clone(),
            },
        )
        .await;
        self.send_peer(username, PeerMsg::PlaceInQueueRequest { filename })
            .await;
    }

    pub(super) fn cancel_download(&mut self, id: DownloadId) {
        if let Some(d) = self.downloads.get_mut(&id)
            && !d.state.is_finished()
        {
            if let Some(task) = d.task.take() {
                task.abort();
            }
            self.set_state(
                id,
                DownloadState::Failed {
                    reason: "cancelled".to_owned(),
                },
            );
        }
    }

    fn emit_download(&self, id: DownloadId) {
        if let Some(d) = self.downloads.get(&id) {
            self.emit(Event::Download {
                id,
                username: d.username.clone(),
                filename: d.filename.clone(),
                state: d.state.clone(),
            });
        }
    }

    fn set_state(&mut self, id: DownloadId, state: DownloadState) {
        if let Some(d) = self.downloads.get_mut(&id) {
            d.state = state;
            self.emit_download(id);
        }
    }

    /// The active (unfinished) download of `filename` from `username`.
    fn find_download(&self, username: &str, filename: &str) -> Option<DownloadId> {
        self.downloads
            .iter()
            .find(|(_, d)| {
                d.username == username && d.filename == filename && !d.state.is_finished()
            })
            .map(|(id, _)| *id)
    }

    /// Returns the message if it is not about transfers.
    pub(super) fn on_transfer_message(&mut self, username: &str, msg: PeerMsg) -> Option<PeerMsg> {
        match msg {
            PeerMsg::TransferRequest {
                direction: TransferDirection::Upload,
                token,
                filename,
                size,
            } => {
                let found = self
                    .find_download(username, &filename)
                    .filter(|id| self.downloads[id].task.is_none());
                let response = match found {
                    Some(id) => {
                        let size = size.unwrap_or(0);
                        self.downloads.get_mut(&id).unwrap().transfer = Some((token, size));
                        let tx = self.internal.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(FILE_CONNECTION_TIMEOUT).await;
                            let _ = tx.send(Internal::FileConnectionTimeout { id, token });
                        });
                        PeerMsg::TransferResponse {
                            token,
                            allowed: true,
                            size: None,
                            reason: None,
                        }
                    }
                    None => PeerMsg::TransferResponse {
                        token,
                        allowed: false,
                        size: None,
                        reason: Some("Cancelled".to_owned()),
                    },
                };
                self.reply(username, response);
            }
            PeerMsg::TransferRequest {
                direction: TransferDirection::Download,
                token,
                ..
            } => self.reply(
                username,
                PeerMsg::TransferResponse {
                    token,
                    allowed: false,
                    size: None,
                    reason: Some(NOT_SHARED.to_owned()),
                },
            ),
            PeerMsg::QueueUpload { filename } => self.reply(
                username,
                PeerMsg::UploadDenied {
                    filename,
                    reason: NOT_SHARED.to_owned(),
                },
            ),
            PeerMsg::PlaceInQueueResponse { filename, place } => {
                if let Some(id) = self.find_download(username, &filename)
                    && matches!(self.downloads[&id].state, DownloadState::Queued { .. })
                {
                    self.set_state(id, DownloadState::Queued { place: Some(place) });
                }
            }
            PeerMsg::UploadDenied { filename, reason } => {
                if let Some(id) = self.find_download(username, &filename)
                    && self.downloads[&id].task.is_none()
                {
                    self.set_state(id, DownloadState::Failed { reason });
                }
            }
            PeerMsg::UploadFailed { filename } => {
                if let Some(id) = self.find_download(username, &filename)
                    && self.downloads[&id].task.is_none()
                {
                    self.set_state(
                        id,
                        DownloadState::Failed {
                            reason: "upload failed on the peer's side".to_owned(),
                        },
                    );
                }
            }
            other => return Some(other),
        }
        None
    }

    fn reply(&self, username: &str, msg: PeerMsg) {
        if let Some(handle) = self.peers.get(username) {
            handle.send(msg);
        }
    }

    /// An `F` connection from `username`; read its token off the actor.
    pub(super) fn on_file_connection(&self, username: String, mut stream: TcpStream) {
        let tx = self.internal.clone();
        tokio::spawn(async move {
            match transfer::read_transfer_init(&mut stream).await {
                Ok(token) => {
                    let _ = tx.send(Internal::FileConnection {
                        username,
                        token,
                        stream,
                    });
                }
                Err(e) => tracing::debug!(%username, %e, "bad file connection"),
            }
        });
    }

    pub(super) fn on_file_transfer_init(
        &mut self,
        username: String,
        token: u32,
        stream: TcpStream,
    ) {
        let found = self.downloads.iter().find(|(_, d)| {
            d.username == username
                && d.task.is_none()
                && !d.state.is_finished()
                && d.transfer.is_some_and(|(t, _)| t == token)
        });
        let Some((&id, d)) = found else {
            tracing::debug!(%username, token, "file connection for unknown transfer");
            return;
        };
        let size = d.transfer.unwrap().1;
        let target = transfer::local_path(&self.download_dir, &d.filename);

        let tx = self.internal.clone();
        let progress_tx = tx.clone();
        let task = tokio::spawn(async move {
            let result = transfer::receive(stream, target, size, |received| {
                let _ = progress_tx.send(Internal::DownloadProgress { id, received });
            })
            .await;
            let _ = tx.send(Internal::DownloadDone { id, result });
        });
        self.downloads.get_mut(&id).unwrap().task = Some(task.abort_handle());
        self.set_state(id, DownloadState::Transferring { received: 0, size });
    }

    pub(super) fn on_download_progress(&mut self, id: DownloadId, received: u64) {
        if let Some(d) = self.downloads.get(&id)
            && let DownloadState::Transferring { size, .. } = d.state
        {
            self.set_state(id, DownloadState::Transferring { received, size });
        }
    }

    pub(super) fn on_download_done(&mut self, id: DownloadId, result: io::Result<PathBuf>) {
        let Some(d) = self.downloads.get_mut(&id) else {
            return;
        };
        if d.state.is_finished() {
            return; // cancelled meanwhile
        }
        d.task = None;
        let state = match result {
            Ok(path) => DownloadState::Completed { path },
            Err(e) => DownloadState::Failed {
                reason: e.to_string(),
            },
        };
        self.set_state(id, state);
    }

    pub(super) fn on_file_connection_timeout(&mut self, id: DownloadId, token: u32) {
        if let Some(d) = self.downloads.get(&id)
            && d.task.is_none()
            && !d.state.is_finished()
            && d.transfer.is_some_and(|(t, _)| t == token)
        {
            self.set_state(
                id,
                DownloadState::Failed {
                    reason: "peer never opened the file connection".to_owned(),
                },
            );
        }
    }

    /// The `P` connection needed to queue these downloads failed.
    pub(super) fn fail_queued_downloads(&mut self, username: &str, reason: &str) {
        let ids: Vec<_> = self
            .downloads
            .iter()
            .filter(|(_, d)| {
                d.username == username && d.transfer.is_none() && !d.state.is_finished()
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.set_state(
                id,
                DownloadState::Failed {
                    reason: format!("could not reach user: {reason}"),
                },
            );
        }
    }
}
