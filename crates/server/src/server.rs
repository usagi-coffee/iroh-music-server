use std::sync::{Arc, RwLock};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::ServerConfig;
use crate::error::{Error, Result};
use crate::index::{CoverArtSource, LibraryIndex};
use crate::lastfm::LastfmClient;
use crate::scanner::scan_music_dir;
use protocol::{
    AlbumId, ArtistId, BackendRequest, BackendResponse, CoverArtBytes, CoverArtId, SearchQuery,
    StreamDescriptor, TrackId,
};

pub struct MusicServer {
    config: ServerConfig,
    library: Arc<RwLock<LibraryIndex>>,
    _watcher: RecommendedWatcher,
    _watch_task: JoinHandle<()>,
}

impl MusicServer {
    pub fn load(config: ServerConfig) -> Result<Self> {
        let mut initial_library = scan_music_dir(&config.music_dir)?;
        enrich_with_lastfm(&mut initial_library, config.lastfm_api_key.as_deref());
        let library = Arc::new(RwLock::new(initial_library));
        let (watcher, watch_task) = spawn_library_watcher(
            config.music_dir.clone(),
            config.lastfm_api_key.clone(),
            Arc::clone(&library),
        )?;

        Ok(Self {
            config,
            library,
            _watcher: watcher,
            _watch_task: watch_task,
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn library(&self) -> LibraryIndex {
        self.library.read().expect("library lock poisoned").clone()
    }

    pub fn handle(&self, request: BackendRequest) -> Result<BackendResponse> {
        let library = self.library.read().expect("library lock poisoned");

        match request {
            BackendRequest::GetLibrarySummary => Ok(BackendResponse::LibrarySummary {
                artist_count: library.artist_count(),
                album_count: library.album_count(),
                track_count: library.track_count(),
            }),
            BackendRequest::ListArtists => Ok(BackendResponse::Artists(
                library.artists.values().cloned().collect(),
            )),
            BackendRequest::GetArtist { artist_id } => Self::get_artist(&library, artist_id),
            BackendRequest::GetAlbum { album_id } => Self::get_album(&library, album_id),
            BackendRequest::GetTrack { track_id } => Self::get_track(&library, track_id),
            BackendRequest::GetCoverArt { cover_art_id } => {
                self.get_cover_art(&library, cover_art_id)
            }
            BackendRequest::Search { query } => Self::search(&library, query),
            BackendRequest::OpenStream { track_id } => self.open_stream(&library, track_id),
        }
    }

    fn get_artist(library: &LibraryIndex, artist_id: ArtistId) -> Result<BackendResponse> {
        let artist = library
            .artists
            .get(&artist_id)
            .cloned()
            .ok_or_else(|| Error::NotFound("artist", artist_id.0))?;
        Ok(BackendResponse::Artist(artist))
    }

    fn get_album(library: &LibraryIndex, album_id: AlbumId) -> Result<BackendResponse> {
        let album = library
            .albums
            .get(&album_id)
            .cloned()
            .ok_or_else(|| Error::NotFound("album", album_id.0))?;
        Ok(BackendResponse::Album(album))
    }

    fn get_track(library: &LibraryIndex, track_id: TrackId) -> Result<BackendResponse> {
        let track = library
            .tracks
            .get(&track_id)
            .cloned()
            .ok_or_else(|| Error::NotFound("track", track_id.0))?;
        Ok(BackendResponse::Track(track))
    }

    fn search(library: &LibraryIndex, query: SearchQuery) -> Result<BackendResponse> {
        if query.limit == 0 {
            return Err(Error::InvalidRequest(
                "search limit must be greater than zero".to_string(),
            ));
        }

        let term = query.term.to_ascii_lowercase();
        let artists = library
            .artists
            .values()
            .filter(|artist| artist.name.to_ascii_lowercase().contains(&term))
            .take(query.limit)
            .cloned()
            .collect();
        let albums = library
            .albums
            .values()
            .filter(|album| {
                album.title.to_ascii_lowercase().contains(&term)
                    || album.artist.to_ascii_lowercase().contains(&term)
            })
            .take(query.limit)
            .cloned()
            .collect();
        let tracks = library
            .tracks
            .values()
            .filter(|track| {
                track.title.to_ascii_lowercase().contains(&term)
                    || track.artist.to_ascii_lowercase().contains(&term)
                    || track.album.to_ascii_lowercase().contains(&term)
            })
            .take(query.limit)
            .cloned()
            .collect();

        Ok(BackendResponse::SearchResults {
            artists,
            albums,
            tracks,
        })
    }

    fn open_stream(&self, library: &LibraryIndex, track_id: TrackId) -> Result<BackendResponse> {
        let track = library
            .tracks
            .get(&track_id)
            .ok_or_else(|| Error::NotFound("track", track_id.0.clone()))?;
        let full_path = self.config.music_dir.join(&track.relative_path);
        Ok(BackendResponse::Stream(StreamDescriptor {
            track_id: track.id.clone(),
            path: full_path,
            content_type: track.content_type.clone(),
            file_size: track.file_size,
        }))
    }

    fn get_cover_art(
        &self,
        library: &LibraryIndex,
        cover_art_id: CoverArtId,
    ) -> Result<BackendResponse> {
        let source = library
            .cover_arts
            .get(&cover_art_id)
            .ok_or_else(|| Error::NotFound("cover art", cover_art_id.0.clone()))?;

        match source {
            CoverArtSource::Sidecar {
                relative_path,
                content_type,
            } => {
                let bytes = std::fs::read(self.config.music_dir.join(relative_path))?;
                Ok(BackendResponse::CoverArt(CoverArtBytes {
                    cover_art_id,
                    content_type: content_type.clone(),
                    bytes,
                }))
            }
            CoverArtSource::Embedded { track_id } => Err(Error::InvalidRequest(format!(
                "embedded cover art extraction is not implemented for track {}",
                track_id.0
            ))),
            CoverArtSource::External { url } => {
                let response = reqwest::blocking::get(url)?.error_for_status()?;
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = response.bytes()?.to_vec();
                Ok(BackendResponse::CoverArt(CoverArtBytes {
                    cover_art_id,
                    content_type,
                    bytes,
                }))
            }
        }
    }
}

fn spawn_library_watcher(
    music_dir: std::path::PathBuf,
    lastfm_api_key: Option<String>,
    library: Arc<RwLock<LibraryIndex>>,
) -> Result<(RecommendedWatcher, JoinHandle<()>)> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = tx.send(result);
    })?;
    watcher.watch(&music_dir, RecursiveMode::Recursive)?;

    let watch_task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                Ok(_) => match scan_music_dir(&music_dir) {
                    Ok(updated) => {
                        let mut updated = updated;
                        enrich_with_lastfm(&mut updated, lastfm_api_key.as_deref());
                        if let Ok(mut current) = library.write() {
                            *current = updated;
                        }
                    }
                    Err(error) => eprintln!("failed to refresh library index: {error}"),
                },
                Err(error) => eprintln!("watch error: {error}"),
            }
        }
    });

    Ok((watcher, watch_task))
}

fn enrich_with_lastfm(library: &mut LibraryIndex, api_key: Option<&str>) {
    if let Some(api_key) = api_key.filter(|api_key| !api_key.trim().is_empty()) {
        eprintln!("[lastfm] enriching album metadata");
        LastfmClient::new(api_key).enrich_library(library);
    }
}
