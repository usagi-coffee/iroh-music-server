use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::error::{Error, Result};
use crate::index::{CoverArtSource, LibraryIndex};
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use protocol::{Album, AlbumId, Artist, ArtistId, CoverArtId, Track, TrackId};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

const CACHE_DB_FILE: &str = "iroh-music-server.db";
const SCAN_PROGRESS_BATCH_SIZE: usize = 100;

pub fn scan_music_dir(root: &Path) -> Result<LibraryIndex> {
    let scan_started = Instant::now();
    eprintln!("[scanner] trace scanner_build=post-progress-diagnostics-v1");
    if !root.is_dir() {
        return Err(Error::InvalidMusicDir(root.to_path_buf()));
    }

    let walk_started = Instant::now();
    let library_files = collect_library_files(root)?;
    eprintln!(
        "[scanner] scan start root={} tracks={} cover_dirs={} cache={} walk_ms={}",
        root.display(),
        library_files.audio_files.len(),
        library_files.sidecar_by_dir.len(),
        root.join(CACHE_DB_FILE).display(),
        walk_started.elapsed().as_millis()
    );

    let cache_started = Instant::now();
    let mut cache = ScanCache::open(root)?;
    let mut scanned_tracks = Vec::with_capacity(library_files.audio_files.len());
    let mut seen_audio_paths = Vec::with_capacity(library_files.audio_files.len());
    let mut cache_hits = 0;
    let mut cache_misses = 0;

    for (index, path) in library_files.audio_files.iter().enumerate() {
        let mut scanned_track = ScannedTrack::from_path(root, path)?;
        seen_audio_paths.push(scanned_track.relative_path_string.clone());
        match cache.load_track_tags(
            &scanned_track.relative_path_string,
            scanned_track.file_size,
            scanned_track.modified_unix,
        )? {
            Some(tags) => {
                cache_hits += 1;
                scanned_track.tags = Some(tags);
            }
            None => {
                cache_misses += 1;
                scanned_track.needs_cache_store = true;
            }
        }
        scanned_tracks.push(scanned_track);

        let scanned = index + 1;
        if scanned % SCAN_PROGRESS_BATCH_SIZE == 0 || scanned == library_files.audio_files.len() {
            eprintln!(
                "[scanner] scan progress tracks={}/{} cache_hits={} cache_misses={} last_path={}",
                scanned,
                library_files.audio_files.len(),
                cache_hits,
                cache_misses,
                path.strip_prefix(root).unwrap_or(path).display()
            );
        }
    }
    eprintln!("[scanner] scan loop exited");
    eprintln!(
        "[scanner] cache lookup complete tracks={} cache_hits={} cache_misses={} elapsed_ms={}",
        library_files.audio_files.len(),
        cache_hits,
        cache_misses,
        cache_started.elapsed().as_millis()
    );

    let extraction_started = Instant::now();
    fill_missing_tags_parallel(&mut scanned_tracks, cache_misses);
    if cache_misses > 0 {
        eprintln!(
            "[scanner] tag extraction complete misses={} elapsed_ms={}",
            cache_misses,
            extraction_started.elapsed().as_millis()
        );
    }

    let store_started = Instant::now();
    eprintln!("[scanner] cache store start");
    cache.store_track_tags_batch(&scanned_tracks)?;
    eprintln!(
        "[scanner] cache store complete elapsed_ms={}",
        store_started.elapsed().as_millis()
    );

    let prune_started = Instant::now();
    if cache_misses == 0 {
        eprintln!("[scanner] cache prune skipped reason=warm-cache");
    } else {
        cache.prune_missing(&seen_audio_paths)?;
        eprintln!(
            "[scanner] cache prune complete elapsed_ms={}",
            prune_started.elapsed().as_millis()
        );
    }

    let index_started = Instant::now();
    eprintln!("[scanner] index build dispatch");
    let builder = build_index_parallel(root, &scanned_tracks, library_files.sidecar_by_dir)?;
    eprintln!(
        "[scanner] index build complete elapsed_ms={}",
        index_started.elapsed().as_millis()
    );
    eprintln!(
        "[scanner] scan total elapsed_ms={}",
        scan_started.elapsed().as_millis()
    );
    Ok(builder.build(cache_hits, cache_misses))
}

