#[derive(Debug, Clone)]
pub struct RequestContext {
    pub path: String,
    pub query: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Child {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub content_type: String,
}

#[derive(Debug, Clone)]
pub struct IndexArtist {
    pub id: String,
    pub name: String,
}
