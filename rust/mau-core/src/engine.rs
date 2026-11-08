//! The engine: runs the requested actions against media files.
//!
//! Every change is made on a copy of the file (or, for an MKV, on a rewritten file),
//! read back and checked, and only then moved over the original.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use serde_json::{json, Value};

use crate::model::*;
use crate::install::{detect_installer, Installer};
use crate::mp4;
use crate::online;
use crate::detect;
use crate::languages::resolve_language;
use crate::subtitles;
use crate::track::{
    format_tracks, human_size, parse_mediainfo_tracks, parse_mkv_tracks, Kind, Track, TrackSpec,
};
use crate::util::{
    copy_file, copy_stat, detect_kind, disk_free, fnmatch, run_command,
    shell_join, strerror, which, Completed, Ran,
};
use crate::VERSION;

/// Room on disk that running jobs have set aside, so several at once can't each find
/// enough and then use it up between them.
#[derive(Default)]
struct Space {
    reserved: HashMap<u64, i64>,
    holds: HashMap<String, Vec<(u64, i64)>>,
}

/// How much room a folder's disk has and which disk it is (see `disk_free`).
type DiskFn = Box<dyn Fn(&Path) -> io::Result<(u64, u64)> + Send + Sync>;
/// Where a command is (see `which`).
type FindFn = Box<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

struct Shared {
    /// Temporary files to remove if we are stopped.
    pending: Mutex<Vec<String>>,
    space: Mutex<Space>,
    install: Mutex<()>,
    log: Mutex<()>,
    /// The real disk and `PATH`; the tests put others in their place, to get a full disk or
    /// missing commands without having either.
    disk: DiskFn,
    finder: FindFn,
}

impl Default for Shared {
    fn default() -> Shared {
        Shared {
            pending: Mutex::default(),
            space: Mutex::default(),
            install: Mutex::default(),
            log: Mutex::default(),
            disk: Box::new(|folder| disk_free(folder)),
            finder: Box::new(|tool| which(tool)),
        }
    }
}

/// What a run did.
pub struct RunEnd {
    pub results: Vec<Outcome>,
    /// The exit status.
    pub code: i32,
}

pub struct Mau {
    pub config: Config,
    shared: Arc<Shared>,
    sink: Sink,
    /// What `--install` uses on this system, if it is known.
    installer: Option<Installer>,
    /// Where the whisper.cpp model is (see `detect::model_path`); `None` leaves the model alone.
    model: Option<PathBuf>,
}

/// Why the forced flag of a track cannot be set, if it cannot: it is set for MKV tracks of any kind and for MP4
/// subtitle tracks (their `kind` box, which mediainfo reads back), and for nothing else.
fn forced_refusal(container: Container, kind: Kind) -> Option<&'static str> {
    match (container, kind) {
        (Container::Mkv, _) | (Container::Mp4, Kind::Subtitles) => None,
        (Container::Mp4, _) => Some("the forced flag of an MP4 can only be set on a subtitle track"),
        _ => Some("the forced flag can only be set for MKV and MP4 subtitle tracks"),
    }
}

fn skip(mut result: Outcome, message: &str) -> Outcome {
    result.status = Status::Skipped;
    result.message = message.to_string();
    result
}

fn mark_skipped(result: &mut Outcome, message: &str) {
    result.status = Status::Skipped;
    result.message = message.to_string();
}

/// A codec name as it is compared for `--codec`: lower case, and without hyphens and spaces, so `ac-3` matches
/// `ac3` (ffprobe's name) and `AC-3` (mkvtoolnix's).
fn codec_key(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Whether a command is one the tool needs for every action (not an optional one like `whisper-cli`).
fn is_required(tool: &str) -> bool {
    required_tools().iter().any(|(t, _)| *t == tool)
}

/// The commands not found: the required ones only, if `required_only`.
fn missing_in(found: &[(String, Option<PathBuf>)], required_only: bool) -> Vec<String> {
    found
        .iter()
        .filter(|(tool, path)| path.is_none() && (!required_only || is_required(tool)))
        .map(|(tool, _)| tool.clone())
        .collect()
}

/// An absolute, tidied path (without following links), like Python's `os.path.abspath`.
/// The file a path leads to. A symbolic link is followed, so the file it points to is the one
/// that is changed: replacing the link itself with the changed copy would leave the real file as
/// it was, and the link would stop being a link. A link that points nowhere is returned as it is.
pub fn followed(path: &str) -> String {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)
            .map(|target| target.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string()),
        _ => path.to_string(),
    }
}

pub fn abspath(path: &str) -> String {
    let full = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for part in full.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

fn temp_path(path: &str) -> String {
    // ffmpeg picks the output format from the extension, so the real one is kept
    let p = Path::new(path);
    match p.extension() {
        Some(ext) => format!(
            "{}{}.{}",
            p.with_extension("").to_string_lossy(),
            TEMP_MARK,
            ext.to_string_lossy()
        ),
        None => format!("{path}{TEMP_MARK}"),
    }
}

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The tracks, with the one being moved put at a place among its kind.
pub fn reordered(all: &[Track], moving: Option<&Track>, to: usize) -> Vec<Track> {
    match moving {
        Some(moving) => reordered_by(all, &[(moving.clone(), to)]),
        None => all.to_vec(),
    }
}

/// The tracks, with each track in `moves` put at its place (from 1) among its kind, and the
/// kind's other tracks filling the places left, in their old order. A place past the end is
/// the last; two tracks wanting one place get the next free one.
pub fn reordered_by(all: &[Track], moves: &[(Track, usize)]) -> Vec<Track> {
    let mut tracks = all.to_vec();
    let mut kinds: Vec<Kind> = Vec::new();
    for (track, _) in moves {
        if !kinds.contains(&track.kind) {
            kinds.push(track.kind);
        }
    }
    for kind in kinds {
        let slots: Vec<usize> =
            tracks.iter().enumerate().filter(|(_, t)| t.kind == kind).map(|(i, _)| i).collect();
        let count = slots.len();
        let mut placed: Vec<Option<Track>> = vec![None; count];
        let mut moving: Vec<(Kind, usize)> = Vec::new();
        for (track, to) in moves.iter().filter(|(t, _)| t.kind == kind) {
            let want = (*to).clamp(1, count.max(1)) - 1;
            if let Some(at) = (want..count).chain(0..want).find(|i| placed[*i].is_none()) {
                placed[at] = Some(track.clone());
                moving.push(track.key());
            }
        }
        let mut rest = tracks
            .iter()
            .filter(|t| t.kind == kind && !moving.contains(&t.key()))
            .cloned()
            .collect::<Vec<_>>()
            .into_iter();
        for (slot, track) in slots.iter().zip(placed) {
            if let Some(track) = track.or_else(|| rest.next()) {
                tracks[*slot] = track;
            }
        }
    }
    tracks
}

/// The track numbers of a kind put in the order of a list of languages: the tracks in the
/// first language, then those in the second, and so on, then the rest; each group in its
/// old order.
pub fn order_by_languages(tracks: &[Track], languages: &[String]) -> Vec<usize> {
    let rank = |t: &Track| languages.iter().position(|l| *l == t.language).unwrap_or(languages.len());
    let mut sorted: Vec<&Track> = tracks.iter().collect();
    sorted.sort_by_key(|t| rank(t)); // stable
    sorted.iter().map(|t| t.number).collect()
}

/// The edits that make one track the only default (none if `chosen` is `None`), give it
/// a language if one is given, and move it first if asked. Tracks that are right are
/// left alone.
pub fn default_edits(
    tracks: &[Track],
    chosen: Option<&Track>,
    language: &str,
    move_first: bool,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    for track in tracks {
        let is_chosen = chosen.is_some_and(|c| c.number == track.number);
        let default = if track.default == is_chosen { None } else { Some(is_chosen) };
        let new_language = (is_chosen && !language.is_empty() && language != track.language)
            .then(|| language.to_string());
        let moving = move_first && is_chosen && track.number != tracks[0].number;
        if default.is_some() || new_language.is_some() || moving {
            let mut edit = Edit::new(track);
            edit.default = default;
            edit.language = new_language;
            edit.move_to = moving.then_some(1);
            edits.push(edit);
        }
    }
    edits
}

/// The tracks the edits move, with the place each goes to.
fn moves_of(edits: &[Edit]) -> Vec<(Track, usize)> {
    edits.iter().filter_map(|e| e.move_to.map(|to| (e.track.clone(), to))).collect()
}

/// The tracks of each kind, in the order they will have in the new file; `replaced`
/// gives the new list for a kind that changes.
fn kept_tracks(all: &[Track], replaced: Option<(Kind, Vec<Track>)>) -> Vec<(Kind, Vec<Track>)> {
    let mut kept: Vec<(Kind, Vec<Track>)> = Vec::new();
    for track in all {
        match kept.iter_mut().find(|(kind, _)| *kind == track.kind) {
            Some((_, list)) => list.push(track.clone()),
            None => kept.push((track.kind, vec![track.clone()])),
        }
    }
    if let Some((kind, tracks)) = replaced {
        match kept.iter_mut().find(|(k, _)| *k == kind) {
            Some(slot) => slot.1 = tracks,
            None => kept.push((kind, tracks)),
        }
    }
    kept
}

/// ffmpeg options that write the track names back into a rewritten MP4.
///
/// ffmpeg drops the names it cannot read from an MP4 (the ones mediainfo reports as the
/// title), so each is written again, at the track's place in the new file. `renamed`
/// gives new names for tracks being renamed (an empty name removes it).
fn name_args(
    kept: &[(Kind, Vec<Track>)],
    renamed: &HashMap<(Kind, usize), String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (kind, tracks) in kept {
        for (index, track) in tracks.iter().enumerate() {
            let name = renamed.get(&track.key()).unwrap_or(&track.name);
            if !name.is_empty() {
                out.push(format!("-metadata:s:{}:{}", kind.selector(), index));
                out.push(format!("title={name}"));
            }
        }
    }
    out
}

/// Point the entries of a `--log` file that are about `from` at `to`, after the file was renamed,
/// so that `--undo` still finds it. Other lines are kept as they are. The log is rewritten
/// through a temporary file, so it is never left half written. Returns how many entries were
/// changed; a log that does not exist has none.
pub fn rename_in_log(log: &str, from: &str, to: &str) -> io::Result<usize> {
    let text = match fs::read_to_string(log) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let (from, to) = (abspath(from), abspath(to));
    let mut changed = 0;
    let lines: Vec<String> = text
        .lines()
        .map(|line| match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(mut entry)) if entry.get("file").and_then(Value::as_str) == Some(&from) => {
                entry.insert("file".to_string(), json!(to));
                changed += 1;
                Value::Object(entry).to_string()
            }
            _ => line.to_string(),
        })
        .collect();
    if changed > 0 {
        let temp_file = format!("{log}{TEMP_MARK}");
        let mut out = lines.join("\n");
        if text.ends_with('\n') {
            out.push('\n');
        }
        let written = fs::write(&temp_file, out)
            .and_then(|()| copy_stat(Path::new(log), Path::new(&temp_file)))
            .and_then(|()| fs::rename(&temp_file, log));
        if let Err(error) = written {
            let _ = fs::remove_file(&temp_file);
            return Err(error);
        }
    }
    Ok(changed)
}

/// The tracks before and after a change, for a front end that shows what it does. It goes in the
/// result's `data` as `{"layout": {"before": [...], "after": [...]}}`.
fn layout_data(before: &[Track], after: &[Track]) -> Value {
    json!({"layout": {"before": before, "after": after}})
}

/// The commands an action needs for a file of this kind (the ones `require` checks before it starts).
fn tools_for(task: &str, container: Container) -> &'static [&'static str] {
    if task == "search" {
        // only curl is needed, and that is checked when the request is made
        &[]
    } else if container == Container::Mkv && cfg!(feature = "native-mkv") && matches!(task, "set" | "delete" | "preserve") {
        // the flags, names, order and removals of an MKV are written in this program (see ebml_edit)
        &[]
    } else if container == Container::Avi && cfg!(feature = "native-avi") && matches!(task, "info" | "get" | "delete" | "preserve") {
        // an AVI's streams are read from its header in this program (see avi::stream_tracks), and its streams are
        // removed in this program too
        &[]
    } else if matches!(task, "set" | "swap") && container == Container::Avi && cfg!(feature = "native-avi") {
        // an AVI's audio streams are put in order in this program (see avi); nothing else is changed for an AVI
        container.read_tools()
    } else if task == "set" && container == Container::Mp4 && cfg!(feature = "native-mp4") {
        // an MP4's languages, default flags, order, names and subtitle forced flags are changed in place; any other
        // change needs ffmpeg, which edit_tracks checks for when it is used
        container.read_tools()
    } else if task == "add" && container == Container::Mp4 && cfg!(feature = "native-mp4") {
        // SubRip files are added to an MP4 in this program; any other file is converted by ffmpeg, which add_subtitles
        // checks for when it is used
        container.read_tools()
    } else if matches!(task, "delete" | "preserve") && container == Container::Mp4 && cfg!(feature = "native-mp4") {
        // an MP4's tracks are removed in this program (see mp4::remove_traks)
        container.read_tools()
    } else if matches!(task, "delete" | "preserve") && container == Container::Avi && cfg!(feature = "native-avi") {
        // an AVI's streams are removed in this program (see avi::remove_streams)
        container.read_tools()
    } else if task == "add" && container == Container::Mkv && cfg!(feature = "native-mkv") {
        // SubRip files are added in this program; other subtitle files are checked for the tools they need (see add_subtitles)
        &[]
    } else if task == "detect" && container == Container::Mkv && native_detect() {
        // the clips of an MKV are made in this program on macOS (see clip); whisper.cpp is checked by the action
        &[]
    } else if task == "extract" && container == Container::Mkv && cfg!(feature = "native-mkv") {
        // SubRip and SubStationAlpha tracks are written in this program; any other track still needs mkvextract or ffmpeg,
        // which the action reports when it runs without them
        &[]
    } else if matches!(task, "extract" | "detect") && container == Container::Mkv {
        // subtitles are extracted and clips cut with ffmpeg, even when the tracks are read natively
        &["ffmpeg"]
    } else if matches!(task, "info" | "get" | "extract" | "detect") {
        container.read_tools()
    } else {
        container.write_tools()
    }
}

/// Whether the clips of an MKV are decoded in this program: the native-detect feature, on macOS (the only place
/// with the decoder, see audio_decode).
fn native_detect() -> bool {
    cfg!(all(feature = "native-detect", target_os = "macos"))
}

/// Whether a subtitle file is SubRip, by its name.

/// Whether a subtitle file is one this program writes to an MKV in its own codec (SubRip, ASS, SSA or WebVTT).
fn is_native_mkv_text(file: &str) -> bool {
    Path::new(file).extension().is_some_and(|ext| ["srt", "ass", "ssa", "vtt"].iter().any(|k| ext.eq_ignore_ascii_case(k)))
}

/// Whether a subtitle file can be written to an MKV in this program: a file that is not, for a reason this program
/// can't write (a WebVTT note after its first cue), is left to mkvmerge, and so is not an error here.
fn mkv_text_writable(path: &Path) -> bool {
    match read_mkv_text(path) {
        Err(error) => error.kind() != io::ErrorKind::Unsupported,
        Ok(_) => true,
    }
}

/// A subtitle file as an MKV track: its codec ID, the codec private data (the ASS header; none for SubRip), and its
/// cues (the payload of an ASS cue is the block's, see `text_subs::read_ass_for_mkv`).
fn read_mkv_text(path: &Path) -> io::Result<(&'static str, Option<Vec<u8>>, Vec<crate::ebml_edit::NewCue>)> {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if extension == "vtt" {
        let (header, cues) = crate::text_subs::read_vtt_for_mkv(path)?;
        let cues = cues
            .into_iter()
            .map(|(start_ms, end_ms, text, additions)| crate::ebml_edit::NewCue { start_ms, end_ms, text, additions })
            .collect();
        return Ok(("S_TEXT/WEBVTT", Some(header), cues));
    }
    if extension == "ass" || extension == "ssa" {
        let (header, cues) = if extension == "ssa" {
            crate::text_subs::read_ssa_for_mkv(path)?
        } else {
            crate::text_subs::read_ass_for_mkv(path)?
        };
        let cues = cues
            .into_iter()
            .map(|(start_ms, end_ms, text)| crate::ebml_edit::NewCue { start_ms, end_ms, text, additions: vec![] })
            .collect();
        let codec = if extension == "ssa" { "S_TEXT/SSA" } else { "S_TEXT/ASS" };
        return Ok((codec, Some(header), cues));
    }
    Ok(("S_TEXT/UTF8", None, crate::ebml_subs::read_srt(path)?))
}

/// Whether a subtitle file is one this program reads and writes to an MP4 (SubRip, SubStationAlpha or WebVTT).
fn is_native_text(file: &str) -> bool {
    Path::new(file)
        .extension()
        .is_some_and(|ext| ["srt", "ass", "ssa", "vtt"].iter().any(|k| ext.eq_ignore_ascii_case(k)))
}

/// The cues of a subtitle file (start, end, text in milliseconds), by its format.
fn text_cues(path: &Path) -> io::Result<Vec<(i64, i64, Vec<u8>)>> {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "srt" => Ok(crate::ebml_subs::read_srt(path)?
            .into_iter()
            .map(|c| (c.start_ms, c.end_ms, c.text))
            .collect()),
        "ass" | "ssa" => crate::text_subs::read_ass(path),
        "vtt" => crate::text_subs::read_vtt(path),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "not a subtitle format this program reads")),
    }
}

/// The extension a subtitle track of this codec is extracted with, or `None` if it can't be
/// extracted to a single file (VobSub is two files). A text subtitle in an MP4 is written as `.srt`.
pub fn subtitle_extension(container: Container, codec: &str) -> Option<&'static str> {
    let codec = codec.to_lowercase();
    if container == Container::Mp4 {
        // the text formats an MP4 holds that this program reads (see mp4::read_text_cues); CEA-608 captions are not
        let text = ["timed text", "tx3g", "mov_text", "wvtt", "webvtt", "stpp", "ttml"];
        return text.iter().any(|k| codec.contains(k)).then_some("srt");
    }
    if codec.contains("subrip") || codec.contains("srt") {
        Some("srt")
    } else if codec.contains("substation") || codec == "ass" || codec == "ssa" {
        Some("ass")
    } else if codec.contains("webvtt") {
        Some("vtt")
    } else if codec.contains("pgs") {
        Some("sup")
    } else if codec.contains("dvd_subtitle") || codec.contains("vobsub") {
        // a VobSub is two files: the index (this name) and the .sub beside it
        Some("idx")
    } else {
        None
    }
}

