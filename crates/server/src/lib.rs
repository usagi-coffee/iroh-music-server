pub mod config;
pub mod error;
pub mod index;
pub mod iroh_rpc;
pub mod lastfm;
pub mod scanner;
pub mod server;
pub mod stream;

pub use config::ServerConfig;
pub use error::{Error, Result};
pub use index::{CoverArtSource, LibraryIndex};
pub use iroh_rpc::{IrohConfig, RemoteClient, ServerHandle, spawn_iroh_server};
pub use protocol::{
    Album, AlbumId, AlbumMetadata, Artist, ArtistId, BackendRequest, BackendResponse,
    CoverArtBytes, CoverArtId, IROH_ALPN, ResolvedId, SearchQuery, StreamDescriptor, Track,
    TrackId,
};
pub use scanner::scan_music_dir;
pub use server::MusicServer;
pub use stream::read_file_prefix;
