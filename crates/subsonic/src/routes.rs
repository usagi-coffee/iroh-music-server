use client::{Error, Result};
use protocol::{BackendResponse, ResolvedId, StreamDescriptor};
use serde_json::json;

use crate::auth::Credentials;
use crate::backend::Backend;
use crate::config::SubsonicConfig;
use crate::models::RequestContext;
use crate::response::SubsonicResponse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseFormat {
    Xml,
    Json,
}

pub async fn handle_request(
    config: &SubsonicConfig,
    backend: &impl Backend,
    request: RequestContext,
) -> Result<SubsonicResponse> {
    let format = response_format(&request);
    let credentials = Credentials::from_request(&request);
    if !credentials.matches(config) {
        return Ok(error_response(format, "authentication failed"));
    }

    match request.path.as_str() {
        "/rest/ping" => Ok(ok_message(format, "pong")),
        "/rest/getLicense" => Ok(map_license(format)),
        "/rest/getMusicFolders" => Ok(map_music_folders(format)),
        "/rest/getArtists" => map_artists(format, backend.artists().await?),
        "/rest/getIndexes" => map_indexes(format, backend.artists().await?),
        "/rest/getAlbumList" => map_album_list(format, &request, backend.albums().await?, false),
        "/rest/getAlbumList2" => map_album_list(format, &request, backend.albums().await?, true),
        "/rest/getMusicDirectory" => {
            let id = query_value(&request, "id").unwrap_or("root");
            map_directory(format, backend, id).await
        }
        "/rest/getAlbum" => {
            let album_id = query_value(&request, "id").unwrap_or_default();
            map_album(format, backend, album_id).await
        }
        "/rest/getSong" => {
            let track_id = query_value(&request, "id").unwrap_or_default();
            map_track(format, backend.track(track_id).await?)
        }
        "/rest/getCoverArt" => {
            let cover_art_id = query_value(&request, "id").unwrap_or_default();
            eprintln!("[subsonic-cover] request getCoverArt id={}", cover_art_id);
            map_cover_art(backend.cover_art(cover_art_id).await?)
        }
        "/rest/getStarred2" => map_starred2(format, backend.starred().await?),
        "/rest/search3" => {
            let term = query_value(&request, "query").unwrap_or_default();
            map_search(format, backend.search(term, 20).await?)
        }
        "/rest/scrobble" => Ok(empty_ok(format)),
        "/rest/star" => {
            let id = query_value(&request, "id").unwrap_or_default();
            map_empty(backend.set_starred(id, true).await?, format)
        }
        "/rest/unstar" => {
            let id = query_value(&request, "id").unwrap_or_default();
            map_empty(backend.set_starred(id, false).await?, format)
        }
        "/rest/stream" => {
            let track_id = query_value(&request, "id").unwrap_or_default();
            map_stream(backend.stream(track_id).await?)
        }
        _ => Ok(error_response(
            format,
            &format!("unsupported route: {}", request.path),
        )),
    }
}

