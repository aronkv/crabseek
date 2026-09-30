//! The client actor: owns the server connection, the listener and every
//! peer connection. Callers talk to it through [`Client`] and receive
//! [`Event`]s.

mod distrib;
mod downloads;
mod sharing;
mod uploads;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use seekr_proto::ConnectionType;
use seekr_proto::distrib::DistribMsg;
use seekr_proto::peer::{PeerMsg, UserInfo};
use seekr_proto::peer_init::PeerInitMsg;
use seekr_proto::search::SearchResponse;
use seekr_proto::server::{ServerRequest, ServerResponse};
use seekr_proto::shares::SharedFileList;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::connect;
use crate::peer::{self, ConnId, PeerHandle};
use crate::server::{self, LoginError, LoginInfo, ServerConnection, ServerReader, ServerWriter};
use crate::shares::ShareIndex;

use distrib::Distrib;
pub use distrib::DistribStatus;
use downloads::Download;
pub use downloads::{DownloadId, DownloadState};
use uploads::Upload;
pub use uploads::{UploadId, UploadState};

/// Give up on a peer if neither the direct nor the indirect attempt has
/// produced a connection by then.
const PENDING_TIMEOUT: Duration = Duration::from_secs(30);

/// The spec allows at most one `ServerPing` a minute.
const PING_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server: String,
    pub username: String,
    pub password: String,
    pub listen_port: u16,
    pub download_dir: PathBuf,
    pub shared_dirs: Vec<PathBuf>,
    /// Where audio properties of shared files are cached between runs.
    pub share_cache: Option<PathBuf>,
    /// Uploads that may run at the same time.
    pub upload_slots: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("cannot listen on port {port}: {source}")]
    Listen { port: u16, source: io::Error },
    #[error(transparent)]
    Login(#[from] LoginError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, thiserror::Error)]
#[error("client has shut down")]
pub struct ShutDown;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectMethod {
    /// We connected to the peer's listen port.
    Direct,
    /// The peer connected to us after our `ConnectToPeer` request.
    Indirect,
    /// The peer opened the connection on its own initiative.
    Inbound,
}

#[derive(Debug)]
pub enum Event {
    PeerConnected {
        username: String,
        method: ConnectMethod,
    },
    PeerConnectFailed {
        username: String,
        reason: String,
    },
    PeerDisconnected {
        username: String,
        reason: String,
    },
    PeerMessage {
        username: String,
        msg: PeerMsg,
    },
    /// Results for a search started with [`Client::search`].
    SearchResult(SearchResponse),
    /// A user's complete share list, after [`Client::browse`].
    BrowseResult {
        username: String,
        list: SharedFileList,
    },
    /// A download changed state (also sent for progress).
    Download {
        id: DownloadId,
        username: String,
        filename: String,
        state: DownloadState,
    },
    /// An upload changed state (also sent for progress).
    Upload {
        id: UploadId,
        username: String,
        filename: String,
        state: UploadState,
    },
    /// The shared folders are being (re)scanned.
    SharesScanning,
    SharesScanned {
        folders: usize,
        files: usize,
        /// Shared folders that could not be read.
        errors: Vec<String>,
    },
    /// Our place in the distributed search network changed.
    Distrib(DistribStatus),
    /// We answered someone's search with `results` files.
    SearchAnswered {
        username: String,
        query: String,
        results: usize,
    },
    /// Outcome of [`Client::set_listen_port`].
    ListenPort {
        port: u16,
        result: Result<(), String>,
    },
    /// Server messages the client does not handle itself.
    ServerMessage(ServerResponse),
    /// The actor stops after this.
    ServerClosed {
        reason: String,
    },
}

#[derive(Clone)]
pub struct Client {
    tx: mpsc::UnboundedSender<Internal>,
    tokens: Tokens,
    download_ids: Arc<std::sync::atomic::AtomicU64>,
}

impl Client {
    /// Binds the listen port, logs in and starts the actor.
    pub async fn start(
        cfg: ClientConfig,
    ) -> Result<(Self, LoginInfo, mpsc::UnboundedReceiver<Event>), StartError> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, cfg.listen_port))
            .await
            .map_err(|source| StartError::Listen {
                port: cfg.listen_port,
                source,
            })?;

        let (mut conn, info) =
            ServerConnection::login(&cfg.server, &cfg.username, &cfg.password).await?;
        conn.send(&ServerRequest::SetWaitPort {
            port: cfg.listen_port.into(),
        })
        .await?;
        conn.send(&ServerRequest::SetStatus { status: 2 }).await?;
        // Join the distributed network as a child only.
        conn.send(&ServerRequest::HaveNoParent(true)).await?;
        conn.send(&ServerRequest::AcceptChildren(false)).await?;
        let (server_reader, server_writer) = conn.split();

        let (tx, rx) = mpsc::unbounded_channel();
        let tokens = Tokens::new();
        let _ = tx.send(Internal::RescanShares(cfg.shared_dirs.clone()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        tokio::spawn(read_server(server_reader, tx.clone()));
        let accept_task = tokio::spawn(accept_loop(listener, tx.clone())).abort_handle();
        let ping_tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PING_INTERVAL);
            interval.tick().await; // the first tick is immediate
            loop {
                interval.tick().await;
                if ping_tx.send(Internal::Ping).is_err() {
                    return;
                }
            }
        });
        tokio::spawn(
            Actor {
                own_username: cfg.username,
                own_ip: info.own_ip,
                listen_port: cfg.listen_port,
                server: server_writer,
                internal: tx.clone(),
                events: event_tx,
                peers: HashMap::new(),
                outbox: HashMap::new(),
                pending: HashMap::new(),
                awaiting_address: HashMap::new(),
                searches: HashSet::new(),
                tokens: tokens.clone(),
                next_conn_id: 0,
                downloads: HashMap::new(),
                download_dir: cfg.download_dir,
                accept_task,
                shares: Arc::new(ShareIndex::default()),
                shared_dirs: cfg.shared_dirs,
                share_cache: cfg.share_cache,
                uploads: BTreeMap::new(),
                next_upload_id: 0,
                upload_slots: cfg.upload_slots.max(1),
                upload_speed: 0,
                distrib: Distrib::default(),
            }
            .run(rx),
        );

        Ok((
            Self {
                tx,
                tokens,
                download_ids: Default::default(),
            },
            info,
            event_rx,
        ))
    }

    /// A client that is not connected to anything; every call returns
    /// [`ShutDown`]. For tests of code built on top of `Client`.
    pub fn offline() -> Self {
        let (tx, _) = mpsc::unbounded_channel();
        Self {
            tx,
            tokens: Tokens::new(),
            download_ids: Default::default(),
        }
    }

    /// Sends a message to a peer, connecting first if needed. Failures are
    /// reported as [`Event::PeerConnectFailed`].
    pub fn send_peer(&self, username: impl Into<String>, msg: PeerMsg) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::SendPeer {
                username: username.into(),
                msg,
            })
            .map_err(|_| ShutDown)
    }

    /// Starts a network-wide search. Results arrive as
    /// [`Event::SearchResult`] with the returned token until
    /// [`Client::stop_search`] is called.
    pub fn search(&self, query: impl Into<String>) -> Result<u32, ShutDown> {
        let token = self.tokens.next();
        self.tx
            .send(Internal::Search {
                token,
                query: query.into(),
            })
            .map_err(|_| ShutDown)?;
        Ok(token)
    }

    /// Asks `username` for everything they share. The answer arrives as
    /// [`Event::BrowseResult`]; an unreachable user as
    /// [`Event::PeerConnectFailed`].
    pub fn browse(&self, username: impl Into<String>) -> Result<(), ShutDown> {
        self.send_peer(username, PeerMsg::SharedFileListRequest)
    }

    /// Ignores further results for this search.
    pub fn stop_search(&self, token: u32) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::StopSearch { token })
            .map_err(|_| ShutDown)
    }

    /// Queues `filename` (the full remote path from a search result) for
    /// download from `username`. Progress arrives as [`Event::Download`].
    pub fn download(
        &self,
        username: impl Into<String>,
        filename: impl Into<String>,
    ) -> Result<DownloadId, ShutDown> {
        let id = self.download_ids.fetch_add(1, Ordering::Relaxed) + 1;
        self.tx
            .send(Internal::Download {
                id,
                username: username.into(),
                filename: filename.into(),
            })
            .map_err(|_| ShutDown)?;
        Ok(id)
    }

    /// Where downloads that start from now on are saved.
    /// Rescans the shared folders (e.g. after they changed). Progress
    /// arrives as [`Event::SharesScanning`] and [`Event::SharesScanned`].
    pub fn rescan_shares(&self, dirs: Vec<PathBuf>) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::RescanShares(dirs))
            .map_err(|_| ShutDown)
    }

    pub fn cancel_upload(&self, id: UploadId) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::CancelUpload { id })
            .map_err(|_| ShutDown)
    }

    /// Forgets finished uploads.
    pub fn clear_finished_uploads(&self) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::ClearFinishedUploads)
            .map_err(|_| ShutDown)
    }

    /// Moves the listener to another port and tells the server. The result
    /// arrives as [`Event::ListenPort`]; on failure the old port stays.
    pub fn set_listen_port(&self, port: u16) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::SetListenPort(port))
            .map_err(|_| ShutDown)
    }

    pub fn set_download_dir(&self, dir: PathBuf) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::SetDownloadDir(dir))
            .map_err(|_| ShutDown)
    }

    pub fn cancel_download(&self, id: DownloadId) -> Result<(), ShutDown> {
        self.tx
            .send(Internal::CancelDownload { id })
            .map_err(|_| ShutDown)
    }
}

