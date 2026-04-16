use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::file::TaggedFileExt;
use notify::{
    Event, RecommendedWatcher, RecursiveMode, Watcher,
    event::{DataChange, EventKind, ModifyKind},
};
use rusqlite::{Connection, params};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::ServerConfig;
use crate::error::{Error, Result};
use crate::index::{CoverArtSource, LibraryIndex};
use crate::scanner::scan_music_dir;
use protocol::{
    AlbumId, ArtistId, BackendRequest, BackendResponse, CoverArtBytes, CoverArtId, ResolvedId,
    SearchQuery, StarredSet, StreamDescriptor, TrackId,
};

const STATE_DB_FILE: &str = "iroh-fm.db";

pub struct MusicServer {
    config: ServerConfig,
    library: Arc<RwLock<LibraryIndex>>,
    starred_db: std::sync::Mutex<Connection>,
    _watcher: RecommendedWatcher,
    _watch_task: JoinHandle<()>,
}

impl MusicServer {
    pub fn load(config: ServerConfig) -> Result<Self> {
        let initial_library = scan_music_dir(&config.music_dir)?;
        let library = Arc::new(RwLock::new(initial_library));
        let starred_db = open_state_db(&config.music_dir)?;
        let (watcher, watch_task) =
            spawn_library_watcher(config.music_dir.clone(), Arc::clone(&library))?;

        Ok(Self {
            config,
            library,
            starred_db: std::sync::Mutex::new(starred_db),
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
            BackendRequest::GetStarred => self.get_starred(&library),
            BackendRequest::SetStarred { id, starred } => self.set_starred(&library, id, starred),
            BackendRequest::GetArtist { artist_id } => Self::get_artist(&library, artist_id),
            BackendRequest::GetAlbum { album_id } => Self::get_album(&library, album_id),
            BackendRequest::GetAlbumTracks { album_id } => {
                Self::get_album_tracks(&library, album_id)
            }
            BackendRequest::GetTrack { track_id } => Self::get_track(&library, track_id),
            BackendRequest::GetCoverArt { cover_art_id } => {
                self.get_cover_art(&library, cover_art_id)
            }
            BackendRequest::ResolveId { id } => Self::resolve_id(&library, id),
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

    fn get_starred(&self, library: &LibraryIndex) -> Result<BackendResponse> {
        let conn = self.starred_db.lock().expect("starred db lock poisoned");
        let mut stmt =
            conn.prepare("SELECT id, kind FROM starred_items ORDER BY starred_unix DESC, id ASC")?;
        let mut artists = Vec::new();
        let mut albums = Vec::new();
        let mut tracks = Vec::new();
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (id, kind) = row?;
            match kind.as_str() {
                "artist" => {
                    if let Some(artist) = library.artists.get(&ArtistId(id)) {
                        artists.push(artist.clone());
                    }
                }
                "album" => {
                    if let Some(album) = library.albums.get(&AlbumId(id)) {
                        albums.push(album.clone());
                    }
                }
                "track" => {
                    if let Some(track) = library.tracks.get(&TrackId(id)) {
                        tracks.push(track.clone());
                    }
                }
                _ => {}
            }
        }

        Ok(BackendResponse::Starred(StarredSet {
            artists,
            albums,
            tracks,
        }))
    }

    fn get_album(library: &LibraryIndex, album_id: AlbumId) -> Result<BackendResponse> {
        let album = library
            .albums
            .get(&album_id)
            .cloned()
            .ok_or_else(|| Error::NotFound("album", album_id.0))?;
        Ok(BackendResponse::Album(album))
    }

    fn get_album_tracks(library: &LibraryIndex, album_id: AlbumId) -> Result<BackendResponse> {
        let album = library
            .albums
            .get(&album_id)
            .ok_or_else(|| Error::NotFound("album", album_id.0))?;
        let mut tracks = Vec::with_capacity(album.track_ids.len());
        for track_id in &album.track_ids {
            let track = library
                .tracks
                .get(track_id)
                .cloned()
                .ok_or_else(|| Error::NotFound("track", track_id.0.clone()))?;
            tracks.push(track);
        }
        Ok(BackendResponse::Tracks(tracks))
    }

    fn get_track(library: &LibraryIndex, track_id: TrackId) -> Result<BackendResponse> {
        let track = library
            .tracks
            .get(&track_id)
            .cloned()
            .ok_or_else(|| Error::NotFound("track", track_id.0))?;
        Ok(BackendResponse::Track(track))
    }

    fn set_starred(
        &self,
        library: &LibraryIndex,
        id: String,
        starred: bool,
    ) -> Result<BackendResponse> {
        let kind = if library.artists.contains_key(&ArtistId(id.clone())) {
            "artist"
        } else if library.albums.contains_key(&AlbumId(id.clone())) {
            "album"
        } else if library.tracks.contains_key(&TrackId(id.clone())) {
            "track"
        } else {
            return Err(Error::NotFound("id", id));
        };

        let conn = self.starred_db.lock().expect("starred db lock poisoned");
        if starred {
            conn.execute(
                r#"
                INSERT INTO starred_items (id, kind, starred_unix)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    starred_unix = excluded.starred_unix
                "#,
                params![id, kind, now_unix()],
            )?;
        } else {
            conn.execute("DELETE FROM starred_items WHERE id = ?1", params![id])?;
        }

        Ok(BackendResponse::Empty)
    }

    fn resolve_id(library: &LibraryIndex, id: String) -> Result<BackendResponse> {
        let album_id = AlbumId(id.clone());
        if let Some(album) = library.albums.get(&album_id) {
            return Ok(BackendResponse::ResolvedId(ResolvedId::Album(
                album.clone(),
            )));
        }

        let artist_id = ArtistId(id.clone());
        if let Some(artist) = library.artists.get(&artist_id) {
            return Ok(BackendResponse::ResolvedId(ResolvedId::Artist(
                artist.clone(),
            )));
        }

        let track_id = TrackId(id.clone());
        if let Some(track) = library.tracks.get(&track_id) {
            return Ok(BackendResponse::ResolvedId(ResolvedId::Track(
                track.clone(),
            )));
        }

        Err(Error::NotFound("id", id))
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
        eprintln!("[server-cover] request cover_art_id={}", cover_art_id.0);
        let source = library.cover_arts.get(&cover_art_id).ok_or_else(|| {
            eprintln!(
                "[server-cover] missing cover_art_id={} known_cover_arts={}",
                cover_art_id.0,
                library.cover_arts.len()
            );
            Error::NotFound("cover art", cover_art_id.0.clone())
        })?;

        match source {
            CoverArtSource::Sidecar {
                relative_path,
                content_type,
            } => {
                let full_path = self.config.music_dir.join(relative_path);
                eprintln!(
                    "[server-cover] source=sidecar cover_art_id={} path={} content_type={}",
                    cover_art_id.0,
                    full_path.display(),
                    content_type
                );
                let bytes = match std::fs::read(&full_path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        eprintln!(
                            "[server-cover] read failed cover_art_id={} path={} error={}",
                            cover_art_id.0,
                            full_path.display(),
                            error
                        );
                        return Err(error.into());
                    }
                };
                eprintln!(
                    "[server-cover] served cover_art_id={} bytes={} content_type={}",
                    cover_art_id.0,
                    bytes.len(),
                    content_type
                );
                Ok(BackendResponse::CoverArt(CoverArtBytes {
                    cover_art_id,
                    content_type: content_type.clone(),
                    bytes,
                }))
            }
            CoverArtSource::Embedded { track_id } => {
                let track = library
                    .tracks
                    .get(track_id)
                    .ok_or_else(|| Error::NotFound("track", track_id.0.clone()))?;
                let full_path = self.config.music_dir.join(&track.relative_path);
                eprintln!(
                    "[server-cover] source=embedded cover_art_id={} track_id={} path={}",
                    cover_art_id.0,
                    track_id.0,
                    full_path.display()
                );
                let tagged_file =
                    lofty::probe::Probe::open(&full_path).and_then(|probe| probe.read())?;
                let picture = tagged_file
                    .primary_tag()
                    .or_else(|| tagged_file.first_tag())
                    .and_then(select_embedded_picture)
                    .ok_or_else(|| {
                        Error::InvalidRequest(format!(
                            "embedded cover art missing for track {}",
                            track_id.0
                        ))
                    })?;
                let content_type = picture
                    .mime_type()
                    .map(|mime: &lofty::picture::MimeType| mime.as_str().to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                let bytes = picture.data().to_vec();
                eprintln!(
                    "[server-cover] served cover_art_id={} bytes={} content_type={} source=embedded",
                    cover_art_id.0,
                    bytes.len(),
                    content_type
                );
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
                Ok(event) => {
                    if !event_needs_rescan(&event) {
                        continue;
                    }

                    tokio::time::sleep(Duration::from_millis(500)).await;
                    while let Ok(event) = rx.try_recv() {
                        match event {
                            Ok(event) if !event_needs_rescan(&event) => {}
                            Ok(_) => {}
                            Err(error) => eprintln!("watch error: {error}"),
                        }
                    }

                    match scan_music_dir(&music_dir) {
                        Ok(updated) => {
                            if let Ok(mut current) = library.write() {
                                *current = updated;
                            }
                        }
                        Err(error) => eprintln!("failed to refresh library index: {error}"),
                    }
                }
                Err(error) => eprintln!("watch error: {error}"),
            }
        }
    });

    Ok((watcher, watch_task))
}

fn open_state_db(root: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(root.join(STATE_DB_FILE))?;
    conn.execute_batch(
        r#"
        PRAGMA synchronous = NORMAL;

        CREATE TABLE IF NOT EXISTS starred_items (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL,
            starred_unix INTEGER NOT NULL
        );
        "#,
    )?;
    Ok(conn)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn select_embedded_picture(tag: &lofty::tag::Tag) -> Option<&lofty::picture::Picture> {
    tag.get_picture_type(lofty::picture::PictureType::CoverFront)
        .or_else(|| tag.pictures().first())
}

fn event_needs_rescan(event: &Event) -> bool {
    if !event_kind_needs_rescan(event.kind) {
        return false;
    }

    event
        .paths
        .iter()
        .any(|path| is_library_relevant_path(path))
}

fn event_kind_needs_rescan(kind: EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Name(_)) => true,
        EventKind::Modify(ModifyKind::Data(
            DataChange::Any | DataChange::Size | DataChange::Content | DataChange::Other,
        )) => true,
        EventKind::Modify(ModifyKind::Any | ModifyKind::Other) => true,
        EventKind::Any => true,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_)) | EventKind::Other => {
            false
        }
    }
}

fn is_library_relevant_path(path: &std::path::Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    if matches!(
        file_name,
        "iroh-fm.db"
            | "iroh-fm.db-journal"
            | "iroh-fm.db-wal"
            | "iroh-fm.db-shm"
            | "iroh-music-server.db"
            | "iroh-music-server.db-journal"
            | "iroh-music-server.db-wal"
            | "iroh-music-server.db-shm"
    ) {
        return false;
    }

    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "mp3" | "flac" | "ogg" | "opus" | "m4a" | "wav" | "jpg" | "jpeg" | "png" | "webp" | "gif"
            )
    )
}
