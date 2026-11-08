//! Tracks, and reading them from the output of `mkvmerge` and `mediainfo`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::languages::normalise_language;

/// The kinds of track in a media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Audio,
    Subtitles,
}

impl Kind {
    /// The letter `mkvpropedit` (`track:a1`) and `ffmpeg` (`-map 0:a:0`) use.
    pub fn selector(self) -> &'static str {
        match self {
            Kind::Video => "v",
            Kind::Audio => "a",
            Kind::Subtitles => "s",
        }
    }

    /// How a kind of track is named in messages.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Subtitles => "subtitle",
        }
    }

    /// The name used in JSON and in the log.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Subtitles => "subtitles",
        }
    }

    /// The kind of track a command line word names (`sub` works for subtitles).
    pub fn parse(text: &str) -> Option<Kind> {
        match text.trim().to_lowercase().as_str() {
            "audio" => Some(Kind::Audio),
            "sub" | "subs" | "subtitle" | "subtitles" => Some(Kind::Subtitles),
            _ => None,
        }
    }
}

/// What `--track` names: a position among the tracks of the kind being worked on, or one
/// track of any kind by its place (`subtitles:18`), its ID (`id:20`) or its name (`name:Forced`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackSpec {
    /// Position from 1 among the tracks of the kind being worked on.
    Number(usize),
    /// The last track of the kind being worked on.
    Last,
    /// Position from 1 among the tracks of the given kind (`None` is the last one).
    Of(Kind, Option<usize>),
    /// The track ID, as `--info` and `mkvmerge` show it.
    Id(i64),
    /// The track name, ignoring case.
    Name(String),
}

impl TrackSpec {
    pub fn parse(text: &str) -> Result<TrackSpec, String> {
        let bad = || format!("\"{text}\" is not a track number (from 1), \"last\", audio:N, subtitles:N, id:N or name:TEXT");
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        let position = |s: &str| if digits(s) { s.parse::<usize>().ok().filter(|n| *n >= 1) } else { None };
        if text == "last" {
            return Ok(TrackSpec::Last);
        }
        if let Some(n) = position(text) {
            return Ok(TrackSpec::Number(n));
        }
        let Some((head, rest)) = text.split_once(':') else { return Err(bad()) };
        match head.to_lowercase().as_str() {
            "id" if digits(rest) => rest.parse().map(TrackSpec::Id).map_err(|_| bad()),
            "name" if !rest.is_empty() => Ok(TrackSpec::Name(rest.to_string())),
            other => {
                let kind = if other == "video" { Some(Kind::Video) } else { Kind::parse(other) };
                match (kind, rest) {
                    (Some(kind), "last") => Ok(TrackSpec::Of(kind, None)),
                    (Some(kind), _) => position(rest).map(|n| TrackSpec::Of(kind, Some(n))).ok_or_else(bad),
                    _ => Err(bad()),
                }
            }
        }
    }
}

/// A track in a media file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    /// The track ID (the one `mkvmerge` uses).
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Position among the tracks of its kind, from 1.
    pub number: usize,
    pub codec: String,
    /// ISO 639-2/B.
    pub language: String,
    pub default: bool,
    pub forced: bool,
    pub name: String,
    /// The number of audio channels, when the tool says (0 when it does not, and for other kinds).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub channels: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Track {
    pub fn label(&self) -> String {
        format!("{} track {}", self.kind.word(), self.number)
    }

    /// The same track, whichever file or position it is read from: what identifies it
    /// among the tracks of one file.
    pub fn key(&self) -> (Kind, usize) {
        (self.kind, self.number)
    }
}

