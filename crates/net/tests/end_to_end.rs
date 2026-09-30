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
use seekr_net::{Client, ClientConfig, ConnectMethod, DownloadState, Event, UploadState};
use seekr_proto::wire::{Reader, WireWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// What the fake server knows about a logged-in user.
struct Session {
    port: u32,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

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
/// GetPeerAddress and relaying ConnectToPeer. Everything is on 127.0.0.1.
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
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(serve(stream, sessions.clone(), opts.clone()));
        }
    });
    addr
}

async fn serve(stream: TcpStream, sessions: Sessions, opts: Arc<Opts>) {
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
            }
            // SetWaitPort
            2 => {
                let port = r.u32().unwrap();
                sessions.lock().unwrap().get_mut(&me).unwrap().port = port;
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
                }
            }
            // FileSearch: hand it to the distributed network.
            26 => {
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
            // SetStatus, ServerPing, SharedFoldersFiles, SendUploadSpeed,
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
    let dir = std::env::temp_dir().join(format!("seekr-e2e-{name}-{}", std::process::id()));
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