async fn map_directory(
    format: ResponseFormat,
    backend: &impl Backend,
    id: &str,
) -> Result<SubsonicResponse> {
    if id == "root" {
        let BackendResponse::Artists(artists) = backend.artists().await? else {
            return Err(Error::InvalidRequest(
                "backend returned unexpected response for getMusicDirectory root".to_string(),
            ));
        };
        let children = artists
            .into_iter()
            .map(|artist| DirectoryChild {
                id: artist.id.0,
                title: artist.name,
                is_dir: true,
                artist: None,
                album: None,
                content_type: None,
                cover_art: None,
                track: None,
                disc_number: None,
                duration: None,
                size: None,
                parent: Some("root".to_string()),
            })
            .collect::<Vec<_>>();
        return Ok(render_directory(format, "root", "Music", None, children));
    }

    let resolved = match backend.resolve_id(id).await {
        Ok(BackendResponse::ResolvedId(resolved)) => resolved,
        Ok(_) => {
            return Err(Error::InvalidRequest(
                "backend returned unexpected response for ResolveId".to_string(),
            ));
        }
        Err(_) => return Ok(error_response(format, "unknown directory id")),
    };

    match resolved {
        ResolvedId::Album(album) => {
            let BackendResponse::Tracks(tracks) = backend.album_tracks(id).await? else {
                return Err(Error::InvalidRequest(
                    "backend returned unexpected response for album tracks".to_string(),
                ));
            };
            let children = tracks
                .into_iter()
                .map(|track| DirectoryChild {
                    id: track.id.0.clone(),
                    title: track.title,
                    is_dir: false,
                    artist: Some(track.artist),
                    album: Some(track.album),
                    content_type: Some(track.content_type),
                    cover_art: track.cover_art_id.map(|cover_art_id| cover_art_id.0),
                    track: track.track_number,
                    disc_number: track.disc_number,
                    duration: track.duration_seconds,
                    size: Some(track.file_size),
                    parent: Some(id.to_string()),
                })
                .collect::<Vec<_>>();
            Ok(render_directory(format, id, &album.title, None, children))
        }
        ResolvedId::Artist(artist) => {
            let mut children = Vec::new();
            for album_id in &artist.album_ids {
                let BackendResponse::Album(album) = backend.album(&album_id.0).await? else {
                    return Err(Error::InvalidRequest(
                        "backend returned unexpected response for artist album".to_string(),
                    ));
                };
                children.push(DirectoryChild {
                    id: album.id.0,
                    title: album.title.clone(),
                    is_dir: true,
                    artist: Some(album.artist.clone()),
                    album: Some(album.title),
                    content_type: None,
                    cover_art: album.cover_art_id.map(|cover_art_id| cover_art_id.0),
                    track: None,
                    disc_number: None,
                    duration: album.duration_seconds,
                    size: Some(album.size_bytes),
                    parent: Some(id.to_string()),
                });
            }
            Ok(render_directory(
                format,
                id,
                &artist.name,
                Some("root"),
                children,
            ))
        }
        ResolvedId::Track(track) => {
            let children = vec![DirectoryChild {
                id: track.id.0.clone(),
                title: track.title.clone(),
                is_dir: false,
                artist: Some(track.artist.clone()),
                album: Some(track.album.clone()),
                content_type: Some(track.content_type.clone()),
                cover_art: track.cover_art_id.map(|cover_art_id| cover_art_id.0),
                track: track.track_number,
                disc_number: track.disc_number,
                duration: track.duration_seconds,
                size: Some(track.file_size),
                parent: None,
            }];
            Ok(render_directory(format, id, &track.title, None, children))
        }
    }
}

fn map_artists(format: ResponseFormat, response: BackendResponse) -> Result<SubsonicResponse> {
    let BackendResponse::Artists(artists) = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getArtists".to_string(),
        ));
    };

    match format {
        ResponseFormat::Xml => {
            let mut body = String::from("<artists>");
            for artist in artists {
                body.push_str(&format!(
                    "<index name=\"{name}\"><artist id=\"{id}\" name=\"{name}\" /></index>",
                    id = xml_escape(&artist.id.0),
                    name = xml_escape(&artist.name)
                ));
            }
            body.push_str("</artists>");
            Ok(SubsonicResponse::Xml(wrap_xml(&body)))
        }
        ResponseFormat::Json => {
            let indexes = artists
                .into_iter()
                .map(|artist| {
                    json!({
                        "name": artist.name,
                        "artist": [
                            { "id": artist.id.0, "name": artist.name }
                        ]
                    })
                })
                .collect::<Vec<_>>();
            Ok(SubsonicResponse::Json(wrap_json(
                json!({ "artists": { "index": indexes } }),
            )))
        }
    }
}

fn map_indexes(format: ResponseFormat, response: BackendResponse) -> Result<SubsonicResponse> {
    let BackendResponse::Artists(artists) = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getIndexes".to_string(),
        ));
    };

    let mut grouped: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    for artist in artists {
        let index_name = artist
            .name
            .chars()
            .next()
            .map(|ch| ch.to_ascii_uppercase().to_string())
            .unwrap_or_else(|| "#".to_string());
        grouped
            .entry(index_name)
            .or_default()
            .push((artist.id.0, artist.name));
    }

    match format {
        ResponseFormat::Xml => {
            let mut body = String::from("<indexes lastModified=\"0\" ignoredArticles=\"\">");
            for (name, artists) in grouped {
                body.push_str(&format!("<index name=\"{}\">", xml_escape(&name)));
                for (id, artist_name) in artists {
                    body.push_str(&format!(
                        "<artist id=\"{}\" name=\"{}\" />",
                        xml_escape(&id),
                        xml_escape(&artist_name)
                    ));
                }
                body.push_str("</index>");
            }
            body.push_str("</indexes>");
            Ok(SubsonicResponse::Xml(wrap_xml(&body)))
        }
        ResponseFormat::Json => {
            let indexes = grouped
                .into_iter()
                .map(|(name, artists)| {
                    json!({
                        "name": name,
                        "artist": artists.into_iter().map(|(id, artist_name)| {
                            json!({ "id": id, "name": artist_name })
                        }).collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            Ok(SubsonicResponse::Json(wrap_json(json!({
                "indexes": {
                    "lastModified": 0,
                    "ignoredArticles": "",
                    "index": indexes
                }
            }))))
        }
    }
}

fn map_album_list(
    format: ResponseFormat,
    request: &RequestContext,
    response: BackendResponse,
    use_v2_name: bool,
) -> Result<SubsonicResponse> {
    let BackendResponse::Albums(albums) = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getAlbumList".to_string(),
        ));
    };

    let list_name = if use_v2_name { "albumList2" } else { "albumList" };
    let list_size = query_value(request, "size")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10);
    let offset = query_value(request, "offset")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let list_type = query_value(request, "type").unwrap_or("alphabeticalByName");

    let mut albums = albums;
    sort_albums(&mut albums, list_type);

    let albums = albums
        .into_iter()
        .skip(offset)
        .take(list_size)
        .collect::<Vec<_>>();

    match format {
        ResponseFormat::Xml => {
            let mut body = format!("<{}>", list_name);
            for album in albums {
                body.push_str(&render_album_xml(&album));
            }
            body.push_str(&format!("</{}>", list_name));
            Ok(SubsonicResponse::Xml(wrap_xml(&body)))
        }
        ResponseFormat::Json => Ok(SubsonicResponse::Json(wrap_json(json!({
            list_name: {
                "album": albums.into_iter().map(render_album_json).collect::<Vec<_>>()
            }
        })))),
    }
}