struct LibraryFiles {
    audio_files: Vec<PathBuf>,
    sidecar_by_dir: BTreeMap<PathBuf, Option<PathBuf>>,
}

fn collect_library_files(root: &Path) -> Result<LibraryFiles> {
    let mut files = LibraryFiles {
        audio_files: Vec::new(),
        sidecar_by_dir: BTreeMap::new(),
    };
    visit_dir(root, root, &mut files)?;
    files.audio_files.sort();
    Ok(files)
}

fn visit_dir(root: &Path, dir: &Path, files: &mut LibraryFiles) -> Result<()> {
    let mut image_files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            visit_dir(root, &path, files)?;
            continue;
        }

        if path.file_name().and_then(|name| name.to_str()) == Some(CACHE_DB_FILE) {
            continue;
        }

        if is_audio_file(&path) {
            files.audio_files.push(path);
        } else if detect_image_content_type(&path).is_some() {
            image_files.push(path);
        }
    }

    files
        .sidecar_by_dir
        .insert(dir.to_path_buf(), choose_sidecar_image(image_files));
    Ok(())
}

fn is_audio_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "mp3" | "flac" | "ogg" | "opus" | "m4a" | "wav")
    )
}

#[derive(Default)]
struct LibraryBuilder {
    artists_by_name: BTreeMap<String, ArtistId>,
    albums_by_key: BTreeMap<(String, String), AlbumId>,
    cover_art_by_path: BTreeMap<PathBuf, CoverArtId>,
    sidecar_by_dir: BTreeMap<PathBuf, Option<PathBuf>>,
    artists: BTreeMap<ArtistId, Artist>,
    albums: BTreeMap<AlbumId, Album>,
    tracks: BTreeMap<TrackId, Track>,
    cover_arts: BTreeMap<CoverArtId, CoverArtSource>,
}

impl LibraryBuilder {
    fn new(sidecar_by_dir: BTreeMap<PathBuf, Option<PathBuf>>) -> Self {
        Self {
            sidecar_by_dir,
            ..Self::default()
        }
    }

    fn add_track(&mut self, root: &Path, scanned: ScannedTrack) -> Result<()> {
        let path = scanned.path;
        let relative_path = scanned.relative_path;
        let tags = scanned.tags.ok_or_else(|| {
            Error::InvalidRequest(format!("missing scanned tags for {}", path.display()))
        })?;

        let album_artist = tags
            .album_artist
            .clone()
            .unwrap_or_else(|| tags.artist.clone());
        let artist_id = self.artist_id_for(&album_artist);
        let album_id = self.album_id_for(&album_artist, &tags.album);
        let track_id = TrackId(slugify(&format!(
            "{}:{}:{}",
            album_artist,
            tags.album,
            relative_path.display()
        )));
        let cover_art_id = self.cover_art_for(root, &path)?;

        let track = Track {
            id: track_id.clone(),
            title: tags.title,
            artist: tags.artist,
            album: tags.album.clone(),
            album_artist: tags.album_artist.clone(),
            track_number: tags.track_number,
            disc_number: tags.disc_number,
            duration_seconds: tags.duration_seconds,
            bitrate: tags.bitrate,
            sample_rate: tags.sample_rate,
            channels: tags.channels,
            codec: tags.codec,
            genres: tags.genres,
            date: tags.date,
            musicbrainz_track_id: tags.musicbrainz_track_id,
            musicbrainz_recording_id: tags.musicbrainz_recording_id,
            musicbrainz_album_id: tags.musicbrainz_album_id.clone(),
            musicbrainz_release_group_id: tags.musicbrainz_release_group_id.clone(),
            cover_art_id: cover_art_id.clone(),
            relative_path,
            file_size: scanned.file_size,
            modified_at: scanned.modified_at,
            content_type: detect_content_type(&path),
        };

        let artist = self.artists.get_mut(&artist_id).expect("artist inserted");
        if !artist.album_ids.contains(&album_id) {
            artist.album_ids.push(album_id.clone());
        }

        let album = self.albums.get_mut(&album_id).expect("album inserted");
        album.track_ids.push(track_id.clone());
        merge_album_track_metadata(album, &track, scanned.file_size, cover_art_id);
        self.tracks.insert(track_id.clone(), track);

        Ok(())
    }

