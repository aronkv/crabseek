//! Soulseek protocol messages, without any I/O.
//!
//! Reference: `docs/SLSKPROTOCOL.md` (Nicotine+ protocol documentation).

pub mod distrib;
pub mod frame;
pub mod peer;
pub mod peer_init;
pub mod search;
pub mod server;
pub mod shares;
pub mod wire;

pub use frame::{FrameCodec, FrameError};
pub use peer_init::ConnectionType;
pub use wire::DecodeError;
