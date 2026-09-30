//! Messages exchanged with the central server (`uint32` message codes).

use std::net::Ipv4Addr;

use bytes::{BufMut, Bytes, BytesMut};
use md5::{Digest, Md5};

use crate::peer_init::ConnectionType;
use crate::wire::{DecodeResult, Reader, WireWrite, write_frame};

pub const DEFAULT_SERVER: &str = "server.slsknet.org:2242";

/// 177 is reserved for "experimental development and testing" until this
/// client gets its own number.
pub const MAJOR_VERSION: u32 = 177;
pub const MINOR_VERSION: u32 = 1;

mod code {
    pub const LOGIN: u32 = 1;
    pub const SET_WAIT_PORT: u32 = 2;
    pub const GET_PEER_ADDRESS: u32 = 3;
    pub const CONNECT_TO_PEER: u32 = 18;
    pub const FILE_SEARCH: u32 = 26;
    pub const CANT_CONNECT_TO_PEER: u32 = 1001;
}

/// Messages we send to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerRequest {
    Login {
        username: String,
        password: String,
        major_version: u32,
        minor_version: u32,
    },
    SetWaitPort {
        port: u32,
    },
    GetPeerAddress {
        username: String,
    },
    /// Indirect connection request: the server asks `username` to connect
    /// to us and greet us with `PierceFireWall(token)`.
    ConnectToPeer {
        token: u32,
        username: String,
        conn_type: ConnectionType,
    },
    /// Tells the server we could not answer a `ConnectToPeer` from `username`.
    CantConnectToPeer {
        token: u32,
        username: String,
    },
    /// Network-wide search; results arrive from peers as
    /// `FileSearchResponse` carrying the same token.
    FileSearch {
        token: u32,
        query: String,
    },
}

impl ServerRequest {
    pub fn login(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::Login {
            username: username.into(),
            password: password.into(),
            major_version: MAJOR_VERSION,
            minor_version: MINOR_VERSION,
        }
    }

    /// Appends the complete frame, length prefix included.
    pub fn encode(&self, dst: &mut BytesMut) {
        write_frame(dst, |b| match self {
            Self::Login {
                username,
                password,
                major_version,
                minor_version,
            } => {
                b.put_u32_le(code::LOGIN);
                b.put_string_wire(username);
                b.put_string_wire(password);
                b.put_u32_le(*major_version);
                b.put_string_wire(&login_hash(username, password));
                b.put_u32_le(*minor_version);
            }
            Self::SetWaitPort { port } => {
                b.put_u32_le(code::SET_WAIT_PORT);
                b.put_u32_le(*port);
            }
            Self::GetPeerAddress { username } => {
                b.put_u32_le(code::GET_PEER_ADDRESS);
                b.put_string_wire(username);
            }
            Self::ConnectToPeer {
                token,
                username,
                conn_type,
            } => {
                b.put_u32_le(code::CONNECT_TO_PEER);
                b.put_u32_le(*token);
                b.put_string_wire(username);
                b.put_string_wire(conn_type.as_str());
            }
            Self::CantConnectToPeer { token, username } => {
                b.put_u32_le(code::CANT_CONNECT_TO_PEER);
                b.put_u32_le(*token);
                b.put_string_wire(username);
            }
            Self::FileSearch { token, query } => {
                b.put_u32_le(code::FILE_SEARCH);
                b.put_u32_le(*token);
                b.put_string_wire(query);
            }
        });
    }
}

