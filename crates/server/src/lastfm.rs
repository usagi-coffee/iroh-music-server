use reqwest::blocking::Client;
use serde::Deserialize;

use crate::index::CoverArtSource;
use crate::{AlbumMetadata, LibraryIndex};
use protocol::CoverArtId;

const LASTFM_API_URL: &str = "https://ws.audioscrobbler.com/2.0/";

#[derive(Debug, Clone)]
pub struct LastfmClient {
    api_key: String,
    client: Client,
}

impl LastfmClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            client: Client::new(),
        }
    }

    pub fn enrich_library(&self, library: &mut LibraryIndex) {
        let mut external_cover_arts = Vec::new();
        for album in library.albums.values_mut() {
            match self.fetch_album_metadata(&album.artist, &album.title) {
                Ok(Some(metadata)) => {
                    if album.cover_art_id.is_none() {
                        if let Some(image_url) =
                            metadata.image_url.as_ref().filter(|url| !url.is_empty())
                        {
                            let cover_art_id = CoverArtId(format!("lastfm-album-{}", album.id.0));
                            album.cover_art_id = Some(cover_art_id.clone());
                            external_cover_arts.push((cover_art_id, image_url.clone()));
                        }
                    }
                    album.metadata = Some(metadata);
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!(
                        "[lastfm] album lookup failed artist={} album={} error={error}",
                        album.artist, album.title
                    );
                }
            }
        }

        for (cover_art_id, image_url) in external_cover_arts {
            library
                .cover_arts
                .insert(cover_art_id, CoverArtSource::External { url: image_url });
        }
    }

    fn fetch_album_metadata(
        &self,
        artist: &str,
        album: &str,
    ) -> Result<Option<AlbumMetadata>, reqwest::Error> {
        let response = self
            .client
            .get(LASTFM_API_URL)
            .query(&[
                ("method", "album.getInfo"),
                ("api_key", self.api_key.as_str()),
                ("artist", artist),
                ("album", album),
                ("format", "json"),
                ("autocorrect", "1"),
            ])
            .send()?;

        let payload: LastfmAlbumResponse = response.json()?;
        Ok(payload.album.map(AlbumMetadata::from))
    }
}

#[derive(Debug, Deserialize)]
struct LastfmAlbumResponse {
    album: Option<LastfmAlbum>,
}

#[derive(Debug, Deserialize)]
struct LastfmAlbum {
    url: Option<String>,
    mbid: Option<String>,
    listeners: Option<String>,
    playcount: Option<String>,
    wiki: Option<LastfmWiki>,
    tags: Option<LastfmTags>,
    image: Option<Vec<LastfmImage>>,
}

#[derive(Debug, Deserialize)]
struct LastfmWiki {
    published: Option<String>,
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmTags {
    tag: Vec<LastfmTag>,
}

#[derive(Debug, Deserialize)]
struct LastfmTag {
    name: String,
}

#[derive(Debug, Deserialize)]
struct LastfmImage {
    #[serde(rename = "#text")]
    text: String,
}

impl From<LastfmAlbum> for AlbumMetadata {
    fn from(value: LastfmAlbum) -> Self {
        let image_url = value
            .image
            .and_then(|images| {
                images
                    .into_iter()
                    .rev()
                    .find(|image| !image.text.is_empty())
            })
            .map(|image| image.text);
        Self {
            lastfm_url: value.url,
            lastfm_mbid: value.mbid.filter(|mbid| !mbid.is_empty()),
            published: value.wiki.as_ref().and_then(|wiki| wiki.published.clone()),
            summary: value.wiki.and_then(|wiki| wiki.summary),
            image_url,
            tags: value
                .tags
                .map(|tags| tags.tag.into_iter().map(|tag| tag.name).collect())
                .unwrap_or_default(),
            listeners: value.listeners.and_then(|listeners| listeners.parse().ok()),
            playcount: value.playcount.and_then(|playcount| playcount.parse().ok()),
        }
    }
}
