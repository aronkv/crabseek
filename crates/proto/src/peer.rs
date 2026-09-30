//! Messages on `P` connections (`uint32` message codes). The same types are
//! sent and received, so one enum covers both directions.

use bytes::{BufMut, Bytes, BytesMut};

use crate::search::SearchResponse;
use crate::shares::{FolderContents, SharedFileList};
use crate::wire::{DecodeResult, Reader, WireWrite, write_frame};

mod code {
    pub const SHARED_FILE_LIST_REQUEST: u32 = 4;
    pub const SHARED_FILE_LIST_RESPONSE: u32 = 5;
    pub const FOLDER_CONTENTS_REQUEST: u32 = 36;
    pub const FOLDER_CONTENTS_RESPONSE: u32 = 37;
    pub const FILE_SEARCH_RESPONSE: u32 = 9;
    pub const USER_INFO_REQUEST: u32 = 15;
    pub const USER_INFO_RESPONSE: u32 = 16;
    pub const TRANSFER_REQUEST: u32 = 40;
    pub const TRANSFER_RESPONSE: u32 = 41;
    pub const QUEUE_UPLOAD: u32 = 43;
    pub const PLACE_IN_QUEUE_RESPONSE: u32 = 44;
    pub const UPLOAD_FAILED: u32 = 46;
    pub const UPLOAD_DENIED: u32 = 50;
    pub const PLACE_IN_QUEUE_REQUEST: u32 = 51;
}