fn map_track(format: ResponseFormat, response: BackendResponse) -> Result<SubsonicResponse> {
    let BackendResponse::Track(track) = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getSong".to_string(),
        ));
    };
    eprintln!(
        "[subsonic-cover] song id={} album={} cover_art_id={}",
        track.id.0,
        track.album,
        track
            .cover_art_id
            .as_ref()
            .map(|id| id.0.as_str())
            .unwrap_or("<none>")
    );

    match format {
        ResponseFormat::Xml => Ok(SubsonicResponse::Xml(wrap_xml(&format!(
            "<song id=\"{}\" title=\"{}\" artist=\"{}\" album=\"{}\" contentType=\"{}\"{}{}{}{} />",
            xml_escape(&track.id.0),
            xml_escape(&track.title),
            xml_escape(&track.artist),
            xml_escape(&track.album),
            xml_escape(&track.content_type),
            optional_attr(
                "coverArt",
                track.cover_art_id.as_ref().map(|id| id.0.as_str())
            ),
            optional_number_attr("track", track.track_number),
            optional_number_attr("discNumber", track.disc_number),
            optional_number_attr("duration", track.duration_seconds)
        )))),
        ResponseFormat::Json => Ok(SubsonicResponse::Json(wrap_json(json!({
            "song": {
                "id": track.id.0,
                "title": track.title,
                "artist": track.artist,
                "album": track.album,
                "albumArtist": track.album_artist,
                "contentType": track.content_type,
                "coverArt": track.cover_art_id.map(|id| id.0),
                "track": track.track_number,
                "discNumber": track.disc_number,
                "duration": track.duration_seconds,
                "bitRate": track.bitrate,
                "size": track.file_size,
                "suffix": track.suffix,
                "genre": track.genres.first().cloned()
            }
        })))),
    }
}

async fn map_album(
    format: ResponseFormat,
    backend: &impl Backend,
    album_id: &str,
) -> Result<SubsonicResponse> {
    let BackendResponse::Album(album) = backend.album(album_id).await? else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getAlbum".to_string(),
        ));
    };
    let BackendResponse::Tracks(tracks) = backend.album_tracks(album_id).await? else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getAlbum tracks".to_string(),
        ));
    };
    eprintln!(
        "[subsonic-cover] album id={} title={} cover_art_id={}",
        album.id.0,
        album.title,
        album
            .cover_art_id
            .as_ref()
            .map(|id| id.0.as_str())
            .unwrap_or("<none>")
    );

    match format {
        ResponseFormat::Xml => {
            let mut body = render_album_xml_open(&album);
            for track in tracks {
                body.push_str(&render_song_xml(&track, Some(&album.id.0)));
            }
            body.push_str("</album>");
            Ok(SubsonicResponse::Xml(wrap_xml(&body)))
        }
        ResponseFormat::Json => Ok(SubsonicResponse::Json(wrap_json(json!({
            "album": {
                "id": album.id.0,
                "name": album.title,
                "artist": album.artist,
                "albumArtist": album.album_artist,
                "songCount": album.track_ids.len(),
                "genre": album.genres.first().cloned(),
                "year": album.year.map(|year| year.to_string()),
                "coverArt": album.cover_art_id.map(|id| id.0),
                "duration": album.duration_seconds,
                "size": album.size_bytes,
                "song": tracks
                    .into_iter()
                    .map(|track| render_song_json(&track, Some(&album.id.0)))
                    .collect::<Vec<_>>()
            }
        })))),
    }
}