    fn artist_id_for(&mut self, artist_name: &str) -> ArtistId {
        if let Some(existing) = self.artists_by_name.get(artist_name) {
            return existing.clone();
        }

        let id = ArtistId(slugify(artist_name));
        self.artists_by_name
            .insert(artist_name.to_string(), id.clone());
        self.artists.insert(
            id.clone(),
            Artist {
                id: id.clone(),
                name: artist_name.to_string(),
                album_ids: Vec::new(),
            },
        );
        id
    }

    fn album_id_for(&mut self, artist_name: &str, album_name: &str) -> AlbumId {
        let key = (artist_name.to_string(), album_name.to_string());
        if let Some(existing) = self.albums_by_key.get(&key) {
            return existing.clone();
        }

        let id = AlbumId(slugify(&format!("{artist_name}:{album_name}")));
        self.albums_by_key.insert(key, id.clone());
        self.albums.insert(
            id.clone(),
            Album {
                id: id.clone(),
                title: album_name.to_string(),
                artist: artist_name.to_string(),
                album_artist: Some(artist_name.to_string()),
                track_ids: Vec::new(),
                date: None,
                original_date: None,
                year: None,
                genres: Vec::new(),
                labels: Vec::new(),
                catalog_number: None,
                comment: None,
                musicbrainz_album_id: None,
                musicbrainz_release_group_id: None,
                disc_count: None,
                duration_seconds: None,
                size_bytes: 0,
                cover_art_id: None,
                metadata: None,
            },
        );
        id
    }

    fn cover_art_for(&mut self, root: &Path, track_path: &Path) -> Result<Option<CoverArtId>> {
        let Some(parent) = track_path.parent() else {
            return Ok(None);
        };
        let sidecar = self.sidecar_by_dir.get(parent).cloned().flatten();
        let Some(sidecar) = sidecar else {
            return Ok(None);
        };
        let relative_path = sidecar.strip_prefix(root).map(PathBuf::from).map_err(|_| {
            Error::InvalidRequest(format!("path outside music root: {}", sidecar.display()))
        })?;

        if let Some(existing) = self.cover_art_by_path.get(&relative_path) {
            return Ok(Some(existing.clone()));
        }

        let id = CoverArtId(slugify(&format!("cover:{}", relative_path.display())));
        let content_type = detect_image_content_type(&sidecar)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        self.cover_art_by_path
            .insert(relative_path.clone(), id.clone());
        self.cover_arts.insert(
            id.clone(),
            CoverArtSource::Sidecar {
                relative_path,
                content_type,
            },
        );
        Ok(Some(id))
    }

    fn build(self, cache_hits: usize, cache_misses: usize) -> LibraryIndex {
        eprintln!(
            "[scanner] finalizing library artists={} albums={} tracks={} cover_arts={}",
            self.artists.len(),
            self.albums.len(),
            self.tracks.len(),
            self.cover_arts.len()
        );
        eprintln!(
            "[scanner] scan complete artists={} albums={} tracks={} cover_arts={} cache_hits={} cache_misses={}",
            self.artists.len(),
            self.albums.len(),
            self.tracks.len(),
            self.cover_arts.len(),
            cache_hits,
            cache_misses
        );
        LibraryIndex {
            artists: self.artists,
            albums: self.albums,
            tracks: self.tracks,
            cover_arts: self.cover_arts,
        }
    }