/// MD5 hex digest of username + password, as the Login message expects.
pub fn login_hash(username: &str, password: &str) -> String {
    let digest = Md5::new()
        .chain_update(username)
        .chain_update(password)
        .finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Messages the server sends us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerResponse {
    Login(LoginResponse),
    /// IP and port are zero when the user is offline.
    PeerAddress {
        username: String,
        ip: Ipv4Addr,
        port: u32,
    },
    /// A peer wants us to connect to them and send `PierceFireWall(token)`.
    ConnectToPeer {
        username: String,
        conn_type: ConnectionType,
        ip: Ipv4Addr,
        port: u32,
        token: u32,
        privileged: bool,
    },
    /// The peer could not answer our `ConnectToPeer` with this token.
    CantConnectToPeer {
        token: u32,
    },
    /// Someone searches our shares (user and room searches; regular
    /// searches come through the distributed network).
    FileSearch {
        username: String,
        token: u32,
        query: String,
    },
    /// Anything not implemented yet; kept so callers can log it.
    Unknown {
        code: u32,
        payload: Bytes,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginResponse {
    Success {
        greeting: String,
        own_ip: Ipv4Addr,
        /// MD5 of the password; absent from older servers.
        password_hash: Option<String>,
        is_supporter: Option<bool>,
    },
    Failure {
        reason: String,
        /// Only sent with `INVALIDUSERNAME`.
        detail: Option<String>,
    },
}

impl ServerResponse {
    /// Decodes a frame payload as produced by [`crate::FrameCodec`].
    pub fn decode(payload: Bytes) -> DecodeResult<Self> {
        let mut r = Reader::new(&payload);
        let code = r.u32()?;
        Ok(match code {
            code::LOGIN => Self::Login(decode_login(&mut r)?),
            // Obfuscation fields that may follow are ignored: we only
            // support plain connections, like Nicotine+.
            code::GET_PEER_ADDRESS => Self::PeerAddress {
                username: r.string()?,
                ip: r.ipv4()?,
                port: r.u32()?,
            },
            code::CONNECT_TO_PEER => Self::ConnectToPeer {
                username: r.string()?,
                conn_type: ConnectionType::decode(&mut r)?,
                ip: r.ipv4()?,
                port: r.u32()?,
                token: r.u32()?,
                privileged: (!r.is_empty())
                    .then(|| r.bool())
                    .transpose()?
                    .unwrap_or(false),
            },
            code::CANT_CONNECT_TO_PEER => Self::CantConnectToPeer { token: r.u32()? },
            code::FILE_SEARCH => Self::FileSearch {
                username: r.string()?,
                token: r.u32()?,
                query: r.string()?,
            },
            _ => Self::Unknown {
                code,
                payload: payload.slice(4..),
            },
        })
    }
}

fn decode_login(r: &mut Reader) -> DecodeResult<LoginResponse> {
    if r.bool()? {
        Ok(LoginResponse::Success {
            greeting: r.string()?,
            own_ip: r.ipv4()?,
            password_hash: (!r.is_empty()).then(|| r.string()).transpose()?,
            is_supporter: (!r.is_empty()).then(|| r.bool()).transpose()?,
        })
    } else {
        Ok(LoginResponse::Failure {
            reason: r.string()?,
            detail: (!r.is_empty()).then(|| r.string()).transpose()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::WireWrite;

    /// The Login example from the protocol documentation, byte for byte.
    #[test]
    fn login_matches_documented_example() {
        let mut buf = BytesMut::new();
        ServerRequest::Login {
            username: "username".into(),
            password: "password".into(),
            major_version: 177,
            minor_version: 1,
        }
        .encode(&mut buf);

        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x48, 0, 0, 0, 1, 0, 0, 0]);
        expected.extend_from_slice(&[8, 0, 0, 0]);
        expected.extend_from_slice(b"username");
        expected.extend_from_slice(&[8, 0, 0, 0]);
        expected.extend_from_slice(b"password");
        expected.extend_from_slice(&[0xb1, 0, 0, 0, 0x20, 0, 0, 0]);
        expected.extend_from_slice(b"d51c9a7e9353746a6020f9602d452929");
        expected.extend_from_slice(&[1, 0, 0, 0]);

        assert_eq!(&buf[..], &expected[..]);
    }

    #[test]
    fn set_wait_port() {
        let mut buf = BytesMut::new();
        ServerRequest::SetWaitPort { port: 2234 }.encode(&mut buf);
        assert_eq!(&buf[..], &[8, 0, 0, 0, 2, 0, 0, 0, 0xba, 0x08, 0, 0]);
    }

    fn payload(body: impl FnOnce(&mut BytesMut)) -> Bytes {
        let mut b = BytesMut::new();
        body(&mut b);
        b.freeze()
    }

    #[test]
    fn decode_login_success() {
        let p = payload(|b| {
            b.put_u32_le(1);
            b.put_bool_wire(true);
            b.put_string_wire("Welcome");
            b.put_ipv4_wire(Ipv4Addr::new(84, 2, 3, 4));
            b.put_string_wire("5f4dcc3b5aa765d61d8327deb882cf99");
            b.put_bool_wire(false);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::Login(LoginResponse::Success {
                greeting: "Welcome".into(),
                own_ip: Ipv4Addr::new(84, 2, 3, 4),
                password_hash: Some("5f4dcc3b5aa765d61d8327deb882cf99".into()),
                is_supporter: Some(false),
            }))
        );
    }

    #[test]
    fn decode_login_failure() {
        let p = payload(|b| {
            b.put_u32_le(1);
            b.put_bool_wire(false);
            b.put_string_wire("INVALIDPASS");
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::Login(LoginResponse::Failure {
                reason: "INVALIDPASS".into(),
                detail: None,
            }))
        );
    }

    #[test]
    fn connect_to_peer_request() {
        let mut buf = BytesMut::new();
        ServerRequest::ConnectToPeer {
            token: 7,
            username: "bob".into(),
            conn_type: ConnectionType::Peer,
        }
        .encode(&mut buf);
        let mut expected = vec![20, 0, 0, 0, 18, 0, 0, 0, 7, 0, 0, 0, 3, 0, 0, 0];
        expected.extend_from_slice(b"bob");
        expected.extend_from_slice(&[1, 0, 0, 0, b'P']);
        assert_eq!(&buf[..], &expected[..]);
    }

    #[test]
    fn decode_peer_address() {
        let p = payload(|b| {
            b.put_u32_le(3);
            b.put_string_wire("bob");
            b.put_ipv4_wire(Ipv4Addr::new(1, 2, 3, 4));
            b.put_u32_le(2234);
            b.put_u32_le(0);
            b.put_u16_le(0);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::PeerAddress {
                username: "bob".into(),
                ip: Ipv4Addr::new(1, 2, 3, 4),
                port: 2234,
            })
        );
    }

    #[test]
    fn decode_connect_to_peer() {
        let p = payload(|b| {
            b.put_u32_le(18);
            b.put_string_wire("bob");
            b.put_string_wire("F");
            b.put_ipv4_wire(Ipv4Addr::new(1, 2, 3, 4));
            b.put_u32_le(2234);
            b.put_u32_le(99);
            b.put_bool_wire(true);
            b.put_u32_le(0);
            b.put_u32_le(0);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::ConnectToPeer {
                username: "bob".into(),
                conn_type: ConnectionType::File,
                ip: Ipv4Addr::new(1, 2, 3, 4),
                port: 2234,
                token: 99,
                privileged: true,
            })
        );
    }

    #[test]
    fn file_search_request() {
        let mut buf = BytesMut::new();
        ServerRequest::FileSearch {
            token: 5,
            query: "ab".into(),
        }
        .encode(&mut buf);
        assert_eq!(
            &buf[..],
            &[14, 0, 0, 0, 26, 0, 0, 0, 5, 0, 0, 0, 2, 0, 0, 0, b'a', b'b']
        );
    }

    #[test]
    fn decode_incoming_file_search() {
        let p = payload(|b| {
            b.put_u32_le(26);
            b.put_string_wire("bob");
            b.put_u32_le(5);
            b.put_string_wire("ab");
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::FileSearch {
                username: "bob".into(),
                token: 5,
                query: "ab".into(),
            })
        );
    }

    #[test]
    fn unknown_code_is_preserved() {
        let p = payload(|b| {
            b.put_u32_le(9999);
            b.put_u8(42);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::Unknown {
                code: 9999,
                payload: Bytes::from_static(&[42]),
            })
        );
    }
}