fn map_search(format: ResponseFormat, response: BackendResponse) -> Result<SubsonicResponse> {
    let BackendResponse::SearchResults {
        artists,
        albums,
        tracks,
    } = response
    else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for search3".to_string(),
        ));
    };

    match format {
        ResponseFormat::Xml => {
            let mut body = String::from("<searchResult3>");
            for artist in artists {
                body.push_str(&format!(
                    "<artist id=\"{}\" name=\"{}\" />",
                    xml_escape(&artist.id.0),
                    xml_escape(&artist.name)
                ));
            }
            for album in albums {
                body.push_str(&format!(
                    "<album id=\"{}\" name=\"{}\" artist=\"{}\"{} />",
                    xml_escape(&album.id.0),
                    xml_escape(&album.title),
                    xml_escape(&album.artist),
                    optional_attr(
                        "coverArt",
                        album.cover_art_id.as_ref().map(|id| id.0.as_str())
                    )
                ));
            }
            for track in tracks {
                body.push_str(&format!(
                    "<song id=\"{}\" title=\"{}\" artist=\"{}\" album=\"{}\"{} />",
                    xml_escape(&track.id.0),
                    xml_escape(&track.title),
                    xml_escape(&track.artist),
                    xml_escape(&track.album),
                    optional_attr(
                        "coverArt",
                        track.cover_art_id.as_ref().map(|id| id.0.as_str())
                    )
                ));
            }
            body.push_str("</searchResult3>");
            Ok(SubsonicResponse::Xml(wrap_xml(&body)))
        }
        ResponseFormat::Json => Ok(SubsonicResponse::Json(wrap_json(json!({
            "searchResult3": {
                "artist": artists.into_iter().map(|artist| json!({
                    "id": artist.id.0,
                    "name": artist.name
                })).collect::<Vec<_>>(),
                "album": albums.into_iter().map(|album| json!({
                    "id": album.id.0,
                    "name": album.title,
                    "artist": album.artist,
                    "coverArt": album.cover_art_id.map(|id| id.0)
                })).collect::<Vec<_>>(),
                "song": tracks.into_iter().map(|track| json!({
                    "id": track.id.0,
                    "title": track.title,
                    "artist": track.artist,
                    "album": track.album,
                    "coverArt": track.cover_art_id.map(|id| id.0)
                })).collect::<Vec<_>>()
            }
        })))),
    }
}

fn map_starred2(format: ResponseFormat, response: BackendResponse) -> Result<SubsonicResponse> {
    let BackendResponse::Starred(starred) = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getStarred2".to_string(),
        ));
    };

    match format {
        ResponseFormat::Xml => {
            let mut body = String::from("<starred2>");
            for artist in starred.artists {
                body.push_str(&format!(
                    "<artist id=\"{}\" name=\"{}\" />",
                    xml_escape(&artist.id.0),
                    xml_escape(&artist.name)
                ));
            }
            for album in starred.albums {
                body.push_str(&format!(
                    "<album id=\"{}\" name=\"{}\" artist=\"{}\"{} />",
                    xml_escape(&album.id.0),
                    xml_escape(&album.title),
                    xml_escape(&album.artist),
                    optional_attr(
                        "coverArt",
                        album.cover_art_id.as_ref().map(|id| id.0.as_str())
                    )
                ));
            }
            for track in starred.tracks {
                body.push_str(&format!(
                    "<song id=\"{}\" title=\"{}\" artist=\"{}\" album=\"{}\"{} />",
                    xml_escape(&track.id.0),
                    xml_escape(&track.title),
                    xml_escape(&track.artist),
                    xml_escape(&track.album),
                    optional_attr(
                        "coverArt",
                        track.cover_art_id.as_ref().map(|id| id.0.as_str())
                    )
                ));
            }
            body.push_str("</starred2>");
            Ok(SubsonicResponse::Xml(wrap_xml(&body)))
        }
        ResponseFormat::Json => Ok(SubsonicResponse::Json(wrap_json(json!({
            "starred2": {
                "artist": starred.artists.into_iter().map(|artist| json!({
                    "id": artist.id.0,
                    "name": artist.name
                })).collect::<Vec<_>>(),
                "album": starred.albums.into_iter().map(|album| json!({
                    "id": album.id.0,
                    "name": album.title,
                    "artist": album.artist,
                    "coverArt": album.cover_art_id.map(|id| id.0)
                })).collect::<Vec<_>>(),
                "song": starred.tracks.into_iter().map(|track| json!({
                    "id": track.id.0,
                    "title": track.title,
                    "artist": track.artist,
                    "album": track.album,
                    "coverArt": track.cover_art_id.map(|id| id.0)
                })).collect::<Vec<_>>()
            }
        })))),
    }
}