/// A JSON value as text, the way Python's `str()` shows strings and numbers.
fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn digits(value: Option<&Value>) -> Option<i64> {
    let text = text(value);
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

/// Tracks from the output of `mkvmerge -F json -i`.
pub fn parse_mkv_tracks(data: &Value) -> Result<Vec<Track>, String> {
    let rows = match data.get("tracks") {
        None => return Ok(Vec::new()),
        Some(Value::Array(rows)) => rows,
        Some(_) => return Err("tracks is not a list".into()),
    };
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut tracks = Vec::new();
    for (index, raw) in rows.iter().enumerate() {
        let kind_name = text(raw.get("type"));
        let count = counts.entry(kind_name.clone()).or_insert(0);
        *count += 1;
        let kind = match kind_name.as_str() {
            "video" => Kind::Video,
            "audio" => Kind::Audio,
            "subtitles" => Kind::Subtitles,
            // buttons and the like are not tracks that can be edited
            _ => continue,
        };
        let props = raw.get("properties");
        let prop = |name: &str| props.and_then(|p| p.get(name));
        tracks.push(Track {
            id: raw.get("id").and_then(Value::as_i64).unwrap_or(index as i64),
            kind,
            number: *count,
            codec: text(raw.get("codec")),
            language: normalise_language(prop("language").and_then(Value::as_str)),
            default: prop("default_track").and_then(Value::as_bool).unwrap_or(false),
            forced: prop("forced_track").and_then(Value::as_bool).unwrap_or(false),
            name: text(prop("track_name")),
            channels: prop("audio_channels").and_then(Value::as_u64).unwrap_or(0) as usize,
        });
    }
    Ok(tracks)
}

/// Tracks from the output of `ffprobe -show_streams -of json` (an MKV file, with no mkvtoolnix). A track's ID is
/// its place among the file's tracks (attachments and the like are not counted), which is the ID mkvmerge gives.
pub fn parse_ffprobe_tracks(data: &Value) -> Result<Vec<Track>, String> {
    let rows = match data.get("streams") {
        None => return Ok(Vec::new()),
        Some(Value::Array(rows)) => rows,
        Some(_) => return Err("streams is not a list".into()),
    };
    let mut counts: HashMap<Kind, usize> = HashMap::new();
    let mut tracks = Vec::new();
    let mut id = 0i64;
    for raw in rows {
        let kind = match text(raw.get("codec_type")).as_str() {
            "video" => Kind::Video,
            "audio" => Kind::Audio,
            "subtitle" => Kind::Subtitles,
            _ => continue,
        };
        let count = counts.entry(kind).or_insert(0);
        *count += 1;
        let tags = raw.get("tags");
        let disposition = raw.get("disposition");
        let flag = |name: &str| disposition.and_then(|d| d.get(name)).and_then(Value::as_i64).unwrap_or(0) == 1;
        tracks.push(Track {
            id,
            kind,
            number: *count,
            codec: text(raw.get("codec_name")),
            language: normalise_language(tags.and_then(|t| t.get("language")).and_then(Value::as_str)),
            default: flag("default"),
            forced: flag("forced"),
            name: text(tags.and_then(|t| t.get("title"))),
            channels: raw.get("channels").and_then(Value::as_u64).unwrap_or(0) as usize,
        });
        id += 1;
    }
    Ok(tracks)
}

/// Tracks from the output of `mediainfo --Output=JSON`.
pub fn parse_mediainfo_tracks(data: &Value) -> Result<Vec<Track>, String> {
    let rows = match data.get("media").and_then(|media| media.get("track")) {
        None => return Ok(Vec::new()),
        Some(Value::Array(rows)) => rows,
        Some(_) => return Err("track is not a list".into()),
    };
    let mut counts: HashMap<Kind, usize> = HashMap::new();
    let mut tracks = Vec::new();
    for (index, raw) in rows.iter().enumerate() {
        let kind = match text(raw.get("@type")).as_str() {
            "Video" => Kind::Video,
            "Audio" => Kind::Audio,
            "Text" => Kind::Subtitles,
            _ => continue,
        };
        let count = counts.entry(kind).or_insert(0);
        *count += 1;
        let number = digits(raw.get("@typeorder")).map_or(*count, |n| n as usize);
        tracks.push(Track {
            id: digits(raw.get("ID")).unwrap_or(index as i64),
            kind,
            number,
            codec: text(raw.get("Format")),
            language: normalise_language(raw.get("Language").and_then(Value::as_str)),
            default: raw.get("Default").and_then(Value::as_str) == Some("Yes"),
            // mediainfo writes "Yes / No" for a forced MP4 subtitle (and "No" for a plain one), so any
            // value that starts with "Yes" counts
            forced: raw.get("Forced").and_then(Value::as_str).is_some_and(|f| f.starts_with("Yes")),
            name: text(raw.get("Title")),
            channels: if kind == Kind::Audio { digits(raw.get("Channels")).unwrap_or(0) as usize } else { 0 },
        });
    }
    Ok(tracks)
}

/// A table of tracks.
pub fn format_tracks(tracks: &[Track]) -> String {
    let mut rows: Vec<Vec<String>> = vec![["Track", "ID", "Codec", "Language", "Default", "Forced", "Ch", "Name"]
        .iter()
        .map(|s| s.to_string())
        .collect()];
    for track in tracks {
        rows.push(vec![
            format!("{}:{}", track.kind.name(), track.number),
            track.id.to_string(),
            track.codec.clone(),
            track.language.clone(),
            if track.default { "yes" } else { "-" }.into(),
            if track.forced { "yes" } else { "-" }.into(),
            if track.channels > 0 { track.channels.to_string() } else { "-".into() },
            track.name.clone(),
        ]);
    }
    let widths: Vec<usize> = (0..8)
        .map(|column| rows.iter().map(|row| row[column].chars().count()).max().unwrap_or(0))
        .collect();
    rows.iter()
        .map(|row| {
            let line: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    let pad = widths[column] - cell.chars().count();
                    format!("{cell}{}", " ".repeat(pad))
                })
                .collect();
            line.join("  ").trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// A size in bytes as a short string (1.5 GB).
pub fn human_size(size: f64) -> String {
    let mut size = size;
    for unit in ["bytes", "KB", "MB", "GB"] {
        if size < 1024.0 {
            return if unit == "bytes" {
                format!("{size:.0} {unit}")
            } else {
                format!("{size:.1} {unit}")
            };
        }
        size /= 1024.0;
    }
    format!("{size:.1} TB")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_forced_mp4_subtitle_is_read_as_forced_whatever_mediainfo_writes_after_yes() {
        let data = json!({"media": {"track": [
            {"@type": "Text", "@typeorder": "1", "ID": "3", "Language": "it", "Default": "Yes",
             "Forced": "Yes / No"},
            {"@type": "Text", "@typeorder": "2", "ID": "4", "Language": "en", "Default": "No",
             "Forced": "No"},
        ]}});
        let tracks = parse_mediainfo_tracks(&data).unwrap();
        assert_eq!((tracks[0].forced, tracks[1].forced), (true, false));
    }

    #[test]
    fn mkv_tracks_are_numbered_within_their_kind() {
        let data = json!({"tracks": [
            {"id": 0, "type": "video", "codec": "HEVC", "properties": {}},
            {"id": 1, "type": "audio", "codec": "AAC",
             "properties": {"language": "ita", "default_track": true, "track_name": "Italiano"}},
            {"id": 2, "type": "audio", "codec": "AAC",
             "properties": {"language": "eng", "default_track": false}},
            {"id": 3, "type": "subtitles", "codec": "SRT",
             "properties": {"language": "fra", "forced_track": true}},
        ]});
        let tracks = parse_mkv_tracks(&data).unwrap();
        assert_eq!(tracks.len(), 4);
        assert_eq!((tracks[1].kind, tracks[1].number, tracks[1].default), (Kind::Audio, 1, true));
        assert_eq!((tracks[2].number, tracks[2].language.as_str()), (2, "eng"));
        assert_eq!(tracks[1].name, "Italiano");
        assert_eq!(tracks[3].language, "fre");
        assert!(tracks[3].forced);
    }

    #[test]
    fn mediainfo_languages_are_normalised() {
        let data = json!({"media": {"track": [
            {"@type": "General"},
            {"@type": "Audio", "@typeorder": "2", "ID": "3", "Format": "AAC",
             "Language": "en", "Default": "Yes", "Title": "Main"},
            {"@type": "Text", "Language": "it", "Default": "No"},
        ]}});
        let tracks = parse_mediainfo_tracks(&data).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!((tracks[0].number, tracks[0].id), (2, 3));
        assert_eq!(tracks[0].language, "eng");
        assert!(tracks[0].default);
        assert_eq!((tracks[1].kind, tracks[1].number, tracks[1].language.as_str()),
                   (Kind::Subtitles, 1, "ita"));
    }

    #[test]
    fn table_is_aligned_and_trimmed() {
        let data = json!({"tracks": [
            {"id": 1, "type": "audio", "codec": "AAC",
             "properties": {"language": "eng", "audio_channels": 6}}]});
        let table = format_tracks(&parse_mkv_tracks(&data).unwrap());
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "Track    ID  Codec  Language  Default  Forced  Ch  Name");
        assert_eq!(lines[1], "audio:1  1   AAC    eng       -        -       6");
    }

    #[test]
    fn channels_are_read_from_both_tools_and_only_for_audio() {
        let mkv = json!({"tracks": [
            {"id": 0, "type": "video", "codec": "AVC", "properties": {}},
            {"id": 1, "type": "audio", "codec": "AC-3", "properties": {"audio_channels": 2}},
            {"id": 2, "type": "audio", "codec": "AAC", "properties": {}}]});
        let tracks = parse_mkv_tracks(&mkv).unwrap();
        assert_eq!(tracks.iter().map(|t| t.channels).collect::<Vec<_>>(), [0, 2, 0]);
        let info = json!({"media": {"track": [
            {"@type": "Video", "ID": "1", "Channels": "9"},
            {"@type": "Audio", "ID": "2", "Channels": "6"},
            {"@type": "Audio", "ID": "3"}]}});
        let tracks = parse_mediainfo_tracks(&info).unwrap();
        assert_eq!(tracks.iter().map(|t| t.channels).collect::<Vec<_>>(), [0, 6, 0]);
        // 0 is left out of the JSON, so the output of the older tools is the same
        assert!(serde_json::to_value(&tracks[2]).unwrap().get("channels").is_none());
        assert_eq!(serde_json::to_value(&tracks[1]).unwrap()["channels"], 6);
    }

    #[test]
    fn sizes_are_short() {
        assert_eq!(human_size(10.0), "10 bytes");
        assert_eq!(human_size(1536.0 * 1024.0), "1.5 MB");
    }
}