    fn merge(&mut self, other: LibraryBuilder) {
        for (path, cover_art_id) in other.cover_art_by_path {
            self.cover_art_by_path.entry(path).or_insert(cover_art_id);
        }
        for (cover_art_id, source) in other.cover_arts {
            self.cover_arts.entry(cover_art_id).or_insert(source);
        }

        for (artist_id, artist) in other.artists {
            let target = self
                .artists
                .entry(artist_id.clone())
                .or_insert_with(|| Artist {
                    id: artist_id.clone(),
                    name: artist.name,
                    album_ids: Vec::new(),
                });
            for album_id in artist.album_ids {
                if !target.album_ids.contains(&album_id) {
                    target.album_ids.push(album_id);
                }
            }
        }

        for (key, album_id) in other.albums_by_key {
            self.albums_by_key.entry(key).or_insert(album_id);
        }
        for (name, artist_id) in other.artists_by_name {
            self.artists_by_name.entry(name).or_insert(artist_id);
        }

        for (album_id, album) in other.albums {
            if !self.albums.contains_key(&album_id) {
                self.albums.insert(album_id, album);
                continue;
            }

            let target = self.albums.get_mut(&album_id).expect("album exists");
            for track_id in album.track_ids {
                let Some(track) = other.tracks.get(&track_id) else {
                    continue;
                };
                if !target.track_ids.contains(&track_id) {
                    target.track_ids.push(track_id.clone());
                    merge_album_track_metadata(
                        target,
                        track,
                        track.file_size,
                        track.cover_art_id.clone(),
                    );
                }
            }
        }

        for (track_id, track) in other.tracks {
            self.tracks.entry(track_id).or_insert(track);
        }
    }
}