fn map_stream(
    (stream, recv): (StreamDescriptor, iroh::endpoint::RecvStream),
) -> Result<SubsonicResponse> {
    Ok(SubsonicResponse::Stream {
        content_type: stream.content_type,
        content_length: Some(stream.file_size),
        stream: recv,
    })
}

fn map_cover_art(response: BackendResponse) -> Result<SubsonicResponse> {
    let BackendResponse::CoverArt(cover_art) = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getCoverArt".to_string(),
        ));
    };
    eprintln!(
        "[subsonic-cover] response getCoverArt id={} bytes={} content_type={}",
        cover_art.cover_art_id.0,
        cover_art.bytes.len(),
        cover_art.content_type
    );

    Ok(SubsonicResponse::Binary {
        content_type: cover_art.content_type,
        bytes: cover_art.bytes,
    })
}

fn map_empty(response: BackendResponse, format: ResponseFormat) -> Result<SubsonicResponse> {
    let BackendResponse::Empty = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for empty result".to_string(),
        ));
    };
    Ok(empty_ok(format))
}

fn map_license(format: ResponseFormat) -> SubsonicResponse {
    match format {
        ResponseFormat::Xml => SubsonicResponse::Xml(wrap_xml(
            "<license valid=\"true\" email=\"\" licenseExpires=\"\" />",
        )),
        ResponseFormat::Json => SubsonicResponse::Json(wrap_json(json!({
            "license": {
                "valid": true,
                "email": "",
                "licenseExpires": ""
            }
        }))),
    }
}

fn map_music_folders(format: ResponseFormat) -> SubsonicResponse {
    match format {
        ResponseFormat::Xml => SubsonicResponse::Xml(wrap_xml(
            "<musicFolders><musicFolder id=\"1\" name=\"Music\" /></musicFolders>",
        )),
        ResponseFormat::Json => SubsonicResponse::Json(wrap_json(json!({
            "musicFolders": {
                "musicFolder": [
                    { "id": 1, "name": "Music" }
                ]
            }
        }))),
    }
}

#[derive(Debug, Clone)]
struct DirectoryChild {
    id: String,
    title: String,
    is_dir: bool,
    artist: Option<String>,
    album: Option<String>,
    content_type: Option<String>,
    cover_art: Option<String>,
    track: Option<u32>,
    disc_number: Option<u32>,
    duration: Option<u32>,
    size: Option<u64>,
    parent: Option<String>,
}

fn render_directory(
    format: ResponseFormat,
    id: &str,
    name: &str,
    parent: Option<&str>,
    children: Vec<DirectoryChild>,
) -> SubsonicResponse {
    match format {
        ResponseFormat::Xml => {
            let mut body = format!(
                "<directory id=\"{}\" name=\"{}\"{}>",
                xml_escape(id),
                xml_escape(name),
                parent
                    .map(|parent| format!(" parent=\"{}\"", xml_escape(parent)))
                    .unwrap_or_default()
            );
            for child in children {
                body.push_str(&format!(
                    "<child id=\"{}\" title=\"{}\" isDir=\"{}\"{}{}{}{}{}{}{}{}{} />",
                    xml_escape(&child.id),
                    xml_escape(&child.title),
                    child.is_dir,
                    optional_attr("artist", child.artist.as_deref()),
                    optional_attr("album", child.album.as_deref()),
                    optional_attr("contentType", child.content_type.as_deref()),
                    optional_attr("coverArt", child.cover_art.as_deref()),
                    optional_number_attr("track", child.track),
                    optional_number_attr("discNumber", child.disc_number),
                    optional_number_attr("duration", child.duration),
                    optional_number_attr("size", child.size),
                    optional_attr("parent", child.parent.as_deref()),
                ));
            }
            body.push_str("</directory>");
            SubsonicResponse::Xml(wrap_xml(&body))
        }
        ResponseFormat::Json => {
            let children = children
                .into_iter()
                .map(|child| {
                    let mut value = serde_json::Map::new();
                    value.insert("id".to_string(), json!(child.id));
                    value.insert("title".to_string(), json!(child.title));
                    value.insert("isDir".to_string(), json!(child.is_dir));
                    if let Some(artist) = child.artist {
                        value.insert("artist".to_string(), json!(artist));
                    }
                    if let Some(album) = child.album {
                        value.insert("album".to_string(), json!(album));
                    }
                    if let Some(content_type) = child.content_type {
                        value.insert("contentType".to_string(), json!(content_type));
                    }
                    if let Some(cover_art) = child.cover_art {
                        value.insert("coverArt".to_string(), json!(cover_art));
                    }
                    if let Some(track) = child.track {
                        value.insert("track".to_string(), json!(track));
                    }
                    if let Some(disc_number) = child.disc_number {
                        value.insert("discNumber".to_string(), json!(disc_number));
                    }
                    if let Some(duration) = child.duration {
                        value.insert("duration".to_string(), json!(duration));
                    }
                    if let Some(size) = child.size {
                        value.insert("size".to_string(), json!(size));
                    }
                    if let Some(parent) = child.parent {
                        value.insert("parent".to_string(), json!(parent));
                    }
                    serde_json::Value::Object(value)
                })
                .collect::<Vec<_>>();

            let mut directory = serde_json::Map::new();
            directory.insert("id".to_string(), json!(id));
            directory.insert("name".to_string(), json!(name));
            if let Some(parent) = parent {
                directory.insert("parent".to_string(), json!(parent));
            }
            directory.insert("child".to_string(), json!(children));
            SubsonicResponse::Json(wrap_json(json!({
                "directory": serde_json::Value::Object(directory)
            })))
        }
    }
}

