//! The types shared by the engine and its front ends.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::{json, Value};

use crate::track::{yes_no, Kind, Track};

/// The commands the tool runs and the package that provides each.
pub const REQUIRED_TOOLS: &[(&str, &str)] = &[
    ("mkvmerge", "mkvtoolnix"),
    ("mkvpropedit", "mkvtoolnix"),
    ("mediainfo", "mediainfo"),
    ("ffmpeg", "ffmpeg"),
];

/// Whether this build does the MKV work with ffmpeg and never uses mkvtoolnix: the `no-mkvtoolnix` feature (the
/// build made for the App Store), or, for the tests, the environment variable MAU_SPIKE_NO_MKVTOOLNIX.
pub fn mkvtoolnix_hidden() -> bool {
    cfg!(feature = "no-mkvtoolnix") || std::env::var_os("MAU_SPIKE_NO_MKVTOOLNIX").is_some()
}

/// Whether this build has no ffmpeg at all: the `no-ffmpeg` feature (the build with no LGPL or GPL code, made by
/// `build.sh --sandboxed --nolgpl`), or, for the tests, the environment variable MAU_SPIKE_NO_FFMPEG. ffmpeg and
/// ffprobe are then never started, and a change that needs them is refused.
pub fn ffmpeg_hidden() -> bool {
    cfg!(feature = "no-ffmpeg") || std::env::var_os("MAU_SPIKE_NO_FFMPEG").is_some()
}

/// The commands every action needs in this build: mkvtoolnix's only when this build uses it, and ffmpeg's only when
/// this build has ffmpeg.
pub fn required_tools() -> Vec<(&'static str, &'static str)> {
    REQUIRED_TOOLS
        .iter()
        .copied()
        .filter(|(tool, _)| !(mkvtoolnix_hidden() && tool.starts_with("mkv")))
        .filter(|(tool, _)| !(ffmpeg_hidden() && *tool == "ffmpeg"))
        .collect()
}

/// Commands that only some actions use, and the package that provides each. A missing one is not
/// an error: `--check` reports it, and `--install` installs it where a package is known.
/// `whisper-cli` (whisper.cpp) is what `--detect` runs.
pub const OPTIONAL_TOOLS: &[(&str, &str)] = &[("whisper-cli", "whisper-cpp")];

/// Temporary files are named after the file they replace, with this in the name.
pub const TEMP_MARK: &str = ".fml-tmp";

/// With a backup the original is kept beside the result, with this added to its name.
pub const BACKUP_MARK: &str = ".fml-bak";

/// What `mkvmerge` exit codes mean: 0 success, 1 success with warnings, 2 error.
pub const MKVMERGE_OK: &[i32] = &[0, 1];

pub const FFMPEG_QUIET: &[&str] = &["-hide_banner", "-loglevel", "error", "-y"];

/// The kind of container a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mkv,
    Mp4,
    Avi,
}

impl Container {
    pub fn name(self) -> &'static str {
        match self {
            Container::Mkv => "Matroska",
            Container::Mp4 => "MP4",
            Container::Avi => "AVI",
        }
    }

    /// The commands needed to read a file of this kind (for `info` and `get`).
    pub fn read_tools(self) -> &'static [&'static str] {
        match self {
            // an MKV's tracks are read in this program with the native-mkv feature, else ffprobe (from ffmpeg)
            Container::Mkv if cfg!(feature = "native-mkv") => &[],
            Container::Mkv => &["ffmpeg"],
            _ => &["mediainfo"],
        }
    }

    /// The commands needed to change a file of this kind.
    pub fn write_tools(self) -> &'static [&'static str] {
        match self {
            Container::Mkv if mkvtoolnix_hidden() => &["mediainfo", "ffmpeg"],
            Container::Mkv => &["mkvmerge", "mkvpropedit"],
            _ => &["mediainfo", "ffmpeg"],
        }
    }
}

/// The outcome of an action on a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Changed,
    Unchanged,
    /// A warning: the file was left alone, and that is not a failure.
    Skipped,
    Error,
}

/// The result of an action on a file.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub file: String,
    pub action: String,
    pub status: Status,
    pub message: String,
    pub changes: Vec<String>,
    pub commands: Vec<Vec<String>>,
    pub data: Value,
    pub dry_run: bool,
    /// Where the original was kept, with a backup.
    pub backup: String,
    /// The place the track that was moved first had (for the log).
    pub moved_from: usize,
    /// When several tracks moved: the old number of each track of the kind, in the new order.
    pub order: Vec<usize>,
}

