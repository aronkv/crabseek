//! End-to-end transfer test: a fake Soulseek server on localhost and two
//! real clients. Client A shares a folder, client B downloads from it, so
//! the whole upload path runs over real sockets: QueueUpload,
//! TransferRequest/Response, the F connection opened by the uploader
//! (direct or through the server's ConnectToPeer relay), FileOffset and
//! the data itself.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{BufMut, BytesMut};
use crabseek_net::{
    Client, ClientConfig, ConnectMethod, DownloadId, DownloadState, Event, UploadState, connect,
};
use crabseek_proto::peer::PeerMsg;
use crabseek_proto::peer_init::PeerInitMsg;
use crabseek_proto::wire::{Reader, WireWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// What the fake server knows about a logged-in user.
struct Session {
    port: u32,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    /// From SharedFoldersFiles.
    dirs: u32,
    files: u32,
}

type Sessions = Arc<Mutex<HashMap<String, Session>>>;
/// Who watches whom: watched username → watchers' connections.
type Watchers = Arc<Mutex<HashMap<String, Vec<mpsc::UnboundedSender<Vec<u8>>>>>>;

/// How the fake server behaves.
#[derive(Default)]
struct Opts {
    /// Users whose listen port the server hides, as if they were behind a
    /// firewall: others then only reach them through ConnectToPeer.
    firewalled: Vec<String>,
    /// Offer this distributed parent (name, port) to users without one.
    parent: Option<(String, u16)>,
    /// Hand every search to this channel (a fake distributed parent).
    searches: Option<mpsc::UnboundedSender<(String, u32, String)>>,
    /// Send searches to everyone else as EmbeddedMessage, as if they were
    /// branch roots.
    embed_searches: bool,
    /// Report every MessageAcked id here.
    acks: Option<mpsc::UnboundedSender<u32>>,
    /// Right after login, ask the client this many times to connect to an
    /// address that never answers, and report each CantConnectToPeer.
    flood: Option<(usize, mpsc::UnboundedSender<u32>)>,
}

fn frame(code: u32, body: impl FnOnce(&mut BytesMut)) -> Vec<u8> {
    let mut b = BytesMut::new();
    b.put_u32_le(0);
    b.put_u32_le(code);
    body(&mut b);
    let len = (b.len() - 4) as u32;
    b[..4].copy_from_slice(&len.to_le_bytes());
    b.to_vec()
}

/// Implements just enough of the server: login, SetWaitPort,
/// GetPeerAddress, relaying ConnectToPeer and watching users. Every
/// watched name counts as an existing account. Everything is on 127.0.0.1.
async fn fake_server(firewalled: &[&str]) -> String {
    fake_server_with(Opts {
        firewalled: firewalled.iter().map(|s| s.to_string()).collect(),
        ..Opts::default()
    })
    .await
}

async fn fake_server_with(opts: Opts) -> String {
    let opts = Arc::new(opts);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let sessions: Sessions = Arc::default();
    let watchers: Watchers = Arc::default();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(serve(
                stream,
                sessions.clone(),
                watchers.clone(),
                opts.clone(),
            ));
        }
    });
    addr
}

fn user_stats(b: &mut BytesMut, session: Option<&Session>) {
    b.put_u32_le(0); // avgspeed
    b.put_u32_le(0); // uploadnum
    b.put_u32_le(0); // unknown
    b.put_u32_le(session.map_or(0, |s| s.files));
    b.put_u32_le(session.map_or(0, |s| s.dirs));
}

