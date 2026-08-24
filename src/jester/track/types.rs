use songbird::tracks::TrackHandle;

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

pub struct NowPlaying {
    pub track: TrackInfo,
    pub handle: TrackHandle,
}