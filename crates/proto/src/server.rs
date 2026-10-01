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
    pub const WATCH_USER: u32 = 5;
    pub const UNWATCH_USER: u32 = 6;
    pub const GET_USER_STATUS: u32 = 7;
    pub const CONNECT_TO_PEER: u32 = 18;
    pub const FILE_SEARCH: u32 = 26;
    pub const SET_STATUS: u32 = 28;
    pub const SERVER_PING: u32 = 32;
    pub const SHARED_FOLDERS_FILES: u32 = 35;
    pub const GET_USER_STATS: u32 = 36;
    pub const HAVE_NO_PARENT: u32 = 71;
    pub const EMBEDDED_MESSAGE: u32 = 93;
    pub const ACCEPT_CHILDREN: u32 = 100;
    pub const POSSIBLE_PARENTS: u32 = 102;
    pub const BRANCH_LEVEL: u32 = 126;
    pub const BRANCH_ROOT: u32 = 127;
    pub const RESET_DISTRIBUTED: u32 = 130;
    pub const SEND_UPLOAD_SPEED: u32 = 121;
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
    /// Keeps us updated about a user: the server answers with
    /// [`ServerResponse::WatchUser`], then sends
    /// [`ServerResponse::UserStatus`] whenever their status changes.
    WatchUser(String),
    UnwatchUser(String),
    /// Asks for a user's current stats; the answer is
    /// [`ServerResponse::UserStats`].
    GetUserStats(String),
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
    /// 1 = away, 2 = online.
    SetStatus {
        status: i32,
    },
    /// Keep-alive, at most once a minute.
    Ping,
    /// How much we share, shown to other users.
    SharedFoldersFiles {
        folders: u32,
        files: u32,
    },
    /// Bytes per second of a finished upload, for our speed statistics.
    SendUploadSpeed {
        speed: u32,
    },
    /// Whether we still look for a distributed parent.
    HaveNoParent(bool),
    /// Whether we take distributed children.
    AcceptChildren(bool),
    /// Our generation in the distributed branch.
    BranchLevel(u32),
    /// The root of our distributed branch.
    BranchRoot(String),
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
            Self::WatchUser(username) => {
                b.put_u32_le(code::WATCH_USER);
                b.put_string_wire(username);
            }
            Self::UnwatchUser(username) => {
                b.put_u32_le(code::UNWATCH_USER);
                b.put_string_wire(username);
            }
            Self::GetUserStats(username) => {
                b.put_u32_le(code::GET_USER_STATS);
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
            Self::SetStatus { status } => {
                b.put_u32_le(code::SET_STATUS);
                b.put_i32_le(*status);
            }
            Self::Ping => b.put_u32_le(code::SERVER_PING),
            Self::SharedFoldersFiles { folders, files } => {
                b.put_u32_le(code::SHARED_FOLDERS_FILES);
                b.put_u32_le(*folders);
                b.put_u32_le(*files);
            }
            Self::SendUploadSpeed { speed } => {
                b.put_u32_le(code::SEND_UPLOAD_SPEED);
                b.put_u32_le(*speed);
            }
            Self::HaveNoParent(no_parent) => {
                b.put_u32_le(code::HAVE_NO_PARENT);
                b.put_bool_wire(*no_parent);
            }
            Self::AcceptChildren(accept) => {
                b.put_u32_le(code::ACCEPT_CHILDREN);
                b.put_bool_wire(*accept);
            }
            Self::BranchLevel(level) => {
                b.put_u32_le(code::BRANCH_LEVEL);
                b.put_u32_le(*level);
            }
            Self::BranchRoot(root) => {
                b.put_u32_le(code::BRANCH_ROOT);
                b.put_string_wire(root);
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
    /// Answer to [`ServerRequest::WatchUser`]; `user` is `None` when no
    /// such account exists.
    WatchUser {
        username: String,
        user: Option<WatchedUser>,
    },
    /// A watched user went online, away or offline.
    UserStatus {
        username: String,
        status: OnlineStatus,
        privileged: bool,
    },
    /// Answer to [`ServerRequest::GetUserStats`].
    UserStats {
        username: String,
        stats: UserStats,
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
    /// Up to 10 users we could adopt as distributed parent.
    PossibleParents(Vec<PossibleParent>),
    /// A distributed message from the server; we are a branch root.
    EmbeddedMessage {
        code: u8,
        payload: Bytes,
    },
    /// Drop our distributed parent and children.
    ResetDistributed,
    /// Anything not implemented yet; kept so callers can log it.
    Unknown {
        code: u32,
        payload: Bytes,
    },
}

/// A user's presence ("User Status Codes" in the spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum OnlineStatus {
    #[default]
    Offline,
    Away,
    Online,
}

impl OnlineStatus {
    fn from_code(code: u32) -> Self {
        match code {
            1 => Self::Away,
            2 => Self::Online,
            _ => Self::Offline,
        }
    }
}

/// What the server knows about a user's sharing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserStats {
    /// Average upload speed in bytes per second.
    pub avg_speed: u32,
    pub upload_num: u32,
    pub files: u32,
    pub dirs: u32,
}

