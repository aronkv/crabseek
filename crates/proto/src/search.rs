//! Search results (`FileSearchResponse`, peer code 9). The body after the
//! message code is zlib-compressed.

use std::io::{Read, Write};

use bytes::{BufMut, BytesMut};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::wire::{DecodeError, DecodeResult, Reader, WireWrite};

/// Guards against zip bombs; real responses are far below this.
const MAX_DECOMPRESSED: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResponse {
    pub username: String,
    pub token: u32,
    pub files: Vec<SearchFile>,
    pub slot_free: bool,
    /// Bytes per second, as measured by the server.
    pub avg_speed: u32,
    pub queue_length: u32,
    /// Files only shared with the peer's buddies; usually not downloadable.
    pub private_files: Vec<SearchFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFile {
    /// Full path as the peer shares it, with `\` as separator.
    pub filename: String,
    pub size: u64,
    pub extension: String,
    pub attributes: Vec<(u32, u32)>,
}

/// See "File Attribute Types" in the spec.
pub mod attr {
    pub const BITRATE: u32 = 0;
    pub const DURATION: u32 = 1;
    pub const VBR: u32 = 2;
    pub const SAMPLE_RATE: u32 = 4;
    pub const BIT_DEPTH: u32 = 5;
}

impl SearchFile {
    pub fn attribute(&self, code: u32) -> Option<u32> {
        self.attributes
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, v)| *v)
    }

    pub fn bitrate(&self) -> Option<u32> {
        self.attribute(attr::BITRATE)
    }

    pub fn duration(&self) -> Option<u32> {
        self.attribute(attr::DURATION)
    }

    pub fn sample_rate(&self) -> Option<u32> {
        self.attribute(attr::SAMPLE_RATE)
    }

    pub fn bit_depth(&self) -> Option<u32> {
        self.attribute(attr::BIT_DEPTH)
    }

    /// Everything after the last path separator.
    pub fn basename(&self) -> &str {
        self.filename
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&self.filename)
    }

    /// Everything before the last path separator.
    pub fn folder(&self) -> &str {
        self.filename
            .rfind(['\\', '/'])
            .map_or("", |i| &self.filename[..i])
    }
}

impl SearchResponse {
    /// Decodes the compressed body (everything after the message code).
    pub fn decode_compressed(body: &[u8]) -> DecodeResult<Self> {
        let mut raw = Vec::new();
        ZlibDecoder::new(body)
            .take(MAX_DECOMPRESSED)
            .read_to_end(&mut raw)
            .map_err(|e| DecodeError::Decompress(e.to_string()))?;
        Self::decode(&mut Reader::new(&raw))
    }

    fn decode(r: &mut Reader) -> DecodeResult<Self> {
        let username = r.string()?;
        let token = r.u32()?;
        let files = decode_files(r)?;
        let slot_free = r.bool()?;
        let avg_speed = r.u32()?;
        let queue_length = r.u32()?;
        // Older clients stop here; newer ones add an unused field and the
        // private results.
        let mut private_files = Vec::new();
        if !r.is_empty() {
            r.u32()?;
            if !r.is_empty() {
                private_files = decode_files(r)?;
            }
        }
        Ok(Self {
            username,
            token,
            files,
            slot_free,
            avg_speed,
            queue_length,
            private_files,
        })
    }

    /// Writes the compressed body (without message code).
    pub fn encode_compressed(&self, dst: &mut BytesMut) {
        let mut raw = BytesMut::new();
        raw.put_string_wire(&self.username);
        raw.put_u32_le(self.token);
        encode_files(&mut raw, &self.files);
        raw.put_bool_wire(self.slot_free);
        raw.put_u32_le(self.avg_speed);
        raw.put_u32_le(self.queue_length);
        raw.put_u32_le(0);
        encode_files(&mut raw, &self.private_files);

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&raw).expect("writing to a Vec cannot fail");
        dst.put_slice(&enc.finish().expect("writing to a Vec cannot fail"));
    }
}

fn decode_files(r: &mut Reader) -> DecodeResult<Vec<SearchFile>> {
    let count = r.u32()? as usize;
    // Each entry is at least 21 bytes; don't trust the count for allocation.
    let mut files = Vec::with_capacity(count.min(r.remaining() / 21));
    for _ in 0..count {
        r.u8()?; // always 1
        let filename = r.string()?;
        let size = r.u64()?;
        let extension = r.string()?;
        let attr_count = r.u32()? as usize;
        let mut attributes = Vec::with_capacity(attr_count.min(r.remaining() / 8));
        for _ in 0..attr_count {
            attributes.push((r.u32()?, r.u32()?));
        }
        files.push(SearchFile {
            filename,
            size,
            extension,
            attributes,
        });
    }
    Ok(files)
}

fn encode_files(b: &mut BytesMut, files: &[SearchFile]) {
    b.put_u32_le(files.len() as u32);
    for f in files {
        b.put_u8(1);
        b.put_string_wire(&f.filename);
        b.put_u64_le(f.size);
        b.put_string_wire(&f.extension);
        b.put_u32_le(f.attributes.len() as u32);
        for (code, value) in &f.attributes {
            b.put_u32_le(*code);
            b.put_u32_le(*value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SearchResponse {
        SearchResponse {
            username: "bob".into(),
            token: 77,
            files: vec![
                SearchFile {
                    filename: "@@music\\Artist\\Album\\01 - Song.flac".into(),
                    size: 31_000_000,
                    extension: "flac".into(),
                    attributes: vec![(1, 245), (4, 44100), (5, 16)],
                },
                SearchFile {
                    filename: "@@music\\Artist\\Album\\02 - Other.mp3".into(),
                    size: 8_000_000,
                    extension: "mp3".into(),
                    attributes: vec![(0, 320), (1, 200), (2, 0)],
                },
            ],
            slot_free: true,
            avg_speed: 1_500_000,
            queue_length: 2,
            private_files: vec![],
        }
    }

    #[test]
    fn roundtrip() {
        let mut buf = BytesMut::new();
        sample().encode_compressed(&mut buf);
        assert_eq!(SearchResponse::decode_compressed(&buf), Ok(sample()));
    }

    #[test]
    fn old_clients_without_trailing_fields() {
        let s = sample();
        let mut raw = BytesMut::new();
        raw.put_string_wire(&s.username);
        raw.put_u32_le(s.token);
        encode_files(&mut raw, &s.files);
        raw.put_bool_wire(s.slot_free);
        raw.put_u32_le(s.avg_speed);
        raw.put_u32_le(s.queue_length);
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&raw).unwrap();
        let body = enc.finish().unwrap();

        assert_eq!(SearchResponse::decode_compressed(&body), Ok(s));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(matches!(
            SearchResponse::decode_compressed(b"not zlib"),
            Err(DecodeError::Decompress(_))
        ));
    }

    #[test]
    fn path_helpers_and_attributes() {
        let f = &sample().files[0];
        assert_eq!(f.basename(), "01 - Song.flac");
        assert_eq!(f.folder(), "@@music\\Artist\\Album");
        assert_eq!(f.sample_rate(), Some(44100));
        assert_eq!(f.bitrate(), None);
    }
}
