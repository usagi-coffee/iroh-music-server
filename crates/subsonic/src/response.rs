#[derive(Debug, Clone)]
pub enum SubsonicResponse {
    Xml(String),
    Json(String),
    Binary {
        content_type: String,
        bytes: Vec<u8>,
    },
}