/// The track that becomes the default when the default is removed.
///
/// If none of the tracks left has the default flag, the first one gets it. That is
/// always done for audio, so a file isn't left without a default track, and for an MP4,
/// where the muxer insists on a default track for every kind. An MKV may have no default
/// subtitle track, so none is made. AVI has no flags.
fn promotion(container: Container, survivors: &[Track]) -> Option<Track> {
    if container == Container::Avi || survivors.is_empty() || survivors.iter().any(|t| t.default) {
        return None;
    }
    let first = &survivors[0];
    (first.kind == Kind::Audio || container == Container::Mp4).then(|| first.clone())
}

/// A track name as comparable text: mkvtoolnix on macOS stores text in decomposed form
/// (`c` and a combining cedilla for `ç`), so the name read back is not byte for byte the name given.
fn same_name(a: &str, b: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;
    a.trim().nfc().eq(b.trim().nfc())
}

fn same_state(first: &[Track], second: &[Track]) -> bool {
    first.len() == second.len()
        && first.iter().zip(second).all(|(a, b)| {
            (a.kind, &a.language, a.default, a.forced) == (b.kind, &b.language, b.default, b.forced)
                && same_name(&a.name, &b.name)
        })
}

/// Whether a path (below the directory) matches one of the patterns, ignoring case.
///
/// A pattern with no `/` matches the file name or the name of any folder above it, so
/// `Extras` skips everything in a folder of that name. A pattern with a `/` is matched
/// against the whole path, as in `Season 1/*`.
pub fn pattern_hit(relative: &Path, patterns: &[String]) -> bool {
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let text = parts.join("/");
    patterns.iter().map(|p| p.to_lowercase()).any(|pattern| {
        if pattern.contains('/') {
            fnmatch(&pattern, &text)
        } else {
            parts.iter().any(|part| fnmatch(&pattern, part))
        }
    })
}

/// Every path in a folder, and below it when `recursive`. A symbolic link to a folder is followed
/// like a folder, but a folder reached twice (a link back up the tree, say) is only read once.
fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let mut seen = std::collections::HashSet::new();
    walk_into(dir, recursive, out, &mut seen);
}

fn walk_into(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>, seen: &mut std::collections::HashSet<PathBuf>) {
    if let Ok(real) = fs::canonicalize(dir) {
        if !seen.insert(real) {
            return;
        }
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        out.push(path.clone());
        if recursive && path.is_dir() {
            walk_into(&path, recursive, out, seen);
        }
    }
}

impl Mau {
    pub fn new(config: Config, sink: Sink) -> Mau {
        Mau {
            config,
            shared: Arc::new(Shared::default()),
            sink,
            installer: detect_installer(),
            model: detect::model_path(),
        }
    }

    /// Match the subtitle files in `--sub-dir` to the media files that are going to be processed
    /// (all of them at once, so that the longest name wins), and say what is left over.
    fn plan_subtitle_folder(&mut self, paths: &[String]) {
        let files = subtitles::subtitle_files_in(Path::new(&self.config.sub_dir));
        let plan = subtitles::plan_from_folder(paths, &files, &self.config.lang);
        for file in &plan.unmatched {
            self.warn(&format!("no media file matches the subtitle {file}"));
        }
        for file in &plan.no_language {
            self.warn(&format!(
                "the language of the subtitle {file} is not in its name (give --lang to use one for such files)"
            ));
        }
        self.config.sub_matches = plan.specs;
    }

    /// Of the subtitles matched from a folder, those that can be added to this file: not ones it
    /// has already (a track in the same language with the same name), and for an MP4 not ones that
    /// can't be converted. Each one left out is said.
    fn addable_from_folder(&self, media: &Media, all: &[Track], specs: Vec<SubSpec>) -> Vec<SubSpec> {
        let mut keep = Vec::new();
        for spec in specs {
            let file = Path::new(&spec.file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let has = all.iter().any(|t| {
                t.kind == Kind::Subtitles && t.language == spec.lang && same_name(&t.name, &spec.name)
            });
            if has {
                self.warn(&format!(
                    "{}: already has a subtitle track in {}{}, so {file} was not added",
                    media.path,
                    spec.lang,
                    if spec.name.is_empty() { String::new() } else { format!(" named \"{}\"", spec.name) }
                ));
            } else if let Some(why) = media.container.eq(&Container::Mp4).then(|| Self::mp4_subtitle_refusal(std::slice::from_ref(&spec))).flatten() {
                self.warn(&format!("{}: {file} was not added: {why}", media.path));
            } else {
                keep.push(spec);
            }
        }
        keep
    }

    /// The same engine, with the settings of one action on top of its own.
    fn for_task(&self, task: &Task) -> Mau {
        Mau {
            config: self.config.for_task(task),
            shared: Arc::clone(&self.shared),
            sink: Arc::clone(&self.sink),
            installer: self.installer,
            model: self.model.clone(),
        }
    }

    /// The same engine, working on another kind of track.
    fn with_kind(&self, kind: Kind) -> Mau {
        let mut config = self.config.clone();
        config.kind = kind;
        Mau {
            config,
            shared: Arc::clone(&self.shared),
            sink: Arc::clone(&self.sink),
            installer: self.installer,
            model: self.model.clone(),
        }
    }

    fn kind(&self) -> Kind {
        self.config.kind
    }

    fn word(&self) -> &'static str {
        self.config.kind.word()
    }

    // Messages and commands ----------------------------------------------------

    pub fn warn(&self, message: &str) {
        (self.sink)(Event::Warn(message.to_string()));
    }

    /// A message shown with --verbose.
    fn note(&self, message: &str) {
        if self.config.verbose {
            (self.sink)(Event::Note(message.to_string()));
        }
    }

    /// Run a read-only command (also during a dry run). `None` if it can't be run.
    fn query(&self, cmd: &[String]) -> Res<Option<Completed>> {
        self.note(&format!("Executing:\t{}", shell_join(cmd)));
        match run_command(cmd) {
            Ok(Ran::Done(done)) => Ok(Some(done)),
            Ok(Ran::Cancelled) => Err(Stop::Interrupted),
            Err(_) => Ok(None),
        }
    }

    /// Run a command that changes things, unless this is a dry run. The command is
    /// recorded in the result. On failure the result becomes an error.
    fn apply(&self, result: &mut Outcome, cmd: Vec<String>, ok_codes: &[i32]) -> Res<bool> {
        self.note(&format!("Executing:\t{}", shell_join(&cmd)));
        result.commands.push(cmd.clone());
        if self.config.dryrun {
            return Ok(true);
        }
        match run_command(&cmd) {
            Err(error) => {
                result.error(format!("{} failed: {}", cmd[0], strerror(&error)));
                Ok(false)
            }
            Ok(Ran::Cancelled) => Err(Stop::Interrupted),
            Ok(Ran::Done(done)) if !ok_codes.contains(&done.code) => {
                let text = if done.stderr.trim().is_empty() { &done.stdout } else { &done.stderr };
                let last = text.trim().lines().last().map(str::to_string);
                result.error(format!(
                    "{} failed with exit code {}{}",
                    cmd[0],
                    done.code,
                    last.map(|l| format!(": {l}")).unwrap_or_default()
                ));
                Ok(false)
            }
            Ok(Ran::Done(_)) => Ok(true),
        }
    }

    // Environment --------------------------------------------------------------

    /// Where a command is, if it is on `PATH`.
    fn find(&self, tool: &str) -> Option<PathBuf> {
        (self.shared.finder)(tool)
    }

    fn install_package(&self, package: &str) {
        let Some(installer) = self.installer else {
            let hint = if cfg!(windows) { " (install winget or Chocolatey)" } else { "" };
            self.warn(&format!("don't know how to install \"{package}\" on this OS{hint}"));
            return;
        };
        if !installer.supports(package) {
            self.warn(&format!(
                "there is no \"{package}\" package for {}: install it by hand (see https://github.com/ggml-org/whisper.cpp)",
                installer.name()
            ));
            return;
        }
        let cmd = installer.command(package);
        self.note(&format!("Executing:\t{}", shell_join(&cmd)));
        if !self.config.dryrun {
            // found through PATHEXT on Windows (winget is winget.exe)
            let program = which(&cmd[0]).unwrap_or_else(|| PathBuf::from(&cmd[0]));
            let _ = Command::new(program).args(&cmd[1..]).status();
        }
    }

    fn install_missing(&self, missing: &[String]) {
        let mut done: Vec<&str> = Vec::new();
        for tool in missing {
            let required = required_tools();
            if let Some(&(_, package)) = required.iter().chain(OPTIONAL_TOOLS.iter()).find(|(t, _)| t == tool) {
                if !done.contains(&package) {
                    done.push(package);
                    self.install_package(package);
                }
            }
        }
        // the installers change PATH for programs started later, not for this one
        if !self.config.dryrun && !done.is_empty() {
            crate::util::add_tool_folders();
        }
    }

    /// Where a tool is. `whisper-cli` is also looked for as `whisper-cpp`, and `MAU_WHISPER` names
    /// it when it is set (and then only that file is used, as `--detect` does).
    fn find_tool(&self, tool: &str) -> Option<PathBuf> {
        if tool != "whisper-cli" {
            return self.find(tool);
        }
        match crate::util::mau_env("WHISPER") {
            Ok(path) if !path.trim().is_empty() => Some(PathBuf::from(path)).filter(|p| p.is_file()),
            _ => self.find("whisper-cli").or_else(|| self.find("whisper-cpp")),
        }
    }

    /// Report which of the commands are installed: the required ones, and the optional ones
    /// (`whisper-cli`, which `--detect` runs). A missing optional one is reported but is not an
    /// error. The model `--detect` reads (`MAU_WHISPER_MODEL`) is reported too, and is not
    /// installed: it is a large download.
    pub fn check_tools(&self) -> Res<Outcome> {
        let find = || -> Vec<(String, Option<PathBuf>)> {
            required_tools()
                .into_iter()
                .map(|(t, _)| t)
                .chain(OPTIONAL_TOOLS.iter().map(|(t, _)| *t))
                .map(|t| (t.to_string(), self.find_tool(t)))
                .collect()
        };
        let mut found = find();
        if !missing_in(&found, false).is_empty() && self.config.install {
            self.install_missing(&missing_in(&found, false));
            if !self.config.dryrun {
                found = find();
            }
        }
        let mut result = Outcome::new("", "check", self.config.dryrun);
        if self.config.install {
            self.install_model(&mut result)?;
        }
        let required_missing = missing_in(&found, true);
        let mut entries: Vec<Value> = found
            .iter()
            .map(|(tool, path)| {
                let package = required_tools()
                    .into_iter()
                    .chain(OPTIONAL_TOOLS.iter().copied())
                    .find(|(t, _)| t == tool)
                    .map(|(_, p)| p);
                json!({"tool": tool, "path": path.as_ref().map(|p| p.to_string_lossy()),
                       "package": package, "optional": !is_required(tool)})
            })
            .collect();
        let model = self.model.clone().filter(|p| p.is_file());
        entries.push(json!({"tool": "MAU_WHISPER_MODEL",
                            "path": model.as_ref().map(|p| p.to_string_lossy()),
                            "package": null, "optional": true}));
        result.data = Value::Array(entries);
        if !required_missing.is_empty() && !(self.config.install && self.config.dryrun) {
            let mut message = format!("missing: {}", required_missing.join(", "));
            if !self.config.install {
                message.push_str(" (use --install to install them)");
            }
            result.error(message);
        }
        Ok(result)
    }