/// Tokens for searches and connection requests. They only need to be
/// unique among our own requests; starting from the clock avoids reusing
/// tokens from a previous run.
#[derive(Clone)]
struct Tokens(Arc<AtomicU32>);

impl Tokens {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        Self(Arc::new(AtomicU32::new(seed)))
    }

    fn next(&self) -> u32 {
        self.0.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }
}

/// Everything the actor reacts to, from callers and from its own tasks.
pub(crate) enum Internal {
    SendPeer {
        username: String,
        msg: PeerMsg,
    },
    Search {
        token: u32,
        query: String,
    },
    StopSearch {
        token: u32,
    },
    Download {
        id: DownloadId,
        username: String,
        filename: String,
    },
    CancelDownload {
        id: DownloadId,
    },
    SetDownloadDir(PathBuf),
    SetListenPort(u16),
    Ping,
    RescanShares(Vec<PathBuf>),
    SharesScanned {
        index: ShareIndex,
        errors: Vec<String>,
    },
    CancelUpload {
        id: UploadId,
    },
    ClearFinishedUploads,
    UploadTimeout {
        id: UploadId,
        token: u32,
    },
    UploadConnectFailed {
        id: UploadId,
        reason: String,
    },
    UploadProgress {
        id: UploadId,
        sent: u64,
    },
    UploadDone {
        id: UploadId,
        result: io::Result<(u64, Duration)>,
    },
    DistribMessage {
        id: ConnId,
        username: String,
        msg: DistribMsg,
    },
    DistribClosed {
        id: ConnId,
        username: String,
        reason: String,
    },
    /// An `F` connection whose `FileTransferInit` has been read.
    FileConnection {
        username: String,
        token: u32,
        stream: TcpStream,
    },
    FileConnectionTimeout {
        id: DownloadId,
        token: u32,
    },
    DownloadProgress {
        id: DownloadId,
        received: u64,
    },
    DownloadDone {
        id: DownloadId,
        result: io::Result<PathBuf>,
    },
    Server(ServerResponse),
    ServerClosed(String),
    Inbound {
        stream: TcpStream,
        init: PeerInitMsg,
    },
    DirectDone {
        token: u32,
        result: io::Result<TcpStream>,
    },
    PierceDone {
        username: String,
        conn_type: ConnectionType,
        token: u32,
        result: io::Result<TcpStream>,
    },
    PendingTimeout {
        token: u32,
    },
    PeerMessage {
        username: String,
        msg: PeerMsg,
    },
    PeerClosed {
        id: ConnId,
        username: String,
        reason: String,
    },
}