impl UserStats {
    fn decode(r: &mut Reader) -> DecodeResult<Self> {
        let avg_speed = r.u32()?;
        let upload_num = r.u32()?;
        let _unknown = r.u32()?;
        Ok(Self {
            avg_speed,
            upload_num,
            files: r.u32()?,
            dirs: r.u32()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedUser {
    pub status: OnlineStatus,
    pub stats: UserStats,
    /// Uppercase country code; only sent while the user is online or away.
    pub country: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PossibleParent {
    pub username: String,
    pub ip: Ipv4Addr,
    pub port: u32,
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
            code::WATCH_USER => {
                let username = r.string()?;
                let user = if r.bool()? {
                    let status = OnlineStatus::from_code(r.u32()?);
                    let stats = UserStats::decode(&mut r)?;
                    let country = (status != OnlineStatus::Offline && !r.is_empty())
                        .then(|| r.string())
                        .transpose()?;
                    Some(WatchedUser {
                        status,
                        stats,
                        country,
                    })
                } else {
                    None
                };
                Self::WatchUser { username, user }
            }
            code::GET_USER_STATUS => Self::UserStatus {
                username: r.string()?,
                status: OnlineStatus::from_code(r.u32()?),
                privileged: (!r.is_empty())
                    .then(|| r.bool())
                    .transpose()?
                    .unwrap_or(false),
            },
            code::GET_USER_STATS => Self::UserStats {
                username: r.string()?,
                stats: UserStats::decode(&mut r)?,
            },
            code::CANT_CONNECT_TO_PEER => Self::CantConnectToPeer { token: r.u32()? },
            code::POSSIBLE_PARENTS => {
                let count = r.u32()? as usize;
                let mut parents = Vec::with_capacity(count.min(16));
                for _ in 0..count {
                    parents.push(PossibleParent {
                        username: r.string()?,
                        ip: r.ipv4()?,
                        port: r.u32()?,
                    });
                }
                Self::PossibleParents(parents)
            }
            code::EMBEDDED_MESSAGE => Self::EmbeddedMessage {
                code: r.u8()?,
                payload: payload.slice(5..),
            },
            code::RESET_DISTRIBUTED => Self::ResetDistributed,
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
    fn small_requests() {
        let enc = |msg: ServerRequest| {
            let mut buf = BytesMut::new();
            msg.encode(&mut buf);
            buf.to_vec()
        };
        assert_eq!(enc(ServerRequest::Ping), [4, 0, 0, 0, 32, 0, 0, 0]);
        assert_eq!(
            enc(ServerRequest::SetStatus { status: 2 }),
            [8, 0, 0, 0, 28, 0, 0, 0, 2, 0, 0, 0]
        );
        assert_eq!(
            enc(ServerRequest::SharedFoldersFiles {
                folders: 3,
                files: 40
            }),
            [12, 0, 0, 0, 35, 0, 0, 0, 3, 0, 0, 0, 40, 0, 0, 0]
        );
        assert_eq!(
            enc(ServerRequest::SendUploadSpeed { speed: 258 }),
            [8, 0, 0, 0, 121, 0, 0, 0, 2, 1, 0, 0]
        );
    }

    #[test]
    fn distributed_messages() {
        let enc = |msg: ServerRequest| {
            let mut buf = BytesMut::new();
            msg.encode(&mut buf);
            buf.to_vec()
        };
        assert_eq!(
            enc(ServerRequest::HaveNoParent(true)),
            [5, 0, 0, 0, 71, 0, 0, 0, 1]
        );
        assert_eq!(
            enc(ServerRequest::AcceptChildren(false)),
            [5, 0, 0, 0, 100, 0, 0, 0, 0]
        );
        assert_eq!(
            enc(ServerRequest::BranchLevel(2)),
            [8, 0, 0, 0, 126, 0, 0, 0, 2, 0, 0, 0]
        );

        let p = payload(|b| {
            b.put_u32_le(102);
            b.put_u32_le(1);
            b.put_string_wire("dad");
            b.put_ipv4_wire(Ipv4Addr::new(1, 2, 3, 4));
            b.put_u32_le(2234);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::PossibleParents(vec![PossibleParent {
                username: "dad".into(),
                ip: Ipv4Addr::new(1, 2, 3, 4),
                port: 2234,
            }]))
        );

        let p = payload(|b| {
            b.put_u32_le(93);
            b.put_u8(3);
            b.put_slice(&[9, 9]);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::EmbeddedMessage {
                code: 3,
                payload: Bytes::from_static(&[9, 9]),
            })
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
    fn watch_requests() {
        let enc = |msg: ServerRequest| {
            let mut buf = BytesMut::new();
            msg.encode(&mut buf);
            buf.to_vec()
        };
        let frame = |code: u8| {
            let mut v = vec![11, 0, 0, 0, code, 0, 0, 0, 3, 0, 0, 0];
            v.extend_from_slice(b"bob");
            v
        };
        assert_eq!(enc(ServerRequest::WatchUser("bob".into())), frame(5));
        assert_eq!(enc(ServerRequest::UnwatchUser("bob".into())), frame(6));
        assert_eq!(enc(ServerRequest::GetUserStats("bob".into())), frame(36));
    }

    fn stats(b: &mut BytesMut) {
        b.put_u32_le(150_000); // avgspeed
        b.put_u32_le(42); // uploadnum
        b.put_u32_le(0); // unknown
        b.put_u32_le(1200); // files
        b.put_u32_le(80); // dirs
    }

    const STATS: UserStats = UserStats {
        avg_speed: 150_000,
        upload_num: 42,
        files: 1200,
        dirs: 80,
    };

    #[test]
    fn decode_watch_user() {
        let online = payload(|b| {
            b.put_u32_le(5);
            b.put_string_wire("bob");
            b.put_bool_wire(true);
            b.put_u32_le(2);
            stats(b);
            b.put_string_wire("HU");
        });
        assert_eq!(
            ServerResponse::decode(online),
            Ok(ServerResponse::WatchUser {
                username: "bob".into(),
                user: Some(WatchedUser {
                    status: OnlineStatus::Online,
                    stats: STATS,
                    country: Some("HU".into()),
                }),
            })
        );

        // Offline users come without a country code.
        let offline = payload(|b| {
            b.put_u32_le(5);
            b.put_string_wire("bob");
            b.put_bool_wire(true);
            b.put_u32_le(0);
            stats(b);
        });
        assert_eq!(
            ServerResponse::decode(offline),
            Ok(ServerResponse::WatchUser {
                username: "bob".into(),
                user: Some(WatchedUser {
                    status: OnlineStatus::Offline,
                    stats: STATS,
                    country: None,
                }),
            })
        );

        let missing = payload(|b| {
            b.put_u32_le(5);
            b.put_string_wire("nobody");
            b.put_bool_wire(false);
        });
        assert_eq!(
            ServerResponse::decode(missing),
            Ok(ServerResponse::WatchUser {
                username: "nobody".into(),
                user: None,
            })
        );
    }

    #[test]
    fn decode_user_status_and_stats() {
        let p = payload(|b| {
            b.put_u32_le(7);
            b.put_string_wire("bob");
            b.put_u32_le(1);
            b.put_bool_wire(true);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::UserStatus {
                username: "bob".into(),
                status: OnlineStatus::Away,
                privileged: true,
            })
        );

        let p = payload(|b| {
            b.put_u32_le(36);
            b.put_string_wire("bob");
            stats(b);
        });
        assert_eq!(
            ServerResponse::decode(p),
            Ok(ServerResponse::UserStats {
                username: "bob".into(),
                stats: STATS,
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