async fn serve(stream: TcpStream, sessions: Sessions, watchers: Watchers, opts: Arc<Opts>) {
    let (mut read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if write.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });

    let mut me = String::new();
    loop {
        let Ok(len) = read.read_u32_le().await else {
            sessions.lock().unwrap().remove(&me);
            return;
        };
        let mut payload = vec![0; len as usize];
        read.read_exact(&mut payload).await.unwrap();
        let mut r = Reader::new(&payload);
        match r.u32().unwrap() {
            // Login
            1 => {
                me = r.string().unwrap();
                sessions.lock().unwrap().insert(
                    me.clone(),
                    Session {
                        port: 0,
                        tx: tx.clone(),
                        dirs: 0,
                        files: 0,
                    },
                );
                tx.send(frame(1, |b| {
                    b.put_bool_wire(true);
                    b.put_string_wire("welcome to the fake server");
                    b.put_ipv4_wire(Ipv4Addr::LOCALHOST);
                    b.put_string_wire("hash");
                    b.put_bool_wire(false);
                }))
                .unwrap();
                // GetUserStatus: tell everyone watching that we are online.
                for watcher in watchers.lock().unwrap().get(&me).into_iter().flatten() {
                    let _ = watcher.send(frame(7, |b| {
                        b.put_string_wire(&me);
                        b.put_u32_le(2);
                        b.put_bool_wire(false);
                    }));
                }
            }
            // WatchUser
            5 => {
                let user = r.string().unwrap();
                watchers
                    .lock()
                    .unwrap()
                    .entry(user.clone())
                    .or_default()
                    .push(tx.clone());
                let sessions = sessions.lock().unwrap();
                let session = sessions.get(&user);
                tx.send(frame(5, |b| {
                    b.put_string_wire(&user);
                    b.put_bool_wire(true);
                    b.put_u32_le(if session.is_some() { 2 } else { 0 });
                    user_stats(b, session);
                    if session.is_some() {
                        b.put_string_wire("HU");
                    }
                }))
                .unwrap();
            }
            // UnwatchUser
            6 => {
                let user = r.string().unwrap();
                if let Some(list) = watchers.lock().unwrap().get_mut(&user) {
                    list.retain(|w| !w.same_channel(&tx));
                }
            }
            // GetUserStats
            36 => {
                let user = r.string().unwrap();
                let sessions = sessions.lock().unwrap();
                tx.send(frame(36, |b| {
                    b.put_string_wire(&user);
                    user_stats(b, sessions.get(&user));
                }))
                .unwrap();
            }
            // SharedFoldersFiles
            35 => {
                let (dirs, files) = (r.u32().unwrap(), r.u32().unwrap());
                if let Some(s) = sessions.lock().unwrap().get_mut(&me) {
                    s.dirs = dirs;
                    s.files = files;
                }
            }
            // SetWaitPort
            2 => {
                let port = r.u32().unwrap();
                sessions.lock().unwrap().get_mut(&me).unwrap().port = port;
                if let Some((count, _)) = &opts.flood {
                    for i in 0..*count as u32 {
                        tx.send(frame(18, |b| {
                            b.put_string_wire(&format!("firewalled{i}"));
                            b.put_string_wire("P");
                            // TEST-NET-1: connecting there just hangs.
                            b.put_ipv4_wire(Ipv4Addr::new(192, 0, 2, 1));
                            b.put_u32_le(9);
                            b.put_u32_le(i);
                            b.put_bool_wire(false);
                            b.put_u32_le(0);
                            b.put_u32_le(0);
                        }))
                        .unwrap();
                    }
                }
            }
            // CantConnectToPeer
            1001 => {
                if let Some((_, declined)) = &opts.flood {
                    let _ = declined.send(r.u32().unwrap());
                }
            }
            // GetPeerAddress
            3 => {
                let user = r.string().unwrap();
                let port = if opts.firewalled.contains(&user) {
                    1 // nothing listens there
                } else {
                    sessions.lock().unwrap().get(&user).map_or(0, |s| s.port)
                };
                tx.send(frame(3, |b| {
                    b.put_string_wire(&user);
                    b.put_ipv4_wire(if port == 0 {
                        Ipv4Addr::UNSPECIFIED
                    } else {
                        Ipv4Addr::LOCALHOST
                    });
                    b.put_u32_le(port);
                    b.put_u32_le(0);
                    b.put_u16_le(0);
                }))
                .unwrap();
            }
            // ConnectToPeer: relay to the target with our address.
            18 => {
                let token = r.u32().unwrap();
                let target = r.string().unwrap();
                let conn_type = r.string().unwrap();
                let sessions = sessions.lock().unwrap();
                let my_port = sessions[&me].port;
                if let Some(target) = sessions.get(&target) {
                    target
                        .tx
                        .send(frame(18, |b| {
                            b.put_string_wire(&me);
                            b.put_string_wire(&conn_type);
                            b.put_ipv4_wire(Ipv4Addr::LOCALHOST);
                            b.put_u32_le(my_port);
                            b.put_u32_le(token);
                            b.put_bool_wire(false);
                            b.put_u32_le(0);
                            b.put_u32_le(0);
                        }))
                        .unwrap();
                } else {
                    // Nobody to relay to; say so at once rather than leave
                    // the client waiting for its pending timeout.
                    tx.send(frame(1001, |b| b.put_u32_le(token))).unwrap();
                }
            }
            // FileSearch and WishlistSearch: hand them to the distributed
            // network.
            26 | 103 => {
                let token = r.u32().unwrap();
                let query = r.string().unwrap();
                if let Some(searches) = &opts.searches {
                    let _ = searches.send((me.clone(), token, query.clone()));
                }
                if opts.embed_searches {
                    for (user, session) in sessions.lock().unwrap().iter() {
                        if *user != me {
                            let _ = session.tx.send(frame(93, |b| {
                                b.put_u8(3);
                                b.put_u32_le(49);
                                b.put_string_wire(&me);
                                b.put_u32_le(token);
                                b.put_string_wire(&query);
                            }));
                        }
                    }
                }
            }
            // MessageUser: deliver it with an id the recipient must ack.
            22 => {
                let target = r.string().unwrap();
                let message = r.string().unwrap();
                if let Some(session) = sessions.lock().unwrap().get(&target) {
                    let _ = session.tx.send(frame(22, |b| {
                        b.put_u32_le(4242);
                        b.put_u32_le(1_759_300_000);
                        b.put_string_wire(&me);
                        b.put_string_wire(&message);
                        b.put_bool_wire(true);
                    }));
                }
            }
            // MessageAcked
            23 => {
                if let Some(acks) = &opts.acks {
                    let _ = acks.send(r.u32().unwrap());
                }
            }
            // HaveNoParent(true): offer the fake parent.
            71 => {
                if r.u8().unwrap() == 1
                    && let Some((name, port)) = &opts.parent
                {
                    tx.send(frame(102, |b| {
                        b.put_u32_le(1);
                        b.put_string_wire(name);
                        b.put_ipv4_wire(Ipv4Addr::LOCALHOST);
                        b.put_u32_le(u32::from(*port));
                    }))
                    .unwrap();
                }
            }
            // SetStatus, ServerPing, SendUploadSpeed,
            // CantConnectToPeer, AcceptChildren, BranchLevel, BranchRoot:
            // nothing to answer.
            _ => {}
        }
    }
}

async fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crabseek-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn start(
    server: &str,
    username: &str,
    download_dir: &Path,
    shared_dirs: Vec<PathBuf>,
) -> (Client, mpsc::UnboundedReceiver<Event>) {
    let (client, info, events) = Client::start(ClientConfig {
        server: server.to_owned(),
        username: username.to_owned(),
        password: "pw".to_owned(),
        listen_port: free_port().await,
        download_dir: download_dir.to_owned(),
        shared_dirs,
        share_cache: None,
        upload_slots: 2,
        upnp: false,
    })
    .await
    .unwrap();
    assert_eq!(info.own_ip, Ipv4Addr::LOCALHOST);
    (client, events)
}

/// Waits for the first event `pick` accepts, failing after 20 seconds.
async fn wait_for<T>(
    events: &mut mpsc::UnboundedReceiver<Event>,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let event = events.recv().await.expect("client stopped");
            if let Some(t) = pick(event) {
                return t;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

fn song(len: usize) -> Vec<u8> {
    (0..len as u32).map(|i| (i * 31 + 7) as u8).collect()
}

#[tokio::test]
async fn upload_and_resume_between_two_clients() {
    upload_and_resume("direct", &[]).await;
}

/// Bob hides his port, so Alice's file connection has to go through the
/// server: she sends ConnectToPeer, Bob connects back with PierceFireWall.
#[tokio::test]
async fn upload_to_firewalled_downloader() {
    upload_and_resume("indirect", &["bob"]).await;
}

async fn upload_and_resume(name: &str, firewalled: &[&str]) {
    let root = temp_dir(name);
    let share = root.join("Music");
    let album = share.join("Artist").join("Album");
    std::fs::create_dir_all(&album).unwrap();
    let data = song(3_000_000);
    std::fs::write(album.join("01 - Song.flac"), &data).unwrap();
    let remote = "Music\\Artist\\Album\\01 - Song.flac";

    let server = fake_server(firewalled).await;
    let (_alice, mut alice_events) = start(
        &server,
        "alice",
        &root.join("alice-dl"),
        vec![share.clone()],
    )
    .await;
    let (bob, mut bob_events) = start(&server, "bob", &root.join("bob-dl"), vec![]).await;

    let files = wait_for(&mut alice_events, |e| match e {
        Event::SharesScanned { files, .. } => Some(files),
        _ => None,
    })
    .await;
    assert_eq!(files, 1);

    // Pretend an earlier attempt got a third of the file.
    let target = root.join("bob-dl").join("Album").join("01 - Song.flac");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(
        target.with_file_name("01 - Song.flac.part"),
        &data[..1_000_000],
    )
    .unwrap();

    let id = bob.download("alice", remote).unwrap();
    let path = wait_for(&mut bob_events, |e| match e {
        Event::Download {
            id: got,
            state: DownloadState::Completed { path },
            ..
        } if got == id => Some(path),
        Event::Download {
            state: DownloadState::Failed { reason },
            ..
        } => panic!("download failed: {reason}"),
        _ => None,
    })
    .await;
    assert_eq!(path, target);
    assert_eq!(std::fs::read(&path).unwrap(), data);

    // Alice saw the upload through to the end.
    wait_for(&mut alice_events, |e| match e {
        Event::Upload {
            state: UploadState::Completed,
            username,
            ..
        } => {
            assert_eq!(username, "bob");
            Some(())
        }
        Event::Upload {
            state: UploadState::Failed { reason },
            ..
        } => panic!("upload failed: {reason}"),
        _ => None,
    })
    .await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unshared_file_is_denied() {
    let root = temp_dir("denied");
    let server = fake_server(&[]).await;
    let (_alice, _alice_events) = start(&server, "alice", &root.join("a"), vec![]).await;
    let (bob, mut bob_events) = start(&server, "bob", &root.join("b"), vec![]).await;

    let id = bob.download("alice", "Music\\secret.flac").unwrap();
    let reason = wait_for(&mut bob_events, |e| match e {
        Event::Download {
            id: got,
            state: DownloadState::Failed { reason },
            ..
        } if got == id => Some(reason),
        _ => None,
    })
    .await;
    assert_eq!(reason, "File not shared.");
    std::fs::remove_dir_all(root).unwrap();
}

fn distrib_frame(code: u8, body: impl FnOnce(&mut BytesMut)) -> Vec<u8> {
    let mut b = BytesMut::new();
    b.put_u32_le(0);
    b.put_u8(code);
    body(&mut b);
    let len = (b.len() - 4) as u32;
    b[..4].copy_from_slice(&len.to_le_bytes());
    b.to_vec()
}

/// Shares one file as alice, searches for it as bob, and returns what bob
/// found.
async fn search_alice_from_bob(server: &str, root: &Path) -> Vec<String> {
    let share = root.join("Music");
    std::fs::create_dir_all(share.join("Album")).unwrap();
    std::fs::write(share.join("Album").join("Kaini Industries.flac"), b"flac").unwrap();

    let (_alice, mut alice_events) =
        start(server, "alice", &root.join("a"), vec![share.clone()]).await;
    wait_for(&mut alice_events, |e| match e {
        Event::SharesScanned { .. } => Some(()),
        _ => None,
    })
    .await;
    let (bob, mut bob_events) = start(server, "bob", &root.join("b"), vec![]).await;
    let token = bob.search("kaini").unwrap();
    wait_for(&mut bob_events, |e| match e {
        Event::SearchResult(r) if r.token == token => {
            assert_eq!(r.username, "alice");
            Some(r.files.into_iter().map(|f| f.filename).collect())
        }
        _ => None,
    })
    .await
}

/// Alice adopts a distributed parent offered by the server, and bob's
/// search reaches her through it.
#[tokio::test]
async fn search_through_distributed_parent() {
    let root = temp_dir("distrib");
    let parent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let parent_port = parent.local_addr().unwrap().port();
    let (search_tx, mut search_rx) = mpsc::unbounded_channel();
    let server = fake_server_with(Opts {
        parent: Some(("dad".to_owned(), parent_port)),
        searches: Some(search_tx),
        ..Opts::default()
    })
    .await;

    // The fake parent: accept alice's D connection, tell her our branch,
    // then pass on every search the server hands us.
    tokio::spawn(async move {
        let (mut child, _) = parent.accept().await.unwrap();
        let len = child.read_u32_le().await.unwrap();
        let mut init = vec![0; len as usize];
        child.read_exact(&mut init).await.unwrap();
        assert_eq!(init[0], 1, "expected PeerInit");
        assert!(
            init.ends_with(&[1, 0, 0, 0, b'D', 0, 0, 0, 0]),
            "expected a D connection"
        );
        child
            .write_all(&distrib_frame(4, |b| b.put_i32_le(0)))
            .await
            .unwrap();
        child
            .write_all(&distrib_frame(5, |b| b.put_string_wire("dad")))
            .await
            .unwrap();
        while let Some((user, token, query)) = search_rx.recv().await {
            child
                .write_all(&distrib_frame(3, |b| {
                    b.put_u32_le(49);
                    b.put_string_wire(&user);
                    b.put_u32_le(token);
                    b.put_string_wire(&query);
                }))
                .await
                .unwrap();
        }
    });

    let files = search_alice_from_bob(&server, &root).await;
    assert_eq!(files, ["Music\\Album\\Kaini Industries.flac"]);
    std::fs::remove_dir_all(root).unwrap();
}

/// The server embeds searches directly when alice is a branch root.
#[tokio::test]
async fn search_as_branch_root() {
    let root = temp_dir("branch-root");
    let server = fake_server_with(Opts {
        embed_searches: true,
        ..Opts::default()
    })
    .await;
    let files = search_alice_from_bob(&server, &root).await;
    assert_eq!(files.len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn browse_shares() {
    let root = temp_dir("browse");
    let share = root.join("Music");
    std::fs::create_dir_all(share.join("A").join("B")).unwrap();
    std::fs::write(share.join("A").join("one.flac"), b"1").unwrap();
    std::fs::write(share.join("A").join("B").join("two.mp3"), b"22").unwrap();

    let server = fake_server(&[]).await;
    let (_alice, mut alice_events) = start(&server, "alice", &root.join("a"), vec![share]).await;
    wait_for(&mut alice_events, |e| match e {
        Event::SharesScanned { .. } => Some(()),
        _ => None,
    })
    .await;
    let (bob, mut bob_events) = start(&server, "bob", &root.join("b"), vec![]).await;
    bob.browse("alice").unwrap();
    let list = wait_for(&mut bob_events, |e| match e {
        Event::BrowseResult { username, list } if username == "alice" => Some(list),
        _ => None,
    })
    .await;
    let dirs: Vec<(String, Vec<String>)> = list
        .dirs
        .into_iter()
        .map(|d| (d.path, d.files.into_iter().map(|f| f.filename).collect()))
        .collect();
    assert_eq!(
        dirs,
        [
            ("Music\\A".to_owned(), vec!["one.flac".to_owned()]),
            ("Music\\A\\B".to_owned(), vec!["two.mp3".to_owned()]),
        ]
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// After a listen-port change, peers reach us on the new port.
#[tokio::test]
async fn listen_port_change_keeps_us_reachable() {
    let root = temp_dir("port-change");
    let share = root.join("Music");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(share.join("x.flac"), b"x").unwrap();
    let server = fake_server(&[]).await;
    let (alice, mut alice_events) = start(&server, "alice", &root.join("a"), vec![share]).await;
    wait_for(&mut alice_events, |e| match e {
        Event::SharesScanned { .. } => Some(()),
        _ => None,
    })
    .await;

    let new_port = free_port().await;
    alice.set_listen_port(new_port).unwrap();
    let result = wait_for(&mut alice_events, |e| match e {
        Event::ListenPort { port, result } if port == new_port => Some(result),
        _ => None,
    })
    .await;
    assert_eq!(result, Ok(()));

    let (bob, mut bob_events) = start(&server, "bob", &root.join("b"), vec![]).await;
    bob.browse("alice").unwrap();
    // Direct, not through the indirect fallback: the new listener works.
    let method = wait_for(&mut bob_events, |e| match e {
        Event::PeerConnected { username, method } if username == "alice" => Some(method),
        Event::PeerConnectFailed { reason, .. } => panic!("alice unreachable: {reason}"),
        _ => None,
    })
    .await;
    assert_eq!(method, ConnectMethod::Direct);
    wait_for(&mut bob_events, |e| match e {
        Event::BrowseResult { username, .. } if username == "alice" => Some(()),
        _ => None,
    })
    .await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Bob watches Alice before she logs in, sees her come online, and then
/// asks for her stats once she has scanned her shares.
#[tokio::test]
async fn watch_user_status_and_stats() {
    use crabseek_proto::server::{OnlineStatus, ServerResponse, UserStats};

    let root = temp_dir("watch");
    let share = root.join("Music");
    std::fs::create_dir_all(share.join("Album")).unwrap();
    std::fs::write(share.join("Album").join("01.flac"), song(1000)).unwrap();

    let server = fake_server(&[]).await;
    let (bob, mut bob_events) = start(&server, "bob", &root.join("bob-dl"), vec![]).await;

    bob.watch_user("alice").unwrap();
    let user = wait_for(&mut bob_events, |e| match e {
        Event::ServerMessage(ServerResponse::WatchUser { username, user })
            if username == "alice" =>
        {
            Some(user)
        }
        _ => None,
    })
    .await
    .expect("alice should exist");
    assert_eq!(user.status, OnlineStatus::Offline);
    assert_eq!(user.country, None);

    let (_alice, mut alice_events) =
        start(&server, "alice", &root.join("alice-dl"), vec![share]).await;
    let status = wait_for(&mut bob_events, |e| match e {
        Event::ServerMessage(ServerResponse::UserStatus {
            username, status, ..
        }) if username == "alice" => Some(status),
        _ => None,
    })
    .await;
    assert_eq!(status, OnlineStatus::Online);

    wait_for(&mut alice_events, |e| {
        matches!(e, Event::SharesScanned { .. }).then_some(())
    })
    .await;
    // Alice's SharedFoldersFiles travels on her own connection, so the
    // server may see Bob's request first; ask until it has arrived.
    let mut stats = UserStats::default();
    for _ in 0..50 {
        bob.user_stats("alice").unwrap();
        stats = wait_for(&mut bob_events, |e| match e {
            Event::ServerMessage(ServerResponse::UserStats { username, stats })
                if username == "alice" =>
            {
                Some(stats)
            }
            _ => None,
        })
        .await;
        if stats.files == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!((stats.files, stats.dirs), (1, 1));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn wishlist_search_finds_results() {
    let root = temp_dir("wishlist");
    let share = root.join("Music");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(share.join("Kaini Industries.flac"), b"x").unwrap();
    let server = fake_server_with(Opts {
        embed_searches: true,
        ..Opts::default()
    })
    .await;
    let (_alice, mut alice_events) = start(&server, "alice", &root.join("a"), vec![share]).await;
    wait_for(&mut alice_events, |e| match e {
        Event::SharesScanned { .. } => Some(()),
        _ => None,
    })
    .await;
    let (bob, mut bob_events) = start(&server, "bob", &root.join("b"), vec![]).await;
    let token = bob.wishlist_search("kaini").unwrap();
    let files = wait_for(&mut bob_events, |e| match e {
        Event::SearchResult(r) if r.token == token => Some(r.files.len()),
        _ => None,
    })
    .await;
    assert_eq!(files, 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn private_message_is_delivered_and_acked() {
    let root = temp_dir("pm");
    let (ack_tx, mut ack_rx) = mpsc::unbounded_channel();
    let server = fake_server_with(Opts {
        acks: Some(ack_tx),
        ..Opts::default()
    })
    .await;
    let (alice, _alice_events) = start(&server, "alice", &root.join("a"), vec![]).await;
    let (_bob, mut bob_events) = start(&server, "bob", &root.join("b"), vec![]).await;

    alice
        .message_user("bob", "szia, megvan még a Geogaddi?")
        .unwrap();
    let (from, text, timestamp) = wait_for(&mut bob_events, |e| match e {
        Event::PrivateMessage {
            username,
            message,
            timestamp,
            ..
        } => Some((username, message, timestamp)),
        _ => None,
    })
    .await;
    assert_eq!(from, "alice");
    assert_eq!(text, "szia, megvan még a Geogaddi?");
    assert_eq!(timestamp, 1_759_300_000);
    // Bob acknowledged it, so the server stops re-sending.
    let ack = tokio::time::timeout(Duration::from_secs(5), ack_rx.recv())
        .await
        .unwrap();
    assert_eq!(ack, Some(4242));
    std::fs::remove_dir_all(root).unwrap();
}

/// A burst of indirect requests must not open a socket each: past the cap,
/// the client declines at once instead of piling up hanging connects.
#[tokio::test]
async fn indirect_request_burst_is_capped() {
    const BURST: usize = 300;
    const CAP: usize = 128; // MAX_PIERCES in client.rs
    let root = temp_dir("flood");
    let (declined_tx, mut declined) = mpsc::unbounded_channel();
    let server = fake_server_with(Opts {
        flood: Some((BURST, declined_tx)),
        ..Opts::default()
    })
    .await;
    let (_alice, _events) = start(&server, "alice", &root.join("a"), vec![]).await;

    let mut count = 0;
    let _ = tokio::time::timeout(Duration::from_millis(1500), async {
        while declined.recv().await.is_some() {
            count += 1;
        }
    })
    .await;
    // Where the address fails fast instead of hanging, everything is
    // declined quickly, which is fine too.
    assert!(
        count >= BURST - CAP,
        "only {count} of {BURST} declined quickly; the rest are holding sockets"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Reads one peer message from a raw `P` connection.
async fn read_peer_msg(stream: &mut TcpStream) -> PeerMsg {
    let len = stream.read_u32_le().await.unwrap();
    let mut payload = vec![0; len as usize];
    stream.read_exact(&mut payload).await.unwrap();
    PeerMsg::decode(payload.into()).unwrap()
}

/// Accepts Bob's next `P` connection and answers his PlaceInQueueRequest
/// with `place`. Returns the connection and what else he sent before
/// asking.
async fn answer_place(
    listener: &TcpListener,
    remote: &str,
    place: u32,
) -> (TcpStream, Vec<PeerMsg>) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let init = connect::read_init(&mut stream).await.unwrap();
    assert!(matches!(init, PeerInitMsg::PeerInit { username, .. } if username == "bob"));
    let mut before = Vec::new();
    loop {
        match read_peer_msg(&mut stream).await {
            PeerMsg::PlaceInQueueRequest { filename } => {
                assert_eq!(filename, remote);
                break;
            }
            other => before.push(other),
        }
    }
    let mut out = BytesMut::new();
    PeerMsg::PlaceInQueueResponse {
        filename: remote.to_owned(),
        place,
    }
    .encode(&mut out);
    stream.write_all(&out).await.unwrap();
    (stream, before)
}

async fn queue_place(events: &mut mpsc::UnboundedReceiver<Event>, id: DownloadId) -> u32 {
    wait_for(events, |e| match e {
        Event::Download {
            id: got,
            state: DownloadState::Queued { place: Some(p) },
            ..
        } if got == id => Some(p),
        _ => None,
    })
    .await
}

/// Closes Carol's side, as an uploader does with idle connections, and
/// waits until Bob notices.
async fn hang_up(stream: TcpStream, events: &mut mpsc::UnboundedReceiver<Event>) {
    drop(stream);
    wait_for(events, |e| match e {
        Event::PeerDisconnected { username, .. } if username == "carol" => Some(()),
        _ => None,
    })
    .await;
}

/// Queue places go stale as the uploader's queue moves, so Bob asks again.
/// An uploader that cannot be reached for that must not fail a download
/// it already has in its queue.
#[tokio::test]
async fn queue_place_is_asked_again() {
    let root = temp_dir("place");
    let remote = "Music\\Album\\01 - Song.flac";
    let server = fake_server(&[]).await;

    // Carol is a bare socket that keeps Bob queued.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut carol = TcpStream::connect(&server).await.unwrap();
    carol
        .write_all(&frame(1, |b| b.put_string_wire("carol")))
        .await
        .unwrap();
    carol
        .write_all(&frame(2, |b| b.put_u32_le(port.into())))
        .await
        .unwrap();
    // Our own address comes back only after the server took the port.
    carol
        .write_all(&frame(3, |b| b.put_string_wire("carol")))
        .await
        .unwrap();
    loop {
        let len = carol.read_u32_le().await.unwrap();
        let mut payload = vec![0; len as usize];
        carol.read_exact(&mut payload).await.unwrap();
        if Reader::new(&payload).u32().unwrap() == 3 {
            break;
        }
    }

    let (bob, mut bob_events) = start(&server, "bob", &root.join("b"), vec![]).await;
    let id = bob.download("carol", remote).unwrap();

    let (stream, before) = answer_place(&listener, remote, 5).await;
    assert!(matches!(&before[..], [PeerMsg::QueueUpload { filename }] if filename == remote));
    assert_eq!(queue_place(&mut bob_events, id).await, 5);
    hang_up(stream, &mut bob_events).await;

    bob.ask_queue_place(id).unwrap();
    let (stream, before) = answer_place(&listener, remote, 2).await;
    assert!(before.is_empty(), "queued twice: {before:?}");
    assert_eq!(queue_place(&mut bob_events, id).await, 2);
    hang_up(stream, &mut bob_events).await;

    // Carol logs off.
    drop(listener);
    drop(carol);
    tokio::time::sleep(Duration::from_millis(200)).await;
    bob.ask_queue_place(id).unwrap();
    wait_for(&mut bob_events, |e| match e {
        Event::PeerConnectFailed { username, .. } if username == "carol" => Some(()),
        Event::Download {
            state: DownloadState::Failed { reason },
            ..
        } => panic!("queued download failed: {reason}"),
        _ => None,
    })
    .await;

    // A download whose QueueUpload never got out does fail.
    let unsent = bob
        .download("carol", "Music\\Album\\02 - Other.flac")
        .unwrap();
    let reason = wait_for(&mut bob_events, |e| match e {
        Event::Download {
            id: got,
            state: DownloadState::Failed { reason },
            ..
        } => {
            assert_eq!(got, unsent, "queued download failed: {reason}");
            Some(reason)
        }
        _ => None,
    })
    .await;
    assert!(reason.starts_with("could not reach user"), "{reason}");
    std::fs::remove_dir_all(root).unwrap();
}
