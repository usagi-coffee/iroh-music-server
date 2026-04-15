use iroh::{EndpointAddr, EndpointId, RelayUrl};
use protocol::{
    BackendRequest, BackendResponse, CoverArtId, SearchQuery, StreamDescriptor, TrackId,
};
use server::{Error, RemoteClient, Result};

#[allow(async_fn_in_trait)]
pub trait Backend {
    async fn summary(&self) -> Result<BackendResponse>;
    async fn artists(&self) -> Result<BackendResponse>;
    async fn artist(&self, artist_id: &str) -> Result<BackendResponse>;
    async fn album(&self, album_id: &str) -> Result<BackendResponse>;
    async fn album_tracks(&self, album_id: &str) -> Result<BackendResponse>;
    async fn track(&self, track_id: &str) -> Result<BackendResponse>;
    async fn resolve_id(&self, id: &str) -> Result<BackendResponse>;
    async fn cover_art(&self, cover_art_id: &str) -> Result<BackendResponse>;
    async fn search(&self, term: &str, limit: usize) -> Result<BackendResponse>;
    async fn stream(&self, track_id: &str) -> Result<(server::StreamDescriptor, Vec<u8>)>;
}

#[derive(Debug, Clone)]
pub struct RemoteBackend {
    client: RemoteClient,
}

impl RemoteBackend {
    pub async fn connect(endpoint: EndpointId, relay: Option<RelayUrl>) -> Result<Self> {
        let client = RemoteClient::connect(endpoint, relay).await?;
        Ok(Self { client })
    }

    pub async fn connect_addr(addr: EndpointAddr) -> Result<Self> {
        let client = RemoteClient::connect_addr(addr).await?;
        Ok(Self { client })
    }
}

impl Backend for RemoteBackend {
    async fn summary(&self) -> Result<BackendResponse> {
        self.client.request(BackendRequest::GetLibrarySummary).await
    }

    async fn artists(&self) -> Result<BackendResponse> {
        self.client.request(BackendRequest::ListArtists).await
    }

    async fn artist(&self, artist_id: &str) -> Result<BackendResponse> {
        self.client
            .request(BackendRequest::GetArtist {
                artist_id: protocol::ArtistId(artist_id.to_string()),
            })
            .await
    }

    async fn album(&self, album_id: &str) -> Result<BackendResponse> {
        self.client
            .request(BackendRequest::GetAlbum {
                album_id: protocol::AlbumId(album_id.to_string()),
            })
            .await
    }

    async fn album_tracks(&self, album_id: &str) -> Result<BackendResponse> {
        self.client
            .request(BackendRequest::GetAlbumTracks {
                album_id: protocol::AlbumId(album_id.to_string()),
            })
            .await
    }

    async fn track(&self, track_id: &str) -> Result<BackendResponse> {
        self.client
            .request(BackendRequest::GetTrack {
                track_id: TrackId(track_id.to_string()),
            })
            .await
    }

    async fn resolve_id(&self, id: &str) -> Result<BackendResponse> {
        self.client
            .request(BackendRequest::ResolveId { id: id.to_string() })
            .await
    }

    async fn cover_art(&self, cover_art_id: &str) -> Result<BackendResponse> {
        self.client
            .request(BackendRequest::GetCoverArt {
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

        self.client
            .request(BackendRequest::Search {
                query: SearchQuery {
                    term: term.to_string(),
                    limit,
                },
            })
            .await
    }

    async fn stream(&self, track_id: &str) -> Result<(StreamDescriptor, Vec<u8>)> {
        self.client.stream(TrackId(track_id.to_string())).await
    }
}