    /// `--install` also gets the model `--detect` reads, when there is none (see `detect::model_path`).
    /// It is downloaded only when the disk has room for it, with a tenth to spare: otherwise the
    /// user is told and nothing is downloaded. A dry run only says what it would do. The download
    /// is checked against the published SHA-256 before it takes the place of anything.
    fn install_model(&self, result: &mut Outcome) -> Res<()> {
        let Some(path) = self.model.clone() else {
            self.warn("no place to keep the whisper.cpp model: set MAU_WHISPER_MODEL to a file");
            return Ok(());
        };
        if path.is_file() {
            return Ok(());
        }
        let megabytes = |bytes: u64| bytes / 1_000_000;
        // the model goes on the disk of the nearest folder that exists
        let mut folder = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        while !folder.exists() {
            match folder.parent().filter(|p| !p.as_os_str().is_empty()) {
                Some(parent) => folder = parent.to_path_buf(),
                None => break,
            }
        }
        let free = match crate::util::disk_free(&folder) {
            Ok((free, _)) => free,
            Err(error) => {
                self.warn(&format!(
                    "could not check the free space at {}, so the whisper.cpp model is not downloaded: {error}",
                    folder.display()
                ));
                return Ok(());
            }
        };
        if !detect::room_for(free, detect::MODEL_BYTES) {
            self.warn(&format!(
                "the whisper.cpp model needs {} MB free at {}, and {} MB are free: not downloaded \
                 (free some space, or set MAU_WHISPER_MODEL to a file on a disk with room)",
                megabytes(detect::MODEL_BYTES),
                folder.display(),
                megabytes(free)
            ));
            return Ok(());
        }
        let url = crate::util::mau_env("WHISPER_MODEL_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| detect::MODEL_URL.to_string());
        if self.config.dryrun {
            self.note(&format!(
                "would download the whisper.cpp model ({} MB) from {url} to {}",
                megabytes(detect::MODEL_BYTES),
                path.display()
            ));
            return Ok(());
        }
        let temp = PathBuf::from(format!("{}{TEMP_MARK}", path.display()));
        let temp_text = temp.to_string_lossy().into_owned();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        self.pend(&temp_text);
        self.note(&format!(
            "Downloading the whisper.cpp model ({} MB) to {}",
            megabytes(detect::MODEL_BYTES),
            path.display()
        ));
        let cmd = args(&["curl", "--fail", "--location", "--silent", "--show-error", "--output", &temp_text, &url]);
        if !self.apply(result, cmd, &[0])? {
            let _ = fs::remove_file(&temp);
            return Ok(());
        }
        match detect::file_sha256(&temp) {
            Ok(hash) if hash == detect::MODEL_SHA256 => {
                match fs::rename(&temp, &path) {
                    Ok(()) => result.changes.push(format!("the whisper.cpp model is in place at {}", path.display())),
                    Err(error) => {
                        result.error(format!("could not put the model in place at {}: {error}", path.display()));
                        let _ = fs::remove_file(&temp);
                    }
                }
            }
            _ => {
                let _ = fs::remove_file(&temp);
                result.error(
                    "the downloaded whisper.cpp model is not the published file (its SHA-256 differs), so it was thrown away"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    /// Make sure commands are installed, installing them if install was asked for.
    fn require(&self, tools: &[&str]) -> Res<()> {
        let missing_now = || -> Vec<String> {
            tools.iter().filter(|t| self.find(t).is_none()).map(|t| t.to_string()).collect()
        };
        let mut missing = missing_now();
        if missing.is_empty() {
            return Ok(());
        }
        if self.config.install {
            {
                // the first thread installs, the others then find them
                let _guard = self.shared.install.lock().unwrap();
                missing = missing_now();
                if !missing.is_empty() {
                    self.install_missing(&missing);
                }
            }
            if self.config.dryrun {
                return Ok(());
            }
            missing = missing_now();
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(Stop::Fatal(format!(
                "command not found: {} (use --install to install, or --check to see what is missing)",
                missing.join(", ")
            )))
        }
    }

    // Files --------------------------------------------------------------------

    /// The file, or the files in the directory (that include and exclude allow).
    pub fn files(&self) -> Res<Vec<String>> {
        if !self.config.paths.is_empty() {
            return Ok(self.config.paths.clone());
        }
        if !self.config.file.is_empty() {
            return Ok(vec![self.config.file.clone()]);
        }
        let base = Path::new(&self.config.dir);
        if !base.is_dir() {
            return Err(Stop::Fatal(format!("directory \"{}\" does not exist", self.config.dir)));
        }
        let mut paths = Vec::new();
        walk(base, self.config.recursive, &mut paths);
        paths.sort();
        let mut files = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for path in paths {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !path.is_file() || name.contains(TEMP_MARK) || name.contains(BACKUP_MARK) {
                continue;
            }
            let relative = path.strip_prefix(base).unwrap_or(&path);
            if !self.config.include.is_empty() && !pattern_hit(relative, &self.config.include) {
                continue;
            }
            if pattern_hit(relative, &self.config.exclude) {
                continue;
            }
            // two links to one file are one file: it is listed once, by the first name
            let text = path.to_string_lossy().into_owned();
            let real = fs::canonicalize(&path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| text.clone());
            if seen.insert(real) {
                files.push(text);
            }
        }
        if files.is_empty() {
            self.warn(&format!("no media files found in \"{}\"", self.config.dir));
        }
        Ok(files)
    }

    /// Read the tracks of a file.
    pub fn read_tracks(&self, media: &Media) -> Res<Vec<Track>> {
        // with the native-mkv feature an MKV's tracks are read from its header in this program (see `ebml`),
        // with no tool run; otherwise ffprobe reads them, as before
        if cfg!(feature = "native-mkv") && media.container == Container::Mkv {
            return match crate::ebml::read_file_tracks(Path::new(&media.path)) {
                Ok(tracks) => Ok(tracks),
                Err(_) => {
                    self.warn(&format!("could not read the tracks of {}", media.path));
                    Ok(Vec::new())
                }
            };
        }
        // with the native-avi feature an AVI's streams are read from its header in this program (see avi::stream_tracks)
        if cfg!(feature = "native-avi") && media.container == Container::Avi {
            return match crate::avi::stream_tracks(Path::new(&media.path)) {
                Ok(tracks) => Ok(tracks),
                Err(error) => {
                    self.warn(&format!("could not read the streams of {}: {}", media.path, strerror(&error)));
                    Ok(Vec::new())
                }
            };
        }
        let mkv_by_ffprobe = media.container == Container::Mkv;
        let cmd = if mkv_by_ffprobe {
            args(&["ffprobe", "-v", "error", "-show_streams", "-of", "json", &media.path])
        } else {
            args(&["mediainfo", "--Output=JSON", &media.path])
        };
        let Some(done) = self.query(&cmd)? else { return Ok(Vec::new()) };
        let parsed = serde_json::from_str::<Value>(&done.stdout)
            .map_err(|e| e.to_string())
            .and_then(|data| {
                if mkv_by_ffprobe {
                    crate::track::parse_ffprobe_tracks(&data)
                } else if media.container == Container::Mkv {
                    parse_mkv_tracks(&data)
                } else {
                    parse_mediainfo_tracks(&data)
                }
            });
        match parsed {
            Ok(tracks) => Ok(tracks),
            Err(_) => {
                self.warn(&format!("could not read the tracks of {}", media.path));
                Ok(Vec::new())
            }
        }
    }

    /// The tracks of a file (read once, then remembered).
    fn tracks(&self, media: &mut Media) -> Res<Vec<Track>> {
        if media.tracks.is_none() {
            media.tracks = Some(self.read_tracks(media)?);
        }
        Ok(media.tracks.clone().unwrap_or_default())
    }

    /// The tracks of the kind being worked on.
    fn selected(&self, media: &mut Media) -> Res<Vec<Track>> {
        Ok(self.tracks(media)?.into_iter().filter(|t| t.kind == self.kind()).collect())
    }

    fn result(&self, media: &Media, action: &str) -> Outcome {
        Outcome::new(&media.path, action, self.config.dryrun)
    }

    /// The track chosen with --track (the first if none was).
    fn resolve_track(&self, media: &mut Media, result: &mut Outcome) -> Res<Option<Track>> {
        let Some(track) = self.resolve_any_track(media, result)? else { return Ok(None) };
        if track.kind != self.kind() {
            mark_skipped(
                result,
                &format!(
                    "{} is a {} track, not {} (use --kind {})",
                    self.config.track,
                    track.kind.word(),
                    self.word(),
                    track.kind.name()
                ),
            );
            return Ok(None);
        }
        Ok(Some(track))
    }

    /// The track chosen with --track, of whatever kind: a position among the tracks of the kind
    /// being worked on, or `audio:N`/`subtitles:N`, `id:N` or `name:TEXT`.
    fn resolve_any_track(&self, media: &mut Media, result: &mut Outcome) -> Res<Option<Track>> {
        let all = self.tracks(media)?;
        let text = if self.config.track.is_empty() { "1" } else { self.config.track.as_str() };
        let spec = match TrackSpec::parse(text) {
            Ok(spec) => spec,
            Err(message) => {
                mark_skipped(result, &message);
                return Ok(None);
            }
        };
        let of_kind = |kind: Kind| -> Vec<Track> { all.iter().filter(|t| t.kind == kind).cloned().collect() };
        let by_position = |kind: Kind, wanted: Option<usize>| -> Result<Track, String> {
            let tracks = of_kind(kind);
            let number = wanted.unwrap_or(tracks.len());
            if number < 1 || number > tracks.len() {
                return Err(format!(
                    "has no {w} track {text} ({n} {w} tracks)",
                    w = kind.word(),
                    n = tracks.len()
                ));
            }
            Ok(tracks[number - 1].clone())
        };
        let found = match &spec {
            TrackSpec::Number(n) => by_position(self.kind(), Some(*n)),
            TrackSpec::Last => by_position(self.kind(), None),
            TrackSpec::Of(kind, n) => by_position(*kind, *n),
            TrackSpec::Id(id) => all
                .iter()
                .find(|t| t.id == *id)
                .cloned()
                .ok_or_else(|| format!("has no track with ID {id}")),
            TrackSpec::Name(name) => {
                let hits: Vec<&Track> =
                    all.iter().filter(|t| t.name.to_lowercase() == name.to_lowercase()).collect();
                match hits.len() {
                    0 => Err(format!("has no track named \"{name}\"")),
                    1 => Ok(hits[0].clone()),
                    n => Err(format!(
                        "has {n} tracks named \"{name}\" (choose one with id:N, for ID {})",
                        hits.iter().map(|t| t.id.to_string()).collect::<Vec<_>>().join(", ")
                    )),
                }
            }
        };
        match found {
            Ok(track) => Ok(Some(track)),
            Err(message) => {
                mark_skipped(result, &message);
                Ok(None)
            }
        }
    }

    /// Check there is room beside a file to work on a copy of it as big as the file.
    ///
    /// The room is set aside until the file is done (`release_space`), so that several
    /// jobs at once can't each find enough and then use it up between them.
    fn check_space(&self, result: &mut Outcome, path: &str, what: &str) -> bool {
        let checked = (|| -> io::Result<(i64, u64, i64)> {
            let need = fs::metadata(path)?.len() as i64;
            let folder = fs::canonicalize(path)?.parent().map(Path::to_path_buf).unwrap_or_default();
            let (free, device) = (self.shared.disk)(&folder)?;
            Ok((need, device, free as i64))
        })();
        let (need, device, free_now) = match checked {
            Ok(values) => values,
            Err(error) => {
                result.error(format!("could not check the free space: {}", strerror(&error)));
                return false;
            }
        };
        let mut space = self.shared.space.lock().unwrap();
        let free = free_now - space.reserved.get(&device).copied().unwrap_or(0);
        if free >= need {
            *space.reserved.entry(device).or_insert(0) += need;
            space.holds.entry(path.to_string()).or_default().push((device, need));
            return true;
        }
        result.changes.clear();
        result.error(format!(
            "not enough free space to {what}: needs {}, {} free",
            human_size(need as f64),
            human_size(free.max(0) as f64)
        ));
        false
    }

    /// Give back the room a file set aside.
    fn release_space(&self, path: &str) {
        let mut space = self.shared.space.lock().unwrap();
        if let Some(holds) = space.holds.remove(path) {
            for (device, need) in holds {
                *space.reserved.entry(device).or_insert(0) -= need;
            }
        }
    }

    fn pend(&self, temp_file: &str) {
        self.shared.pending.lock().unwrap().push(temp_file.to_string());
    }

    /// Copy a file, so the copy can be changed and swapped in if that works.
    /// Run `work`, which writes `temp_file`, and while it does tell the front end how big the
    /// file has got compared with `source` (a copy, rewrite or removal makes a file about that
    /// size). It tells nothing unless asked (`Config::progress`) or in a dry run.
    fn track_progress<T>(&self, file: &str, source: &str, temp_file: &str, stage: &str, work: impl FnOnce() -> T) -> T {
        if !self.config.progress || self.config.dryrun {
            return work();
        }
        let total = fs::metadata(source).map(|m| m.len()).unwrap_or(0);
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // the largest size seen so far: a rewrite truncates the file before it fills it again,
                // so a single reading can be 0, and the bar must not go back
                let mut highest = 0;
                while !stop.load(Ordering::SeqCst) {
                    let size = fs::metadata(temp_file).map(|m| m.len()).unwrap_or(0);
                    highest = highest.max(size.min(total));
                    (self.sink)(Event::Progress {
                        file: file.to_string(),
                        stage: stage.to_string(),
                        done: highest,
                        total,
                    });
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            });
            let outcome = work();
            stop.store(true, Ordering::SeqCst);
            outcome
        })
    }

    /// `apply` for a command that writes `temp_file` from `source`, with progress.
    fn apply_tracked(
        &self,
        result: &mut Outcome,
        cmd: Vec<String>,
        ok_codes: &[i32],
        source: &str,
        temp_file: &str,
        stage: &str,
    ) -> Res<bool> {
        self.track_progress(source, source, temp_file, stage, || self.apply(result, cmd, ok_codes))
    }

    fn start_copy(&self, result: &mut Outcome, path: &str, temp_file: &str) -> Res<bool> {
        if !self.check_space(result, path, "copy the file") {
            return Ok(false);
        }
        self.pend(temp_file);
        self.note(&format!("Copying:\t{path} -> {temp_file}"));
        if self.config.dryrun {
            return Ok(true);
        }
        let copied = self.track_progress(path, path, temp_file, "Copying", || {
            copy_file(Path::new(path), Path::new(temp_file))
        });
        match copied {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(Stop::Interrupted),
            Err(error) => {
                let _ = fs::remove_file(temp_file);
                result.error(format!("could not copy the file: {}", strerror(&error)));
                Ok(false)
            }
        }
    }

    /// Where the original is kept: the file's name and `.fml-bak`, or a numbered one if
    /// that is taken.
    fn backup_path(path: &str) -> String {
        let mut backup = format!("{path}{BACKUP_MARK}");
        let mut number = 0;
        while fs::symlink_metadata(&backup).is_ok() {
            number += 1;
            backup = format!("{path}{BACKUP_MARK}.{number}");
        }
        backup
    }

    /// Keep the original as a backup. A hard link costs nothing, and is only possible
    /// while the file is going to be replaced, not edited; if it fails, or a copy is
    /// needed, the file is copied. Returns false (and an error result) if it can't be.
    fn keep_original(&self, result: &mut Outcome, path: &str, link: bool) -> Res<bool> {
        let backup = Self::backup_path(path);
        result.backup = backup.clone();
        result.changes.push(format!(
            "the original {} kept as {backup}",
            if self.config.dryrun { "would be" } else { "is" }
        ));
        if self.config.dryrun {
            return Ok(true);
        }
        if !link || fs::hard_link(path, &backup).is_err() {
            if !self.check_space(result, path, "keep a backup") {
                return Ok(false);
            }
            match copy_file(Path::new(path), Path::new(&backup)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    let _ = fs::remove_file(&backup);
                    return Err(Stop::Interrupted);
                }
                Err(error) => {
                    let _ = fs::remove_file(&backup);
                    result.error(format!("could not keep a backup: {}", strerror(&error)));
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Replace a file with its changed copy, keeping its permissions and times.
    ///
    /// On an error the copy is removed and the original is left as it was. With a backup
    /// the original is kept first, and if that fails nothing is replaced.
    fn replace_file(&self, result: &mut Outcome, temp_file: &str, path: &str) -> Res<()> {
        if self.config.backup && result.status != Status::Error {
            self.keep_original(result, path, true)?;
        }
        if self.config.dryrun {
            return Ok(());
        }
        if result.status == Status::Error {
            let _ = fs::remove_file(temp_file);
        } else if let Err(error) = copy_stat(Path::new(path), Path::new(temp_file))
            .and_then(|()| fs::rename(temp_file, path))
        {
            let _ = fs::remove_file(temp_file);
            result.error(format!("could not replace the file: {}", strerror(&error)));
        }
        self.shared.pending.lock().unwrap().retain(|p| p != temp_file);
        Ok(())
    }

    /// Remove any temporary file left when we are stopped part way.
    pub fn cleanup(&self) {
        let mut pending = self.shared.pending.lock().unwrap();
        for temp_file in pending.iter() {
            let _ = fs::remove_file(temp_file);
        }
        pending.clear();
    }

    // Choosing tracks ----------------------------------------------------------

    /// The track to use for a language: with --forced the first forced track, otherwise
    /// the first one (preferring a track that is not forced, for subtitles).
    fn pick_track(&self, tracks: &[Track], lang: &str) -> Option<Track> {
        let mut candidates: Vec<&Track> = tracks.iter().filter(|t| t.language == lang).collect();
        if self.config.forced {
            candidates.retain(|t| t.forced);
        } else if self.kind() == Kind::Subtitles {
            let unforced: Vec<&Track> = candidates.iter().copied().filter(|t| !t.forced).collect();
            if !unforced.is_empty() {
                candidates = unforced;
            }
        }
        candidates.first().map(|t| (*t).clone())
    }

    /// The languages to look for, in order: --prefer, or the single language.
    fn languages(&self, single: &str) -> Vec<String> {
        if !self.config.prefer.is_empty() {
            self.config.prefer.clone()
        } else if !single.is_empty() {
            vec![single.to_string()]
        } else {
            Vec::new()
        }
    }

    /// The first language that has a track, with that track.
    fn choose(&self, tracks: &[Track], languages: &[String]) -> Option<(String, Track)> {
        languages
            .iter()
            .find_map(|lang| self.pick_track(tracks, lang).map(|track| (lang.clone(), track)))
    }

    fn missing_message(&self, languages: &[String]) -> String {
        if languages.len() == 1 {
            format!("has no {} track with language \"{}\"", self.word(), languages[0])
        } else {
            format!("has no {} track in any of: {}", self.word(), languages.join(", "))
        }
    }

    // Editing tracks -----------------------------------------------------------

    /// The tracks the file should have after the edits.
    fn expected_layout(&self, all: &[Track], edits: &[Edit]) -> Vec<Track> {
        let by_track: HashMap<(Kind, usize), &Edit> =
            edits.iter().map(|e| (e.track.key(), e)).collect();
        reordered_by(all, &moves_of(edits))
            .into_iter()
            .map(|mut track| {
                if let Some(edit) = by_track.get(&track.key()) {
                    if let Some(default) = edit.default {
                        track.default = default;
                    }
                    if let Some(language) = edit.language.as_ref().filter(|l| !l.is_empty()) {
                        track.language = language.clone();
                    }
                    if let Some(name) = &edit.name {
                        track.name = name.clone();
                    }
                    if let Some(forced) = edit.forced {
                        track.forced = forced;
                    }
                }
                track
            })
            .collect()
    }

    /// Check a changed copy has the expected tracks, in order, with the expected
    /// languages and names and (for the kind being edited) default flags. A tool can exit
    /// successfully without applying a change, or can lose something, so this is checked
    /// before the copy replaces the original.
    fn verify_layout(
        &self,
        result: &mut Outcome,
        rewritten: &Media,
        expected: &[Track],
        tool: &str,
    ) -> Res<()> {
        let actual = self.read_tracks(rewritten)?;
        let wrong = actual.len() != expected.len()
            || actual.iter().zip(expected).any(|(got, want)| {
                got.kind != want.kind
                    || got.language != want.language
                    || !same_name(&got.name, &want.name)
                    || (rewritten.container != Container::Avi && got.forced != want.forced)
                    || (want.kind == self.kind() && got.default != want.default)
            });
        if wrong {
            result.error(format!("{tool} did not make the change to the file"));
        }
        Ok(())
    }

    /// Apply edits to a file.
    ///
    /// An MKV is edited with mkvpropedit, on a copy that replaces the file if it worked.
    /// If a track has to be moved, or the file is an MP4,
    /// the file is rewritten instead, with mkvmerge or ffmpeg. Either way the result is
    /// read back and checked before it replaces the original.
    fn edit_tracks(
        &self,
        media: &mut Media,
        mut result: Outcome,
        edits: Vec<Edit>,
        unchanged: &str,
    ) -> Res<Outcome> {
        if edits.is_empty() {
            result.status = Status::Unchanged;
            result.message = unchanged.to_string();
            return Ok(result);
        }
        result.changes.extend(edits.iter().map(Edit::describe));
        let moves = moves_of(&edits);
        if let [(track, _)] = moves.as_slice() {
            result.moved_from = track.number;
        } else if !moves.is_empty() {
            // several tracks move: the log keeps the old number of each track, in the new order
            let all = self.tracks(media)?;
            result.order = reordered_by(&all, &moves)
                .iter()
                .filter(|t| t.kind == self.kind())
                .map(|t| t.number)
                .collect();
        }
        // the languages, default flags and order of an MP4 are changed in place (the native-mp4 feature, see mp4)
        if cfg!(feature = "native-mp4")
            && media.container == Container::Mp4
            && edits.iter().all(|e| e.forced.is_none() || e.track.kind == Kind::Subtitles)
        {
            return self.edit_mp4_natively(media, result, &edits, &moves);
        }
        if media.container == Container::Mkv && moves.is_empty() {
            return self.edit_mkv(media, result, &edits);
        }
        if cfg!(feature = "native-mkv") && media.container == Container::Mkv {
            return self.native_rewrite(media, result, &edits);
        }
        // an MP4 is rewritten with ffmpeg here (its names and forced flags, and the removal of tracks)
        if media.container == Container::Mp4 {
            self.require(&["ffmpeg"])?;
        }
        if !self.check_space(&mut result, &media.path, "rewrite the file") {
            return Ok(result);
        }
        let all = self.tracks(media)?;
        let expected = self.expected_layout(&all, &edits);
        result.data = layout_data(&all, &expected);
        let (temp_file, cmd, tool, ok): (String, Vec<String>, &str, &[i32]) =
            // SPIKE: an MKV is rewritten with ffmpeg when mkvmerge is missing
            if media.container == Container::Mkv && self.find("mkvmerge").is_some() {
                let temp_file = format!("{}{TEMP_MARK}", media.path);
                let cmd = self.mkvmerge_command(media, &all, &edits, &temp_file);
                (temp_file, cmd, "mkvmerge", MKVMERGE_OK)
            } else {
                let temp_file = temp_path(&media.path);
                let cmd = self.ffmpeg_command(media, &all, &edits, &temp_file);
                (temp_file, cmd, "ffmpeg", &[0])
            };
        self.pend(&temp_file);
        let done = self.apply_tracked(&mut result, cmd, ok, &media.path, &temp_file, "Rewriting")?;
        if done && !self.config.dryrun {
            let rewritten = Media::new(&temp_file, media.container);
            self.verify_layout(&mut result, &rewritten, &expected, tool)?;
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Change the languages, default flags and order of an MP4 in place, in a copy (the native-mp4 feature, see
    /// `mp4::edit_in_place`). The copy is read back before it replaces the file.
    fn edit_mp4_natively(&self, media: &mut Media, mut result: Outcome, edits: &[Edit], moves: &[(Track, usize)]) -> Res<Outcome> {
        let all = self.tracks(media)?;
        let expected = self.expected_layout(&all, edits);
        result.data = layout_data(&all, &expected);
        // a track is matched to its trak box by its track ID (mediainfo's ID), since the boxes can include tracks that
        // mediainfo does not list (such as a timecode track)
        let ids = match crate::mp4::track_ids(Path::new(&media.path)) {
            Ok(ids) => ids,
            Err(error) => {
                result.error(format!("could not read the tracks: {}", strerror(&error)));
                return Ok(result);
            }
        };
        let place = |track: &Track| ids.iter().position(|&id| id as i64 == track.id);
        let temp_file = temp_path(&media.path);
        if !self.start_copy(&mut result, &media.path, &temp_file)? {
            return Ok(result);
        }
        let mut changes = Vec::new();
        for edit in edits {
            let Some(trak) = place(&edit.track) else {
                result.error("a track was not found in the file".to_string());
                return Ok(result);
            };
            changes.push(crate::mp4::TrakChange {
                trak,
                language: edit.language.clone(),
                enabled: edit.default,
                name: edit.name.clone(),
                forced: edit.forced,
            });
        }
        let order: Option<Vec<usize>> = if moves.is_empty() {
            None
        } else {
            let order: Option<Vec<usize>> = reordered_by(&all, moves).iter().map(|t| place(t)).collect();
            match order {
                Some(order) => Some(order),
                None => {
                    result.error("a track was not found in the file".to_string());
                    return Ok(result);
                }
            }
        };
        result.commands.push(vec!["native-mp4".into(), "edit-in-place".into(), temp_file.clone()]);
        self.note(&format!("Executing:\tnative-mp4 edit-in-place {temp_file}"));
        if !self.config.dryrun {
            match crate::mp4::edit_in_place(Path::new(&temp_file), &changes, order.as_deref()) {
                Ok(()) => {
                    let rewritten = Media::new(&temp_file, media.container);
                    self.verify_layout(&mut result, &rewritten, &expected, "native")?;
                }
                Err(error) => result.error(format!("could not change the tracks: {}", strerror(&error))),
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Change the default flags of an MP4 by changing the "enabled" bit in the track headers of a copy
    /// of the file, which `ffmpeg` can't do (it enables the first track of each kind again). Nothing
    /// is rewritten, so it is as quick as a copy; the copy is read back before it replaces the file.
    fn edit_mp4_flags(
        &self,
        media: &mut Media,
        mut result: Outcome,
        edits: Vec<Edit>,
        unchanged: &str,
    ) -> Res<Outcome> {
        if edits.is_empty() {
            result.status = Status::Unchanged;
            result.message = unchanged.to_string();
            return Ok(result);
        }
        result.changes.extend(edits.iter().map(Edit::describe));
        let all = self.tracks(media)?;
        let expected = self.expected_layout(&all, &edits);
        result.data = layout_data(&all, &expected);
        let temp_file = temp_path(&media.path);
        if !self.start_copy(&mut result, &media.path, &temp_file)? {
            return Ok(result);
        }
        self.note(&format!("Patching:\tthe enabled bit of {} track header(s) in {temp_file}", edits.len()));
        if !self.config.dryrun {
            let patched = mp4::read_track_flags(Path::new(&temp_file)).and_then(|flags| {
                let mut changes = Vec::new();
                for edit in &edits {
                    let Some(flag) = flags.iter().find(|f| f.kind == edit.track.kind && f.number == edit.track.number)
                    else {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "a track was not found in the file"));
                    };
                    changes.push((flag.offset, edit.default.unwrap_or(flag.enabled)));
                }
                mp4::set_enabled(Path::new(&temp_file), &changes)
            });
            match patched {
                Ok(()) => {
                    let rewritten = Media::new(&temp_file, media.container);
                    self.verify_layout(&mut result, &rewritten, &expected, "the flag change")?;
                }
                Err(error) => result.error(format!("could not change the track flags: {}", strerror(&error))),
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// SPIKE: the ffmpeg command that makes the flag, language and name changes that mkvpropedit makes, remuxing the
    /// source into `output` with -c copy. Each edited track gets its whole disposition (default and forced) and its
    /// language and title. Tracks that are not edited are copied as they are.
    fn ffmpeg_edit_command(&self, source: &str, output: &str, all: &[Track], edits: &[Edit]) -> Vec<String> {
        let selector = self.kind().selector();
        let mut cmd = args(&["ffmpeg", "-v", "error", "-y", "-i", source, "-map", "0", "-c", "copy",
            "-map_metadata", "0", "-map_chapters", "0"]);
        // the Matroska muxer makes the first track of a kind default when none is: every track that should not
        // be default is cleared first (the flags asked for are set below)
        for track in all {
            let edited = edits.iter().find(|e| e.track.key() == track.key()).and_then(|e| e.default);
            if !edited.unwrap_or(track.default) {
                cmd.extend(args(&[&format!("-disposition:{}:{}", track.kind.selector(), track.number - 1), "-default"]));
            }
        }
        for edit in edits {
            let track = &edit.track;
            // track numbers count from 1, ffmpeg's stream specifiers from 0
            let stream = format!("{selector}:{}", track.number - 1);
            // only the flags asked for change: "+flag" and "-flag" leave the others (hearing-impaired and the like) as they are
            if let Some(default) = edit.default {
                cmd.extend(args(&[&format!("-disposition:{stream}"), if default { "+default" } else { "-default" }]));
            }
            if let Some(forced) = edit.forced {
                cmd.extend(args(&[&format!("-disposition:{stream}"), if forced { "+forced" } else { "-forced" }]));
            }
            if let Some(language) = &edit.language {
                cmd.extend(args(&[&format!("-metadata:s:{stream}"), &format!("language={language}")]));
            }
            if let Some(name) = &edit.name {
                cmd.extend(args(&[&format!("-metadata:s:{stream}"), &format!("title={name}")]));
            }
        }
        cmd.extend(args(&["-f", "matroska", output]));
        cmd
    }

    /// Change flags and languages of an MKV with mkvpropedit, on a copy.
    fn edit_mkv(&self, media: &mut Media, mut result: Outcome, edits: &[Edit]) -> Res<Outcome> {
        let selector = self.kind().selector();
        let mut options: Vec<String> = Vec::new();
        for edit in edits {
            options.extend(args(&["--edit", &format!("track:{selector}{}", edit.track.number)]));
            if let Some(default) = edit.default {
                options.extend(args(&["--set", &format!("flag-default={}", default as i32)]));
            }
            if let Some(language) = &edit.language {
                options.extend(args(&["--set", &format!("language={language}")]));
            }
            if let Some(name) = &edit.name {
                if name.is_empty() {
                    options.extend(args(&["--delete", "name"]));
                } else {
                    options.extend(args(&["--set", &format!("name={name}")]));
                }
            }
            if let Some(forced) = edit.forced {
                options.extend(args(&["--set", &format!("flag-forced={}", forced as i32)]));
            }
        }
        let command = |file: &str| -> Vec<String> {
            let mut cmd = args(&["mkvpropedit", file]);
            cmd.extend(options.iter().cloned());
            cmd
        };
        let temp_file = format!("{}{TEMP_MARK}", media.path);
        if !self.start_copy(&mut result, &media.path, &temp_file)? {
            return Ok(result);
        }
        let all = self.tracks(media)?;
        let expected = self.expected_layout(&all, edits);
        result.data = layout_data(&all, &expected);
        // with the native-mkv feature the flags, languages and names are written in this program (see ebml_edit)
        if cfg!(feature = "native-mkv") {
            self.native_edit(&mut result, &temp_file, edits)?;
            if result.status != Status::Error && !self.config.dryrun {
                let rewritten = Media::new(&temp_file, media.container);
                self.verify_layout(&mut result, &rewritten, &expected, "native")?;
            }
            self.replace_file(&mut result, &temp_file, &media.path)?;
            return Ok(result.finish());
        }
        // SPIKE: without mkvpropedit, ffmpeg remuxes the original into the copy's place
        let (edit_cmd, tool) = if self.find("mkvpropedit").is_none() {
            (self.ffmpeg_edit_command(&media.path, &temp_file, &all, edits), "ffmpeg")
        } else {
            (command(&temp_file), "mkvpropedit")
        };
        if self.apply(&mut result, edit_cmd, &[0])? && !self.config.dryrun {
            let rewritten = Media::new(&temp_file, media.container);
            self.verify_layout(&mut result, &rewritten, &expected, tool)?;
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// A change that moves tracks, made in this program (the native-mkv feature): the copy gets the flag,
    /// language and name changes (`native_edit`), then its tracks are renumbered in the new order (see
    /// `ebml_edit::reorder_tracks_in_place`). Both are checked by reading the result back before it replaces
    /// the file.
    fn native_rewrite(&self, media: &mut Media, mut result: Outcome, edits: &[Edit]) -> Res<Outcome> {
        let all = self.tracks(media)?;
        let expected = self.expected_layout(&all, edits);
        result.data = layout_data(&all, &expected);
        let temp_file = format!("{}{TEMP_MARK}", media.path);
        if !self.start_copy(&mut result, &media.path, &temp_file)? {
            return Ok(result);
        }
        self.native_edit(&mut result, &temp_file, edits)?;
        if result.status != Status::Error {
            // the tracks in their new order, as mkvmerge would write them
            let order: Vec<u64> = reordered_by(&all, &moves_of(edits)).iter().map(|t| t.id as u64 + 1).collect();
            result.commands.push(vec!["native-mkv".into(), "reorder-tracks".into(), temp_file.clone()]);
            self.note(&format!("Executing:\tnative-mkv reorder-tracks {temp_file}"));
            if !self.config.dryrun {
                if let Err(error) = crate::ebml_edit::reorder_tracks_in_place(Path::new(&temp_file), &order) {
                    result.error(format!("could not reorder the tracks: {}", strerror(&error)));
                }
            }
        }
        if result.status != Status::Error && !self.config.dryrun {
            let rewritten = Media::new(&temp_file, media.container);
            self.verify_layout(&mut result, &rewritten, &expected, "native")?;
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Make the flag, language and name changes in the copy at `temp_file`, by writing a new file beside it
    /// (see `ebml_edit`) and putting that in its place. A dry run changes nothing. On an error the copy is
    /// left as it was, and the error is in `result`.
    fn native_edit(&self, result: &mut Outcome, temp_file: &str, edits: &[Edit]) -> Res<()> {
        // the step is listed like a command, so that a dry run and --verbose show it
        result.commands.push(vec!["native-mkv".into(), "write-tracks".into(), temp_file.to_string()]);
        self.note(&format!("Executing:\tnative-mkv write-tracks {temp_file}"));
        if self.config.dryrun {
            return Ok(());
        }
        let changes: Vec<crate::ebml_edit::TrackEdit> = edits
            .iter()
            .map(|edit| crate::ebml_edit::TrackEdit {
                // the TrackNumber of a track is its mkvmerge ID plus one
                number: edit.track.id as u64 + 1,
                default: edit.default,
                forced: edit.forced,
                language: edit.language.clone(),
                name: edit.name.clone(),
            })
            .collect();
        let writing = format!("{temp_file}{TEMP_MARK}");
        self.pend(&writing);
        let written = crate::ebml_edit::rewrite_tracks(Path::new(temp_file), Path::new(&writing), &changes)
            .and_then(|()| fs::rename(&writing, temp_file));
        if let Err(error) = written {
            let _ = fs::remove_file(&writing);
            if error.kind() == io::ErrorKind::Interrupted {
                return Err(Stop::Interrupted);
            }
            result.error(format!("could not change the tracks: {}", strerror(&error)));
        }
        Ok(())
    }

    /// Rewrite an MKV with the tracks in a new order and new flags and languages.
    fn mkvmerge_command(
        &self,
        media: &Media,
        all: &[Track],
        edits: &[Edit],
        temp_file: &str,
    ) -> Vec<String> {
        let order = reordered_by(all, &moves_of(edits));
        let order_text =
            order.iter().map(|t| format!("0:{}", t.id)).collect::<Vec<_>>().join(",");
        let mut cmd = args(&["mkvmerge", "-o", temp_file, "--track-order", &order_text]);
        for edit in edits {
            let id = edit.track.id;
            if let Some(default) = edit.default {
                cmd.extend(args(&["--default-track-flag", &format!("{id}:{}", default as i32)]));
            }
            if let Some(language) = &edit.language {
                cmd.extend(args(&["--language", &format!("{id}:{language}")]));
            }
            if let Some(name) = &edit.name {
                cmd.extend(args(&["--track-name", &format!("{id}:{name}")]));
            }
            if let Some(forced) = edit.forced {
                cmd.extend(args(&["--forced-display-flag", &format!("{id}:{}", forced as i32)]));
            }
        }
        cmd.push(media.path.clone());
        cmd
    }

    /// Rewrite a file with ffmpeg, copying the streams, with new flags and languages and,
    /// if a track is being moved, the streams of its kind in the new order.
    fn ffmpeg_command(
        &self,
        media: &Media,
        all: &[Track],
        edits: &[Edit],
        temp_file: &str,
    ) -> Vec<String> {
        let selector = self.kind().selector();
        let tracks: Vec<Track> = all.iter().filter(|t| t.kind == self.kind()).cloned().collect();
        let mut order = tracks.clone();
        let mut maps = args(&["-map", "0"]);
        let moves = moves_of(edits);
        if !moves.is_empty() {
            order = reordered_by(all, &moves)
                .into_iter()
                .filter(|t| t.kind == self.kind())
                .collect();
            let group = |kind: Kind| -> Vec<String> {
                if kind == self.kind() {
                    order
                        .iter()
                        .flat_map(|t| args(&["-map", &format!("0:{selector}:{}", t.number - 1)]))
                        .collect()
                } else {
                    args(&["-map", &format!("0:{}?", kind.selector())])
                }
            };
            maps = [Kind::Video, Kind::Audio, Kind::Subtitles]
                .into_iter()
                .flat_map(group)
                .chain(args(&["-map", "0:d?", "-map", "0:t?"]))
                .collect();
        }
        let renamed: HashMap<(Kind, usize), String> = edits
            .iter()
            .filter_map(|e| e.name.as_ref().map(|n| (e.track.key(), n.clone())))
            .collect();
        let mut cmd = vec!["ffmpeg".to_string()];
        cmd.extend(args(crate::model::FFMPEG_QUIET));
        cmd.extend(args(&["-i", &media.path]));
        cmd.extend(maps);
        cmd.extend(args(&["-c", "copy"]));
        cmd.extend(name_args(&kept_tracks(all, Some((self.kind(), order.clone()))), &renamed));
        // SPIKE: the Matroska muxer makes the first track of a kind default when none is, so every track that should
        // not be default is cleared first (the edits below set the ones that should)
        if media.container == Container::Mkv {
            for kind in [Kind::Video, Kind::Audio, Kind::Subtitles] {
                let list: Vec<Track> = if kind == self.kind() {
                    order.clone()
                } else {
                    all.iter().filter(|t| t.kind == kind).cloned().collect()
                };
                for (index, track) in list.iter().enumerate() {
                    let edited = edits.iter().find(|e| e.track.key() == track.key()).and_then(|e| e.default);
                    if !edited.unwrap_or(track.default) {
                        cmd.push(format!("-disposition:{}:{index}", kind.selector()));
                        cmd.push("-default".to_string());
                    }
                }
            }
        }
        let position: HashMap<usize, usize> =
            order.iter().enumerate().map(|(i, t)| (t.number, i)).collect();
        for edit in edits {
            let index = position[&edit.track.number];
            if let Some(default) = edit.default {
                cmd.push(format!("-disposition:{selector}:{index}"));
                cmd.push(if default { "+default" } else { "-default" }.to_string());
            }
            if let Some(forced) = edit.forced {
                cmd.push(format!("-disposition:{selector}:{index}"));
                cmd.push(if forced { "+forced" } else { "-forced" }.to_string());
            }
            if let Some(language) = &edit.language {
                cmd.push(format!("-metadata:s:{selector}:{index}"));
                cmd.push(format!("language={language}"));
            }
        }
        cmd.extend(Self::ffmpeg_output(media, temp_file));
        cmd
    }

    /// The output options of an ffmpeg rewrite.
    fn ffmpeg_output(media: &Media, temp_file: &str) -> Vec<String> {
        if media.container == Container::Mp4 {
            args(&["-movflags", "+faststart", temp_file])
        } else {
            args(&[temp_file])
        }
    }

    /// Rewrite a file without the given tracks.
    /// Remove tracks from an MKV in this program (the native-mkv feature, see `ebml_edit::remove_tracks`): the
    /// new file is written beside the original as the temporary file, checked by reading it back, and replaces
    /// the original only if that passes. The promoted track (the new default when the default track went) is
    /// set in the same pass.
    fn native_remove(
        &self,
        media: &mut Media,
        mut result: Outcome,
        remove: &[Track],
        promote: Option<&Track>,
        expected: &[Track],
    ) -> Res<Outcome> {
        let temp_file = format!("{}{TEMP_MARK}", media.path);
        result.commands.push(vec!["native-mkv".into(), "remove-tracks".into(), temp_file.clone()]);
        self.note(&format!("Executing:\tnative-mkv remove-tracks {temp_file}"));
        if !self.config.dryrun {
            self.pend(&temp_file);
            // the TrackNumber of a track is its mkvmerge ID plus one
            let gone: Vec<u64> = remove.iter().map(|t| t.id as u64 + 1).collect();
            let promoted = promote.map(|p| p.id as u64 + 1);
            if let Err(error) = crate::ebml_edit::remove_tracks(Path::new(&media.path), Path::new(&temp_file), &gone, promoted) {
                if error.kind() == io::ErrorKind::Interrupted {
                    return Err(Stop::Interrupted);
                }
                result.error(format!("could not remove the tracks: {}", strerror(&error)));
            }
            if result.status != Status::Error {
                let rewritten = Media::new(&temp_file, media.container);
                self.verify_layout(&mut result, &rewritten, expected, "native")?;
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Remove streams from an AVI in this program (the native-avi feature, see `avi::remove_streams`). Written
    /// beside the original, read back, and put in its place only if that passes.
    fn native_remove_avi(&self, media: &mut Media, mut result: Outcome, remove: &[Track], expected: &[Track]) -> Res<Outcome> {
        // a track of an AVI is its stream: the ID is the stream's place in the header's strl lists (see avi::stream_tracks)
        let removed: Vec<usize> = remove.iter().map(|t| t.id as usize).collect();
        let temp_file = temp_path(&media.path);
        result.commands.push(vec!["native-avi".into(), "remove-streams".into(), temp_file.clone()]);
        self.note(&format!("Executing:\tnative-avi remove-streams {temp_file}"));
        if !self.config.dryrun {
            self.pend(&temp_file);
            if let Err(error) = crate::avi::remove_streams(Path::new(&media.path), Path::new(&temp_file), &removed) {
                result.error(format!("could not remove the streams: {}", strerror(&error)));
            }
            if result.status != Status::Error {
                let rewritten = Media::new(&temp_file, media.container);
                self.verify_layout(&mut result, &rewritten, expected, "native")?;
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Remove tracks from an MP4 in this program (the native-mp4 feature, see `mp4::remove_traks`). Written
    /// beside the original, read back, and put in its place only if that passes; the promoted track is made the
    /// default in the same pass.
    fn native_remove_mp4(
        &self,
        media: &mut Media,
        mut result: Outcome,
        remove: &[Track],
        promote: Option<&Track>,
        expected: &[Track],
    ) -> Res<Outcome> {
        let ids = match crate::mp4::track_ids(Path::new(&media.path)) {
            Ok(ids) => ids,
            Err(error) => {
                result.error(format!("could not read the tracks: {}", strerror(&error)));
                return Ok(result);
            }
        };
        let place = |track: &Track| ids.iter().position(|&id| id as i64 == track.id);
        let Some(removed) = remove.iter().map(place).collect::<Option<Vec<usize>>>() else {
            result.error("a track was not found in the file".to_string());
            return Ok(result);
        };
        let mut changes = Vec::new();
        if let Some(trak) = promote.and_then(place) {
            changes.push(crate::mp4::TrakChange { trak, language: None, enabled: Some(true), name: None, forced: None });
        }
        let temp_file = temp_path(&media.path);
        result.commands.push(vec!["native-mp4".into(), "remove-tracks".into(), temp_file.clone()]);
        self.note(&format!("Executing:\tnative-mp4 remove-tracks {temp_file}"));
        if !self.config.dryrun {
            self.pend(&temp_file);
            if let Err(error) = crate::mp4::remove_traks(Path::new(&media.path), Path::new(&temp_file), &removed, &changes) {
                result.error(format!("could not remove the tracks: {}", strerror(&error)));
            }
            if result.status != Status::Error {
                let rewritten = Media::new(&temp_file, media.container);
                self.verify_layout(&mut result, &rewritten, expected, "native")?;
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    fn remove_tracks(
        &self,
        media: &mut Media,
        mut result: Outcome,
        remove: Vec<Track>,
    ) -> Res<Outcome> {
        let tracks = self.selected(media)?;
        if remove.is_empty() {
            return Ok(skip(result, &format!("has no matching {} tracks to remove", self.word())));
        }
        if self.kind() == Kind::Audio && remove.len() == tracks.len() && !self.config.force {
            return Ok(skip(result, "this would remove every audio track (use --force to allow)"));
        }
        if !self.check_space(&mut result, &media.path, "rewrite the file") {
            return Ok(result);
        }
        let removed: Vec<(Kind, usize)> = remove.iter().map(Track::key).collect();
        let survivors: Vec<Track> =
            tracks.iter().filter(|t| !removed.contains(&t.key())).cloned().collect();
        let promote = promotion(media.container, &survivors);
        for track in &remove {
            result.changes.push(format!("removed {} ({})", track.label(), track.language));
        }
        if let Some(promote) = &promote {
            result
                .changes
                .push(format!("{} ({}): default no -> yes", promote.label(), promote.language));
        }
        let all = self.tracks(media)?;
        let expected: Vec<Track> = all
            .iter()
            .filter(|t| !removed.contains(&t.key()))
            .map(|t| {
                let mut t = t.clone();
                if promote.as_ref().is_some_and(|p| p.key() == t.key()) {
                    t.default = true;
                }
                t
            })
            .collect();
        result.data = layout_data(&all, &expected);
        if cfg!(feature = "native-mkv") && media.container == Container::Mkv {
            return self.native_remove(media, result, &remove, promote.as_ref(), &expected);
        }
        if cfg!(feature = "native-mp4") && media.container == Container::Mp4 {
            return self.native_remove_mp4(media, result, &remove, promote.as_ref(), &expected);
        }
        if cfg!(feature = "native-avi") && media.container == Container::Avi {
            return self.native_remove_avi(media, result, &remove, &expected);
        }
        let (temp_file, cmd, tool, ok): (String, Vec<String>, &str, &[i32]) =
            // SPIKE: an MKV's tracks are removed with ffmpeg when mkvmerge is missing
            if media.container == Container::Mkv && self.find("mkvmerge").is_some() {
                let ids = remove.iter().map(|t| t.id.to_string()).collect::<Vec<_>>().join(",");
                let temp_file = format!("{}{TEMP_MARK}", media.path);
                let select = if self.kind() == Kind::Audio { "--audio-tracks" } else { "--subtitle-tracks" };
                let mut cmd = args(&["mkvmerge", "-o", &temp_file, select, &format!("!{ids}")]);
                if let Some(promote) = &promote {
                    cmd.extend(args(&["--default-track-flag", &format!("{}:1", promote.id)]));
                }
                cmd.push(media.path.clone());
                (temp_file, cmd, "mkvmerge", MKVMERGE_OK)
            } else {
                let temp_file = temp_path(&media.path);
                let selector = self.kind().selector();
                let mut cmd = vec!["ffmpeg".to_string()];
                cmd.extend(args(crate::model::FFMPEG_QUIET));
                cmd.extend(args(&["-i", &media.path, "-map", "0"]));
                for track in &remove {
                    cmd.extend(args(&["-map", &format!("-0:{selector}:{}", track.number - 1)]));
                }
                cmd.extend(args(&["-c", "copy"]));
                cmd.extend(name_args(
                    &kept_tracks(&all, Some((self.kind(), survivors.clone()))),
                    &HashMap::new(),
                ));
                if promote.is_some() {
                    cmd.extend(args(&[&format!("-disposition:{selector}:0"), "+default"]));
                }
                // SPIKE: the Matroska muxer makes the first track of a kind default when none is, so the others are cleared
                if media.container == Container::Mkv {
                    for (index, track) in survivors.iter().enumerate() {
                        if !track.default && promote.as_ref().is_none_or(|p| p.key() != track.key()) {
                            cmd.extend(args(&[&format!("-disposition:{selector}:{index}"), "-default"]));
                        }
                    }
                }
                cmd.extend(Self::ffmpeg_output(media, &temp_file));
                (temp_file, cmd, "ffmpeg", &[0])
            };
        self.pend(&temp_file);
        let done = self.apply_tracked(&mut result, cmd, ok, &media.path, &temp_file, "Rewriting")?;
        if done && !self.config.dryrun {
            let rewritten = Media::new(&temp_file, media.container);
            self.verify_layout(&mut result, &rewritten, &expected, tool)?;
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    // Operations ---------------------------------------------------------------

    fn do_info(&self, media: &mut Media) -> Res<Outcome> {
        let mut result = self.result(media, "info");
        let tracks = self.tracks(media)?;
        result.data = json!({"container": media.container.name(), "tracks": tracks});
        Ok(result)
    }

    fn do_get(&self, media: &mut Media) -> Res<Outcome> {
        let mut result = self.result(media, "get");
        let tracks = self.selected(media)?;
        if !self.config.lang.is_empty() {
            let matches: Vec<&Track> =
                tracks.iter().filter(|t| t.language == self.config.lang).collect();
            if matches.is_empty() {
                return Ok(skip(result, &self.missing_message(&[self.config.lang.clone()])));
            }
            result.data = json!({"tracks": matches});
        } else {
            let mut chosen: Vec<&Track> = tracks.iter().filter(|t| t.default).collect();
            if chosen.is_empty() && self.kind() == Kind::Audio {
                chosen = tracks.iter().take(1).collect(); // AVI has no default flag
            }
            let languages: Vec<&String> = chosen.iter().map(|t| &t.language).collect();
            let mut data = serde_json::Map::new();
            data.insert(format!("default_{}", self.kind().name()), json!(languages));
            result.data = Value::Object(data);
        }
        Ok(result)
    }

    /// Guess the language of the audio tracks (or of `--track`) from their speech, with
    /// whisper.cpp (see `detect`). The file is not changed: the clips are cut into a temporary
    /// folder, which is removed afterwards. The result's data lists what each track sounds like.
    fn do_detect(&self, media: &mut Media) -> Res<Outcome> {
        let mut result = self.result(media, "detect");
        if self.kind() != Kind::Audio {
            return Ok(skip(result, "--detect works on audio tracks"));
        }
        let tracks = if self.config.track.is_empty() {
            self.selected(media)?
        } else {
            let Some(track) = self.resolve_track(media, &mut result)? else { return Ok(result) };
            vec![track]
        };
        if tracks.is_empty() {
            return Ok(skip(result, "has no audio tracks to detect"));
        }
        let Some(model) = self.model.clone().filter(|p| p.is_file()) else {
            let place = self.model.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(no folder)".into());
            result.error(format!(
                "--detect needs a whisper.cpp model, and there is none at {place}: run mau --check --install to download it, or set MAU_WHISPER_MODEL to a model file"
            ));
            return Ok(result);
        };
        let model = model.to_string_lossy().into_owned();
        let Some(program) = detect::whisper_program() else {
            result.error("--detect needs whisper.cpp: install it so that whisper-cli is on PATH, or set MAU_WHISPER to it".into());
            return Ok(result);
        };
        let native = native_detect() && media.container == Container::Mkv;
        let duration = if native {
            crate::ebml_subs::read_duration(Path::new(&media.path)).unwrap_or(None)
        } else {
            self.duration(&media.path)?
        };
        let starts = detect::clip_starts(duration);
        let dir = match detect::ClipDir::new() {
            Ok(dir) => dir,
            Err(error) => {
                result.error(format!("could not make a folder for the clips: {error}"));
                return Ok(result);
            }
        };
        let clip_seconds = format!("{}", detect::CLIP_SECONDS);
        let mut found: Vec<Value> = Vec::new();
        for track in &tracks {
            let mut clips: Vec<(String, f64)> = Vec::new();
            for (number, start) in starts.iter().enumerate() {
                let clip = dir.0.join(format!("track{}-{number}.wav", track.number)).to_string_lossy().into_owned();
                if native {
                    // the frames are read and decoded here (see clip); the track's TrackNumber is its ID plus one
                    self.note(&format!("Executing:\tnative-audio write-clip {clip}"));
                    if let Err(error) = crate::clip::write_clip(Path::new(&media.path), track.id as u64 + 1, *start, detect::CLIP_SECONDS, Path::new(&clip)) {
                        result.error(format!("could not read the sound of track {} of {}: {}", track.number, media.path, strerror(&error)));
                        return Ok(result);
                    }
                } else {
                    let start_text = format!("{start:.3}");
                    let map = format!("0:a:{}", track.number - 1);
                    let cut = args(&[
                        "ffmpeg", "-v", "error", "-ss", &start_text, "-t", &clip_seconds, "-i", &media.path,
                        "-map", &map, "-vn", "-sn", "-ac", "1", "-ar", "16000", &clip,
                    ]);
                    match self.query(&cut)? {
                        Some(done) if done.code == 0 => {}
                        _ => {
                            result.error(format!("could not read the sound of track {} of {}", track.number, media.path));
                            return Ok(result);
                        }
                    }
                }
                let heard = args(&[&program, "-m", &model, "-f", &clip, "-dl"]);
                let Some(done) = self.query(&heard)? else {
                    result.error(format!("{program} could not be run"));
                    return Ok(result);
                };
                let text = format!("{}\n{}", done.stdout, done.stderr);
                match detect::parse_detected(&text) {
                    Some(report) => clips.push(report),
                    None => {
                        let last = text.trim().lines().last().unwrap_or("").to_string();
                        result.error(format!("{program} gave no language for track {}: {last}", track.number));
                        return Ok(result);
                    }
                }
            }
            let Some(pick) = detect::pick_language(&clips) else { continue };
            let language = resolve_language(&pick.language).unwrap_or_else(|_| pick.language.clone());
            found.push(json!({
                "track": track.number, "id": track.id, "current": track.language,
                "language": language, "confidence": pick.confidence,
                "clips": pick.clips, "agreed": pick.agreed,
            }));
        }
        result.data = json!({ "detected": found });
        Ok(result)
    }

    /// Set the language of the audio tracks (or of `--track`) to the language they sound like, as
    /// `--detect` finds it. A track already labelled with that language is left alone; an AVI file
    /// has no language to set. The change is made like any other (a copy, checked, then replacing
    /// the file), and a dry run shows it.
    fn do_update(&self, media: &mut Media) -> Res<Outcome> {
        let mut heard = self.do_detect(media)?;
        heard.action = "update".to_string();
        if heard.status == Status::Error || heard.status == Status::Skipped {
            return Ok(heard);
        }
        let mut result = self.result(media, "update");
        result.data = heard.data.clone();
        if media.container == Container::Avi {
            return Ok(skip(result, "AVI files have no language to set"));
        }
        let tracks = self.selected(media)?;
        let mut edits: Vec<Edit> = Vec::new();
        for item in heard.data["detected"].as_array().into_iter().flatten() {
            let number = item["track"].as_u64().unwrap_or(0) as usize;
            let language = item["language"].as_str().unwrap_or("").to_string();
            let Some(track) = tracks.iter().find(|t| t.number == number) else { continue };
            if language.is_empty() || crate::languages::normalise_language(Some(&track.language)) == language {
                continue;
            }
            let mut edit = Edit::new(track);
            edit.language = Some(language);
            edits.push(edit);
        }
        let mut done = self.edit_tracks(media, result, edits, "every track already has the language it sounds like")?;
        // the edit's data is the track layout: keep what was heard next to it
        let mut data = match std::mem::take(&mut done.data) {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        data.insert("detected".to_string(), heard.data["detected"].clone());
        done.data = Value::Object(data);
        Ok(done)
    }

    /// The length of the file in seconds, from `ffprobe` (`None` if it can't be read).
    fn duration(&self, path: &str) -> Res<Option<f64>> {
        let probe = args(&["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path]);
        Ok(self
            .query(&probe)?
            .filter(|done| done.code == 0)
            .and_then(|done| done.stdout.trim().parse::<f64>().ok()))
    }

    fn do_set(&self, media: &mut Media, what: &str) -> Res<Outcome> {
        let mut result = self.result(media, "set");
        // AVI has no flags or names to set, but its two audio tracks can be swapped (see swap_avi_audio)
        if media.container == Container::Avi && !(what == "order" && self.kind() == Kind::Audio) {
            return Ok(skip(result, "set is only supported for MKV and MP4 files"));
        }
        let tracks = self.selected(media)?;
        if matches!(what, "name" | "forced" | "unforced") {
            return self.set_track_property(media, result, what);
        }
        if what == "track" {
            return self.set_track(media, result);
        }
        if what == "order" {
            let wanted = if self.config.order_lang.is_empty() {
                self.config.order.clone()
            } else {
                order_by_languages(&tracks, &self.config.order_lang)
            };
            let mut sorted = wanted.clone();
            sorted.sort_unstable();
            if sorted != (1..=tracks.len()).collect::<Vec<_>>() {
                let message = format!(
                    "--order must list each of the {} {} tracks once",
                    tracks.len(),
                    self.word()
                );
                return Ok(skip(result, &message));
            }
            if media.container == Container::Avi {
                if tracks.len() != 2 || wanted != [2, 1] {
                    let message = "AVI files can only swap their two audio tracks (--order 2,1)";
                    return Ok(skip(result, message));
                }
                return self.swap_avi_audio(media, result);
            }
            let mut edits: Vec<Edit> = Vec::new();
            for (place, number) in wanted.iter().enumerate() {
                let track = &tracks[number - 1];
                let mut edit = Edit::new(track);
                if *number != place + 1 {
                    edit.move_to = Some(place + 1);
                }
                if self.config.default_first && track.default != (place == 0) {
                    edit.default = Some(place == 0);
                }
                if edit.move_to.is_some() || edit.default.is_some() {
                    edits.push(edit);
                }
            }
            let unchanged = format!(
                "{} tracks are already in that order{}",
                self.word(),
                if self.config.default_first { " and the first is the default" } else { "" }
            );
            return self.edit_tracks(media, result, edits, &unchanged);
        }
        if what == "default" {
            let Some(track) = self.resolve_track(media, &mut result)? else { return Ok(result) };
            let lang = self.config.lang.clone();
            let unchanged = format!("{} is already the default", track.label())
                + &if lang.is_empty() { String::new() } else { format!(" and {lang}") }
                + if self.config.move_first { " and the first track" } else { "" };
            let edits = default_edits(&tracks, Some(&track), &lang, self.config.move_first);
            return self.edit_tracks(media, result, edits, &unchanged);
        }
        if !self.config.default.is_empty() || !self.config.prefer.is_empty() {
            return self.set_default_language(media, result, &tracks);
        }
        let Some(track) = self.resolve_track(media, &mut result)? else { return Ok(result) };
        let mut edits = Vec::new();
        if track.language != self.config.lang {
            let mut edit = Edit::new(&track);
            edit.language = Some(self.config.lang.clone());
            edits.push(edit);
        }
        let unchanged = format!("{} is already {}", track.label(), track.language);
        self.edit_tracks(media, result, edits, &unchanged)
    }

    /// Set the name or the forced flag of the track chosen with --track.
    fn set_track_property(&self, media: &mut Media, mut result: Outcome, what: &str) -> Res<Outcome> {
        let Some(track) = self.resolve_track(media, &mut result)? else { return Ok(result) };
        if what != "name" {
            if let Some(message) = forced_refusal(media.container, track.kind) {
                return Ok(skip(result, message));
            }
        }
        let mut edit = Edit::new(&track);
        let (changed, unchanged);
        if what == "name" {
            let wanted = self.config.name.clone().unwrap_or_default();
            changed = !same_name(&track.name, &wanted);
            unchanged = if wanted.is_empty() {
                format!("{} has no name", track.label())
            } else {
                format!("{} is already named \"{wanted}\"", track.label())
            };
            edit.name = Some(wanted);
        } else {
            let forced = what == "forced";
            changed = track.forced != forced;
            unchanged = format!(
                "{} is already {}",
                track.label(),
                if forced { "forced" } else { "not forced" }
            );
            edit.forced = Some(forced);
        }
        let edits = if changed { vec![edit] } else { Vec::new() };
        self.edit_tracks(media, result, edits, &unchanged)
    }

    /// Change the language, name and forced flag of one track (`--track`) together, in one
    /// edit of the file; only what is given (`--lang`, `--name`, `--forced-flag`) is touched.
    fn set_track(&self, media: &mut Media, result: Outcome) -> Res<Outcome> {
        let mut result = result;
        let Some(track) = self.resolve_track(media, &mut result)? else { return Ok(result) };
        if self.config.forced_flag.is_some() {
            if let Some(message) = forced_refusal(media.container, track.kind) {
                return Ok(skip(result, message));
            }
        }
        let mut edit = Edit::new(&track);
        if !self.config.lang.is_empty() && track.language != self.config.lang {
            edit.language = Some(self.config.lang.clone());
        }
        if let Some(name) = &self.config.name {
            if !same_name(&track.name, name) {
                edit.name = Some(name.clone());
            }
        }
        if let Some(forced) = self.config.forced_flag {
            if track.forced != forced {
                edit.forced = Some(forced);
            }
        }
        let changed = edit.language.is_some() || edit.name.is_some() || edit.forced.is_some();
        let unchanged = format!("{} is already as asked", track.label());
        self.edit_tracks(media, result, if changed { vec![edit] } else { Vec::new() }, &unchanged)
    }

    /// Make the first track in the (preferred) language the only default track.
    fn set_default_language(&self, media: &mut Media, result: Outcome, tracks: &[Track]) -> Res<Outcome> {
        if self.config.default == "none" {
            let unchanged = format!("there is already no default {} track", self.word());
            let edits = default_edits(tracks, None, "", false);
            if media.container == Container::Mp4 {
                // ffmpeg would enable the first track again, so the flags are changed in the file
                return self.edit_mp4_flags(media, result, edits, &unchanged);
            }
            return self.edit_tracks(media, result, edits, &unchanged);
        }
        let languages = self.languages(&self.config.default);
        let Some((lang, track)) = self.choose(tracks, &languages) else {
            return Ok(skip(result, &self.missing_message(&languages)));
        };
        let unchanged = format!("{} ({lang}) is already the default", track.label())
            + if self.config.move_first { " and the first track" } else { "" };
        let edits = default_edits(tracks, Some(&track), "", self.config.move_first);
        self.edit_tracks(media, result, edits, &unchanged)
    }

    fn do_swap(&self, media: &mut Media) -> Res<Outcome> {
        let result = self.result(media, "swap");
        let audio = self.selected(media)?;
        if audio.len() < 2 {
            return Ok(skip(result, "needs at least two audio tracks to swap"));
        }
        if media.container != Container::Avi {
            let unchanged =
                format!("{} ({}) is already the default", audio[1].label(), audio[1].language);
            let edits = default_edits(&audio, Some(&audio[1]), "", false);
            return self.edit_tracks(media, result, edits, &unchanged);
        }
        self.swap_avi_audio(media, result)
    }

    /// Swaps the first two audio tracks of an AVI file, which has no flags to set: the streams are
    /// reordered in a copy with `ffmpeg`, read back, and then replace the file. Used by `--swap`,
    /// and by `--set order --order 2,1` (the reorder of two tracks in the app's Edit dialog).
    fn swap_avi_audio(&self, media: &mut Media, result: Outcome) -> Res<Outcome> {
        if cfg!(feature = "native-avi") {
            return self.swap_avi_natively(media, result);
        }
        let mut result = result;
        if !self.check_space(&mut result, &media.path, "rewrite the file") {
            return Ok(result);
        }
        let temp_file = temp_path(&media.path);
        self.pend(&temp_file);
        result.changes.push("audio tracks 1 and 2 swapped".to_string());
        let before = self.tracks(media)?;
        let mut expected = before.clone();
        let audio_at: Vec<usize> = expected
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == Kind::Audio)
            .map(|(i, _)| i)
            .take(2)
            .collect();
        expected.swap(audio_at[0], audio_at[1]);
        result.data = layout_data(&before, &expected);
        let mut cmd = vec!["ffmpeg".to_string()];
        cmd.extend(args(crate::model::FFMPEG_QUIET));
        cmd.extend(args(&[
            "-i", &media.path, "-map", "0:v?", "-map", "0:a:1", "-map", "0:a:0", "-map",
            "0:a:2?", "-map", "0:s?", "-disposition:a:0", "default", "-disposition:a:1", "none",
            "-c", "copy", &temp_file,
        ]));
        let done = self.apply_tracked(&mut result, cmd, &[0], &media.path, &temp_file, "Rewriting")?;
        if done && !self.config.dryrun {
            let rewritten = Media::new(&temp_file, media.container);
            self.verify_layout(&mut result, &rewritten, &expected, "ffmpeg")?;
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Put the first two audio streams of an AVI in each other's order, in this program (the native-avi feature,
    /// see `avi::reorder_streams_in_place`). The copy is read back before it replaces the file.
    fn swap_avi_natively(&self, media: &mut Media, mut result: Outcome) -> Res<Outcome> {
        let before = self.tracks(media)?;
        let audio_at: Vec<usize> = before
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == Kind::Audio)
            .map(|(i, _)| i)
            .take(2)
            .collect();
        let [first, second] = audio_at[..] else {
            result.error("needs at least two audio tracks to swap".to_string());
            return Ok(result);
        };
        let mut expected = before.clone();
        expected.swap(first, second);
        result.changes.push("audio tracks 1 and 2 swapped".to_string());
        result.data = layout_data(&before, &expected);
        let temp_file = temp_path(&media.path);
        if !self.start_copy(&mut result, &media.path, &temp_file)? {
            return Ok(result);
        }
        // the tracks of an AVI are its streams, in the order of their lists: stream i is track i
        let mut order: Vec<usize> = (0..before.len()).collect();
        order.swap(first, second);
        result.commands.push(vec!["native-avi".into(), "reorder-streams".into(), temp_file.clone()]);
        self.note(&format!("Executing:\tnative-avi reorder-streams {temp_file}"));
        if !self.config.dryrun {
            match crate::avi::reorder_streams_in_place(Path::new(&temp_file), &order) {
                Ok(()) => {
                    let rewritten = Media::new(&temp_file, media.container);
                    self.verify_layout(&mut result, &rewritten, &expected, "native")?;
                }
                Err(error) => result.error(format!("could not reorder the streams: {}", strerror(&error))),
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    fn do_delete(&self, media: &mut Media, what: &str) -> Res<Outcome> {
        let mut result = self.result(media, "delete");
        if what == "track" {
            let Some(track) = self.resolve_any_track(media, &mut result)? else { return Ok(result) };
            if track.kind == Kind::Video {
                return Ok(skip(result, "removing a video track is not supported (audio and subtitles only)"));
            }
            return self.with_kind(track.kind).remove_tracks(media, result, vec![track]);
        }
        if media.container == Container::Avi {
            return Ok(skip(
                result,
                "deleting by language is only supported for MKV and MP4 files",
            ));
        }
        let tracks = self.selected(media)?;
        let remove: Vec<Track> = match what {
            // every track after the first of its language (subtitles: of its language and forced flag)
            "dupes" => {
                let mut seen: Vec<(String, bool)> = Vec::new();
                tracks
                    .into_iter()
                    .filter(|t| {
                        let key = (t.language.clone(), self.kind() == Kind::Subtitles && t.forced);
                        if seen.contains(&key) {
                            true
                        } else {
                            seen.push(key);
                            false
                        }
                    })
                    .collect()
            }
            // the tracks that fit every given language, codec (part of its name) and channel count
            "match" => {
                let codec = self.config.codec.to_lowercase();
                tracks
                    .into_iter()
                    .filter(|t| {
                        (self.config.lang.is_empty() || t.language == self.config.lang)
                            && (codec.is_empty() || codec_key(&t.codec).contains(&codec_key(&codec)))
                            && (self.config.channels == 0 || t.channels == self.config.channels)
                    })
                    .collect()
            }
            _ => tracks.into_iter().filter(|t| t.language == self.config.lang).collect(),
        };
        self.remove_tracks(media, result, remove)
    }

    /// Add subtitle files to an MKV as new subtitle tracks, last among the subtitles, not default,
    /// with the language (and name) each was given. mkvmerge writes a copy with the files added,
    /// and the copy is read back before it replaces the original.
    fn do_add(&self, media: &mut Media, what: &str) -> Res<Outcome> {
        if !self.config.online {
            return self.add_subtitles(media, what, None);
        }
        // --online: find the subtitle on OpenSubtitles, then add it as any file would be
        let mut result = self.result(media, "add");
        if media.container == Container::Avi {
            return Ok(skip(result, "adding a subtitle is only supported for MKV and MP4 files"));
        }
        let hits = match self.online_hits(&media.path) {
            Ok(hits) => hits,
            Err(error) => {
                result.error(error);
                return Ok(result);
            }
        };
        let hit = if self.config.sub_id > 0 {
            hits.iter().find(|h| h.file_id == self.config.sub_id)
        } else {
            online::best_hit(&hits, &self.config.lang)
        };
        let Some(hit) = hit else {
            let message = if self.config.sub_id > 0 {
                format!("the search found no subtitle with ID {} (list them with --search subtitle)", self.config.sub_id)
            } else {
                "OpenSubtitles has no suitable subtitle for this file (machine translated and forced ones are not picked for you)".to_string()
            };
            return Ok(skip(result, &message));
        };
        let lang = if self.config.lang.is_empty() { hit.language.clone() } else { self.config.lang.clone() };
        let name = self.config.name.clone().unwrap_or_else(|| if hit.hearing_impaired { "SDH".to_string() } else { String::new() });
        let from = if hit.release.is_empty() { hit.file_name.clone() } else { hit.release.clone() };
        if self.config.dryrun {
            // a dry run asks nothing of the download quota: it shows what would be added
            let placeholder = SubSpec { file: format!("{}/OpenSubtitles-{}.srt", std::env::temp_dir().display(), hit.file_id), lang, name, forced: false };
            let mut outcome = self.add_subtitles(media, what, Some(placeholder))?;
            outcome.changes.push(format!("from OpenSubtitles: {from} (ID {})", hit.file_id));
            return Ok(outcome);
        }
        let credentials = online::load_credentials();
        let file = match online::download(&credentials, hit.file_id) {
            Ok(path) => path,
            Err(error) => {
                result.error(error);
                return Ok(result);
            }
        };
        let spec = SubSpec { file: file.to_string_lossy().into_owned(), lang, name, forced: false };
        let outcome = self.add_subtitles(media, what, Some(spec));
        online::forget_download(&file);
        let mut outcome = outcome?;
        if outcome.status == Status::Changed {
            outcome.changes.push(format!("from OpenSubtitles: {from} (ID {})", hit.file_id));
        }
        Ok(outcome)
    }

    /// The subtitles OpenSubtitles has for a file, for `--search` and `--add --online`.
    fn online_hits(&self, path: &str) -> Result<Vec<online::Hit>, String> {
        let languages: Vec<String> = if self.config.lang.is_empty() { Vec::new() } else { vec![self.config.lang.clone()] };
        online::search(&online::load_credentials(), Path::new(path), &self.config.query, &languages)
    }

    /// List the subtitles OpenSubtitles has for a file. Nothing is changed.
    fn do_search(&self, media: &mut Media) -> Res<Outcome> {
        let mut result = self.result(media, "search");
        match self.online_hits(&media.path) {
            Ok(hits) => {
                if hits.is_empty() {
                    result.message = "no subtitles found".to_string();
                }
                result.data = json!({ "hits": hits });
            }
            Err(error) => result.error(error),
        }
        Ok(result)
    }

    /// Add subtitle files to an MKV or MP4 as new subtitle tracks (`online` is the one found by `--online`).
    fn add_subtitles(&self, media: &mut Media, _what: &str, online: Option<SubSpec>) -> Res<Outcome> {
        let mut result = self.result(media, "add");
        if media.container == Container::Avi {
            return Ok(skip(result, "adding a subtitle is only supported for MKV and MP4 files"));
        }
        let from_folder = !self.config.sub_dir.is_empty();
        let mut specs: Vec<SubSpec> = if let Some(found) = online {
            vec![found]
        } else if from_folder {
            match self.config.sub_matches.get(&media.path) {
                Some(found) if !found.is_empty() => found.clone(),
                _ => {
                    let message = format!("no subtitle file in {} matches", self.config.sub_dir);
                    return Ok(skip(result, &message));
                }
            }
        } else if self.config.subtitles.is_empty() {
            vec![SubSpec {
                file: self.config.sub_file.clone(),
                lang: self.config.lang.clone(),
                name: self.config.name.clone().unwrap_or_default(),
                forced: false,
            }]
        } else {
            self.config.subtitles.clone()
        };
        let worker = self.with_kind(Kind::Subtitles);
        let all = worker.tracks(media)?;
        if from_folder {
            specs = self.addable_from_folder(media, &all, specs);
            if specs.is_empty() {
                return Ok(skip(result, "nothing to add: the subtitles that match are already there or can't be added"));
            }
        }
        let mut number = all.iter().filter(|t| t.kind == Kind::Subtitles).count();
        if media.container == Container::Mp4 {
            if let Some(refusal) = Self::mp4_subtitle_refusal(&specs) {
                return Ok(skip(result, &refusal));
            }
        }
        if !worker.check_space(&mut result, &media.path, "rewrite the file") {
            return Ok(result);
        }
        if media.container == Container::Mp4 {
            // SubRip files are added to an MP4 in this program (the native-mp4 feature, see mp4::add_text_tracks), when
            // their cues fit mov_text; any other file is converted by ffmpeg
            if cfg!(feature = "native-mp4") && specs.iter().all(|spec| is_native_text(&spec.file)) {
                if let Some(cues) = worker.mp4_cues(&media.path, &specs, &mut result)? {
                    return worker.add_mp4_natively(media, result, &specs, &all, cues);
                }
                if result.status == Status::Error {
                    return Ok(result);
                }
            }
            worker.require(&["ffmpeg"])?;
            return worker.add_to_mp4(media, result, &specs, &all);
        }
        // SubRip files are added to an MKV in this program (the native-mkv feature, see ebml_edit)
        if cfg!(feature = "native-mkv")
            && specs.iter().all(|spec| is_native_mkv_text(&spec.file))
            && specs.iter().all(|spec| self.config.dryrun || mkv_text_writable(Path::new(&spec.file)))
        {
            return worker.add_srt_natively(media, result, &specs, &all);
        }
        // the tools for the other subtitle files (the native build needs none for SubRip)
        if cfg!(feature = "native-mkv") {
            worker.require(Container::Mkv.write_tools())?;
        }
        let temp_file = format!("{}{TEMP_MARK}", media.path);
        // SPIKE: with mkvmerge missing, ffmpeg copies the file and adds each subtitle file as a stream
        let with_ffmpeg = worker.find("mkvmerge").is_none();
        let mut cmd = args(&["mkvmerge", "-o", &temp_file, &media.path]);
        let mut ff_inputs: Vec<String> = Vec::new();
        let mut ff_maps: Vec<String> = Vec::new();
        let mut ff_meta: Vec<String> = Vec::new();
        let mut expected = all.clone();
        for (k, spec) in specs.iter().enumerate() {
            number += 1;
            let new = Track {
                id: expected.len() as i64,
                kind: Kind::Subtitles,
                number,
                codec: String::new(),
                language: spec.lang.clone(),
                default: false,
                forced: spec.forced,
                name: spec.name.clone(),
                channels: 0,
            };
            let file_name = Path::new(&spec.file).file_name().map(|n| n.to_string_lossy().into_owned());
            result.changes.push(format!(
                "added {} ({}) from {}",
                new.label(),
                new.language,
                file_name.unwrap_or_else(|| spec.file.clone())
            ));
            cmd.extend(args(&["--language", &format!("0:{}", new.language)]));
            if !new.name.is_empty() {
                cmd.extend(args(&["--track-name", &format!("0:{}", new.name)]));
            }
            cmd.extend(args(&[
                "--default-track-flag",
                "0:0",
                "--forced-display-flag",
                if spec.forced { "0:1" } else { "0:0" },
            ]));
            cmd.push(spec.file.clone());
            if with_ffmpeg {
                ff_inputs.extend(args(&["-i", &spec.file]));
                ff_maps.extend(args(&["-map", &format!("{}:0", k + 1)]));
                let place = new.number - 1;
                ff_meta.extend(args(&[&format!("-disposition:s:{place}"), if spec.forced { "forced" } else { "0" }]));
                ff_meta.extend(args(&[&format!("-metadata:s:s:{place}"), &format!("language={}", new.language)]));
                if !new.name.is_empty() {
                    ff_meta.extend(args(&[&format!("-metadata:s:s:{place}"), &format!("title={}", new.name)]));
                }
            }
            expected.push(new);
        }
        result.data = layout_data(&all, &expected);
        let cmd = if with_ffmpeg {
            let mut ff = args(&["ffmpeg", "-v", "error", "-y", "-i", &media.path]);
            ff.extend(ff_inputs);
            ff.extend(args(&["-map", "0"]));
            ff.extend(ff_maps);
            ff.extend(args(&["-c", "copy"]));
            ff.extend(ff_meta);
            ff.extend(args(&["-f", "matroska", &temp_file]));
            ff
        } else {
            cmd
        };
        worker.pend(&temp_file);
        let done = worker.apply_tracked(&mut result, cmd, MKVMERGE_OK, &media.path, &temp_file, "Rewriting")?;
        if done && !self.config.dryrun {
            let rewritten = Media::new(&temp_file, media.container);
            worker.verify_layout(&mut result, &rewritten, &expected, "mkvmerge")?;
        }
        worker.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Add SubRip files to an MKV in this program: the cues are read here and written to a copy (see
    /// `ebml_edit::add_subtitle_tracks`), which is read back before it replaces the file.
    fn add_srt_natively(&self, media: &mut Media, mut result: Outcome, specs: &[SubSpec], all: &[Track]) -> Res<Outcome> {
        let mut expected = all.to_vec();
        let mut number = all.iter().filter(|t| t.kind == Kind::Subtitles).count();
        let mut new_tracks = Vec::new();
        for spec in specs {
            number += 1;
            let new = Track {
                id: expected.len() as i64,
                kind: Kind::Subtitles,
                number,
                codec: String::new(),
                language: spec.lang.clone(),
                default: false,
                forced: spec.forced,
                name: spec.name.clone(),
                channels: 0,
            };
            let file_name = Path::new(&spec.file).file_name().map(|n| n.to_string_lossy().into_owned());
            result.changes.push(format!(
                "added {} ({}) from {}",
                new.label(),
                new.language,
                file_name.unwrap_or_else(|| spec.file.clone())
            ));
            // a dry run does not open the file (an online subtitle's placeholder is not there), as the mkvmerge path does not
            if !self.config.dryrun {
                match read_mkv_text(Path::new(&spec.file)) {
                    Ok((codec, private, cues)) => new_tracks.push(crate::ebml_edit::NewSubtitleTrack {
                        language: spec.lang.clone(),
                        name: spec.name.clone(),
                        forced: spec.forced,
                        cues,
                        codec,
                        private,
                    }),
                    Err(error) => {
                        result.error(format!("could not read {}: {}", spec.file, strerror(&error)));
                        return Ok(result);
                    }
                }
            }
            expected.push(new);
        }
        result.data = layout_data(all, &expected);
        let temp_file = format!("{}{TEMP_MARK}", media.path);
        result.commands.push(vec!["native-mkv".into(), "add-subtitles".into(), temp_file.clone()]);
        self.note(&format!("Executing:\tnative-mkv add-subtitles {temp_file}"));
        self.pend(&temp_file);
        if !self.config.dryrun {
            if let Err(error) = crate::ebml_edit::add_subtitle_tracks(Path::new(&media.path), Path::new(&temp_file), &new_tracks) {
                result.error(format!("could not add the subtitles: {}", strerror(&error)));
            }
            if result.status != Status::Error {
                let rewritten = Media::new(&temp_file, media.container);
                self.verify_layout(&mut result, &rewritten, &expected, "native")?;
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Why a set of subtitle files can't be added to an MP4, if so: only text subtitles can be
    /// converted to `mov_text`, and the forced flag of an MP4 can't be read back to check it.
    fn mp4_subtitle_refusal(specs: &[SubSpec]) -> Option<String> {
        for spec in specs {
            let extension = Path::new(&spec.file)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !MP4_SUBTITLE_FILES.contains(&extension.as_str()) {
                return Some(format!(
                    "a .{extension} subtitle can't be added to an MP4 (only {} can be converted)",
                    MP4_SUBTITLE_FILES.join(", ")
                ));
            }
        }
        None
    }

    /// Add subtitle files to an MP4, converted to `mov_text`, with `ffmpeg`. The names of all
    /// the tracks are written back, as ffmpeg can't read them from an MP4, and the muxer makes the
    /// first subtitle track default if none is, which the check expects.
    /// The cues of each subtitle file, if they can all be written in this program. `None` means ffmpeg should do the
    /// work; an unreadable file is an error on the result.
    fn mp4_cues(&self, media_path: &str, specs: &[SubSpec], result: &mut Outcome) -> Res<Option<Vec<Vec<(i64, i64, Vec<u8>)>>>> {
        let mut all = Vec::new();
        for spec in specs {
            // a dry run reads the files that are there (so the choice is the same as a real run's), but an online
            // subtitle's placeholder is not, and is taken as having no cues
            if self.config.dryrun && !Path::new(&spec.file).is_file() {
                all.push(Vec::new());
                continue;
            }
            let cues = match text_cues(Path::new(&spec.file)) {
                Ok(cues) => cues,
                Err(error) => {
                    result.error(format!("could not read {}: {}", spec.file, strerror(&error)));
                    return Ok(None);
                }
            };
            let length = std::fs::metadata(&media_path).map(|m| m.len()).unwrap_or(u64::MAX);
            if !crate::mp4::can_hold(&cues, length) {
                return Ok(None);
            }
            all.push(cues);
        }
        Ok(Some(all))
    }

    /// Add SubRip files to an MP4 in this program (the native-mp4 feature, see `mp4::add_text_tracks`). The default is
    /// chosen as the ffmpeg path chooses it: when no subtitle is the default, the first one is. The file is written
    /// beside the original, read back, and put in its place only if that passes.
    fn add_mp4_natively(
        &self,
        media: &mut Media,
        mut result: Outcome,
        specs: &[SubSpec],
        all: &[Track],
        cues: Vec<Vec<(i64, i64, Vec<u8>)>>,
    ) -> Res<Outcome> {
        let subtitles_before = all.iter().filter(|t| t.kind == Kind::Subtitles).count();
        let mut expected = all.to_vec();
        let mut changes = Vec::new();
        let ids = if self.config.dryrun {
            Vec::new()
        } else {
            match crate::mp4::track_ids(Path::new(&media.path)) {
                Ok(ids) => ids,
                Err(error) => {
                    result.error(format!("could not read the tracks: {}", strerror(&error)));
                    return Ok(result);
                }
            }
        };
        let mut new_tracks = Vec::new();
        let mut any_default = all.iter().any(|t| t.kind == Kind::Subtitles && t.default);
        // the first subtitle is the default when none is: an existing one is made the default in this same pass
        let first_existing = all.iter().position(|t| t.kind == Kind::Subtitles);
        let make_first_existing = !any_default && subtitles_before > 0;
        if make_first_existing {
            if let Some(index) = first_existing {
                expected[index].default = true;
                any_default = true;
                if let Some(trak) = ids.iter().position(|&id| id as i64 == all[index].id) {
                    changes.push(crate::mp4::TrakChange { trak, language: None, enabled: Some(true), name: None, forced: None });
                }
            }
        }
        for (k, spec) in specs.iter().enumerate() {
            let default = !any_default;
            any_default = true;
            let new = Track {
                id: 0,
                kind: Kind::Subtitles,
                number: subtitles_before + k + 1,
                codec: String::new(),
                language: spec.lang.clone(),
                default,
                forced: spec.forced,
                name: spec.name.clone(),
                channels: 0,
            };
            let file_name = Path::new(&spec.file).file_name().map(|n| n.to_string_lossy().into_owned());
            result.changes.push(format!(
                "added {} ({}) from {}",
                new.label(),
                new.language,
                file_name.unwrap_or_else(|| spec.file.clone())
            ));
            new_tracks.push(crate::mp4::NewText {
                language: spec.lang.clone(),
                name: spec.name.clone(),
                forced: spec.forced,
                enabled: default,
                cues: cues.get(k).cloned().unwrap_or_default(),
            });
            expected.push(new);
        }
        result.data = layout_data(all, &expected);
        let temp_file = temp_path(&media.path);
        result.commands.push(vec!["native-mp4".into(), "add-text-tracks".into(), temp_file.clone()]);
        self.note(&format!("Executing:\tnative-mp4 add-text-tracks {temp_file}"));
        if !self.config.dryrun {
            self.pend(&temp_file);
            if let Err(error) = crate::mp4::add_text_tracks(Path::new(&media.path), Path::new(&temp_file), &new_tracks, &changes) {
                result.error(format!("could not add the subtitles: {}", strerror(&error)));
            }
            if result.status != Status::Error {
                let rewritten = Media::new(&temp_file, media.container);
                self.verify_layout(&mut result, &rewritten, &expected, "native")?;
            }
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    fn add_to_mp4(&self, media: &mut Media, mut result: Outcome, specs: &[SubSpec], all: &[Track]) -> Res<Outcome> {
        let first_new = all.iter().filter(|t| t.kind == Kind::Subtitles).count();
        let mut subtitles: Vec<Track> = all.iter().filter(|t| t.kind == Kind::Subtitles).cloned().collect();
        let mut cmd = vec!["ffmpeg".to_string()];
        cmd.extend(args(crate::model::FFMPEG_QUIET));
        cmd.extend(args(&["-i", &media.path]));
        for spec in specs {
            cmd.extend(args(&["-i", &spec.file]));
        }
        cmd.extend(args(&["-map", "0"]));
        for index in 0..specs.len() {
            cmd.extend(args(&["-map", &format!("{}:0", index + 1)]));
        }
        cmd.extend(args(&["-c", "copy"]));
        for (index, spec) in specs.iter().enumerate() {
            let place = first_new + index;
            let new = Track {
                id: (all.len() + index) as i64,
                kind: Kind::Subtitles,
                number: place + 1,
                codec: String::new(),
                language: spec.lang.clone(),
                default: false,
                forced: spec.forced,
                name: spec.name.clone(),
                channels: 0,
            };
            let file_name = Path::new(&spec.file).file_name().map(|n| n.to_string_lossy().into_owned());
            result.changes.push(format!(
                "added {} ({}) from {}",
                new.label(),
                new.language,
                file_name.unwrap_or_else(|| spec.file.clone())
            ));
            cmd.extend(args(&[&format!("-c:s:{place}"), "mov_text"]));
            cmd.extend(args(&[&format!("-metadata:s:s:{place}"), &format!("language={}", spec.lang)]));
            subtitles.push(new);
        }
        // the muxer makes the first subtitle track default when none is
        if !subtitles.is_empty() && !subtitles.iter().any(|t| t.default) {
            subtitles[0].default = true;
        }
        cmd.extend(name_args(&kept_tracks(all, Some((Kind::Subtitles, subtitles.clone()))), &HashMap::new()));
        for (index, spec) in specs.iter().enumerate() {
            // the muxer makes a default track by itself, so the flags are only written for the others (and forced)
            let place = first_new + index;
            let flags = match (subtitles[place].default, spec.forced) {
                (true, true) => Some("default+forced"),
                (true, false) => None,
                (false, true) => Some("forced"),
                (false, false) => Some("0"),
            };
            if let Some(flags) = flags {
                cmd.extend(args(&[&format!("-disposition:s:{place}"), flags]));
            }
        }
        let temp_file = temp_path(&media.path);
        cmd.extend(Self::ffmpeg_output(media, &temp_file));
        let expected: Vec<Track> = all
            .iter()
            .filter(|t| t.kind != Kind::Subtitles)
            .cloned()
            .chain(subtitles)
            .collect();
        result.data = layout_data(all, &expected);
        self.pend(&temp_file);
        let done = self.apply_tracked(&mut result, cmd, &[0], &media.path, &temp_file, "Rewriting")?;
        if done && !self.config.dryrun {
            let rewritten = Media::new(&temp_file, media.container);
            self.verify_layout(&mut result, &rewritten, &expected, "ffmpeg")?;
        }
        self.replace_file(&mut result, &temp_file, &media.path)?;
        Ok(result.finish())
    }

    /// Write a subtitle track to a file of its own (`mkvextract` for an MKV, `ffmpeg` for an MP4,
    /// as `.srt`). The media file is not changed. The file is written under a temporary name and
    /// only given its own if the tool made something.
    fn do_extract(&self, media: &mut Media, _what: &str) -> Res<Outcome> {
        let mut result = self.result(media, "extract");
        if media.container == Container::Avi {
            return Ok(skip(result, "extracting a subtitle is only supported for MKV and MP4 files"));
        }
        let worker = self.with_kind(Kind::Subtitles);
        let Some(track) = worker.resolve_track(media, &mut result)? else { return Ok(result) };
        let Some(extension) = subtitle_extension(media.container, &track.codec) else {
            let codec = if track.codec.is_empty() { "this".to_string() } else { track.codec.clone() };
            return Ok(skip(result, &format!("a {codec} subtitle can't be extracted to a single file")));
        };
        let folder = Path::new(&media.path).parent().map(Path::to_path_buf).unwrap_or_default();
        let wanted = self.config.output.trim();
        let is_folder = wanted.ends_with(['/', '\\']) || Path::new(wanted).is_dir();
        let explicit = !wanted.is_empty() && !is_folder;
        let out: PathBuf = if explicit {
            PathBuf::from(wanted)
        } else {
            let stem = Path::new(&media.path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = format!(
                "{stem}.{}{}.{extension}",
                track.language,
                if track.forced { ".forced" } else { "" }
            );
            if wanted.is_empty() { folder.join(name) } else { Path::new(wanted).join(name) }
        };
        let out_text = out.to_string_lossy().into_owned();
        let parent = out.parent().filter(|p| !p.as_os_str().is_empty());
        if parent.is_some_and(|p| !p.is_dir()) {
            result.error(format!("the folder for {out_text} does not exist"));
            return Ok(result);
        }
        if out.exists() && !(explicit && self.config.force) {
            return Ok(skip(
                result,
                &format!("{out_text} already exists (name another file with --output, or use --force to replace it)"),
            ));
        }
        let temp_file = temp_path(&out_text);
        // a VobSub's .sub goes beside its index, under the same name
        let companion = (track.codec == "dvd_subtitle" && media.container == Container::Mkv).then(|| out.with_extension("sub"));
        if let Some(sub) = &companion {
            if sub.exists() && !(explicit && self.config.force) {
                return Ok(skip(
                    result,
                    &format!("{} already exists (name another file with --output, or use --force to replace it)", sub.display()),
                ));
            }
        }
        let temp_sub = companion.as_ref().map(|sub| temp_path(&sub.to_string_lossy()));
        result.changes.push(format!("extracted {} ({}) to {out_text}", track.label(), track.language));
        self.pend(&temp_file);
        if let Some(temp_sub) = &temp_sub {
            self.pend(temp_sub);
        }
        // a text track of an MKV is written in this program (the native-mkv feature, see ebml_subs), and so is the
        // mov_text track of an MP4 (the native-mp4 feature, see mp4::read_text_cues)
        let native_mkv = cfg!(feature = "native-mkv")
            && media.container == Container::Mkv
            && matches!(track.codec.as_str(), "subrip" | "ass" | "hdmv_pgs_subtitle" | "dvd_subtitle" | "D_WEBVTT/SUBTITLES" | "S_TEXT/WEBVTT");
        let native_mp4 = cfg!(feature = "native-mp4") && media.container == Container::Mp4;
        let native = native_mkv || native_mp4;
        let done = if native_mp4 {
            result.commands.push(vec!["native-mp4".into(), "extract-subtitles".into(), temp_file.clone()]);
            self.note(&format!("Executing:\tnative-mp4 extract-subtitles {temp_file}"));
            if self.config.dryrun {
                true
            } else {
                match crate::mp4::read_text_cues(Path::new(&media.path), track.id as u32) {
                    Ok(cues) => {
                        let cues: Vec<crate::ebml_subs::Cue> = cues
                            .into_iter()
                            .map(|(start_ms, end_ms, payload)| crate::ebml_subs::Cue { start_ms, end_ms, payload })
                            .collect();
                        if let Err(error) = fs::write(&temp_file, crate::ebml_subs::srt_text(&cues)) {
                            result.error(format!("could not extract the subtitles: {}", strerror(&error)));
                        }
                    }
                    Err(error) => result.error(format!("could not extract the subtitles: {}", strerror(&error))),
                }
                result.status != Status::Error
            }
        } else if native_mkv {
            result.commands.push(vec!["native-mkv".into(), "extract-subtitles".into(), temp_file.clone()]);
            self.note(&format!("Executing:\tnative-mkv extract-subtitles {temp_file}"));
            if self.config.dryrun {
                true
            } else {
                let track_number = track.id as u64 + 1;
                let written = match (track.codec.as_str(), &temp_sub) {
                    ("hdmv_pgs_subtitle", _) => crate::ebml_subs::write_pgs(Path::new(&media.path), track_number, Path::new(&temp_file)),
                    ("D_WEBVTT/SUBTITLES", _) => crate::ebml_subs::write_webvtt(Path::new(&media.path), track_number, Path::new(&temp_file)),
                    ("S_TEXT/WEBVTT", _) => crate::ebml_subs::write_webvtt_text(Path::new(&media.path), track_number, Path::new(&temp_file)),
                    ("dvd_subtitle", Some(temp_sub)) => crate::ebml_subs::write_vobsub(
                        Path::new(&media.path),
                        track_number,
                        &track.language,
                        Path::new(&temp_file),
                        Path::new(temp_sub),
                    ),
                    _ => crate::ebml_subs::write_subtitles(Path::new(&media.path), track_number, extension, Path::new(&temp_file)),
                };
                if let Err(error) = written {
                    result.error(format!("could not extract the subtitles: {}", strerror(&error)));
                }
                result.status != Status::Error
            }
        } else {
            // an MP4 is converted by ffmpeg when its text can't be read here, so ffmpeg must be there for that
            if media.container == Container::Mp4 {
                worker.require(&["ffmpeg"])?;
            }
            let cmd = self.extract_command(media, &track, &temp_file);
            self.apply(&mut result, cmd, &[0, 1])?
        };
        if done && !self.config.dryrun {
            let made = fs::metadata(&temp_file).map(|m| m.len() > 0).unwrap_or(false);
            if !made {
                let message = if native {
                    "the track has no subtitles to write".to_string()
                } else {
                    format!("{} wrote nothing", if media.container == Container::Mkv { "mkvextract" } else { "ffmpeg" })
                };
                result.error(message);
            } else if let Err(error) = fs::rename(&temp_file, &out) {
                result.error(format!("could not write {out_text}: {}", strerror(&error)));
            } else if let (Some(temp_sub), Some(sub)) = (&temp_sub, &companion) {
                if let Err(error) = fs::rename(temp_sub, sub) {
                    result.error(format!("could not write {}: {}", sub.display(), strerror(&error)));
                }
            }
        }
        if result.status == Status::Error {
            let _ = fs::remove_file(&temp_file);
            if let Some(temp_sub) = &temp_sub {
                let _ = fs::remove_file(temp_sub);
            }
        }
        self.shared.pending.lock().unwrap().retain(|p| p != &temp_file);
        if let Some(temp_sub) = &temp_sub {
            self.shared.pending.lock().unwrap().retain(|p| p != temp_sub);
        }
        result.data = json!({"output": out_text});
        Ok(result)
    }

    /// The command that writes a subtitle track to `temp_file` with mkvextract, or with ffmpeg when mkvextract
    /// is missing (or the file is not an MKV).
    fn extract_command(&self, media: &Media, track: &Track, temp_file: &str) -> Vec<String> {
        // SPIKE: with mkvextract missing, ffmpeg copies the track out in its own format
        if media.container == Container::Mkv && self.find("mkvextract").is_some() {
            args(&["mkvextract", "tracks", &media.path, &format!("{}:{temp_file}", track.id)])
        } else if media.container == Container::Mkv {
            let mut cmd = vec!["ffmpeg".to_string()];
            cmd.extend(args(crate::model::FFMPEG_QUIET));
            cmd.extend(args(&["-i", &media.path, "-map", &format!("0:s:{}", track.number - 1), "-c", "copy", temp_file]));
            cmd
        } else {
            let mut cmd = vec!["ffmpeg".to_string()];
            cmd.extend(args(crate::model::FFMPEG_QUIET));
            cmd.extend(args(&["-i", &media.path, "-map", &format!("0:s:{}", track.number - 1)]));
            cmd.extend(args(&["-c:s", "srt", temp_file]));
            cmd
        }
    }
    fn do_preserve(&self, media: &mut Media) -> Res<Outcome> {
        let result = self.result(media, "preserve");
        if media.container == Container::Avi {
            return Ok(skip(
                result,
                "preserving by language is only supported for MKV and MP4 files",
            ));
        }
        let tracks = self.selected(media)?;
        let languages = if self.config.keep.is_empty() {
            self.languages(&self.config.lang)
        } else {
            self.config.keep.clone()
        };
        // --keep keeps every language in the list; otherwise the first of them that is present
        let kept: Vec<String> = if self.config.keep.is_empty() {
            languages
                .iter()
                .find(|lang| tracks.iter().any(|t| &t.language == *lang))
                .cloned()
                .into_iter()
                .collect()
        } else {
            languages.iter().filter(|lang| tracks.iter().any(|t| &t.language == *lang)).cloned().collect()
        };
        if kept.is_empty() {
            if !self.config.force {
                let message = format!(
                    "{}, so nothing would be left (use --force to remove them all)",
                    self.missing_message(&languages)
                );
                return Ok(skip(result, &message));
            }
            return self.remove_tracks(media, result, tracks);
        }
        let remove = tracks.into_iter().filter(|t| !kept.contains(&t.language)).collect();
        self.remove_tracks(media, result, remove)
    }

    // Dispatch -----------------------------------------------------------------

    /// Run a task on one file.
    pub fn process(&self, task: &Task, path: &str) -> Res<Option<Outcome>> {
        let path = &followed(path);
        if !Path::new(path).is_file() {
            return Ok(Some(
                Outcome::new(path, &task.name, false).with_status(Status::Error, "does not exist"),
            ));
        }
        let Some(container) = detect_kind(path) else {
            if !self.config.file.is_empty() || !self.config.paths.is_empty() {
                return Ok(Some(
                    Outcome::new(path, &task.name, false)
                        .with_status(Status::Error, "is not a supported media file"),
                ));
            }
            self.note(&format!("Skipping {path}: not a supported media file"));
            return Ok(None);
        };
        let tools = tools_for(&task.name, container);
        self.require(tools)?;
        let mut media = Media::new(path, container);
        if cfg!(feature = "native-mkv") && container == Container::Mkv {
            // read once, here: an MKV the tracks of which cannot be read is an error for this file alone
            match crate::ebml::read_file_tracks(Path::new(path)) {
                Ok(tracks) => media.tracks = Some(tracks),
                Err(error) => {
                    let reason = format!("cannot be read as an MKV: {}", strerror(&error));
                    return Ok(Some(Outcome::new(path, &task.name, false).with_status(Status::Error, &reason)));
                }
            }
        }
        if container == Container::Mkv && self.tracks(&mut media)?.is_empty() {
            // an MKV with no tracks is a cut-short or damaged file (mkvmerge reads one without an error)
            return Ok(Some(Outcome::new(path, &task.name, false).with_status(
                Status::Error,
                "cannot be read as an MKV: no tracks found (the file is cut short or damaged)",
            )));
        }
        let outcome = self.dispatch(task, &mut media);
        self.release_space(path);
        let outcome = outcome?;
        if !self.config.log.is_empty() && outcome.status == Status::Changed && !self.config.dryrun {
            self.write_log(&mut media, &outcome)?;
        }
        Ok(Some(outcome))
    }

    fn dispatch(&self, task: &Task, media: &mut Media) -> Res<Outcome> {
        match task.name.as_str() {
            "info" => self.do_info(media),
            "get" => self.do_get(media),
            "detect" => self.do_detect(media),
            "search" => self.do_search(media),
            "update" => self.do_update(media),
            "set" => self.do_set(media, &task.what),
            "add" => self.do_add(media, &task.what),
            "extract" => self.do_extract(media, &task.what),
            "swap" => self.do_swap(media),
            "delete" => self.do_delete(media, &task.what),
            _ => self.do_preserve(media),
        }
    }

    // The log and undoing ------------------------------------------------------

    /// Add a line to the log about a change: what the tracks were and are now.
    fn write_log(&self, media: &mut Media, result: &Outcome) -> Res<()> {
        let after = self.read_tracks(&Media::new(&media.path, media.container))?;
        let entry = json!({
            "time": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            "version": VERSION, "file": abspath(&media.path),
            "action": result.action, "kind": self.kind().name(), "changes": result.changes,
            "moved_from": if result.moved_from > 0 { json!(result.moved_from) } else { Value::Null },
            "order": if result.order.is_empty() { Value::Null } else { json!(result.order) },
            "backup": if result.backup.is_empty() { Value::Null }
                      else { json!(abspath(&result.backup)) },
            "before": media.tracks.clone().unwrap_or_default(),
            "after": after,
        });
        let _guard = self.shared.log.lock().unwrap();
        let written = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.log)
            .and_then(|mut handle| {
                use std::io::Write;
                writeln!(handle, "{entry}")
            });
        if let Err(error) = written {
            self.warn(&format!("could not write the log {}: {}", self.config.log, strerror(&error)));
        }
        Ok(())
    }

    /// The entries of a log, oldest first.
    pub fn read_log(&self, path: &str) -> io::Result<Vec<Value>> {
        let text = fs::read_to_string(path)?;
        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(entry) => entries.push(entry),
                Err(_) => self.warn(&format!(
                    "{path}: line {} is not valid JSON, so it was ignored",
                    index + 1
                )),
            }
        }
        Ok(entries)
    }

    /// Whether a file in the log is one --file or --dir (if given) asks about.
    fn in_scope(&self, file: &str) -> bool {
        if !self.config.file.is_empty() {
            return followed(&abspath(&self.config.file)) == file;
        }
        if !self.config.dir.is_empty() {
            let base = abspath(&self.config.dir);
            let folder = Path::new(file).parent().unwrap_or(Path::new(""));
            return folder == Path::new(&base)
                || (self.config.recursive && folder.starts_with(&base));
        }
        true
    }

    /// The edits that put the tracks of the kind back as they were.
    fn restore_edits(
        &self,
        current: &[Track],
        before: &[Track],
        moved_from: Option<usize>,
        order: &[usize],
    ) -> Vec<Edit> {
        let current: Vec<Track> = current.iter().filter(|t| t.kind == self.kind()).cloned().collect();
        let wanted: Vec<&Track> = before.iter().filter(|t| t.kind == self.kind()).collect();
        // the places the tracks go back to (by their number now), and the tracks in the old order
        let mut back: HashMap<usize, usize> = HashMap::new();
        let mut restored = current.clone();
        if order.len() == current.len() && !order.is_empty() {
            // current[p] was track order[p]
            restored = (1..=current.len())
                .filter_map(|old| order.iter().position(|o| *o == old).map(|p| current[p].clone()))
                .collect();
            for (p, track) in current.iter().enumerate() {
                if order[p] != p + 1 {
                    back.insert(track.number, order[p]);
                }
            }
        } else if let (Some(from), Some(first)) = (moved_from, current.first()) {
            if from != 1 {
                restored.retain(|t| t.number != first.number);
                restored.insert((from - 1).min(restored.len()), first.clone());
                back.insert(first.number, from);
            }
        }
        let mut edits = Vec::new();
        for (now, old) in restored.iter().zip(wanted) {
            let mut edit = Edit::new(now);
            edit.default = (now.default != old.default).then_some(old.default);
            edit.language = (now.language != old.language).then(|| old.language.clone());
            edit.name = (now.name != old.name).then(|| old.name.clone());
            edit.forced = (now.forced != old.forced).then_some(old.forced);
            edit.move_to = back.get(&now.number).copied();
            if edit.default.is_some() || edit.language.is_some() || edit.name.is_some()
                || edit.forced.is_some() || edit.move_to.is_some()
            {
                edits.push(edit);
            }
        }
        edits
    }

    /// Put the kept original back, once it is checked to be what was logged.
    fn restore_backup(&self, mut result: Outcome, media: &Media, backup: &str, before: &[Track]) -> Res<Outcome> {
        let kept = self.read_tracks(&Media::new(backup, media.container))?;
        if !same_state(&kept, before) {
            return Ok(skip(
                result,
                &format!("the backup {backup} is not what was logged, so it was not used"),
            ));
        }
        result.changes.push(format!("restored from {backup}"));
        if !self.config.dryrun {
            if let Err(error) = fs::rename(backup, &media.path) {
                result.error(format!("could not restore the backup: {}", strerror(&error)));
                return Ok(result);
            }
        }
        result.status = Status::Changed;
        Ok(result)
    }

    /// Undo the changes in a log, newest first.
    pub fn undo_log(&self, path: &str, report: &mut dyn FnMut(Outcome)) -> Res<()> {
        let entries = self
            .read_log(path)
            .map_err(|e| Stop::Fatal(format!("could not read the log {path}: {}", strerror(&e))))?;
        let mut planned: Vec<String> = Vec::new(); // in a dry run, files with only a planned undo
        for entry in entries.iter().rev() {
            let file = entry.get("file").and_then(Value::as_str).unwrap_or("").to_string();
            if !self.in_scope(&file) {
                continue;
            }
            if self.config.dryrun && planned.contains(&file) {
                report(Outcome::new(&file, "undo", true).with_status(
                    Status::Unchanged,
                    "an older change: it can be checked once the newer one above has really been \
                     undone",
                ));
                continue;
            }
            let result = self.undo_entry(entry)?;
            if self.config.dryrun && result.status == Status::Changed {
                planned.push(file);
            }
            report(result);
        }
        Ok(())
    }

    /// Reverse one change from the log, if the file is as that change left it.
    fn undo_entry(&self, entry: &Value) -> Res<Outcome> {
        let path = entry.get("file").and_then(Value::as_str).unwrap_or("").to_string();
        let result = Outcome::new(&path, "undo", self.config.dryrun);
        let parsed = (|| -> Option<(Vec<Track>, Vec<Track>, Kind)> {
            let before = serde_json::from_value(entry.get("before")?.clone()).ok()?;
            let after = serde_json::from_value(entry.get("after")?.clone()).ok()?;
            let kind = serde_json::from_value(entry.get("kind")?.clone()).ok()?;
            Some((before, after, kind))
        })();
        let Some((before, after, kind)) = parsed else {
            return Ok(skip(result, "this log entry is not understood"));
        };
        if !Path::new(&path).is_file() {
            return Ok(skip(result, "no longer exists"));
        }
        let Some(container) = detect_kind(&path) else {
            return Ok(skip(result, "is not a supported media file"));
        };
        self.require(container.write_tools())?;
        let mut media = Media::new(&path, container);
        let current = self.tracks(&mut media)?;
        if same_state(&current, &before) && !same_state(&current, &after) {
            return Ok(result.with_status(Status::Unchanged, "is already as it was before this change"));
        }
        if !same_state(&current, &after) {
            return Ok(skip(result, "has changed since it was logged, so it was left alone"));
        }
        if let Some(backup) = entry.get("backup").and_then(Value::as_str) {
            if Path::new(backup).is_file() {
                return self.restore_backup(result, &media, backup, &before);
            }
        }
        if before.len() != after.len() || container == Container::Avi {
            return Ok(skip(
                result,
                "can't be undone without the backup it would need (the change removed tracks or \
                 rewrote an AVI; use --backup)",
            ));
        }
        let worker = self.with_kind(kind);
        let moved_from = entry.get("moved_from").and_then(Value::as_u64).map(|n| n as usize);
        let order: Vec<usize> = entry
            .get("order")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).map(|n| n as usize).collect())
            .unwrap_or_default();
        let edits = worker.restore_edits(&current, &before, moved_from, &order);
        worker.edit_tracks(&mut media, result, edits, "is already as it was")
    }

    // Running ------------------------------------------------------------------

    fn report(&self, results: &mut Vec<Outcome>, outcome: Outcome) {
        (self.sink)(Event::Result(Box::new(outcome.clone())));
        results.push(outcome);
    }

    /// Process several files at once, reporting them in the order they were found, so the
    /// output doesn't depend on which finished first.
    fn process_in_parallel(&self, task: &Task, paths: &[String], results: &mut Vec<Outcome>) -> Res<()> {
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let (send, receive) = mpsc::channel::<(usize, Res<Option<Outcome>>)>();
        let mut failure = None;
        std::thread::scope(|scope| {
            for _ in 0..self.config.jobs.min(paths.len()) {
                let send = send.clone();
                let (next, stop) = (&next, &stop);
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    if index >= paths.len() || stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = send.send((index, self.process(task, &paths[index])));
                });
            }
            drop(send);
            let mut waiting: BTreeMap<usize, Res<Option<Outcome>>> = BTreeMap::new();
            let mut want = 0;
            for (index, outcome) in receive {
                waiting.insert(index, outcome);
                while let Some(outcome) = waiting.remove(&want) {
                    want += 1;
                    match outcome {
                        Ok(Some(outcome)) => self.report(results, outcome),
                        Ok(None) => {}
                        Err(stopped) => {
                            // a stop or a fatal error: start no more, then let it go on
                            stop.store(true, Ordering::SeqCst);
                            failure.get_or_insert(stopped);
                            waiting.clear();
                            break;
                        }
                    }
                }
            }
        });
        match failure {
            Some(stopped) => Err(stopped),
            None => Ok(()),
        }
    }

    fn run_tasks(&self, results: &mut Vec<Outcome>) -> Res<()> {
        for task in &self.config.tasks {
            if task.name == "check" {
                let outcome = self.check_tools()?;
                self.report(results, outcome);
                continue;
            }
            // only the speech model: unlike `check --install` it installs no packages, so the app can use it
            if task.name == "model" {
                let mut outcome = Outcome::new("", "model", self.config.dryrun);
                self.install_model(&mut outcome)?;
                self.report(results, outcome);
                continue;
            }
            if task.name == "undo" {
                let mut collected = Vec::new();
                let done = self.undo_log(&task.what, &mut |outcome| {
                    (self.sink)(Event::Result(Box::new(outcome.clone())));
                    collected.push(outcome);
                });
                results.extend(collected);
                done?;
                continue;
            }
            // each action works with its own settings on top of the shared ones
            let mut worker = self.for_task(task);
            let paths = worker.files()?;
            if task.name == "add" && !worker.config.sub_dir.is_empty() {
                worker.plan_subtitle_folder(&paths);
            }
            if worker.config.jobs > 1 && paths.len() > 1 {
                worker.process_in_parallel(task, &paths, results)?;
                continue;
            }
            for path in &paths {
                if cancelled() {
                    return Err(Stop::Interrupted);
                }
                if let Some(outcome) = worker.process(task, path)? {
                    worker.report(results, outcome);
                }
            }
        }
        Ok(())
    }

    /// Run the tasks; the results and the exit status.
    pub fn run(&self) -> RunEnd {
        let mut results = Vec::new();
        let ended = self.run_tasks(&mut results);
        self.cleanup();
        let code = match ended {
            Err(Stop::Fatal(message)) => {
                self.warn(&message);
                1
            }
            Err(Stop::Interrupted) => {
                self.warn("interrupted");
                130
            }
            Ok(()) => i32::from(results.iter().any(|r| r.status == Status::Error)),
        };
        RunEnd { results, code }
    }
}

/// The text a front end prints for an `info` result.
pub fn info_text(outcome: &Outcome) -> String {
    let container = outcome.data["container"].as_str().unwrap_or("");
    let tracks: Vec<Track> =
        serde_json::from_value(outcome.data["tracks"].clone()).unwrap_or_default();
    let mut text = format!("{} ({container})\n", outcome.file);
    if tracks.is_empty() {
        text.push_str("no tracks found");
    } else {
        text.push_str(&format_tracks(&tracks));
    }
    text
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
mod follow_tests {
    use super::followed;

    #[test]
    fn a_linked_folder_is_read_and_a_loop_is_not_followed_round_again() {
        let root = std::env::temp_dir().join(format!("mau-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let show = root.join("show");
        let outside = root.join("outside");
        std::fs::create_dir_all(show.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(show.join("a.mkv"), b"x").unwrap();
        std::fs::write(show.join("sub").join("b.mkv"), b"x").unwrap();
        std::fs::write(outside.join("c.mkv"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, show.join("ext")).unwrap();
        std::os::unix::fs::symlink(&show, show.join("sub").join("up")).unwrap();
        let mut found = Vec::new();
        super::walk(&show, true, &mut found);
        let mut files: Vec<String> = found
            .iter()
            .filter(|p| p.is_file())
            .map(|p| p.canonicalize().unwrap().to_string_lossy().into_owned())
            .collect();
        files.sort();
        let want = |p: &std::path::Path| p.canonicalize().unwrap().to_string_lossy().into_owned();
        let mut expected = vec![want(&show.join("a.mkv")), want(&show.join("sub").join("b.mkv")), want(&outside.join("c.mkv"))];
        expected.sort();
        assert_eq!(files, expected, "each file once, the linked folder's file included");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_symbolic_link_leads_to_the_file_it_points_to() {
        let dir = std::env::temp_dir().join(format!("mau-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.mkv");
        let link = dir.join("link.mkv");
        let gone = dir.join("gone.mkv");
        std::fs::write(&real, b"x").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::os::unix::fs::symlink(dir.join("nothing.mkv"), &gone).unwrap();
        let real_text = real.canonicalize().unwrap().to_string_lossy().into_owned();
        assert_eq!(followed(&link.to_string_lossy()), real_text);
        assert_eq!(followed(&real.to_string_lossy()), real.to_string_lossy());
        assert_eq!(followed(&gone.to_string_lossy()), gone.to_string_lossy(), "a dangling link stays as it is");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