fn build_index_parallel(
    root: &Path,
    scanned_tracks: &[ScannedTrack],
    sidecar_by_dir: BTreeMap<PathBuf, Option<PathBuf>>,
) -> Result<LibraryBuilder> {
    if scanned_tracks.is_empty() {
        return Ok(LibraryBuilder::new(sidecar_by_dir));
    }

    let threads = rayon::current_num_threads().max(1);
    let chunk_size = (scanned_tracks.len() / threads).max(256);
    eprintln!(
        "[scanner] index build start tracks={} threads={} chunk_size={}",
        scanned_tracks.len(),
        threads,
        chunk_size
    );

    let built_chunks = AtomicUsize::new(0);
    let partials_started = Instant::now();
    let partials = scanned_tracks
        .par_chunks(chunk_size)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let chunk_started = Instant::now();
            eprintln!(
                "[scanner] index chunk start chunk={} tracks={}",
                chunk_index,
                chunk.len()
            );
            let mut builder = LibraryBuilder::new(sidecar_by_dir.clone());
            for scanned_track in chunk {
                builder.add_track(root, scanned_track.clone())?;
            }
            let completed = built_chunks.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "[scanner] index chunk complete chunk={} tracks={} elapsed_ms={} completed_chunks={}",
                chunk_index,
                chunk.len(),
                chunk_started.elapsed().as_millis(),
                completed
            );
            Ok(builder)
        })
        .collect::<Result<Vec<_>>>()?;
    eprintln!(
        "[scanner] index partial build complete chunks={} elapsed_ms={}",
        partials.len(),
        partials_started.elapsed().as_millis()
    );

    let mut merged = LibraryBuilder::new(sidecar_by_dir);
    let merge_started = Instant::now();
    for (index, partial) in partials.into_iter().enumerate() {
        let partial_artists = partial.artists.len();
        let partial_albums = partial.albums.len();
        let partial_tracks = partial.tracks.len();
        let partial_cover_arts = partial.cover_arts.len();
        let partial_started = Instant::now();
        eprintln!(
            "[scanner] index merge start chunk={} artists={} albums={} tracks={} cover_arts={}",
            index, partial_artists, partial_albums, partial_tracks, partial_cover_arts
        );
        merged.merge(partial);
        eprintln!(
            "[scanner] index merge complete chunk={} elapsed_ms={}",
            index,
            partial_started.elapsed().as_millis()
        );
    }
    eprintln!(
        "[scanner] index merge all complete elapsed_ms={}",
        merge_started.elapsed().as_millis()
    );
    Ok(merged)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrackTags {
    title: String,
    artist: String,
    album: String,
    album_artist: Option<String>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    duration_seconds: Option<u32>,
    bitrate: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
    codec: Option<String>,
    genres: Vec<String>,
    date: Option<String>,
    musicbrainz_track_id: Option<String>,
    musicbrainz_recording_id: Option<String>,
    musicbrainz_album_id: Option<String>,
    musicbrainz_release_group_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ScannedTrack {
    path: PathBuf,
    relative_path: PathBuf,
    relative_path_string: String,
    file_size: u64,
    modified_unix: i64,
    modified_at: std::time::SystemTime,
    fallback_artist: String,
    fallback_album: String,
    fallback_title: String,
    fallback_track_number: Option<u32>,
    needs_cache_store: bool,
    tags: Option<TrackTags>,
}

impl ScannedTrack {
    fn from_path(root: &Path, path: &Path) -> Result<Self> {
        let relative_path = path.strip_prefix(root).map(PathBuf::from).map_err(|_| {
            Error::InvalidRequest(format!("path outside music root: {}", path.display()))
        })?;
        let relative_path_string = relative_path.to_string_lossy().into_owned();
        let metadata = fs::metadata(path)?;
        let modified_unix = modified_unix(&metadata)?;
        let modified_at = metadata.modified()?;
        let (fallback_artist, fallback_album) = infer_artist_and_album(&relative_path);
        let file_name = relative_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Unknown Track");
        let (fallback_track_number, fallback_title) = parse_track_name(file_name);

        Ok(Self {
            path: path.to_path_buf(),
            relative_path,
            relative_path_string,
            file_size: metadata.len(),
            modified_unix,
            modified_at,
            fallback_artist,
            fallback_album,
            fallback_title,
            fallback_track_number,
            needs_cache_store: false,
            tags: None,
        })
    }
}

fn fill_missing_tags_parallel(scanned_tracks: &mut [ScannedTrack], cache_misses: usize) {
    if cache_misses == 0 {
        return;
    }

    eprintln!(
        "[scanner] tag extraction start misses={} threads={}",
        cache_misses,
        rayon::current_num_threads()
    );
    let progress = AtomicUsize::new(0);
    scanned_tracks
        .par_iter_mut()
        .filter(|track| track.tags.is_none())
        .for_each(|track| {
            let tags = read_track_tags(
                &track.path,
                track.fallback_artist.clone(),
                track.fallback_album.clone(),
                track.fallback_title.clone(),
                track.fallback_track_number,
            );
            track.tags = Some(tags);

            let parsed = progress.fetch_add(1, Ordering::Relaxed) + 1;
            if parsed % SCAN_PROGRESS_BATCH_SIZE == 0 || parsed == cache_misses {
                eprintln!(
                    "[scanner] tag extraction progress parsed={}/{}",
                    parsed, cache_misses
                );
            }
        });
}

struct ScanCache {
    conn: Connection,
}

impl Default for ScanCache {
    fn default() -> Self {
        Self {
            conn: Connection::open_in_memory().expect("in-memory sqlite cache"),
        }
    }
}

impl ScanCache {
    fn open(root: &Path) -> Result<Self> {
        let conn = Connection::open(root.join(CACHE_DB_FILE))?;
        let cache = Self { conn };
        cache.migrate()?;
        Ok(cache)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS track_tags (
                relative_path TEXT PRIMARY KEY NOT NULL,
                file_size INTEGER NOT NULL,
                modified_unix INTEGER NOT NULL,
                tags_json TEXT NOT NULL,
                updated_unix INTEGER NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    fn load_track_tags(
        &self,
        relative_path: &str,
        file_size: u64,
        modified_unix: i64,
    ) -> Result<Option<TrackTags>> {
        let row = self
            .conn
            .query_row(
                "SELECT file_size, modified_unix, tags_json FROM track_tags WHERE relative_path = ?1",
                params![relative_path],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;

        let Some((cached_size, cached_modified, tags_json)) = row else {
            return Ok(None);
        };

        if cached_size != i64::try_from(file_size).unwrap_or(i64::MAX)
            || cached_modified != modified_unix
        {
            return Ok(None);
        }

        Ok(Some(serde_json::from_str(&tags_json)?))
    }

    fn store_track_tags_batch(&mut self, scanned_tracks: &[ScannedTrack]) -> Result<()> {
        let tracks_to_store = scanned_tracks
            .iter()
            .filter(|track| track.needs_cache_store)
            .collect::<Vec<_>>();
        if tracks_to_store.is_empty() {
            eprintln!("[scanner] cache store skipped rows=0");
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO track_tags (relative_path, file_size, modified_unix, tags_json, updated_unix)
                VALUES (?1, ?2, ?3, ?4, unixepoch())
                ON CONFLICT(relative_path) DO UPDATE SET
                    file_size = excluded.file_size,
                    modified_unix = excluded.modified_unix,
                    tags_json = excluded.tags_json,
                    updated_unix = excluded.updated_unix
                "#,
            )?;
            for (index, scanned_track) in tracks_to_store.iter().enumerate() {
                let Some(tags) = &scanned_track.tags else {
                    continue;
                };
                stmt.execute(params![
                    scanned_track.relative_path_string.as_str(),
                    i64::try_from(scanned_track.file_size).unwrap_or(i64::MAX),
                    scanned_track.modified_unix,
                    serde_json::to_string(tags)?,
                ])?;
                let stored = index + 1;
                if stored % SCAN_PROGRESS_BATCH_SIZE == 0 || stored == tracks_to_store.len() {
                    eprintln!(
                        "[scanner] cache store progress rows={}/{}",
                        stored,
                        tracks_to_store.len()
                    );
                }
            }
        }
        tx.commit()?;
        eprintln!("[scanner] cache store rows={}", tracks_to_store.len());
        Ok(())
    }

    fn prune_missing(&mut self, seen_audio_paths: &[String]) -> Result<()> {
        eprintln!(
            "[scanner] cache prune start seen={}",
            seen_audio_paths.len()
        );
        let tx = self.conn.transaction()?;
        tx.execute("CREATE TEMP TABLE IF NOT EXISTS seen_audio_paths (relative_path TEXT PRIMARY KEY NOT NULL)", [])?;
        tx.execute("DELETE FROM seen_audio_paths", [])?;
        {
            let mut stmt =
                tx.prepare("INSERT OR IGNORE INTO seen_audio_paths (relative_path) VALUES (?1)")?;
            for (index, relative_path) in seen_audio_paths.iter().enumerate() {
                stmt.execute(params![relative_path])?;
                let inserted = index + 1;
                if inserted % SCAN_PROGRESS_BATCH_SIZE == 0 || inserted == seen_audio_paths.len() {
                    eprintln!(
                        "[scanner] cache prune seen progress rows={}/{}",
                        inserted,
                        seen_audio_paths.len()
                    );
                }
            }
        }
        eprintln!("[scanner] cache prune delete stale start");
        let removed = tx.execute(
            "DELETE FROM track_tags WHERE relative_path NOT IN (SELECT relative_path FROM seen_audio_paths)",
            [],
        )?;
        tx.commit()?;
        if removed > 0 {
            eprintln!("[scanner] pruned stale cache rows={removed}");
        }
        Ok(())
    }
}

fn read_track_tags(
    path: &Path,
    fallback_artist: String,
    fallback_album: String,
    fallback_title: String,
    fallback_track_number: Option<u32>,
) -> TrackTags {
    let mut tags = TrackTags {
        title: fallback_title,
        artist: fallback_artist,
        album: fallback_album,
        album_artist: None,
        track_number: fallback_track_number,
        disc_number: None,
        duration_seconds: None,
        bitrate: None,
        sample_rate: None,
        channels: None,
        codec: path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        genres: Vec::new(),
        date: None,
        musicbrainz_track_id: None,
        musicbrainz_recording_id: None,
        musicbrainz_album_id: None,
        musicbrainz_release_group_id: None,
    };

    let Ok(tagged_file) = Probe::open(path).and_then(|probe| probe.read()) else {
        return tags;
    };

    let properties = tagged_file.properties();
    let duration = properties.duration().as_secs();
    tags.duration_seconds = u32::try_from(duration).ok();
    tags.bitrate = properties.audio_bitrate();
    tags.sample_rate = properties.sample_rate();
    tags.channels = properties.channels().map(u32::from);

    let Some(tag) = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())
    else {
        return tags;
    };

    if let Some(title) = tag.title().filter(|value| !value.trim().is_empty()) {
        tags.title = title.trim().to_string();
    }
    if let Some(artist) = tag.artist().filter(|value| !value.trim().is_empty()) {
        tags.artist = artist.trim().to_string();
    }
    if let Some(album) = tag.album().filter(|value| !value.trim().is_empty()) {
        tags.album = album.trim().to_string();
    }
    tags.album_artist = tag
        .get_string(lofty::tag::ItemKey::AlbumArtist)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    tags.track_number = tag.track().or(tags.track_number);
    tags.disc_number = tag.disk();
    tags.genres = tag
        .genre()
        .map(|genre| split_multi_value(&genre))
        .unwrap_or_default();
    tags.date = tag
        .get_string(lofty::tag::ItemKey::RecordingDate)
        .or_else(|| tag.get_string(lofty::tag::ItemKey::Year))
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    tags.musicbrainz_track_id = tag
        .get_string(lofty::tag::ItemKey::MusicBrainzTrackId)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    tags.musicbrainz_recording_id = tag
        .get_string(lofty::tag::ItemKey::MusicBrainzRecordingId)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    tags.musicbrainz_album_id = tag
        .get_string(lofty::tag::ItemKey::MusicBrainzReleaseId)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    tags.musicbrainz_release_group_id = tag
        .get_string(lofty::tag::ItemKey::MusicBrainzReleaseGroupId)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());

    tags
}

fn merge_album_track_metadata(
    album: &mut Album,
    track: &Track,
    file_size: u64,
    cover_art_id: Option<CoverArtId>,
) {
    album.size_bytes = album.size_bytes.saturating_add(file_size);
    if let Some(duration) = track.duration_seconds {
        album.duration_seconds = Some(album.duration_seconds.unwrap_or(0).saturating_add(duration));
    }
    if let Some(disc_number) = track.disc_number {
        album.disc_count = Some(album.disc_count.unwrap_or(0).max(disc_number));
    }
    if album.album_artist.is_none() {
        album.album_artist = track
            .album_artist
            .clone()
            .or_else(|| Some(track.artist.clone()));
    }
    if album.date.is_none() {
        album.date = track.date.clone();
    }
    if album.year.is_none() {
        album.year = track.date.as_deref().and_then(extract_year);
    }
    if album.musicbrainz_album_id.is_none() {
        album.musicbrainz_album_id = track.musicbrainz_album_id.clone();
    }
    if album.musicbrainz_release_group_id.is_none() {
        album.musicbrainz_release_group_id = track.musicbrainz_release_group_id.clone();
    }
    if album.cover_art_id.is_none() {
        album.cover_art_id = cover_art_id;
    }
    for genre in &track.genres {
        if !album
            .genres
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(genre))
        {
            album.genres.push(genre.clone());
        }
    }
}

