//! Connection to the central Soulseek server.

use std::net::Ipv4Addr;
use std::time::Duration;

use bytes::BytesMut;
use crabseek_proto::server::{LoginResponse, ServerRequest, ServerResponse};
use crabseek_proto::{DecodeError, FrameCodec, FrameError};
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::tcp::OwnedWriteHalf;
use tokio_util::codec::FramedRead;

/// A banned account gets no login response at all, so don't wait forever.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("could not connect to {addr}: {source}")]
    Connect {
        addr: String,
        source: std::io::Error,
    },
    #[error("server rejected login: {reason}{}", detail.as_ref().map(|d| format!(" ({d})")).unwrap_or_default())]
    Rejected {
        reason: String,
        detail: Option<String>,
    },
    #[error("no login response within {LOGIN_TIMEOUT:?} (the account may be banned)")]
    Timeout,
    #[error("server closed the connection during login")]
    Closed,
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("malformed login response: {0}")]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct LoginInfo {
    pub greeting: String,
    pub own_ip: Ipv4Addr,
    pub is_supporter: bool,
}

pub struct ServerConnection {
    reader: ServerReader,
    writer: ServerWriter,
}

pub type ServerReader = FramedRead<OwnedReadHalf, FrameCodec>;

/// Write half of the server connection, usable from another task.
pub struct ServerWriter {
    writer: OwnedWriteHalf,
    buf: BytesMut,
}

impl ServerWriter {
    pub async fn send(&mut self, msg: &ServerRequest) -> std::io::Result<()> {
        self.buf.clear();
        msg.encode(&mut self.buf);
        self.writer.write_all(&self.buf).await
    }
}

impl ServerConnection {
    /// Connects, logs in and waits for the server's verdict. Messages the
    /// server sends before the login response are dropped; there are none
    /// in practice.
    pub async fn login(
        addr: &str,
        username: &str,
        password: &str,
    ) -> Result<(Self, LoginInfo), LoginError> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|source| LoginError::Connect {
                addr: addr.to_owned(),
                source,
            })?;
        stream.set_nodelay(true)?;
        let (read, write) = stream.into_split();
        let mut conn = Self {
            reader: FramedRead::new(read, FrameCodec),
            writer: ServerWriter {
                writer: write,
                buf: BytesMut::new(),
            },
        };

        conn.send(&ServerRequest::login(username, password)).await?;

        let response = tokio::time::timeout(LOGIN_TIMEOUT, async {
            loop {
                match conn.recv().await? {
                    Some(ServerResponse::Login(r)) => return Ok(r),
                    Some(other) => tracing::debug!(?other, "message before login response"),
                    None => return Err(LoginError::Closed),
                }
            }
        })
        .await
        .map_err(|_| LoginError::Timeout)??;

        match response {
            LoginResponse::Success {
                greeting,
                own_ip,
                is_supporter,
                ..
            } => Ok((
                conn,
                LoginInfo {
                    greeting,
                    own_ip,
                    is_supporter: is_supporter.unwrap_or(false),
                },
            )),
            LoginResponse::Failure { reason, detail } => {
                Err(LoginError::Rejected { reason, detail })
            }
        }
    }

    pub async fn send(&mut self, msg: &ServerRequest) -> std::io::Result<()> {
        self.writer.send(msg).await
    }

    /// Next message from the server, or `None` once the connection closes.
    pub async fn recv(&mut self) -> Result<Option<ServerResponse>, LoginError> {
        recv(&mut self.reader).await
    }

    pub fn split(self) -> (ServerReader, ServerWriter) {
        (self.reader, self.writer)
    }
}

pub async fn recv(reader: &mut ServerReader) -> Result<Option<ServerResponse>, LoginError> {
    match reader.next().await {
        Some(frame) => Ok(Some(ServerResponse::decode(frame?)?)),
        None => Ok(None),
    }
}
