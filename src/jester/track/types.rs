// Track Info unified struct
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub id: VideoId,
    pub title: String,
    pub artist: String,
    pub origin: String,
}

// Domain types - semantic safety to prevent mixing incompatible values
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VideoId(pub String);

impl VideoId {
    // pub fn new(id: String) -> Self {
    //     VideoId(id)
    // }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VideoId {
    fn from(s: String) -> Self {
        VideoId(s)
    }
}

impl From<&str> for VideoId {
    fn from(s: &str) -> Self {
        VideoId(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::VideoId;

    #[test]
    fn video_id_converts_from_owned_and_borrowed_strings() {
        let borrowed = VideoId::from("abc");
        let owned = VideoId::from(String::from("abc"));
        assert_eq!(borrowed, owned);
        assert_eq!(borrowed.as_str(), "abc");
    }
}
