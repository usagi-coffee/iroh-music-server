use client::{Client, Error, Result};
use iroh::endpoint::RecvStream;
use protocol::{
    BackendRequest, BackendResponse, CoverArtId, SearchQuery, StreamDescriptor, TrackId,
};

#[allow(async_fn_in_trait)]
pub trait Backend {
    async fn summary(&self) -> Result<BackendResponse>;
    async fn artists(&self) -> Result<BackendResponse>;
    async fn starred(&self) -> Result<BackendResponse>;
    async fn set_starred(&self, id: &str, starred: bool) -> Result<BackendResponse>;
    async fn artist(&self, artist_id: &str) -> Result<BackendResponse>;
    async fn album(&self, album_id: &str) -> Result<BackendResponse>;
    async fn album_tracks(&self, album_id: &str) -> Result<BackendResponse>;
    async fn track(&self, track_id: &str) -> Result<BackendResponse>;
    async fn resolve_id(&self, id: &str) -> Result<BackendResponse>;
    async fn cover_art(&self, cover_art_id: &str) -> Result<BackendResponse>;
    async fn search(&self, term: &str, limit: usize) -> Result<BackendResponse>;
    async fn stream(&self, track_id: &str) -> Result<(StreamDescriptor, RecvStream)>;
}

pub type RemoteBackend = Client;

impl Backend for Client {
    async fn summary(&self) -> Result<BackendResponse> {
        self.request(BackendRequest::GetLibrarySummary).await
    }

    async fn artists(&self) -> Result<BackendResponse> {
        self.request(BackendRequest::ListArtists).await
    }

    async fn starred(&self) -> Result<BackendResponse> {
        self.request(BackendRequest::GetStarred).await
    }

    async fn set_starred(&self, id: &str, starred: bool) -> Result<BackendResponse> {
        self.request(BackendRequest::SetStarred {
            id: id.to_string(),
            starred,
        })
        .await
    }

    async fn artist(&self, artist_id: &str) -> Result<BackendResponse> {
        self.request(BackendRequest::GetArtist {
            artist_id: protocol::ArtistId(artist_id.to_string()),
        })
        .await
    }

    async fn album(&self, album_id: &str) -> Result<BackendResponse> {
        self.request(BackendRequest::GetAlbum {
            album_id: protocol::AlbumId(album_id.to_string()),
        })
        .await
    }

    async fn album_tracks(&self, album_id: &str) -> Result<BackendResponse> {
        self.request(BackendRequest::GetAlbumTracks {
            album_id: protocol::AlbumId(album_id.to_string()),
        })
        .await
    }

    async fn track(&self, track_id: &str) -> Result<BackendResponse> {
        self.request(BackendRequest::GetTrack {
            track_id: TrackId(track_id.to_string()),
        })
        .await
    }

    async fn resolve_id(&self, id: &str) -> Result<BackendResponse> {
        self.request(BackendRequest::ResolveId { id: id.to_string() })
            .await
    }

    async fn cover_art(&self, cover_art_id: &str) -> Result<BackendResponse> {
        self.request(BackendRequest::GetCoverArt {
            cover_art_id: CoverArtId(cover_art_id.to_string()),
        })
        .await
    }

    async fn search(&self, term: &str, limit: usize) -> Result<BackendResponse> {
        if term.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "search term must not be empty".to_string(),
            ));
        }

        self.request(BackendRequest::Search {
            query: SearchQuery {
                term: term.to_string(),
                limit,
            },
        })
        .await
    }

    async fn stream(&self, track_id: &str) -> Result<(StreamDescriptor, RecvStream)> {
        self.stream_open(TrackId(track_id.to_string())).await
    }
}