/// See "Transfer Directions" in the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// The sender wants to download from the recipient (legacy).
    Download,
    /// The sender is ready to upload to the recipient.
    Upload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerMsg {
    SharedFileListRequest,
    SharedFileListResponse(SharedFileList),
    FolderContentsRequest {
        token: u32,
        folder: String,
    },
    FolderContentsResponse(FolderContents),
    FileSearchResponse(SearchResponse),
    UserInfoRequest,
    UserInfoResponse(UserInfo),
    /// The sender is ready to upload `filename` (or, legacy, wants to
    /// download it). `size` is only present for uploads.
    TransferRequest {
        direction: TransferDirection,
        token: u32,
        filename: String,
        size: Option<u64>,
    },
    /// Answer to `TransferRequest`. The deprecated download variant also
    /// carries the size when allowed.
    TransferResponse {
        token: u32,
        allowed: bool,
        size: Option<u64>,
        reason: Option<String>,
    },
    /// Asks the peer to queue an upload of `filename` to us.
    QueueUpload {
        filename: String,
    },
    PlaceInQueueResponse {
        filename: String,
        place: u32,
    },
    UploadFailed {
        filename: String,
    },
    UploadDenied {
        filename: String,
        reason: String,
    },
    PlaceInQueueRequest {
        filename: String,
    },
    Unknown {
        code: u32,
        payload: Bytes,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserInfo {
    pub description: String,
    pub picture: Option<Bytes>,
    pub total_uploads: u32,
    pub queue_size: u32,
    pub slots_free: bool,
    /// Not sent by SoulseekQt. See "Upload Permissions" in the spec.
    pub upload_permitted: Option<u32>,
}

impl PeerMsg {
    pub fn encode(&self, dst: &mut BytesMut) {
        write_frame(dst, |b| match self {
            Self::SharedFileListRequest => b.put_u32_le(code::SHARED_FILE_LIST_REQUEST),
            Self::SharedFileListResponse(list) => {
                b.put_u32_le(code::SHARED_FILE_LIST_RESPONSE);
                list.encode_compressed(b);
            }
            Self::FolderContentsRequest { token, folder } => {
                b.put_u32_le(code::FOLDER_CONTENTS_REQUEST);
                b.put_u32_le(*token);
                b.put_string_wire(folder);
            }
            Self::FolderContentsResponse(contents) => {
                b.put_u32_le(code::FOLDER_CONTENTS_RESPONSE);
                contents.encode_compressed(b);
            }
            Self::FileSearchResponse(resp) => {
                b.put_u32_le(code::FILE_SEARCH_RESPONSE);
                resp.encode_compressed(b);
            }
            Self::UserInfoRequest => b.put_u32_le(code::USER_INFO_REQUEST),
            Self::UserInfoResponse(info) => {
                b.put_u32_le(code::USER_INFO_RESPONSE);
                b.put_string_wire(&info.description);
                match &info.picture {
                    Some(pic) => {
                        b.put_bool_wire(true);
                        b.put_bytes_wire(pic);
                    }
                    None => b.put_bool_wire(false),
                }
                b.put_u32_le(info.total_uploads);
                b.put_u32_le(info.queue_size);
                b.put_bool_wire(info.slots_free);
                if let Some(p) = info.upload_permitted {
                    b.put_u32_le(p);
                }
            }
            Self::TransferRequest {
                direction,
                token,
                filename,
                size,
            } => {
                b.put_u32_le(code::TRANSFER_REQUEST);
                b.put_u32_le(match direction {
                    TransferDirection::Download => 0,
                    TransferDirection::Upload => 1,
                });
                b.put_u32_le(*token);
                b.put_string_wire(filename);
                if let Some(size) = size {
                    b.put_u64_le(*size);
                }
            }
            Self::TransferResponse {
                token,
                allowed,
                size,
                reason,
            } => {
                b.put_u32_le(code::TRANSFER_RESPONSE);
                b.put_u32_le(*token);
                b.put_bool_wire(*allowed);
                if *allowed {
                    if let Some(size) = size {
                        b.put_u64_le(*size);
                    }
                } else {
                    b.put_string_wire(reason.as_deref().unwrap_or("Cancelled"));
                }
            }
            Self::QueueUpload { filename } => {
                b.put_u32_le(code::QUEUE_UPLOAD);
                b.put_string_wire(filename);
            }
            Self::PlaceInQueueResponse { filename, place } => {
                b.put_u32_le(code::PLACE_IN_QUEUE_RESPONSE);
                b.put_string_wire(filename);
                b.put_u32_le(*place);
            }
            Self::UploadFailed { filename } => {
                b.put_u32_le(code::UPLOAD_FAILED);
                b.put_string_wire(filename);
            }
            Self::UploadDenied { filename, reason } => {
                b.put_u32_le(code::UPLOAD_DENIED);
                b.put_string_wire(filename);
                b.put_string_wire(reason);
            }
            Self::PlaceInQueueRequest { filename } => {
                b.put_u32_le(code::PLACE_IN_QUEUE_REQUEST);
                b.put_string_wire(filename);
            }
            Self::Unknown { code, payload } => {
                b.put_u32_le(*code);
                b.put_slice(payload);
            }
        });
    }

    pub fn decode(payload: Bytes) -> DecodeResult<Self> {
        let mut r = Reader::new(&payload);
        let code = r.u32()?;
        Ok(match code {
            code::SHARED_FILE_LIST_REQUEST => Self::SharedFileListRequest,
            code::SHARED_FILE_LIST_RESPONSE => {
                Self::SharedFileListResponse(SharedFileList::decode_compressed(r.rest())?)
            }
            code::FOLDER_CONTENTS_REQUEST => Self::FolderContentsRequest {
                token: r.u32()?,
                folder: r.string()?,
            },
            code::FOLDER_CONTENTS_RESPONSE => {
                Self::FolderContentsResponse(FolderContents::decode_compressed(r.rest())?)
            }
            code::FILE_SEARCH_RESPONSE => {
                Self::FileSearchResponse(SearchResponse::decode_compressed(r.rest())?)
            }
            code::USER_INFO_REQUEST => Self::UserInfoRequest,
            code::USER_INFO_RESPONSE => {
                let description = r.string()?;
                let picture = if r.bool()? {
                    Some(payload.slice_ref(r.bytes()?))
                } else {
                    None
                };
                Self::UserInfoResponse(UserInfo {
                    description,
                    picture,
                    total_uploads: r.u32()?,
                    queue_size: r.u32()?,
                    slots_free: r.bool()?,
                    upload_permitted: (!r.is_empty()).then(|| r.u32()).transpose()?,
                })
            }
            code::TRANSFER_REQUEST => {
                let direction = match r.u32()? {
                    0 => TransferDirection::Download,
                    _ => TransferDirection::Upload,
                };
                let token = r.u32()?;
                let filename = r.string()?;
                let size = (direction == TransferDirection::Upload)
                    .then(|| r.u64())
                    .transpose()?;
                Self::TransferRequest {
                    direction,
                    token,
                    filename,
                    size,
                }
            }
            code::TRANSFER_RESPONSE => {
                let token = r.u32()?;
                let allowed = r.bool()?;
                let (size, reason) = if allowed {
                    ((r.remaining() >= 8).then(|| r.u64()).transpose()?, None)
                } else {
                    (None, (!r.is_empty()).then(|| r.string()).transpose()?)
                };
                Self::TransferResponse {
                    token,
                    allowed,
                    size,
                    reason,
                }
            }
            code::QUEUE_UPLOAD => Self::QueueUpload {
                filename: r.string()?,
            },
            code::PLACE_IN_QUEUE_RESPONSE => Self::PlaceInQueueResponse {
                filename: r.string()?,
                place: r.u32()?,
            },
            code::UPLOAD_FAILED => Self::UploadFailed {
                filename: r.string()?,
            },
            code::UPLOAD_DENIED => Self::UploadDenied {
                filename: r.string()?,
                reason: r.string()?,
            },
            code::PLACE_IN_QUEUE_REQUEST => Self::PlaceInQueueRequest {
                filename: r.string()?,
            },
            _ => Self::Unknown {
                code,
                payload: payload.slice(4..),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: PeerMsg) {
        let mut buf = BytesMut::new();
        msg.encode(&mut buf);
        let payload = buf.freeze().slice(4..);
        assert_eq!(PeerMsg::decode(payload), Ok(msg));
    }

    #[test]
    fn user_info_request_bytes() {
        let mut buf = BytesMut::new();
        PeerMsg::UserInfoRequest.encode(&mut buf);
        assert_eq!(&buf[..], &[4, 0, 0, 0, 15, 0, 0, 0]);
    }

    #[test]
    fn user_info_roundtrips() {
        roundtrip(PeerMsg::UserInfoRequest);
        roundtrip(PeerMsg::UserInfoResponse(UserInfo {
            description: "hi".into(),
            picture: Some(Bytes::from_static(b"\x89PNG")),
            total_uploads: 3,
            queue_size: 1,
            slots_free: true,
            upload_permitted: Some(1),
        }));
        // SoulseekQt layout: no picture, no trailing permission field.
        roundtrip(PeerMsg::UserInfoResponse(UserInfo::default()));
    }

    #[test]
    fn search_response_roundtrips() {
        roundtrip(PeerMsg::FileSearchResponse(SearchResponse {
            username: "bob".into(),
            token: 1,
            ..Default::default()
        }));
    }

    #[test]
    fn transfer_roundtrips() {
        roundtrip(PeerMsg::TransferRequest {
            direction: TransferDirection::Upload,
            token: 9,
            filename: "@@a\\b.flac".into(),
            size: Some(123),
        });
        roundtrip(PeerMsg::TransferRequest {
            direction: TransferDirection::Download,
            token: 9,
            filename: "x".into(),
            size: None,
        });
        roundtrip(PeerMsg::TransferResponse {
            token: 9,
            allowed: true,
            size: None,
            reason: None,
        });
        roundtrip(PeerMsg::TransferResponse {
            token: 9,
            allowed: true,
            size: Some(5),
            reason: None,
        });
        roundtrip(PeerMsg::TransferResponse {
            token: 9,
            allowed: false,
            size: None,
            reason: Some("Queued".into()),
        });
        roundtrip(PeerMsg::QueueUpload {
            filename: "x".into(),
        });
        roundtrip(PeerMsg::PlaceInQueueResponse {
            filename: "x".into(),
            place: 3,
        });
        roundtrip(PeerMsg::UploadFailed {
            filename: "x".into(),
        });
        roundtrip(PeerMsg::UploadDenied {
            filename: "x".into(),
            reason: "File not shared.".into(),
        });
        roundtrip(PeerMsg::PlaceInQueueRequest {
            filename: "x".into(),
        });
    }

    #[test]
    fn accepting_upload_bytes() {
        // Upload response (41 b): token, allowed = true, nothing else.
        let mut buf = BytesMut::new();
        PeerMsg::TransferResponse {
            token: 7,
            allowed: true,
            size: None,
            reason: None,
        }
        .encode(&mut buf);
        assert_eq!(&buf[..], &[9, 0, 0, 0, 41, 0, 0, 0, 7, 0, 0, 0, 1]);
    }

    #[test]
    fn browse_roundtrips() {
        roundtrip(PeerMsg::SharedFileListRequest);
        roundtrip(PeerMsg::SharedFileListResponse(SharedFileList::default()));
        roundtrip(PeerMsg::FolderContentsRequest {
            token: 3,
            folder: "Music\\A".into(),
        });
        roundtrip(PeerMsg::FolderContentsResponse(FolderContents {
            token: 3,
            folder: "Music\\A".into(),
            dirs: vec![],
        }));
    }

    #[test]
    fn unknown_roundtrips() {
        roundtrip(PeerMsg::Unknown {
            code: 99,
            payload: Bytes::from_static(&[1, 2, 3]),
        });
    }
}
