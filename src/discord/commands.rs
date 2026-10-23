mod admin;
mod chronicle;
mod controls;
mod library;
mod management;
mod tags;

pub(crate) use admin::{help, register};
pub(crate) use chronicle::{chronicle, recording, transcript};
pub(crate) use controls::{
    break_cmd, history, join, leave, loop_track, mix, now_playing, pause, play, queue, skip,
};
pub(crate) use library::library;
pub(crate) use management::{download, fix, set_metadata};
pub(crate) use tags::tags;
