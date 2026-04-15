use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::index::{CoverArtSource, LibraryIndex};
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use protocol::{Album, AlbumId, Artist, ArtistId, CoverArtId, Track, TrackId};

pub fn scan_music_dir(root: &Path) -> Result<LibraryIndex> {
    if !root.is_dir() {
        return Err(Error::InvalidMusicDir(root.to_path_buf()));
    }

    let mut builder = LibraryBuilder::default();
    visit_dir(root, root, &mut builder)?;
    Ok(builder.build())
}

fn visit_dir(root: &Path, dir: &Path, builder: &mut LibraryBuilder) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            visit_dir(root, &path, builder)?;
            continue;
        }

        if is_audio_file(&path) {
            builder.add_track(root, &path)?;
        }
    }

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
    artists: BTreeMap<ArtistId, Artist>,
    albums: BTreeMap<AlbumId, Album>,
    tracks: BTreeMap<TrackId, Track>,
    cover_arts: BTreeMap<CoverArtId, CoverArtSource>,
}

impl LibraryBuilder {
    fn add_track(&mut self, root: &Path, path: &Path) -> Result<()> {
        let relative_path = path.strip_prefix(root).map(PathBuf::from).map_err(|_| {
            Error::InvalidRequest(format!("path outside music root: {}", path.display()))
        })?;
        let metadata = fs::metadata(path)?;

        let (fallback_artist, fallback_album) = infer_artist_and_album(&relative_path);
        let file_name = relative_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Unknown Track");
        let (fallback_track_number, fallback_title) = parse_track_name(file_name);
        let tags = read_track_tags(
            path,
            fallback_artist,
            fallback_album,
            fallback_title,
            fallback_track_number,
        );

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
        let cover_art_id = self.cover_art_for(root, path)?;

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
            file_size: metadata.len(),
            modified_at: metadata.modified()?,
            content_type: detect_content_type(path),
        };

        let artist = self.artists.get_mut(&artist_id).expect("artist inserted");
        if !artist.album_ids.contains(&album_id) {
            artist.album_ids.push(album_id.clone());
        }

        let album = self.albums.get_mut(&album_id).expect("album inserted");
        album.track_ids.push(track_id.clone());
        merge_album_track_metadata(album, &track, metadata.len(), cover_art_id);
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
        let Some(sidecar) = find_sidecar_image(parent)? else {
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

    fn build(self) -> LibraryIndex {
        LibraryIndex {
            artists: self.artists,
            albums: self.albums,
            tracks: self.tracks,
            cover_arts: self.cover_arts,
        }
    }
}

#[derive(Debug, Clone)]
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

fn find_sidecar_image(dir: &Path) -> Result<Option<PathBuf>> {
    let mut images = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && detect_image_content_type(&path).is_some() {
            images.push(path);
        }
    }

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

    Ok(images.into_iter().next())
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

    use super::scan_music_dir;

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
