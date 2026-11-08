//! Media Audio Utility: change the default audio and subtitle tracks and languages of
//! media files.
//!
//! MKV files are edited with `mkvpropedit` where possible (only the metadata changes)
//! and rewritten with `mkvmerge` when tracks are moved or removed. MP4 files are
//! rewritten with `ffmpeg` (a copy of the streams, so nothing is re-encoded). AVI files
//! have no track metadata to edit, so they are only rewritten, to swap or remove audio
//! tracks.
//!
//! This crate does the work and prints nothing: a front end (the command line or the
//! GUI) gives it a [`Config`] and an event sink, and shows what it is told.

mod detect;
pub use detect::{model_path, whisper_path, MODEL_BYTES};
mod engine;
pub mod audio_decode;
pub mod avi;
pub mod clip;
pub mod ebml;
pub mod ebml_edit;
pub mod ebml_subs;
pub mod languages;
mod install;
mod language_table;
pub mod model;
pub mod online;
pub mod mp4;
pub mod subtitles;
pub mod text_subs;
pub mod track;
pub mod util;
mod validate;

pub use engine::{
    abspath, default_edits, info_text, order_by_languages, rename_in_log, subtitle_extension, pattern_hit, reordered, reordered_by, Mau,
    RunEnd,
};
pub use model::*;
pub use install::{detect_installer, linux_installer, Installer};
pub use validate::{parse_order, validate};
pub use track::{format_tracks, human_size, Kind, Track, TrackSpec};

pub const VERSION: &str = "1.6.7";
