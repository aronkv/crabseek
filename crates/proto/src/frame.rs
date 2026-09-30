//! Splits a TCP stream into length-prefixed message payloads.

use bytes::{Buf, Bytes, BytesMut};
use tokio_util::codec::Decoder;

/// Room lists and share listings can be several megabytes; anything past
/// this is treated as a broken or hostile peer.
pub const MAX_FRAME_LEN: usize = 128 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN} byte limit")]
    TooLarge(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Yields each frame's payload (message code and contents, without the
/// length prefix). Encoding is done by the message types themselves, so
/// writers just send the bytes they produce.
#[derive(Debug, Default)]
pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = Bytes;
    type Error = FrameError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, FrameError> {
        let Some(len_bytes) = src.get(..4) else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        if src.len() < 4 + len {
            src.reserve(4 + len - src.len());
            return Ok(None);
        }
        src.advance(4);
        Ok(Some(src.split_to(len).freeze()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frames_and_waits_for_partial_ones() {
        let mut codec = FrameCodec;
        let mut buf = BytesMut::from(&[2, 0, 0, 0, 0xaa, 0xbb, 3, 0, 0, 0, 0xcc][..]);

        assert_eq!(
            codec.decode(&mut buf).unwrap().as_deref(),
            Some(&[0xaa, 0xbb][..])
        );
        assert_eq!(codec.decode(&mut buf).unwrap(), None);

        buf.extend_from_slice(&[0xdd, 0xee]);
        assert_eq!(
            codec.decode(&mut buf).unwrap().as_deref(),
            Some(&[0xcc, 0xdd, 0xee][..])
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn rejects_oversized_frames() {
        let mut buf = BytesMut::from(&u32::MAX.to_le_bytes()[..]);
        assert!(matches!(
            FrameCodec.decode(&mut buf),
            Err(FrameError::TooLarge(_))
        ));
    }
}
