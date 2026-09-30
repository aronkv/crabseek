//! Primitive types of the Soulseek wire format.
//!
//! Every integer is little-endian, strings and byte arrays are prefixed with
//! a `uint32` length. See the "Packing" section of `docs/SLSKPROTOCOL.md`.

use std::net::Ipv4Addr;

use bytes::{BufMut, BytesMut};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("unexpected end of message: needed {needed} more bytes")]
    UnexpectedEof { needed: usize },
    #[error("invalid bool value {0}")]
    InvalidBool(u8),
    #[error("invalid connection type {0:?}")]
    InvalidConnectionType(String),
    #[error("unknown message code {0}")]
    UnknownCode(u32),
    #[error("decompression failed: {0}")]
    Decompress(String),
}

pub type DecodeResult<T> = Result<T, DecodeError>;

/// Cursor over a message payload.
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn take(&mut self, n: usize) -> DecodeResult<&'a [u8]> {
        if self.buf.len() < n {
            return Err(DecodeError::UnexpectedEof {
                needed: n - self.buf.len(),
            });
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> DecodeResult<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("take returned N bytes"))
    }

    pub fn u8(&mut self) -> DecodeResult<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> DecodeResult<u16> {
        self.array().map(u16::from_le_bytes)
    }

    pub fn u32(&mut self) -> DecodeResult<u32> {
        self.array().map(u32::from_le_bytes)
    }

    pub fn i32(&mut self) -> DecodeResult<i32> {
        self.array().map(i32::from_le_bytes)
    }

    pub fn u64(&mut self) -> DecodeResult<u64> {
        self.array().map(u64::from_le_bytes)
    }

    pub fn bool(&mut self) -> DecodeResult<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(DecodeError::InvalidBool(other)),
        }
    }

    pub fn bytes(&mut self) -> DecodeResult<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    /// Strings are usually UTF-8, but older clients send Latin-1, so fall
    /// back to that instead of failing the whole message.
    pub fn string(&mut self) -> DecodeResult<String> {
        let raw = self.bytes()?;
        Ok(match std::str::from_utf8(raw) {
            Ok(s) => s.to_owned(),
            Err(_) => raw.iter().map(|&b| b as char).collect(),
        })
    }

    /// IPv4 addresses are sent as a little-endian `uint32` of the
    /// big-endian address, so the numeric value maps directly.
    pub fn ipv4(&mut self) -> DecodeResult<Ipv4Addr> {
        self.u32().map(Ipv4Addr::from)
    }

    pub fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.buf)
    }
}

/// Write helpers mirroring [`Reader`].
pub trait WireWrite {
    fn put_bool_wire(&mut self, v: bool);
    fn put_bytes_wire(&mut self, v: &[u8]);
    fn put_string_wire(&mut self, v: &str);
    fn put_ipv4_wire(&mut self, v: Ipv4Addr);
}

impl WireWrite for BytesMut {
    fn put_bool_wire(&mut self, v: bool) {
        self.put_u8(v as u8);
    }

    fn put_bytes_wire(&mut self, v: &[u8]) {
        self.put_u32_le(v.len() as u32);
        self.put_slice(v);
    }

    fn put_string_wire(&mut self, v: &str) {
        self.put_bytes_wire(v.as_bytes());
    }

    fn put_ipv4_wire(&mut self, v: Ipv4Addr) {
        self.put_u32_le(v.into());
    }
}

/// Writes a complete frame: `uint32` length, then whatever `body` writes.
pub(crate) fn write_frame(dst: &mut BytesMut, body: impl FnOnce(&mut BytesMut)) {
    let start = dst.len();
    dst.put_u32_le(0);
    body(dst);
    let len = (dst.len() - start - 4) as u32;
    dst[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_roundtrip() {
        let mut buf = BytesMut::new();
        buf.put_u8(7);
        buf.put_u16_le(513);
        buf.put_u32_le(0xdead_beef);
        buf.put_i32_le(-5);
        buf.put_u64_le(1 << 40);
        buf.put_bool_wire(true);
        buf.put_string_wire("árvíztűrő");
        buf.put_ipv4_wire(Ipv4Addr::new(192, 168, 1, 20));

        let mut r = Reader::new(&buf);
        assert_eq!(r.u8(), Ok(7));
        assert_eq!(r.u16(), Ok(513));
        assert_eq!(r.u32(), Ok(0xdead_beef));
        assert_eq!(r.i32(), Ok(-5));
        assert_eq!(r.u64(), Ok(1 << 40));
        assert_eq!(r.bool(), Ok(true));
        assert_eq!(r.string().as_deref(), Ok("árvíztűrő"));
        assert_eq!(r.ipv4(), Ok(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(r.is_empty());
    }

    #[test]
    fn ipv4_byte_order() {
        // 192.168.1.20 arrives as 14 01 a8 c0 on the wire.
        let mut r = Reader::new(&[0x14, 0x01, 0xa8, 0xc0]);
        assert_eq!(r.ipv4(), Ok(Ipv4Addr::new(192, 168, 1, 20)));
    }

    #[test]
    fn latin1_fallback() {
        let mut r = Reader::new(&[2, 0, 0, 0, 0xe1, 0x41]);
        assert_eq!(r.string().as_deref(), Ok("áA"));
    }

    #[test]
    fn truncated_input() {
        let mut r = Reader::new(&[10, 0, 0, 0, b'a']);
        assert_eq!(r.string(), Err(DecodeError::UnexpectedEof { needed: 9 }));
    }

    #[test]
    fn frame_length_prefix() {
        let mut buf = BytesMut::new();
        write_frame(&mut buf, |b| b.put_u32_le(2));
        assert_eq!(&buf[..], &[4, 0, 0, 0, 2, 0, 0, 0]);
    }
}