fn ok_message(format: ResponseFormat, message: &str) -> SubsonicResponse {
    match format {
        ResponseFormat::Xml => SubsonicResponse::Xml(wrap_xml(&format!(
            "<message>{}</message>",
            xml_escape(message)
        ))),
        ResponseFormat::Json => SubsonicResponse::Json(wrap_json(json!({ "message": message }))),
    }
}

fn empty_ok(format: ResponseFormat) -> SubsonicResponse {
    match format {
        ResponseFormat::Xml => SubsonicResponse::Xml(wrap_xml("")),
        ResponseFormat::Json => SubsonicResponse::Json(wrap_json(json!({}))),
    }
}

fn error_response(format: ResponseFormat, message: &str) -> SubsonicResponse {
    match format {
        ResponseFormat::Xml => SubsonicResponse::Xml(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response status=\"failed\" version=\"1.16.1\"><error code=\"0\" message=\"{}\" /></subsonic-response>",
            xml_escape(message)
        )),
        ResponseFormat::Json => SubsonicResponse::Json(
            json!({
                "subsonic-response": {
                    "status": "failed",
                    "version": "1.16.1",
                    "error": { "code": 0, "message": message }
                }
            })
            .to_string(),
        ),
    }
}

fn response_format(request: &RequestContext) -> ResponseFormat {
    match query_value(request, "f") {
        Some("json") => ResponseFormat::Json,
        _ => ResponseFormat::Xml,
    }
}

fn wrap_json(body: serde_json::Value) -> String {
    let mut envelope = serde_json::Map::new();
    envelope.insert("status".to_string(), json!("ok"));
    envelope.insert("version".to_string(), json!("1.16.1"));

    if let Some(body) = body.as_object() {
        for (key, value) in body {
            envelope.insert(key.clone(), value.clone());
        }
    }

    json!({
        "subsonic-response": serde_json::Value::Object(envelope)
    })
    .to_string()
}

fn wrap_xml(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response status=\"ok\" version=\"1.16.1\">{body}</subsonic-response>"
    )
}

fn optional_attr(name: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(" {name}=\"{}\"", xml_escape(value)))
        .unwrap_or_default()
}

fn optional_number_attr<T: std::fmt::Display>(name: &str, value: Option<T>) -> String {
    value
        .map(|value| format!(" {name}=\"{value}\""))
        .unwrap_or_default()
}

fn query_value<'a>(request: &'a RequestContext, key: &str) -> Option<&'a str> {
    request
        .query
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}

fn sort_albums(albums: &mut [protocol::Album], list_type: &str) {
    match list_type {
        "newest" => albums.sort_by(|left, right| {
            right
                .date
                .cmp(&left.date)
                .then_with(|| right.year.cmp(&left.year))
                .then_with(|| left.title.cmp(&right.title))
        }),
        "alphabeticalByArtist" => albums.sort_by(|left, right| {
            left.artist
                .cmp(&right.artist)
                .then_with(|| left.title.cmp(&right.title))
        }),
        "recent" | "frequent" | "random" | "starred" | "alphabeticalByName" => {
            albums.sort_by(|left, right| left.title.cmp(&right.title))
        }
        _ => albums.sort_by(|left, right| left.title.cmp(&right.title)),
    }
}

fn render_album_xml(album: &protocol::Album) -> String {
    format!(
        "<album id=\"{}\" name=\"{}\" artist=\"{}\" songCount=\"{}\"{}{}{}{}{}{} />",
        xml_escape(&album.id.0),
        xml_escape(&album.title),
        xml_escape(&album.artist),
        album.track_ids.len(),
        optional_attr("albumArtist", album.album_artist.as_deref()),
        optional_attr("genre", album.genres.first().map(String::as_str)),
        optional_attr("year", album.year.map(|year| year.to_string()).as_deref()),
        optional_attr(
            "coverArt",
            album.cover_art_id.as_ref().map(|id| id.0.as_str())
        ),
        optional_number_attr("duration", album.duration_seconds),
        optional_number_attr("size", Some(album.size_bytes))
    )
}

