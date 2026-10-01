//! A `D` connection to a (possible) distributed parent. We only read from
//! it: as a child we receive searches and branch information.

use crabseek_proto::FrameCodec;
use crabseek_proto::distrib::DistribMsg;
use futures::StreamExt;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio_util::codec::FramedRead;

use crate::client::Internal;
use crate::peer::ConnId;

/// Closes the connection when dropped.
pub struct DistribHandle {
    pub id: ConnId,
    reader: AbortHandle,
    _write: OwnedWriteHalf,
}

impl Drop for DistribHandle {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

pub fn spawn(
    id: ConnId,
    username: String,
    stream: TcpStream,
    internal: mpsc::UnboundedSender<Internal>,
) -> DistribHandle {
    let (read, write) = stream.into_split();
    let reader = tokio::spawn(async move {
        let mut frames = FramedRead::new(read, FrameCodec);
        let reason = loop {
            match frames.next().await {
                Some(Ok(frame)) => match DistribMsg::decode(frame) {
                    Ok(msg) => {
                        let _ = internal.send(Internal::DistribMessage {
                            id,
                            username: username.clone(),
                            msg,
                        });
                    }
                    Err(e) => tracing::debug!(%username, %e, "undecodable distributed message"),
                },
                Some(Err(e)) => break e.to_string(),
                None => break "closed by peer".to_owned(),
            }
        };
        let _ = internal.send(Internal::DistribClosed {
            id,
            username,
            reason,
        });
    })
    .abort_handle();
    DistribHandle {
        id,
        reader,
        _write: write,
    }
}
