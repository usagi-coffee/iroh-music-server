use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub music_dir: PathBuf,
    pub lastfm_api_key: Option<String>,
}

impl ServerConfig {
    pub fn new(music_dir: impl Into<PathBuf>, lastfm_api_key: Option<String>) -> Self {
        Self {
            music_dir: music_dir.into(),
            lastfm_api_key,
        }
    }
}
