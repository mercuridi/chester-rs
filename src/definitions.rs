use serde::{Serialize, Deserialize};


// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

#[derive(Serialize, Deserialize)]
pub struct TrackInfo {
        pub upload_date: i64,
        pub yt_title: String,
        pub yt_channel: String,
        pub track_title: String,
        pub track_artist: String
}