use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("iroh bind error: {0}")]
    Bind(#[from] iroh::endpoint::BindError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("notify error: {0}")]
    Notify(#[from] notify::Error),
    #[error("metadata error: {0}")]
    Lofty(#[from] lofty::error::LoftyError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("iroh dial error: {0}")]
    Connect(#[from] iroh::endpoint::ConnectError),
    #[error("iroh connect error: {0}")]
    Iroh(#[from] iroh::endpoint::ConnectingError),
    #[error("iroh connection error: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("iroh closed stream: {0}")]
    Closed(#[from] iroh::endpoint::ClosedStream),
    #[error("iroh read error: {0}")]
    Read(#[from] iroh::endpoint::ReadError),
    #[error("iroh read_exact error: {0}")]
    ReadExact(#[from] iroh::endpoint::ReadExactError),
    #[error("iroh read_to_end error: {0}")]
    ReadToEnd(#[from] iroh::endpoint::ReadToEndError),
    #[error("iroh write error: {0}")]
    Write(#[from] iroh::endpoint::WriteError),
    #[error("invalid music dir: {0}")]
    InvalidMusicDir(PathBuf),
    #[error("{0} not found: {1}")]
    NotFound(&'static str, String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

pub type Result<T> = std::result::Result<T, Error>;
