//! An established `P` connection: one reader task, one writer task.

use bytes::BytesMut;
use crabseek_proto::FrameCodec;
use crabseek_proto::peer::PeerMsg;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::FramedRead;

use crate::client::Internal;

/// Identifies one connection, so a stale connection closing does not
/// remove a newer one to the same user.
pub type ConnId = u64;

pub struct PeerHandle {
    pub id: ConnId,
    tx: mpsc::UnboundedSender<PeerMsg>,
}

impl PeerHandle {
    /// Returns false if the connection is already gone.
    pub fn send(&self, msg: PeerMsg) -> bool {
        self.tx.send(msg).is_ok()
    }
}

pub fn spawn(
    id: ConnId,
    username: String,
    stream: TcpStream,
    internal: mpsc::UnboundedSender<Internal>,
) -> PeerHandle {
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<PeerMsg>();

    tokio::spawn(async move {
        let mut buf = BytesMut::new();
        while let Some(msg) = rx.recv().await {
            buf.clear();
            msg.encode(&mut buf);
            if let Err(e) = write.write_all(&buf).await {
                tracing::debug!(%e, "peer write failed");
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut frames = FramedRead::new(read, FrameCodec);
        let reason = loop {
            match frames.next().await {
                Some(Ok(frame)) => match PeerMsg::decode(frame) {
                    Ok(msg) => {
                        let _ = internal.send(Internal::PeerMessage {
                            username: username.clone(),
                            msg,
                        });
                    }
                    // One malformed message is not worth dropping the peer.
                    Err(e) => tracing::warn!(%username, %e, "undecodable peer message"),
                },
                Some(Err(e)) => break e.to_string(),
                None => break "closed by peer".to_owned(),
            }
        };
        let _ = internal.send(Internal::PeerClosed {
            id,
            username,
            reason,
        });
    });

    PeerHandle { id, tx }
}