fn render_album_xml_open(album: &protocol::Album) -> String {
    format!(
        "<album id=\"{}\" name=\"{}\" artist=\"{}\" songCount=\"{}\"{}{}{}{}{}{}>",
        xml_escape(&album.id.0),
        xml_escape(&album.title),
        xml_escape(&album.artist),
        album.track_ids.len(),
        optional_attr("albumArtist", album.album_artist.as_deref()),
        optional_attr("genre", album.genres.first().map(String::as_str)),
        optional_attr("year", album.year.map(|year| year.to_string()).as_deref()),
        optional_attr(
            "coverArt",
            album.cover_art_id.as_ref().map(|id| id.0.as_str())
        ),
        optional_number_attr("duration", album.duration_seconds),
        optional_number_attr("size", Some(album.size_bytes))
    )
}

fn render_album_json(album: protocol::Album) -> serde_json::Value {
    json!({
        "id": album.id.0,
        "name": album.title,
        "artist": album.artist,
        "albumArtist": album.album_artist,
        "songCount": album.track_ids.len(),
        "genre": album.genres.first().cloned(),
        "year": album.year.map(|year| year.to_string()),
        "coverArt": album.cover_art_id.map(|id| id.0),
        "duration": album.duration_seconds,
        "size": album.size_bytes
    })
}

fn render_song_xml(track: &protocol::Track, parent: Option<&str>) -> String {
    format!(
        "<song id=\"{}\" title=\"{}\" artist=\"{}\" album=\"{}\" isDir=\"false\" contentType=\"{}\"{}{}{}{}{}{}{}{} />",
        xml_escape(&track.id.0),
        xml_escape(&track.title),
        xml_escape(&track.artist),
        xml_escape(&track.album),
        xml_escape(&track.content_type),
        optional_attr(
            "coverArt",
            track.cover_art_id.as_ref().map(|id| id.0.as_str())
        ),
        optional_attr("suffix", track.suffix.as_deref()),
        optional_number_attr("size", Some(track.file_size)),
        optional_number_attr("track", track.track_number),
        optional_number_attr("discNumber", track.disc_number),
        optional_number_attr("duration", track.duration_seconds),
        optional_number_attr("bitRate", track.bitrate),
        optional_attr("parent", parent)
    )
}