impl Outcome {
    pub fn new(file: &str, action: &str, dry_run: bool) -> Outcome {
        Outcome {
            file: file.to_string(),
            action: action.to_string(),
            status: Status::Ok,
            message: String::new(),
            changes: Vec::new(),
            commands: Vec::new(),
            data: Value::Null,
            dry_run,
            backup: String::new(),
            moved_from: 0,
            order: Vec::new(),
        }
    }

    pub fn with_status(mut self, status: Status, message: &str) -> Outcome {
        self.status = status;
        self.message = message.to_string();
        self
    }

    pub fn error(&mut self, message: String) {
        self.status = Status::Error;
        self.message = message;
    }

    /// Mark a result that made changes as changed, unless it failed.
    pub fn finish(mut self) -> Outcome {
        if self.status == Status::Ok && !self.changes.is_empty() {
            self.status = Status::Changed;
        }
        self
    }

    pub fn to_json(&self) -> Value {
        json!({
            "file": self.file, "action": self.action, "status": self.status,
            "message": self.message, "changes": self.changes, "commands": self.commands,
            "dry_run": self.dry_run,
            "backup": if self.backup.is_empty() { Value::Null } else { json!(self.backup) },
            "data": self.data,
        })
    }
}

/// Settings that belong to one action, given after it on the command line. Only what was given
/// is set; anything else comes from the settings given before the first action, which apply to
/// every action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSettings {
    pub lang: Option<String>,
    pub default: Option<String>,
    pub prefer: Option<Vec<String>>,
    pub keep: Option<Vec<String>>,
    pub codec: Option<String>,
    pub channels: Option<usize>,
    pub track: Option<String>,
    pub kind: Option<Kind>,
    pub forced: Option<bool>,
    pub move_first: Option<bool>,
    pub name: Option<String>,
    pub order: Option<Vec<usize>>,
    pub order_lang: Option<Vec<String>>,
    pub default_first: Option<bool>,
    pub forced_flag: Option<bool>,
    pub sub_file: Option<String>,
    pub sub_dir: Option<String>,
    pub output: Option<String>,
    pub query: Option<String>,
    pub online: Option<bool>,
    pub sub_id: Option<i64>,
}

/// An action to run, with its argument and its own settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub name: String,
    pub what: String,
    pub settings: TaskSettings,
}

impl Task {
    pub fn new(name: &str, what: &str) -> Task {
        Task { name: name.to_string(), what: what.to_string(), settings: TaskSettings::default() }
    }
}

/// The extensions of the subtitle files that can be added to an MP4: text subtitles, which are
/// converted to `mov_text` (a `.sup` picture subtitle can't be).
pub const MP4_SUBTITLE_FILES: &[&str] = &["srt", "ass", "ssa", "vtt"];

/// A subtitle file to add to a media file as a track.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubSpec {
    pub file: String,
    /// ISO 639-2/B.
    pub lang: String,
    pub name: String,
    pub forced: bool,
}

/// The extensions of the subtitle files that can be added as a track (each holds one track).
pub const SUBTITLE_FILES: &[&str] = &["srt", "ass", "ssa", "vtt", "sup"];

