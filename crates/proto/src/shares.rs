//! Browsing messages: a peer's whole share list (`SharedFileListResponse`,
//! peer code 5) and one folder's contents (`FolderContentsResponse`, peer
//! code 37). Both bodies are zlib-compressed, and file names inside a
//! directory are bare names, not full paths.

use std::io::{Read, Write};

use bytes::{BufMut, BytesMut};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::search::{SearchFile, decode_files, encode_files};
use crate::wire::{DecodeError, DecodeResult, Reader, WireWrite};

const MAX_DECOMPRESSED: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedDirectory {
    /// Full virtual path with `\` separators.
    pub path: String,
    pub files: Vec<SearchFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedFileList {
    pub dirs: Vec<SharedDirectory>,
    pub private_dirs: Vec<SharedDirectory>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderContents {
    pub token: u32,
    pub folder: String,
    pub dirs: Vec<SharedDirectory>,
}

fn decompress(body: &[u8]) -> DecodeResult<Vec<u8>> {
    let mut raw = Vec::new();
    ZlibDecoder::new(body)
        .take(MAX_DECOMPRESSED)
        .read_to_end(&mut raw)
        .map_err(|e| DecodeError::Decompress(e.to_string()))?;
    Ok(raw)
}

fn compress(raw: &[u8], dst: &mut BytesMut) {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(raw).expect("writing to a Vec cannot fail");
    dst.put_slice(&enc.finish().expect("writing to a Vec cannot fail"));
}

fn decode_dirs(r: &mut Reader) -> DecodeResult<Vec<SharedDirectory>> {
    let count = r.u32()? as usize;
    let mut dirs = Vec::with_capacity(count.min(r.remaining() / 8));
    for _ in 0..count {
        dirs.push(SharedDirectory {
            path: r.string()?,
            files: decode_files(r)?,
        });
    }
    Ok(dirs)
}

fn encode_dirs(b: &mut BytesMut, dirs: &[SharedDirectory]) {
    b.put_u32_le(dirs.len() as u32);
    for dir in dirs {
        b.put_string_wire(&dir.path);
        encode_files(b, &dir.files);
    }
}

impl SharedFileList {
    pub fn decode_compressed(body: &[u8]) -> DecodeResult<Self> {
        let raw = decompress(body)?;
        let mut r = Reader::new(&raw);
        let dirs = decode_dirs(&mut r)?;
        let mut private_dirs = Vec::new();
        if !r.is_empty() {
            r.u32()?; // unknown, always 0
            if !r.is_empty() {
                private_dirs = decode_dirs(&mut r)?;
            }
        }
        Ok(Self { dirs, private_dirs })
    }

    pub fn encode_compressed(&self, dst: &mut BytesMut) {
        let mut raw = BytesMut::new();
        encode_dirs(&mut raw, &self.dirs);
        raw.put_u32_le(0);
        encode_dirs(&mut raw, &self.private_dirs);
        compress(&raw, dst);
    }
}

impl FolderContents {
    pub fn decode_compressed(body: &[u8]) -> DecodeResult<Self> {
        let raw = decompress(body)?;
        let mut r = Reader::new(&raw);
        Ok(Self {
            token: r.u32()?,
            folder: r.string()?,
            dirs: decode_dirs(&mut r)?,
        })
    }

    pub fn encode_compressed(&self, dst: &mut BytesMut) {
        let mut raw = BytesMut::new();
        raw.put_u32_le(self.token);
        raw.put_string_wire(&self.folder);
        encode_dirs(&mut raw, &self.dirs);
        compress(&raw, dst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> SharedDirectory {
        SharedDirectory {
            path: "Music\\Artist\\Album".into(),
            files: vec![SearchFile {
                filename: "01 - Song.flac".into(),
                size: 123,
                extension: "flac".into(),
                attributes: vec![(1, 200), (4, 44100), (5, 16)],
            }],
        }
    }

    #[test]
    fn file_list_roundtrip() {
        let list = SharedFileList {
            dirs: vec![dir()],
            private_dirs: vec![],
        };
        let mut buf = BytesMut::new();
        list.encode_compressed(&mut buf);
        assert_eq!(SharedFileList::decode_compressed(&buf), Ok(list));
    }

    #[test]
    fn folder_contents_roundtrip() {
        let contents = FolderContents {
            token: 5,
            folder: "Music\\Artist\\Album".into(),
            dirs: vec![dir()],
        };
        let mut buf = BytesMut::new();
        contents.encode_compressed(&mut buf);
        assert_eq!(FolderContents::decode_compressed(&buf), Ok(contents));
    }
}
