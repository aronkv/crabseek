//! Networking on top of `seekr-proto`.

pub mod client;
pub mod connect;
mod peer;
pub mod server;
pub mod shares;
pub mod transfer;

pub use client::{
    Client, ClientConfig, ConnectMethod, DownloadId, DownloadState, Event, StartError, UploadId,
    UploadState,
};
pub use server::{LoginError, LoginInfo, ServerConnection};