/// What an outgoing connection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// The one `P` connection to a user.
    Peer,
    /// The `F` connection of one of our uploads.
    Upload(UploadId),
    /// A `D` connection to a possible distributed parent.
    ParentCandidate,
}

/// An outgoing connection attempt, keyed by the token we sent in
/// `ConnectToPeer`. The direct attempt and the indirect one race; the
/// first to succeed wins.
struct Pending {
    username: String,
    conn_type: ConnectionType,
    purpose: Purpose,
    direct_error: Option<String>,
    indirect_failed: bool,
}

struct Actor {
    own_username: String,
    /// Our public address as the server sees it, and our listen port.
    own_ip: Ipv4Addr,
    listen_port: u16,
    server: ServerWriter,
    internal: mpsc::UnboundedSender<Internal>,
    events: mpsc::UnboundedSender<Event>,
    peers: HashMap<String, PeerHandle>,
    /// Messages waiting for a `P` connection that is being opened.
    outbox: HashMap<String, Vec<PeerMsg>>,
    pending: HashMap<u32, Pending>,
    /// Tokens waiting for a `GetPeerAddress` answer, by username.
    awaiting_address: HashMap<String, Vec<u32>>,
    /// Tokens of searches whose results we still want.
    searches: HashSet<u32>,
    tokens: Tokens,
    next_conn_id: ConnId,
    downloads: HashMap<DownloadId, Download>,
    download_dir: PathBuf,
    accept_task: tokio::task::AbortHandle,
    shares: Arc<ShareIndex>,
    shared_dirs: Vec<PathBuf>,
    share_cache: Option<PathBuf>,
    /// In queue order.
    uploads: BTreeMap<UploadId, Upload>,
    next_upload_id: UploadId,
    upload_slots: usize,
    /// Speed of our last finished upload, reported in search results.
    upload_speed: u32,
    distrib: Distrib,
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Internal>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                Internal::SendPeer { username, msg } => self.send_peer(username, msg).await,
                Internal::Search { token, query } => {
                    self.searches.insert(token);
                    self.send_server(ServerRequest::FileSearch { token, query })
                        .await;
                }
                Internal::StopSearch { token } => {
                    self.searches.remove(&token);
                }
                Internal::Download {
                    id,
                    username,
                    filename,
                } => self.start_download(id, username, filename).await,
                Internal::CancelDownload { id } => self.cancel_download(id),
                Internal::SetDownloadDir(dir) => self.download_dir = dir,
                Internal::SetListenPort(port) => self.set_listen_port(port).await,
                Internal::Ping => self.send_server(ServerRequest::Ping).await,
                Internal::RescanShares(dirs) => self.rescan_shares(dirs),
                Internal::SharesScanned { index, errors } => {
                    self.on_shares_scanned(index, errors).await
                }
                Internal::CancelUpload { id } => self.cancel_upload(id).await,
                Internal::ClearFinishedUploads => self.clear_finished_uploads(),
                Internal::UploadTimeout { id, token } => self.on_upload_timeout(id, token).await,
                Internal::UploadConnectFailed { id, reason } => {
                    self.fail_upload_connection(id, reason).await
                }
                Internal::UploadProgress { id, sent } => self.on_upload_progress(id, sent),
                Internal::UploadDone { id, result } => self.on_upload_done(id, result).await,
                Internal::DistribMessage { id, username, msg } => {
                    self.on_distrib_message(id, username, msg).await
                }
                Internal::DistribClosed {
                    id,
                    username,
                    reason,
                } => self.on_distrib_closed(id, username, reason).await,
                Internal::FileConnection {
                    username,
                    token,
                    stream,
                } => self.on_file_transfer_init(username, token, stream),
                Internal::FileConnectionTimeout { id, token } => {
                    self.on_file_connection_timeout(id, token)
                }
                Internal::DownloadProgress { id, received } => {
                    self.on_download_progress(id, received)
                }
                Internal::DownloadDone { id, result } => self.on_download_done(id, result),
                Internal::Server(resp) => self.on_server(resp).await,
                Internal::ServerClosed(reason) => {
                    self.emit(Event::ServerClosed { reason });
                    return;
                }
                Internal::Inbound { stream, init } => self.on_inbound(stream, init),
                Internal::DirectDone { token, result } => match result {
                    Ok(stream) => {
                        if let Some(p) = self.pending.remove(&token) {
                            self.on_connected(p, stream, ConnectMethod::Direct);
                        }
                    }
                    Err(e) => {
                        if let Some(p) = self.pending.get_mut(&token) {
                            p.direct_error = Some(e.to_string());
                            self.fail_if_exhausted(token);
                        }
                    }
                },
                Internal::PierceDone {
                    username,
                    conn_type,
                    token,
                    result,
                } => match result {
                    Ok(stream) => self.on_remote_connection(username, conn_type, stream),
                    Err(e) => {
                        tracing::debug!(%username, %e, "could not answer indirect request");
                        self.send_server(ServerRequest::CantConnectToPeer { token, username })
                            .await;
                    }
                },
                Internal::PendingTimeout { token } => {
                    if self.pending.contains_key(&token) {
                        self.fail(token, "timed out".to_owned());
                    }
                }
                Internal::PeerMessage { username, msg } => {
                    self.on_peer_message(username, msg).await
                }
                Internal::PeerClosed {
                    id,
                    username,
                    reason,
                } => {
                    if self.peers.get(&username).is_some_and(|h| h.id == id) {
                        self.peers.remove(&username);
                        self.emit(Event::PeerDisconnected { username, reason });
                    }
                }
            }
        }
    }

    async fn set_listen_port(&mut self, port: u16) {
        let result = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await {
            Ok(listener) => {
                self.accept_task.abort();
                self.listen_port = port;
                self.accept_task =
                    tokio::spawn(accept_loop(listener, self.internal.clone())).abort_handle();
                self.send_server(ServerRequest::SetWaitPort { port: port.into() })
                    .await;
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        };
        self.emit(Event::ListenPort { port, result });
    }

    fn emit(&self, event: Event) {
        // The caller dropping its receiver just means nobody is listening.
        let _ = self.events.send(event);
    }

    async fn send_server(&mut self, msg: ServerRequest) {
        // A write failure also closes the reader, which ends the actor.
        if let Err(e) = self.server.send(&msg).await {
            tracing::warn!(%e, "server write failed");
        }
    }

    async fn send_peer(&mut self, username: String, msg: PeerMsg) {
        if let Some(handle) = self.peers.get(&username) {
            if handle.send(msg.clone()) {
                return;
            }
            self.peers.remove(&username);
        }
        if let Some(queue) = self.outbox.get_mut(&username) {
            queue.push(msg);
            return;
        }
        self.outbox.insert(username.clone(), vec![msg]);
        self.start_connect(username, ConnectionType::Peer, Purpose::Peer)
            .await;
    }

    /// Starts the direct and the indirect attempt at the same time, which
    /// is what current clients do ("modern" order in the spec).
    async fn start_connect(
        &mut self,
        username: String,
        conn_type: ConnectionType,
        purpose: Purpose,
    ) {
        let token = self
            .begin_connect(username.clone(), conn_type, purpose)
            .await;
        self.awaiting_address
            .entry(username.clone())
            .or_default()
            .push(token);
        self.send_server(ServerRequest::GetPeerAddress { username })
            .await;
    }

    /// Like [`Self::start_connect`] when the address is already known
    /// (from `PossibleParents`).
    async fn start_connect_known(
        &mut self,
        username: String,
        ip: Ipv4Addr,
        port: u32,
        conn_type: ConnectionType,
        purpose: Purpose,
    ) {
        let token = self.begin_connect(username, conn_type, purpose).await;
        let addrs = self.peer_addrs(ip, port);
        let (own, tx) = (self.own_username.clone(), self.internal.clone());
        tokio::spawn(async move {
            let result = connect::direct(&addrs, &own, conn_type).await;
            let _ = tx.send(Internal::DirectDone { token, result });
        });
    }

    /// Registers the attempt and sends the indirect request; returns the
    /// token.
    async fn begin_connect(
        &mut self,
        username: String,
        conn_type: ConnectionType,
        purpose: Purpose,
    ) -> u32 {
        let token = self.tokens.next();
        self.pending.insert(
            token,
            Pending {
                username: username.clone(),
                conn_type,
                purpose,
                direct_error: None,
                indirect_failed: false,
            },
        );
        self.send_server(ServerRequest::ConnectToPeer {
            token,
            username,
            conn_type,
        })
        .await;
        let tx = self.internal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(PENDING_TIMEOUT).await;
            let _ = tx.send(Internal::PendingTimeout { token });
        });
        token
    }

    fn peer_addrs(&self, ip: Ipv4Addr, port: u32) -> Vec<SocketAddr> {
        connect::candidates(
            SocketAddr::from((ip, port as u16)),
            self.own_ip,
            self.listen_port,
        )
    }

    async fn on_server(&mut self, resp: ServerResponse) {
        match resp {
            ServerResponse::PeerAddress { username, ip, port } => {
                let addrs = self.peer_addrs(ip, port);
                for token in self.awaiting_address.remove(&username).unwrap_or_default() {
                    let Some(p) = self.pending.get(&token) else {
                        continue;
                    };
                    let (own, conn_type, tx) = (
                        self.own_username.clone(),
                        p.conn_type,
                        self.internal.clone(),
                    );
                    let addrs = addrs.clone();
                    tokio::spawn(async move {
                        let result = connect::direct(&addrs, &own, conn_type).await;
                        let _ = tx.send(Internal::DirectDone { token, result });
                    });
                }
            }
            ServerResponse::ConnectToPeer {
                username,
                conn_type,
                ip,
                port,
                token,
                ..
            } => {
                let addrs = self.peer_addrs(ip, port);
                let tx = self.internal.clone();
                tokio::spawn(async move {
                    let result = connect::pierce(&addrs, token).await;
                    let _ = tx.send(Internal::PierceDone {
                        username,
                        conn_type,
                        token,
                        result,
                    });
                });
            }
            ServerResponse::CantConnectToPeer { token } => {
                if let Some(p) = self.pending.get_mut(&token) {
                    p.indirect_failed = true;
                    self.fail_if_exhausted(token);
                }
            }
            ServerResponse::FileSearch {
                username,
                token,
                query,
            } => self.on_search_request(username, token, query).await,
            ServerResponse::PossibleParents(parents) => self.on_possible_parents(parents).await,
            ServerResponse::EmbeddedMessage { code, payload } => {
                self.on_embedded_message(code, payload).await
            }
            ServerResponse::ResetDistributed => self.reset_distributed().await,
            other => self.emit(Event::ServerMessage(other)),
        }
    }

    fn on_inbound(&mut self, stream: TcpStream, init: PeerInitMsg) {
        match init {
            PeerInitMsg::PeerInit {
                username,
                conn_type,
                ..
            } => self.on_remote_connection(username, conn_type, stream),
            PeerInitMsg::PierceFirewall { token } => match self.pending.remove(&token) {
                Some(p) => self.on_connected(p, stream, ConnectMethod::Indirect),
                None => tracing::debug!(token, "PierceFireWall for unknown token"),
            },
        }
    }

    /// A connection the peer asked for, either by connecting to us or by
    /// having us pierce their firewall.
    fn on_remote_connection(
        &mut self,
        username: String,
        conn_type: ConnectionType,
        stream: TcpStream,
    ) {
        match conn_type {
            ConnectionType::Peer => self.register_peer(username, stream, ConnectMethod::Inbound),
            ConnectionType::File => self.on_file_connection(username, stream),
            // We do not accept distributed children yet.
            ConnectionType::Distributed => {
                tracing::debug!(%username, "ignoring distributed child connection")
            }
        }
    }

    fn on_connected(&mut self, p: Pending, stream: TcpStream, method: ConnectMethod) {
        match p.purpose {
            Purpose::Peer => self.register_peer(p.username, stream, method),
            Purpose::Upload(id) => self.on_upload_connection(id, stream),
            Purpose::ParentCandidate => self.on_candidate_connected(p.username, stream),
        }
    }

    fn register_peer(&mut self, username: String, stream: TcpStream, method: ConnectMethod) {
        self.next_conn_id += 1;
        let handle = peer::spawn(
            self.next_conn_id,
            username.clone(),
            stream,
            self.internal.clone(),
        );
        for msg in self.outbox.remove(&username).unwrap_or_default() {
            handle.send(msg);
        }
        // Other attempts to reach this user are no longer needed.
        self.pending
            .retain(|_, p| !(p.username == username && p.purpose == Purpose::Peer));
        // Only one P connection per user; the old one closes when dropped.
        self.peers.insert(username.clone(), handle);
        self.emit(Event::PeerConnected { username, method });
    }

    async fn on_peer_message(&mut self, username: String, msg: PeerMsg) {
        match msg {
            PeerMsg::FileSearchResponse(resp) => {
                if self.searches.contains(&resp.token) {
                    self.emit(Event::SearchResult(resp));
                } else {
                    tracing::debug!(%username, token = resp.token, "result for inactive search");
                }
            }
            PeerMsg::SharedFileListResponse(list) => {
                self.emit(Event::BrowseResult { username, list });
            }
            PeerMsg::UserInfoRequest => {
                if let Some(handle) = self.peers.get(&username) {
                    handle.send(PeerMsg::UserInfoResponse(self.own_user_info()));
                }
                self.emit(Event::PeerMessage { username, msg });
            }
            msg => {
                let Some(msg) = self.on_browse_message(&username, msg) else {
                    return;
                };
                let Some(msg) = self.on_upload_message(&username, msg).await else {
                    return;
                };
                if let Some(msg) = self.on_transfer_message(&username, msg) {
                    self.emit(Event::PeerMessage { username, msg });
                }
            }
        }
    }

    fn fail_if_exhausted(&mut self, token: u32) {
        if let Some(p) = self.pending.get(&token)
            && p.indirect_failed
            && let Some(direct) = &p.direct_error
        {
            let reason = format!("direct: {direct}; indirect: peer could not connect to us");
            self.fail(token, reason);
        }
    }

    fn fail(&mut self, token: u32, reason: String) {
        let Some(p) = self.pending.remove(&token) else {
            return;
        };
        match p.purpose {
            Purpose::Peer => {
                self.outbox.remove(&p.username);
                self.fail_queued_downloads(&p.username, &reason);
            }
            Purpose::Upload(id) => {
                let _ = self.internal.send(Internal::UploadConnectFailed {
                    id,
                    reason: reason.clone(),
                });
            }
            Purpose::ParentCandidate => {
                tracing::debug!(username = %p.username, %reason, "possible parent unreachable");
                return;
            }
        }
        self.emit(Event::PeerConnectFailed {
            username: p.username,
            reason,
        });
    }
}

