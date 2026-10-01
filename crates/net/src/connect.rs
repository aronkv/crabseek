//! Opening peer connections and reading their first (init) message.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use crabseek_proto::ConnectionType;
use crabseek_proto::peer_init::PeerInitMsg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an accepted socket may take to send its init message.
pub const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Init messages are a username and a couple of integers.
const MAX_INIT_LEN: usize = 4096;

/// Addresses to try for a peer. The server reports the peer's public
/// address; when that is our own public address, the peer runs behind the
/// same router (often on this very machine), and many routers cannot
/// connect a LAN host to its own public address ("NAT hairpin"). Then
/// loopback is tried first. Our own listen port is skipped so we never
/// connect to ourselves.
pub fn candidates(addr: SocketAddr, own_ip: Ipv4Addr, own_port: u16) -> Vec<SocketAddr> {
    let mut addrs = Vec::with_capacity(2);
    if addr.ip() == IpAddr::V4(own_ip) && addr.port() != own_port {
        addrs.push(SocketAddr::from((Ipv4Addr::LOCALHOST, addr.port())));
    }
    addrs.push(addr);
    addrs
}

/// Connects to the first reachable address and introduces ourselves with
/// `PeerInit`.
pub async fn direct(
    addrs: &[SocketAddr],
    own_username: &str,
    conn_type: ConnectionType,
) -> io::Result<TcpStream> {
    let msg = PeerInitMsg::PeerInit {
        username: own_username.to_owned(),
        conn_type,
        token: 0,
    };
    open(addrs, &msg).await
}

/// Answers an indirect connection request with `PierceFireWall`.
pub async fn pierce(addrs: &[SocketAddr], token: u32) -> io::Result<TcpStream> {
    open(addrs, &PeerInitMsg::PierceFirewall { token }).await
}

async fn open(addrs: &[SocketAddr], init: &PeerInitMsg) -> io::Result<TcpStream> {
    let mut last_error = io::Error::new(io::ErrorKind::AddrNotAvailable, "no address");
    for &addr in addrs {
        match open_one(addr, init).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

async fn open_one(addr: SocketAddr, init: &PeerInitMsg) -> io::Result<TcpStream> {
    if addr.ip().is_unspecified() || addr.port() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "peer has no address (offline?)",
        ));
    }
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
    stream.set_nodelay(true)?;
    let mut buf = BytesMut::new();
    init.encode(&mut buf);
    stream.write_all(&buf).await?;
    Ok(stream)
}

/// Reads exactly one init frame, leaving the rest of the stream untouched
/// for whoever handles the connection next.
pub async fn read_init(stream: &mut TcpStream) -> io::Result<PeerInitMsg> {
    let read = async {
        let len = stream.read_u32_le().await? as usize;
        if len > MAX_INIT_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("init message of {len} bytes"),
            ));
        }
        let mut payload = vec![0; len];
        stream.read_exact(&mut payload).await?;
        PeerInitMsg::decode(Bytes::from(payload))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    };
    tokio::time::timeout(INIT_TIMEOUT, read)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no init message"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn accept_init(
        send: impl AsyncFnOnce(SocketAddr) -> io::Result<TcpStream>,
    ) -> PeerInitMsg {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(send(addr), listener.accept());
        let mut client = client.unwrap();
        let (mut server, _) = accepted.unwrap();

        let init = read_init(&mut server).await.unwrap();

        // Bytes after the init frame must stay in the stream.
        client.write_all(b"next").await.unwrap();
        let mut rest = [0; 4];
        server.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"next");
        init
    }

    #[tokio::test]
    async fn direct_sends_peer_init() {
        let init =
            accept_init(async |addr| direct(&[addr], "alice", ConnectionType::File).await).await;
        assert_eq!(
            init,
            PeerInitMsg::PeerInit {
                username: "alice".into(),
                conn_type: ConnectionType::File,
                token: 0,
            }
        );
    }

    #[tokio::test]
    async fn pierce_sends_token() {
        let init = accept_init(async |addr| pierce(&[addr], 1234).await).await;
        assert_eq!(init, PeerInitMsg::PierceFirewall { token: 1234 });
    }

    #[test]
    fn loopback_first_for_our_own_public_ip() {
        let own = Ipv4Addr::new(94, 21, 69, 106);
        let peer = SocketAddr::from((own, 2235));
        assert_eq!(
            candidates(peer, own, 2234),
            [SocketAddr::from((Ipv4Addr::LOCALHOST, 2235)), peer]
        );
        // Our own port would be ourselves; other IPs are left alone.
        assert_eq!(
            candidates(SocketAddr::from((own, 2234)), own, 2234).len(),
            1
        );
        let other = SocketAddr::from((Ipv4Addr::new(1, 2, 3, 4), 2235));
        assert_eq!(candidates(other, own, 2234), [other]);
    }

    #[tokio::test]
    async fn falls_back_to_the_next_address() {
        let dead = SocketAddr::from((Ipv4Addr::LOCALHOST, 1));
        let init = accept_init(async |addr| pierce(&[dead, addr], 9).await).await;
        assert_eq!(init, PeerInitMsg::PierceFirewall { token: 9 });
    }

    #[tokio::test]
    async fn offline_peer_fails_fast() {
        let err = direct(&["0.0.0.0:0".parse().unwrap()], "a", ConnectionType::Peer)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }
}
