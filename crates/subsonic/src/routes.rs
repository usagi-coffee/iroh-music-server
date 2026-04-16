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
        "/rest/getMusicDirectory" => {
            let id = query_value(&request, "id").unwrap_or("root");
            map_directory(format, backend, id).await
        }
        "/rest/getAlbum" => {
            let album_id = query_value(&request, "id").unwrap_or_default();
            map_album(format, backend.album(album_id).await?)
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

fn map_album(format: ResponseFormat, response: BackendResponse) -> Result<SubsonicResponse> {
    let BackendResponse::Album(album) = response else {
        return Err(Error::InvalidRequest(
            "backend returned unexpected response for getAlbum".to_string(),
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
        ResponseFormat::Xml => Ok(SubsonicResponse::Xml(wrap_xml(&format!(
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
        )))),
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
                "size": album.size_bytes
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

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