impl Actor {
    fn own_user_info(&self) -> UserInfo {
        UserInfo {
            description: "seekr – Soulseek client in Rust".to_owned(),
            picture: None,
            total_uploads: self.uploads.values().filter(|u| u.is_completed()).count() as u32,
            queue_size: self.upload_queue_len() as u32,
            slots_free: self.upload_slot_free(),
            // Nobody may push files to us unasked.
            upload_permitted: Some(0),
        }
    }
}

async fn read_server(mut reader: ServerReader, tx: mpsc::UnboundedSender<Internal>) {
    let reason = loop {
        match server::recv(&mut reader).await {
            Ok(Some(resp)) => {
                if tx.send(Internal::Server(resp)).is_err() {
                    return;
                }
            }
            Ok(None) => break "connection closed".to_owned(),
            Err(e) => break e.to_string(),
        }
    };
    let _ = tx.send(Internal::ServerClosed(reason));
}

async fn accept_loop(listener: TcpListener, tx: mpsc::UnboundedSender<Internal>) {
    loop {
        let (mut stream, addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!(%e, "accept failed");
                continue;
            }
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);
            match connect::read_init(&mut stream).await {
                Ok(init) => {
                    let _ = tx.send(Internal::Inbound { stream, init });
                }
                Err(e) => tracing::debug!(%addr, %e, "bad inbound connection"),
            }
        });
    }
}