fn infer_artist_and_album(relative_path: &Path) -> (String, String) {
    let parents: Vec<String> = relative_path
        .parent()
        .map(|parent| {
            parent
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();

    match parents.as_slice() {
        [artist, album, ..] => (artist.clone(), album.clone()),
        [album_dir] => split_artist_album_dir(album_dir),
        [] => ("Unknown Artist".to_string(), "Unknown Album".to_string()),
    }
}

fn split_artist_album_dir(dir_name: &str) -> (String, String) {
    if let Some((artist, album)) = dir_name.split_once(" - ") {
        return (artist.trim().to_string(), album.trim().to_string());
    }

    ("Unknown Artist".to_string(), dir_name.to_string())
}

fn parse_track_name(name: &str) -> (Option<u32>, String) {
    let mut segments = name.splitn(2, " - ");
    let first = segments.next().unwrap_or(name);
    let second = segments.next();

    if let Some(title) = second {
        let track_number = first.trim().parse::<u32>().ok();
        return (track_number, title.trim().to_string());
    }

    (None, first.trim().to_string())
}

fn choose_sidecar_image(mut images: Vec<PathBuf>) -> Option<PathBuf> {
    images.sort_by_key(|path| {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let priority = if file_name.starts_with("cover.") {
            0
        } else if file_name.starts_with("folder.") {
            1
        } else if file_name.starts_with("front.") {
            2
        } else {
            3
        };
        (priority, file_name)
    });

    images.into_iter().next()
}

fn modified_unix(metadata: &fs::Metadata) -> Result<i64> {
    let modified = metadata.modified()?;
    let duration = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            Error::InvalidRequest(format!("file modified before unix epoch: {error}"))
        })?;
    Ok(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

fn detect_content_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
    {
        Some(ext) if ext == "mp3" => "audio/mpeg".to_string(),
        Some(ext) if ext == "flac" => "audio/flac".to_string(),
        Some(ext) if ext == "ogg" => "audio/ogg".to_string(),
        Some(ext) if ext == "opus" => "audio/ogg".to_string(),
        Some(ext) if ext == "m4a" => "audio/mp4".to_string(),
        Some(ext) if ext == "wav" => "audio/wav".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn detect_image_content_type(path: &Path) -> Option<String> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
    {
        Some(ext) if ext == "jpg" || ext == "jpeg" => Some("image/jpeg".to_string()),
        Some(ext) if ext == "png" => Some("image/png".to_string()),
        Some(ext) if ext == "webp" => Some("image/webp".to_string()),
        Some(ext) if ext == "gif" => Some("image/gif".to_string()),
        _ => None,
    }
}

fn split_multi_value(value: &str) -> Vec<String> {
    value
        .split([';', ','])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn extract_year(value: &str) -> Option<i32> {
    let year = value
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .take(4)
        .collect::<String>();
    (year.len() == 4).then(|| year.parse().ok()).flatten()
}

fn slugify(input: &str) -> String {
    let mut slug = String::new();

    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }

    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{CACHE_DB_FILE, scan_music_dir};

    #[test]
    fn single_artist_album_folder_scans_as_one_album_with_cover_art() {
        let root = unique_temp_dir();
        let album_dir = root.join("Yunomi - Oedo Controller");
        fs::create_dir_all(&album_dir).expect("create fixture album dir");
        fs::write(
            album_dir.join("01 - Oedo Controller (feat. TORIENA).flac"),
            [],
        )
        .expect("write track 1");
        fs::write(
            album_dir.join("02 - Wakusei Rabbit (feat. TORIENA).flac"),
            [],
        )
        .expect("write track 2");
        fs::write(album_dir.join("cover.jpg"), []).expect("write cover");

        let library = scan_music_dir(&root).expect("scan fixture library");

        assert_eq!(library.artist_count(), 1);
        assert_eq!(library.album_count(), 1);
        assert_eq!(library.track_count(), 2);
        assert_eq!(library.cover_arts.len(), 1);

        let album = library.albums.values().next().expect("album");
        assert_eq!(album.artist, "Yunomi");
        assert_eq!(album.title, "Oedo Controller");
        assert!(album.cover_art_id.is_some());
        assert!(root.join(CACHE_DB_FILE).is_file());

        let cached_library = scan_music_dir(&root).expect("scan fixture library from cache");
        assert_eq!(cached_library.artist_count(), 1);
        assert_eq!(cached_library.album_count(), 1);
        assert_eq!(cached_library.track_count(), 2);

        fs::remove_dir_all(root).expect("remove fixture library");
    }

    fn unique_temp_dir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "irohsonic-scanner-test-{}-{nanos}",
            std::process::id()
        ))
    }
}
