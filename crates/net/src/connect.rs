//! Opening peer connections and reading their first (init) message.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use seekr_proto::ConnectionType;
use seekr_proto::peer_init::PeerInitMsg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an accepted socket may take to send its init message.
pub const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Init messages are a username and a couple of integers.
const MAX_INIT_LEN: usize = 4096;

/// Connects to `addr` and introduces ourselves with `PeerInit`.
pub async fn direct(
    addr: SocketAddr,
    own_username: &str,
    conn_type: ConnectionType,
) -> io::Result<TcpStream> {
    let msg = PeerInitMsg::PeerInit {
        username: own_username.to_owned(),
        conn_type,
        token: 0,
    };
    open(addr, &msg).await
}

/// Answers an indirect connection request with `PierceFireWall`.
pub async fn pierce(addr: SocketAddr, token: u32) -> io::Result<TcpStream> {
    open(addr, &PeerInitMsg::PierceFirewall { token }).await
}

async fn open(addr: SocketAddr, init: &PeerInitMsg) -> io::Result<TcpStream> {
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
            accept_init(async |addr| direct(addr, "alice", ConnectionType::File).await).await;
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
        let init = accept_init(async |addr| pierce(addr, 1234).await).await;
        assert_eq!(init, PeerInitMsg::PierceFirewall { token: 1234 });
    }

    #[tokio::test]
    async fn offline_peer_fails_fast() {
        let err = direct("0.0.0.0:0".parse().unwrap(), "a", ConnectionType::Peer)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }
}
