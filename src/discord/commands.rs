mod admin;
mod chronicle;
mod controls;
mod library;
mod management;

pub(crate) use admin::{help, register};
pub(crate) use chronicle::{chronicle, recording, transcript};
pub(crate) use controls::{
    history, join, leave, loop_track, mix, now_playing, pause, play, queue, skip,
};
pub(crate) use library::library;
pub(crate) use management::{
    add_environment, add_label, add_texture, download, fix, reset_taxonomy, set_metadata,
    set_taxonomy,
};
