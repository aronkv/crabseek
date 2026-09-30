//! The client actor: owns the server connection, the listener and every
//! peer connection. Callers talk to it through [`Client`] and receive
//! [`Event`]s.

mod downloads;

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use seekr_proto::ConnectionType;
use seekr_proto::peer::{PeerMsg, UserInfo};
use seekr_proto::peer_init::PeerInitMsg;
use seekr_proto::search::SearchResponse;
use seekr_proto::server::{ServerRequest, ServerResponse};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::connect;
use crate::peer::{self, ConnId, PeerHandle};
use crate::server::{self, LoginError, LoginInfo, ServerConnection, ServerReader, ServerWriter};

use downloads::Download;
pub use downloads::{DownloadId, DownloadState};

/// Give up on a peer if neither the direct nor the indirect attempt has
/// produced a connection by then.
const PENDING_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server: String,
    pub username: String,
    pub password: String,
    pub listen_port: u16,
    pub download_dir: PathBuf,
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
    /// A download changed state (also sent for progress).
    Download {
        id: DownloadId,
        username: String,
        filename: String,
        state: DownloadState,
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
        let (server_reader, server_writer) = conn.split();

        let (tx, rx) = mpsc::unbounded_channel();
        let tokens = Tokens::new();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        tokio::spawn(read_server(server_reader, tx.clone()));
        tokio::spawn(accept_loop(listener, tx.clone()));
        tokio::spawn(
            Actor {
                own_username: cfg.username,
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

/// An outgoing connection attempt, keyed by the token we sent in
/// `ConnectToPeer`. The direct attempt and the indirect one race; the
/// first to succeed wins.
struct Pending {
    username: String,
    conn_type: ConnectionType,
    direct_error: Option<String>,
    indirect_failed: bool,
}

struct Actor {
    own_username: String,
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
                Internal::PeerMessage { username, msg } => self.on_peer_message(username, msg),
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
        self.start_connect(username, ConnectionType::Peer).await;
    }

    /// Starts the direct and the indirect attempt at the same time, which
    /// is what current clients do ("modern" order in the spec).
    async fn start_connect(&mut self, username: String, conn_type: ConnectionType) {
        let token = self.tokens.next();
        self.pending.insert(
            token,
            Pending {
                username: username.clone(),
                conn_type,
                direct_error: None,
                indirect_failed: false,
            },
        );
        self.awaiting_address
            .entry(username.clone())
            .or_default()
            .push(token);
        self.send_server(ServerRequest::ConnectToPeer {
            token,
            username: username.clone(),
            conn_type,
        })
        .await;
        self.send_server(ServerRequest::GetPeerAddress { username })
            .await;

        let tx = self.internal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(PENDING_TIMEOUT).await;
            let _ = tx.send(Internal::PendingTimeout { token });
        });
    }

    async fn on_server(&mut self, resp: ServerResponse) {
        match resp {
            ServerResponse::PeerAddress { username, ip, port } => {
                let addr = SocketAddr::from((ip, port as u16));
                for token in self.awaiting_address.remove(&username).unwrap_or_default() {
                    let Some(p) = self.pending.get(&token) else {
                        continue;
                    };
                    let (own, conn_type, tx) = (
                        self.own_username.clone(),
                        p.conn_type,
                        self.internal.clone(),
                    );
                    tokio::spawn(async move {
                        let result = connect::direct(addr, &own, conn_type).await;
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
                let addr = SocketAddr::from((ip, port as u16));
                let tx = self.internal.clone();
                tokio::spawn(async move {
                    let result = connect::pierce(addr, token).await;
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
            ConnectionType::Distributed => {
                tracing::debug!(%username, "ignoring distributed connection (not implemented)")
            }
        }
    }

    fn on_connected(&mut self, p: Pending, stream: TcpStream, method: ConnectMethod) {
        match p.conn_type {
            ConnectionType::Peer => self.register_peer(p.username, stream, method),
            other => {
                tracing::info!(username = %p.username, ?other, "dropping connection (not implemented)")
            }
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
            .retain(|_, p| !(p.username == username && p.conn_type == ConnectionType::Peer));
        // Only one P connection per user; the old one closes when dropped.
        self.peers.insert(username.clone(), handle);
        self.emit(Event::PeerConnected { username, method });
    }

    fn on_peer_message(&mut self, username: String, msg: PeerMsg) {
        match msg {
            PeerMsg::FileSearchResponse(resp) => {
                if self.searches.contains(&resp.token) {
                    self.emit(Event::SearchResult(resp));
                } else {
                    tracing::debug!(%username, token = resp.token, "result for inactive search");
                }
            }
            PeerMsg::UserInfoRequest => {
                if let Some(handle) = self.peers.get(&username) {
                    handle.send(PeerMsg::UserInfoResponse(own_user_info()));
                }
                self.emit(Event::PeerMessage { username, msg });
            }
            msg => {
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
        if p.conn_type == ConnectionType::Peer {
            self.outbox.remove(&p.username);
            self.fail_queued_downloads(&p.username, &reason);
        }
        self.emit(Event::PeerConnectFailed {
            username: p.username,
            reason,
        });
    }
}

fn own_user_info() -> UserInfo {
    UserInfo {
        description: "seekr – Soulseek client in Rust".to_owned(),
        picture: None,
        total_uploads: 0,
        queue_size: 0,
        slots_free: false,
        upload_permitted: Some(0),
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
