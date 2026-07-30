#[derive(Debug, Clone)]
pub struct GameStreamResponse {
    pub status: u16,
    pub body: String,
    pub content_type: Option<String>,
}
