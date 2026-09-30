//! The first message on every peer connection (`uint8` message codes).

use bytes::{BufMut, Bytes, BytesMut};

use crate::wire::{DecodeError, DecodeResult, Reader, WireWrite, write_frame};

mod code {
    pub const PIERCE_FIREWALL: u8 = 0;
    pub const PEER_INIT: u8 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionType {
    /// `P`: peer messages (search results, queueing, user info).
    Peer,
    /// `F`: raw file transfer.
    File,
    /// `D`: distributed search network.
    Distributed,
}

impl ConnectionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Peer => "P",
            Self::File => "F",
            Self::Distributed => "D",
        }
    }

    pub(crate) fn decode(r: &mut Reader) -> DecodeResult<Self> {
        match r.string()?.as_str() {
            "P" => Ok(Self::Peer),
            "F" => Ok(Self::File),
            "D" => Ok(Self::Distributed),
            other => Err(DecodeError::InvalidConnectionType(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerInitMsg {
    /// Answer to an indirect connection request; the token comes from the
    /// server's `ConnectToPeer`.
    PierceFirewall { token: u32 },
    /// Opens a direct connection. The token is always 0 nowadays.
    PeerInit {
        username: String,
        conn_type: ConnectionType,
        token: u32,
    },
}

impl PeerInitMsg {
    pub fn encode(&self, dst: &mut BytesMut) {
        write_frame(dst, |b| match self {
            Self::PierceFirewall { token } => {
                b.put_u8(code::PIERCE_FIREWALL);
                b.put_u32_le(*token);
            }
            Self::PeerInit {
                username,
                conn_type,
                token,
            } => {
                b.put_u8(code::PEER_INIT);
                b.put_string_wire(username);
                b.put_string_wire(conn_type.as_str());
                b.put_u32_le(*token);
            }
        });
    }

    pub fn decode(payload: Bytes) -> DecodeResult<Self> {
        let mut r = Reader::new(&payload);
        match r.u8()? {
            code::PIERCE_FIREWALL => Ok(Self::PierceFirewall { token: r.u32()? }),
            code::PEER_INIT => Ok(Self::PeerInit {
                username: r.string()?,
                conn_type: ConnectionType::decode(&mut r)?,
                token: r.u32()?,
            }),
            other => Err(DecodeError::UnknownCode(other.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: PeerInitMsg) {
        let mut buf = BytesMut::new();
        msg.encode(&mut buf);
        let payload = buf.freeze().slice(4..);
        assert_eq!(PeerInitMsg::decode(payload), Ok(msg));
    }

    #[test]
    fn pierce_firewall_bytes() {
        let mut buf = BytesMut::new();
        PeerInitMsg::PierceFirewall { token: 0x0102 }.encode(&mut buf);
        assert_eq!(&buf[..], &[5, 0, 0, 0, 0, 0x02, 0x01, 0, 0]);
    }

    #[test]
    fn peer_init_bytes() {
        let mut buf = BytesMut::new();
        PeerInitMsg::PeerInit {
            username: "al".into(),
            conn_type: ConnectionType::Peer,
            token: 0,
        }
        .encode(&mut buf);
        assert_eq!(
            &buf[..],
            &[
                16, 0, 0, 0, 1, 2, 0, 0, 0, b'a', b'l', 1, 0, 0, 0, b'P', 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn roundtrips() {
        roundtrip(PeerInitMsg::PierceFirewall { token: 42 });
        for conn_type in [
            ConnectionType::Peer,
            ConnectionType::File,
            ConnectionType::Distributed,
        ] {
            roundtrip(PeerInitMsg::PeerInit {
                username: "bob".into(),
                conn_type,
                token: 0,
            });
        }
    }

    #[test]
    fn rejects_unknown_type() {
        let mut buf = BytesMut::new();
        buf.put_u8(1);
        buf.put_string_wire("bob");
        buf.put_string_wire("X");
        buf.put_u32_le(0);
        assert_eq!(
            PeerInitMsg::decode(buf.freeze()),
            Err(DecodeError::InvalidConnectionType("X".into()))
        );
    }
}
