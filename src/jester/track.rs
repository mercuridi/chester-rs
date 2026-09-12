pub(crate) use download::{DownloadConfig, Downloader, download_track};
pub(crate) use resolver::resolve_track;
pub(crate) use types::{TrackInfo, VideoId};

mod download;
mod metadata;
mod resolver;
mod types;
mod youtube;