/// Settings from the command line or the GUI.
#[derive(Debug, Clone)]
pub struct Config {
    pub file: String,
    pub dir: String,
    /// An explicit list of files (from a front end that lets the user pick them); used
    /// instead of `file` and `dir` when it is not empty.
    pub paths: Vec<String>,
    pub recursive: bool,
    pub lang: String,
    /// A language, or "none" for no default subtitle track.
    pub default: String,
    pub prefer: Vec<String>,
    /// For `preserve`: every language to keep (the tracks in all of them stay).
    pub keep: Vec<String>,
    /// For `search subtitle` and `add subtitle --online`: the title to search for (the name of the
    /// media file says it when this is empty).
    pub query: String,
    /// For `add subtitle`: find the subtitle on OpenSubtitles instead of taking `sub_file`.
    pub online: bool,
    /// With `online`: the subtitle (file ID) a search listed; 0 means the best one for `lang`.
    pub sub_id: i64,
    /// Optional features that are off unless named with `--enable`: `search` (the online
    /// subtitle search on OpenSubtitles).
    pub enable: Vec<String>,
    /// For `delete match`: part of the codec name, as `--info` shows it (any case).
    pub codec: String,
    /// For `delete match`: the number of audio channels (0: not given).
    pub channels: usize,
    pub track: String,
    /// The kind of track the actions work on: audio or subtitles.
    pub kind: Kind,
    pub forced: bool,
    pub verbose: bool,
    pub dryrun: bool,
    pub install: bool,
    pub force: bool,
    /// Keep the original beside the changed file.
    pub backup: bool,
    /// A file to record each change in, so that undo can reverse it.
    pub log: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// How many files to process at once.
    pub jobs: usize,
    /// Also make the chosen track the first of its kind.
    pub move_first: bool,
    pub name: Option<String>,
    /// Send `Event::Progress` while a file is copied or rewritten (it costs a thread that looks at
    /// the size of the new file a few times a second).
    pub progress: bool,
    /// For `extract subtitle`: the file to write, or a folder to put a generated name in (the
    /// folder of the media file if empty).
    pub output: String,
    /// For `--set track`: set (`true`) or clear (`false`) the forced flag; `None` leaves it.
    pub forced_flag: Option<bool>,
    /// For `--set order`: the current number of each track of the kind, in the order they
    /// should have.
    pub order: Vec<usize>,
    /// For `set order`, instead of `order`: the languages to put first, in order; tracks in
    /// other languages follow, in their old order. It works on files whose tracks differ.
    pub order_lang: Vec<String>,
    /// For `set order`: also make the track that ends up first the only default of its kind.
    pub default_first: bool,
    /// For `add subtitle`: the subtitle file to add as a track.
    pub sub_file: String,
    /// For `add subtitle`, instead of `sub_file`: a folder of subtitle files, matched to the media
    /// files by name (see `subtitles::plan_from_folder`); `lang` is used where a name has none.
    pub sub_dir: String,
    /// What was matched from `sub_dir` for each media file, made once before the files are
    /// processed (the engine fills it in; a front end leaves it empty).
    pub sub_matches: std::collections::HashMap<String, Vec<SubSpec>>,
    /// For `add subtitle`, instead of `sub_file`, `lang` and `name`: several files, each with its
    /// own language, name and forced flag, added in one rewrite.
    pub subtitles: Vec<SubSpec>,
    pub tasks: Vec<Task>,
}

impl Config {
    /// Whether an optional feature named with `--enable` is on.
    pub fn enabled(&self, feature: &str) -> bool {
        self.enable.iter().any(|name| name == feature)
    }

    /// The settings for one action: these, with the action's own settings on top. Its only task
    /// is that action.
    pub fn for_task(&self, task: &Task) -> Config {
        let mut config = self.clone();
        let own = &task.settings;
        if let Some(v) = &own.lang { config.lang = v.clone(); }
        if let Some(v) = &own.default { config.default = v.clone(); }
        if let Some(v) = &own.prefer { config.prefer = v.clone(); }
        if let Some(v) = &own.keep { config.keep = v.clone(); }
        if let Some(v) = &own.query { config.query = v.clone(); }
        if let Some(v) = own.online { config.online = v; }
        if let Some(v) = own.sub_id { config.sub_id = v; }
        if let Some(v) = &own.codec { config.codec = v.clone(); }
        if let Some(v) = own.channels { config.channels = v; }
        if let Some(v) = &own.track { config.track = v.clone(); }
        if let Some(v) = own.kind { config.kind = v; }
        if let Some(v) = own.forced { config.forced = v; }
        if let Some(v) = own.move_first { config.move_first = v; }
        if let Some(v) = &own.name { config.name = Some(v.clone()); }
        if let Some(v) = &own.order { config.order = v.clone(); }
        if let Some(v) = &own.order_lang { config.order_lang = v.clone(); }
        if let Some(v) = own.default_first { config.default_first = v; }
        if let Some(v) = own.forced_flag { config.forced_flag = Some(v); }
        if let Some(v) = &own.sub_file { config.sub_file = v.clone(); }
        if let Some(v) = &own.sub_dir { config.sub_dir = v.clone(); }
        if let Some(v) = &own.output { config.output = v.clone(); }
        config.tasks = vec![task.clone()];
        config
    }
}

impl Default for Config {
    fn default() -> Config {
        Config {
            file: String::new(),
            dir: String::new(),
            paths: Vec::new(),
            recursive: false,
            lang: String::new(),
            default: String::new(),
            prefer: Vec::new(),
            keep: Vec::new(),
            query: String::new(),
            online: false,
            sub_id: 0,
            enable: Vec::new(),
            codec: String::new(),
            channels: 0,
            track: String::new(),
            kind: Kind::Audio,
            forced: false,
            verbose: false,
            dryrun: false,
            install: false,
            force: false,
            backup: false,
            log: String::new(),
            include: Vec::new(),
            exclude: Vec::new(),
            jobs: 1,
            move_first: false,
            name: None,
            progress: false,
            output: String::new(),
            forced_flag: None,
            order: Vec::new(),
            order_lang: Vec::new(),
            default_first: false,
            sub_file: String::new(),
            sub_dir: String::new(),
            sub_matches: std::collections::HashMap::new(),
            subtitles: Vec::new(),
            tasks: Vec::new(),
        }
    }
}

