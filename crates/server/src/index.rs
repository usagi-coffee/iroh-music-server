use std::collections::BTreeMap;
use std::path::PathBuf;

use protocol::{Album, AlbumId, Artist, ArtistId, CoverArtId, Track, TrackId};

#[derive(Debug, Clone, Default)]
pub struct LibraryIndex {
    pub artists: BTreeMap<ArtistId, Artist>,
    pub albums: BTreeMap<AlbumId, Album>,
    pub tracks: BTreeMap<TrackId, Track>,
    pub cover_arts: BTreeMap<CoverArtId, CoverArtSource>,
}

impl LibraryIndex {
    pub fn artist_count(&self) -> usize {
        self.artists.len()
    }

    pub fn album_count(&self) -> usize {
        self.albums.len()
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }
}

#[derive(Debug, Clone)]
pub enum CoverArtSource {
    Sidecar {
        relative_path: PathBuf,
        content_type: String,
    },
    Embedded {
        track_id: TrackId,
    },
    External {
        url: String,
    },
}
