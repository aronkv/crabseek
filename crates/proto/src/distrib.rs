//! Messages on `D` connections, the distributed search network (`uint8`
//! message codes).

use bytes::{BufMut, Bytes, BytesMut};

use crate::wire::{DecodeError, DecodeResult, Reader, WireWrite, write_frame};

mod code {
    pub const PING: u8 = 0;
    pub const SEARCH: u8 = 3;
    pub const BRANCH_LEVEL: u8 = 4;
    pub const BRANCH_ROOT: u8 = 5;
    pub const CHILD_DEPTH: u8 = 7;
    pub const EMBEDDED_MESSAGE: u8 = 93;
}

/// The identifier every `DistribSearch` carries: ASCII `1`.
const SEARCH_IDENTIFIER: u32 = 49;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistribMsg {
    Ping,
    Search {
        username: String,
        token: u32,
        query: String,
    },
    BranchLevel(i32),
    BranchRoot(String),
    ChildDepth(u32),
    /// A distributed message wrapped by older SoulseekQt versions.
    Embedded {
        code: u8,
        payload: Bytes,
    },
    Unknown {
        code: u8,
        payload: Bytes,
    },
}

impl DistribMsg {
    pub fn encode(&self, dst: &mut BytesMut) {
        write_frame(dst, |b| match self {
            Self::Ping => b.put_u8(code::PING),
            Self::Search {
                username,
                token,
                query,
            } => {
                b.put_u8(code::SEARCH);
                b.put_u32_le(SEARCH_IDENTIFIER);
                b.put_string_wire(username);
                b.put_u32_le(*token);
                b.put_string_wire(query);
            }
            Self::BranchLevel(level) => {
                b.put_u8(code::BRANCH_LEVEL);
                b.put_i32_le(*level);
            }
            Self::BranchRoot(root) => {
                b.put_u8(code::BRANCH_ROOT);
                b.put_string_wire(root);
            }
            Self::ChildDepth(depth) => {
                b.put_u8(code::CHILD_DEPTH);
                b.put_u32_le(*depth);
            }
            Self::Embedded { code, payload } => {
                b.put_u8(code::EMBEDDED_MESSAGE);
                b.put_u8(*code);
                b.put_slice(payload);
            }
            Self::Unknown { code, payload } => {
                b.put_u8(*code);
                b.put_slice(payload);
            }
        });
    }

    /// Decodes a frame payload (code byte first).
    pub fn decode(payload: Bytes) -> DecodeResult<Self> {
        let mut r = Reader::new(&payload);
        let code = r.u8()?;
        Self::decode_body(code, payload.slice(1..))
    }

    /// Decodes a message given its code and the bytes after the code, as
    /// found inside an embedded message.
    pub fn decode_body(code: u8, body: Bytes) -> DecodeResult<Self> {
        let mut r = Reader::new(&body);
        Ok(match code {
            code::PING => Self::Ping,
            code::SEARCH => {
                let identifier = r.u32()?;
                if identifier != SEARCH_IDENTIFIER {
                    return Err(DecodeError::UnknownCode(identifier));
                }
                Self::Search {
                    username: r.string()?,
                    token: r.u32()?,
                    query: r.string()?,
                }
            }
            code::BRANCH_LEVEL => Self::BranchLevel(r.i32()?),
            code::BRANCH_ROOT => Self::BranchRoot(r.string()?),
            code::CHILD_DEPTH => Self::ChildDepth(r.u32()?),
            code::EMBEDDED_MESSAGE => Self::Embedded {
                code: r.u8()?,
                payload: body.slice(1..),
            },
            _ => Self::Unknown {
                code,
                payload: body,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: DistribMsg) {
        let mut buf = BytesMut::new();
        msg.encode(&mut buf);
        assert_eq!(DistribMsg::decode(buf.freeze().slice(4..)), Ok(msg));
    }

    #[test]
    fn search_bytes() {
        let mut buf = BytesMut::new();
        DistribMsg::Search {
            username: "al".into(),
            token: 7,
            query: "x".into(),
        }
        .encode(&mut buf);
        assert_eq!(
            &buf[..],
            &[
                20, 0, 0, 0, 3, 49, 0, 0, 0, 2, 0, 0, 0, b'a', b'l', 7, 0, 0, 0, 1, 0, 0, 0, b'x'
            ]
        );
    }

    #[test]
    fn roundtrips() {
        roundtrip(DistribMsg::Ping);
        roundtrip(DistribMsg::Search {
            username: "bob".into(),
            token: 1,
            query: "boards of canada".into(),
        });
        roundtrip(DistribMsg::BranchLevel(3));
        roundtrip(DistribMsg::BranchRoot("root".into()));
        roundtrip(DistribMsg::ChildDepth(0));
        roundtrip(DistribMsg::Unknown {
            code: 42,
            payload: Bytes::from_static(&[1, 2]),
        });
    }

    #[test]
    fn embedded_search_unpacks() {
        let mut inner = BytesMut::new();
        DistribMsg::Search {
            username: "bob".into(),
            token: 9,
            query: "q".into(),
        }
        .encode(&mut inner);
        // Skip length and code: the embedded payload is the bare body.
        let body = inner.freeze().slice(5..);
        let embedded = DistribMsg::Embedded {
            code: 3,
            payload: body.clone(),
        };
        let mut buf = BytesMut::new();
        embedded.encode(&mut buf);
        let DistribMsg::Embedded { code, payload } =
            DistribMsg::decode(buf.freeze().slice(4..)).unwrap()
        else {
            panic!("expected embedded");
        };
        assert_eq!(
            DistribMsg::decode_body(code, payload),
            Ok(DistribMsg::Search {
                username: "bob".into(),
                token: 9,
                query: "q".into(),
            })
        );
    }

    #[test]
    fn rejects_wrong_identifier() {
        let body = Bytes::from_static(&[50, 0, 0, 0]);
        assert!(DistribMsg::decode_body(3, body).is_err());
    }
}