fn render_song_json(track: &protocol::Track, parent: Option<&str>) -> serde_json::Value {
    json!({
        "id": track.id.0,
        "title": track.title,
        "artist": track.artist,
        "album": track.album,
        "albumArtist": track.album_artist,
        "isDir": false,
        "contentType": track.content_type,
        "coverArt": track.cover_art_id.as_ref().map(|id| id.0.clone()),
        "parent": parent,
        "albumId": parent,
        "type": "music",
        "track": track.track_number,
        "discNumber": track.disc_number,
        "duration": track.duration_seconds,
        "bitRate": track.bitrate,
        "size": track.file_size,
        "suffix": track.suffix,
        "genre": track.genres.first().cloned()
    })
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::Result;
    use protocol::{Album, AlbumId, BackendResponse, CoverArtId, StreamDescriptor, Track, TrackId};
    use std::path::PathBuf;
    use std::time::SystemTime;

    struct MockBackend {
        albums: Vec<Album>,
        tracks: Vec<Track>,
    }

    impl MockBackend {
        fn new(albums: Vec<Album>, tracks: Vec<Track>) -> Self {
            Self { albums, tracks }
        }
    }

    impl Backend for MockBackend {
        async fn summary(&self) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn artists(&self) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn albums(&self) -> Result<BackendResponse> {
            Ok(BackendResponse::Albums(self.albums.clone()))
        }

        async fn starred(&self) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn set_starred(&self, _id: &str, _starred: bool) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn artist(&self, _artist_id: &str) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn album(&self, _album_id: &str) -> Result<BackendResponse> {
            Ok(BackendResponse::Album(
                self.albums.first().cloned().expect("album in mock backend"),
            ))
        }

        async fn album_tracks(&self, _album_id: &str) -> Result<BackendResponse> {
            Ok(BackendResponse::Tracks(self.tracks.clone()))
        }

        async fn track(&self, _track_id: &str) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn resolve_id(&self, _id: &str) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn cover_art(&self, _cover_art_id: &str) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn search(&self, _term: &str, _limit: usize) -> Result<BackendResponse> {
            unimplemented!()
        }

        async fn stream(
            &self,
            _track_id: &str,
        ) -> Result<(StreamDescriptor, iroh::endpoint::RecvStream)> {
            unimplemented!()
        }
    }

    fn request(path: &str, query: &[(&str, &str)]) -> RequestContext {
        RequestContext {
            path: path.to_string(),
            query: query
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    fn sample_album(id: &str, title: &str, artist: &str) -> Album {
        Album {
            id: AlbumId(id.to_string()),
            title: title.to_string(),
            artist: artist.to_string(),
            album_artist: Some(artist.to_string()),
            track_ids: vec![TrackId(format!("{id}-track"))],
            date: Some("2024-01-01".to_string()),
            original_date: None,
            year: Some(2024),
            genres: vec!["Rock".to_string()],
            labels: Vec::new(),
            catalog_number: None,
            comment: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            disc_count: Some(1),
            duration_seconds: Some(1800),
            size_bytes: 1024,
            cover_art_id: Some(CoverArtId(format!("cover-{id}"))),
        }
    }

    fn sample_track(id: &str, title: &str, artist: &str, album: &str) -> Track {
        Track {
            id: TrackId(id.to_string()),
            title: title.to_string(),
            artist: artist.to_string(),
            album: album.to_string(),
            album_artist: Some(artist.to_string()),
            track_number: Some(1),
            disc_number: Some(1),
            duration_seconds: Some(120),
            bitrate: Some(320),
            sample_rate: None,
            channels: None,
            codec: None,
            genres: vec!["Rock".to_string()],
            date: None,
            musicbrainz_track_id: None,
            musicbrainz_recording_id: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            cover_art_id: Some(CoverArtId(format!("cover-{id}"))),
            has_embedded_cover: false,
            suffix: Some("mp3".to_string()),
            relative_path: PathBuf::from(format!("{id}.mp3")),
            file_size: 2048,
            modified_at: SystemTime::UNIX_EPOCH,
            content_type: "audio/mpeg".to_string(),
        }
    }

    #[tokio::test]
    async fn json_album_list_route_includes_album_list_container() {
        let backend = MockBackend::new(
            vec![
                sample_album("album-b", "Beta", "Artist B"),
                sample_album("album-a", "Alpha", "Artist A"),
            ],
            Vec::new(),
        );

        let response = handle_request(
            &SubsonicConfig {
                bind: "127.0.0.1:4040".to_string(),
                endpoint: String::new(),
                ticket: None,
                secret: None,
                username: "user".to_string(),
                password: "pass".to_string(),
                relay: None,
            },
            &backend,
            request(
                "/rest/getAlbumList",
                &[
                    ("u", "user"),
                    ("p", "pass"),
                    ("f", "json"),
                    ("type", "alphabeticalByName"),
                    ("size", "10"),
                    ("offset", "0"),
                ],
            ),
        )
        .await
        .expect("album list response");

        let SubsonicResponse::Json(body) = response else {
            panic!("expected json response");
        };
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        let albums = &value["subsonic-response"]["albumList"]["album"];
        assert!(albums.is_array(), "albumList.album should be an array");
        assert_eq!(albums.as_array().unwrap().len(), 2);
        assert_eq!(albums[0]["name"], "Alpha");
        assert_eq!(albums[1]["name"], "Beta");
    }

    #[tokio::test]
    async fn json_album_route_includes_song_array() {
        let backend = MockBackend::new(
            vec![sample_album("album-a", "Alpha", "Artist A")],
            vec![sample_track("track-a1", "First Song", "Artist A", "Alpha")],
        );

        let response = handle_request(
            &SubsonicConfig {
                bind: "127.0.0.1:4040".to_string(),
                endpoint: String::new(),
                ticket: None,
                secret: None,
                username: "user".to_string(),
                password: "pass".to_string(),
                relay: None,
            },
            &backend,
            request(
                "/rest/getAlbum",
                &[("u", "user"), ("p", "pass"), ("f", "json"), ("id", "album-a")],
            ),
        )
        .await
        .expect("album response");

        let SubsonicResponse::Json(body) = response else {
            panic!("expected json response");
        };
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        let songs = &value["subsonic-response"]["album"]["song"];
        assert!(songs.is_array(), "album.song should be an array");
        assert_eq!(songs.as_array().unwrap().len(), 1);
        assert_eq!(songs[0]["title"], "First Song");
        assert_eq!(songs[0]["isDir"], false);
        assert_eq!(songs[0]["parent"], "album-a");
        assert_eq!(songs[0]["albumId"], "album-a");
        assert_eq!(songs[0]["type"], "music");
        assert_eq!(songs[0]["size"], 2048);
    }
}
