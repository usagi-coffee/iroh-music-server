#[derive(Debug, Clone)]
pub struct SubsonicConfig {
    pub bind: String,
    pub endpoint: String,
    pub relay: Option<String>,
    pub username: String,
    pub password: String,
}

impl Default for SubsonicConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:4040".to_string(),
            endpoint: String::new(),
            relay: None,
            username: "admin".to_string(),
            password: "admin".to_string(),
        }
    }
}
