use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

pub const IROH_ALPN: &[u8] = b"irohifi/1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ArtistId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AlbumId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TrackId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CoverArtId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: ArtistId,
    pub name: String,
    pub album_ids: Vec<AlbumId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: AlbumId,
    pub title: String,
    pub artist: String,
    pub album_artist: Option<String>,
    pub track_ids: Vec<TrackId>,
    pub date: Option<String>,
    pub original_date: Option<String>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub labels: Vec<String>,
    pub catalog_number: Option<String>,
    pub comment: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub disc_count: Option<u32>,
    pub duration_seconds: Option<u32>,
    pub size_bytes: u64,
    pub cover_art_id: Option<CoverArtId>,
    pub metadata: Option<AlbumMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumMetadata {
    pub lastfm_url: Option<String>,
    pub lastfm_mbid: Option<String>,
    pub published: Option<String>,
    pub summary: Option<String>,
    pub image_url: Option<String>,
    pub tags: Vec<String>,
    pub listeners: Option<u64>,
    pub playcount: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration_seconds: Option<u32>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub codec: Option<String>,
    pub genres: Vec<String>,
    pub date: Option<String>,
    pub musicbrainz_track_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub cover_art_id: Option<CoverArtId>,
    pub suffix: Option<String>,
    #[serde(skip_serializing, skip_deserializing, default)]
    pub relative_path: PathBuf,
    pub file_size: u64,
    pub modified_at: SystemTime,
    pub content_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub term: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamDescriptor {
    pub track_id: TrackId,
    #[serde(skip_serializing, skip_deserializing, default)]
    pub path: PathBuf,
    pub content_type: String,
    pub file_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverArtBytes {
    pub cover_art_id: CoverArtId,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolvedId {
    Artist(Artist),
    Album(Album),
    Track(Track),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendRequest {
    GetLibrarySummary,
    ListArtists,
    GetArtist { artist_id: ArtistId },
    GetAlbum { album_id: AlbumId },
    GetAlbumTracks { album_id: AlbumId },
    GetTrack { track_id: TrackId },
    GetCoverArt { cover_art_id: CoverArtId },
    ResolveId { id: String },
    Search { query: SearchQuery },
    OpenStream { track_id: TrackId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendResponse {
    Error {
        message: String,
    },
    LibrarySummary {
        artist_count: usize,
        album_count: usize,
        track_count: usize,
    },
    Artists(Vec<Artist>),
    Artist(Artist),
    Album(Album),
    Tracks(Vec<Track>),
    Track(Track),
    CoverArt(CoverArtBytes),
    ResolvedId(ResolvedId),
    SearchResults {
        artists: Vec<Artist>,
        albums: Vec<Album>,
        tracks: Vec<Track>,
    },
    Stream(StreamDescriptor),
}