/// A media file and the kind of container it is.
#[derive(Debug, Clone)]
pub struct Media {
    pub path: String,
    pub container: Container,
    pub tracks: Option<Vec<Track>>,
}

impl Media {
    pub fn new(path: &str, container: Container) -> Media {
        Media { path: path.to_string(), container, tracks: None }
    }
}

/// A change to a track: its default flag, language, name or forced flag, or its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub track: Track,
    pub default: Option<bool>,
    pub language: Option<String>,
    /// The place to move it to among tracks of its kind (1 is first).
    pub move_to: Option<usize>,
    pub name: Option<String>,
    pub forced: Option<bool>,
}

impl Edit {
    pub fn new(track: &Track) -> Edit {
        Edit {
            track: track.clone(),
            default: None,
            language: None,
            move_to: None,
            name: None,
            forced: None,
        }
    }

    pub fn describe(&self) -> String {
        let track = &self.track;
        let mut parts = Vec::new();
        if let Some(default) = self.default {
            parts.push(format!("default {} -> {}", yes_no(track.default), yes_no(default)));
        }
        if let Some(language) = &self.language {
            parts.push(format!("language {} -> {}", track.language, language));
        }
        if let Some(name) = &self.name {
            parts.push(format!("name \"{}\" -> \"{}\"", track.name, name));
        }
        if let Some(forced) = self.forced {
            parts.push(format!("forced {} -> {}", yes_no(track.forced), yes_no(forced)));
        }
        if let Some(place) = self.move_to {
            parts.push(if place == 1 {
                format!("moved to the first {} track", track.kind.word())
            } else {
                format!("moved to {} track {}", track.kind.word(), place)
            });
        }
        let named = self.default.is_some()
            || self.move_to.is_some()
            || self.name.is_some()
            || self.forced.is_some();
        let label = if named {
            format!("{} ({})", track.label(), track.language)
        } else {
            track.label()
        };
        format!("{label}: {}", parts.join(", "))
    }
}

/// Why everything stops: a problem such as a missing tool, or being asked to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Fatal(String),
    Interrupted,
}

pub type Res<T> = Result<T, Stop>;

/// What the engine tells its front end while it works.
#[derive(Debug, Clone)]
pub enum Event {
    Warn(String),
    /// A message for --verbose.
    Note(String),
    Result(Box<Outcome>),
    /// How far a long step has got, for a progress bar: `done` of `total` bytes of the new file.
    /// Only sent if `Config::progress` is set, and only while a file is being copied or rewritten.
    Progress { file: String, stage: String, done: u64, total: u64 },
}

pub type Sink = std::sync::Arc<dyn Fn(Event) + Send + Sync>;

static CANCEL: AtomicBool = AtomicBool::new(false);

/// Ask the engine to stop: running commands are ended and temporary files removed.
pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

pub fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

/// Forget an earlier request to stop, before a new run.
pub fn reset_cancel() {
    CANCEL.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(number: usize, language: &str, default: bool) -> Track {
        Track {
            id: number as i64,
            kind: Kind::Audio,
            number,
            codec: "AAC".into(),
            language: language.into(),
            default,
            forced: false,
            name: String::new(),
            channels: 0,
        }
    }

    #[test]
    fn edits_describe_themselves() {
        let mut edit = Edit::new(&track(2, "eng", false));
        edit.default = Some(true);
        edit.move_to = Some(1);
        assert_eq!(
            edit.describe(),
            "audio track 2 (eng): default no -> yes, moved to the first audio track"
        );
        let mut edit = Edit::new(&track(1, "ita", true));
        edit.language = Some("eng".into());
        assert_eq!(edit.describe(), "audio track 1: language ita -> eng");
    }

    #[test]
    fn finished_results_with_changes_are_changed() {
        let mut outcome = Outcome::new("a.mkv", "set", false);
        outcome.changes.push("x".into());
        assert_eq!(outcome.finish().status, Status::Changed);
        let mut failed = Outcome::new("a.mkv", "set", false);
        failed.changes.push("x".into());
        failed.error("boom".into());
        assert_eq!(failed.finish().status, Status::Error);
    }
}
