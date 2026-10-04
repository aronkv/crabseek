//! Sharing for the actor: keeping the share index current and answering
//! searches and browse requests from it.

use std::path::PathBuf;
use std::sync::Arc;

use crabseek_proto::peer::PeerMsg;
use crabseek_proto::search::{SearchFile, SearchResponse};
use crabseek_proto::server::ServerRequest;
use crabseek_proto::shares::FolderContents;

use super::{Actor, Event, Internal, MAX_SEARCHES_IN_FLIGHT};
use crate::shares::{MAX_SEARCH_RESULTS, MetadataCache, ShareIndex};

impl Actor {
    /// Rescans the shared folders in the background; the result arrives
    /// as [`Internal::SharesScanned`].
    pub(super) fn rescan_shares(&mut self, dirs: Vec<PathBuf>) {
        self.shared_dirs = dirs.clone();
        let cache_path = self.share_cache.clone();
        let tx = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let old = cache_path
                .as_deref()
                .map(MetadataCache::load)
                .unwrap_or_default();
            let report = ShareIndex::scan(&dirs, &old);
            if let Some(path) = &cache_path
                && let Err(e) = report.cache.save(path)
            {
                tracing::warn!(%e, "could not save the share cache");
            }
            let _ = tx.send(Internal::SharesScanned {
                index: report.index,
                errors: report.errors,
            });
        });
        self.emit(Event::SharesScanning);
    }

    pub(super) async fn on_shares_scanned(&mut self, index: ShareIndex, errors: Vec<String>) {
        let (folders, files) = (index.folder_count(), index.file_count());
        self.shares = Arc::new(index);
        self.send_server(ServerRequest::SharedFoldersFiles {
            folders: folders as u32,
            files: files as u32,
        })
        .await;
        tracing::info!(folders, files, "shares scanned");
        self.emit(Event::SharesScanned {
            folders,
            files,
            errors,
        });
    }

    /// A user or room search relayed by the server. The index is searched
    /// on the blocking pool, so a large share does not stall the actor; the
    /// answer comes back as [`Internal::SearchDone`].
    pub(super) fn on_search_request(&mut self, username: String, token: u32, query: String) {
        if username == self.own_username {
            return;
        }
        if self.searches_in_flight >= MAX_SEARCHES_IN_FLIGHT {
            tracing::debug!(%username, %query, "too many searches in flight, dropped");
            return;
        }
        self.searches_in_flight += 1;
        let shares = Arc::clone(&self.shares);
        let tx = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let files = shares.search(&query, MAX_SEARCH_RESULTS);
            let _ = tx.send(Internal::SearchDone {
                username,
                token,
                query,
                files,
            });
        });
    }

    pub(super) async fn on_search_done(
        &mut self,
        username: String,
        token: u32,
        query: String,
        files: Vec<SearchFile>,
    ) {
        self.searches_in_flight -= 1;
        if files.is_empty() {
            return;
        }
        tracing::debug!(%username, %query, results = files.len(), "answering search");
        self.emit(Event::SearchAnswered {
            username: username.clone(),
            query,
            results: files.len(),
        });
        let response = SearchResponse {
            username: self.own_username.clone(),
            token,
            files,
            slot_free: self.upload_slot_free(),
            avg_speed: self.upload_speed,
            queue_length: self.upload_queue_len() as u32,
            private_files: Vec::new(),
        };
        self.send_peer(username, PeerMsg::FileSearchResponse(response))
            .await;
    }

    /// Returns the message if it is not a browse request. Browse answers
    /// are built on the blocking pool and sent as [`Internal::BrowseReply`].
    pub(super) fn on_browse_message(&mut self, username: &str, msg: PeerMsg) -> Option<PeerMsg> {
        let shares = Arc::clone(&self.shares);
        let build: Box<dyn FnOnce() -> PeerMsg + Send> = match msg {
            PeerMsg::SharedFileListRequest => {
                Box::new(move || PeerMsg::SharedFileListResponse(shares.file_list()))
            }
            PeerMsg::FolderContentsRequest { token, folder } => Box::new(move || {
                let dirs = shares.folder_contents(&folder);
                PeerMsg::FolderContentsResponse(FolderContents {
                    token,
                    folder,
                    dirs,
                })
            }),
            other => return Some(other),
        };
        let username = username.to_owned();
        let tx = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let msg = build();
            let _ = tx.send(Internal::BrowseReply { username, msg });
        });
        None
    }

    /// Sends a browse answer if the requester is still connected.
    pub(super) fn on_browse_reply(&mut self, username: &str, msg: PeerMsg) {
        if let Some(handle) = self.peers.get(username) {
            handle.send(msg);
        }
    }
}
