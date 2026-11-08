//! Reading the track list of a Matroska (MKV, WebM) file without ffprobe or mkvmerge.
//!
//! Matroska is EBML: a tree of elements, each an ID, a size and a payload. The track list is
//! in the `Tracks` element near the start of the `Segment`, before the first `Cluster`, so
//! reading stops there and the media data is never read. This is the first step of removing
//! ffmpeg from the MKV work (see `APPSTORE.md`): the tracks it gives match what `mkvmerge -J`
//! reports (`mkvmerge` is the reference the tests compare with).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::languages::normalise_language;
use crate::track::{Kind, Track};

pub(crate) const EBML_HEADER: u32 = 0x1A45_DFA3;
pub(crate) const SEGMENT: u32 = 0x1853_8067;
pub(crate) const TRACKS: u32 = 0x1654_AE6B;
pub(crate) const TRACK_ENTRY: u32 = 0xAE;
pub(crate) const TRACK_NUMBER: u32 = 0xD7;
pub(crate) const TRACK_TYPE: u32 = 0x83;
pub(crate) const CODEC_ID: u32 = 0x86;
pub(crate) const LANGUAGE: u32 = 0x22B5_9C;
pub(crate) const LANGUAGE_IETF: u32 = 0x22B5_9D;
pub(crate) const NAME: u32 = 0x536E;
pub(crate) const FLAG_DEFAULT: u32 = 0x88;
pub(crate) const FLAG_FORCED: u32 = 0x55AA;
pub(crate) const AUDIO: u32 = 0xE1;
pub(crate) const CHANNELS: u32 = 0x9F;
pub(crate) const CODEC_PRIVATE: u32 = 0x63A2;
pub(crate) const CONTENT_ENCODINGS: u32 = 0x6D80;
pub(crate) const SAMPLING_FREQUENCY: u32 = 0xB5;

/// The largest `Tracks` element read. A real one is a few kilobytes.
pub(crate) const MAX_TRACKS_BYTES: u64 = 32 * 1024 * 1024;

/// A Matroska codec ID, and the name the tools use for the codec (ffprobe's, which the app shows).
const CODEC_NAMES: &[(&str, &str)] = &[
    ("V_MPEG4/ISO/AVC", "h264"),
    ("V_MPEGH/ISO/HEVC", "hevc"),
    ("V_MPEG4/ISO/ASP", "mpeg4"),
    ("V_MPEG4/ISO/SP", "mpeg4"),
    ("V_MPEG4/ISO/AP", "mpeg4"),
    ("V_MPEG1", "mpeg1video"),
    ("V_MPEG2", "mpeg2video"),
    ("V_VP8", "vp8"),
    ("V_VP9", "vp9"),
    ("V_AV1", "av1"),
    ("V_MS/VFW/FOURCC", "vfw"),
    ("A_AAC", "aac"),
    ("A_AC3", "ac3"),
    ("A_EAC3", "eac3"),
    ("A_DTS", "dts"),
    ("A_TRUEHD", "truehd"),
    ("A_MLP", "mlp"),
    ("A_FLAC", "flac"),
    ("A_OPUS", "opus"),
    ("A_VORBIS", "vorbis"),
    ("A_MPEG/L3", "mp3"),
    ("A_MPEG/L2", "mp2"),
    ("A_PCM/INT/LIT", "pcm_s16le"),
    ("S_TEXT/UTF8", "subrip"),
    ("S_TEXT/ASCII", "subrip"),
    ("S_TEXT/ASS", "ass"),
    ("S_TEXT/SSA", "ssa"),
    ("S_VOBSUB", "dvd_subtitle"),
    ("S_HDMV/PGS", "hdmv_pgs_subtitle"),
    ("S_HDMV/TEXTST", "hdmv_text_subtitle"),
];

/// The codec name for a Matroska codec ID: the tool's name when it is known, else the ID itself.
pub fn codec_name(codec_id: &str) -> String {
    CODEC_NAMES
        .iter()
        .find(|(id, _)| *id == codec_id)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| codec_id.to_string())
}

/// One element of an EBML tree: its ID, and where its payload is.
pub(crate) struct Element<'a> {
    pub(crate) id: u32,
    /// `None` for an unknown size (allowed for a `Segment` and a `Cluster` being written).
    pub(crate) payload: Option<&'a [u8]>,
    /// The bytes after this element's header, when its size is unknown.
    pub(crate) rest: &'a [u8],
}

/// Read an element ID (its length marker kept, as the spec defines IDs) and return it with its length.
pub(crate) fn read_id(data: &[u8]) -> Option<(u32, usize)> {
    let first = *data.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 || data.len() < len {
        return None;
    }
    let id = data[..len].iter().fold(0u32, |acc, &b| (acc << 8) | b as u32);
    Some((id, len))
}

/// Read an element size (the length marker removed). `None` in the value means unknown.
pub(crate) fn read_size(data: &[u8]) -> Option<(Option<u64>, usize)> {
    let first = *data.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || data.len() < len {
        return None;
    }
    let mut value = (first as u64) & ((1u64 << (8 - len)) - 1);
    for &b in &data[1..len] {
        value = (value << 8) | b as u64;
    }
    let unknown = value == (1u64 << (7 * len)) - 1;
    Some((if unknown { None } else { Some(value) }, len))
}

/// The next element in `data`, and the bytes after it (after its whole payload, when the size is known).
pub(crate) fn next_element(data: &[u8]) -> Option<(Element<'_>, &[u8])> {
    let (id, id_len) = read_id(data)?;
    let (size, size_len) = read_size(&data[id_len..])?;
    let body = &data[id_len + size_len..];
    match size {
        Some(size) => {
            let size = usize::try_from(size).ok()?;
            if body.len() < size {
                return None;
            }
            Some((Element { id, payload: Some(&body[..size]), rest: body }, &body[size..]))
        }
        None => Some((Element { id, payload: None, rest: body }, &[])),
    }
}

/// Read one unsigned integer payload (1 to 8 bytes, big-endian).
pub(crate) fn uint(payload: &[u8]) -> u64 {
    payload.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64)
}

/// Read a text payload: the bytes up to the first NUL.
fn text(payload: &[u8]) -> String {
    let end = payload.iter().position(|&b| b == 0).unwrap_or(payload.len());
    String::from_utf8_lossy(&payload[..end]).trim().to_string()
}

/// The tracks of one `TrackEntry`, from its child elements. The IDs are the track numbers
/// (`mkvmerge`'s ID is the track number minus one, but `Track::id` is set by the caller).
struct Entry {
    number: u64,
    kind: Option<Kind>,
    codec_id: String,
    language: Option<String>,
    language_ietf: Option<String>,
    name: String,
    default: bool,
    forced: bool,
    channels: usize,
}

fn parse_entry(payload: &[u8]) -> Entry {
    let mut entry = Entry {
        number: 0,
        kind: None,
        codec_id: String::new(),
        language: None,
        language_ietf: None,
        name: String::new(),
        default: true, // FlagDefault is 1 when it is not written
        forced: false,
        channels: 0,
    };
    let mut data = payload;
    while let Some((element, rest)) = next_element(data) {
        data = rest;
        let body = element.payload.unwrap_or(element.rest);
        match element.id {
            TRACK_NUMBER => entry.number = uint(body),
            TRACK_TYPE => {
                entry.kind = match uint(body) {
                    1 => Some(Kind::Video),
                    2 => Some(Kind::Audio),
                    0x11 => Some(Kind::Subtitles),
                    _ => None,
                }
            }
            CODEC_ID => entry.codec_id = text(body),
            LANGUAGE => entry.language = Some(text(body)),
            LANGUAGE_IETF => entry.language_ietf = Some(text(body)),
            NAME => entry.name = text(body),
            FLAG_DEFAULT => entry.default = uint(body) == 1,
            FLAG_FORCED => entry.forced = uint(body) == 1,
            AUDIO => {
                let mut inner = body;
                while let Some((child, after)) = next_element(inner) {
                    inner = after;
                    if child.id == CHANNELS {
                        entry.channels = uint(child.payload.unwrap_or(&[])) as usize;
                    }
                }
            }
            _ => {}
        }
    }
    entry
}

/// The tracks listed in the `Tracks` element's payload, in the order the file lists them.
/// Tracks of other kinds (buttons and the like) are left out, as the other readers do.
pub fn parse_tracks(payload: &[u8]) -> Vec<Track> {
    let mut tracks = Vec::new();
    let mut counts = [0usize; 3];
    let mut data = payload;
    while let Some((element, rest)) = next_element(data) {
        data = rest;
        if element.id != TRACK_ENTRY {
            continue;
        }
        let entry = parse_entry(element.payload.unwrap_or(&[]));
        let Some(kind) = entry.kind else { continue };
        let slot = match kind {
            Kind::Video => 0,
            Kind::Audio => 1,
            Kind::Subtitles => 2,
        };
        counts[slot] += 1;
        // the language is the old three letter one, unless only the IETF tag is written
        let language = entry.language.or(entry.language_ietf.map(|tag| tag.split('-').next().unwrap_or("").to_string()));
        tracks.push(Track {
            id: entry.number as i64 - 1,
            kind,
            number: counts[slot],
            codec: codec_name(&entry.codec_id),
            language: normalise_language(Some(language.as_deref().unwrap_or("und"))),
            default: entry.default,
            forced: entry.forced,
            name: entry.name,
            channels: entry.channels,
        });
    }
    tracks
}

/// The tracks of a Matroska file. The `Tracks` element is normally near the start, but a tool may
/// move it (`mkvpropedit` writes it after the Cues), so every element of the `Segment` is looked at,
/// skipping the media (the clusters) by their sizes. Only the track list is read.
pub fn read_file_tracks(path: &Path) -> io::Result<Vec<Track>> {
    let mut file = File::open(path)?;
    let (body, segment_end) = segment_range(&mut file)?;
    Ok(tracks_payload(&mut file, body, segment_end)?.map(|payload| parse_tracks(&payload)).unwrap_or_default())
}

/// Where the children of the `Segment` are, in an open file: the position after the Segment's header, and
/// the position where it ends (the end of the file for a Segment of unknown size).
pub(crate) fn segment_range(file: &mut File) -> io::Result<(u64, u64)> {
    let file_len = file.metadata()?.len();
    // the EBML header: its ID and size, then the Segment's ID and size after it
    let mut start = [0u8; 12];
    file.read_exact(&mut start).map_err(|_| invalid())?;
    let (id, id_len) = read_id(&start).ok_or_else(invalid)?;
    let (size, size_len) = read_size(&start[id_len..]).ok_or_else(invalid)?;
    if id != EBML_HEADER {
        return Err(invalid());
    }
    let header_len = (id_len + size_len) as u64 + size.ok_or_else(invalid)?;
    file.seek(SeekFrom::Start(header_len))?;
    let mut header = [0u8; 16];
    let read = file.read(&mut header)?;
    let (segment_id, segment_id_len) = read_id(&header[..read]).ok_or_else(invalid)?;
    let (segment_size, segment_size_len) = read_size(&header[segment_id_len..read]).ok_or_else(invalid)?;
    if segment_id != SEGMENT {
        return Err(invalid());
    }
    let body = header_len + (segment_id_len + segment_size_len) as u64;
    let segment_end = segment_size.map(|s| body + s).unwrap_or(file_len).min(file_len);
    Ok((body, segment_end))
}

/// The payload of the `Tracks` element among the children of the segment (`body` to `segment_end`), or
/// `None` when there is none before an element of unknown size.
pub(crate) fn tracks_payload(file: &mut File, body: u64, segment_end: u64) -> io::Result<Option<Vec<u8>>> {
    let length = file.metadata()?.len();
    let mut position = body;
    while position < segment_end {
        file.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 16];
        let read = file.read(&mut header)?;
        if read == 0 {
            break;
        }
        let (child_id, child_id_len) = read_id(&header[..read]).ok_or_else(invalid)?;
        let (child_size, child_size_len) = read_size(&header[child_id_len..read]).ok_or_else(invalid)?;
        let body_start = position + (child_id_len + child_size_len) as u64;
        // an element that says it is longer than the file is cut short: the list cannot be trusted
        if let Some(len) = child_size {
            if body_start.checked_add(len).map_or(true, |end| end > length) {
                return Err(invalid_range());
            }
        }
        match (child_id, child_size) {
            (TRACKS, Some(len)) => {
                if len > MAX_TRACKS_BYTES {
                    return Err(invalid());
                }
                let mut payload = vec![0u8; len as usize];
                file.seek(SeekFrom::Start(body_start))?;
                file.read_exact(&mut payload)?;
                return Ok(Some(payload));
            }
            // a cluster of unknown size cannot be skipped, so the rest of the file cannot be reached
            (_, None) => break,
            (_, Some(len)) => position = body_start + len,
        }
    }
    Ok(None)
}

/// What a track entry says about the track, for the readers that need it: its codec ID, its CodecPrivate
/// (a codec's setup data, such as an AAC AudioSpecificConfig), and for audio its sample rate and channels.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrackEntry {
    pub(crate) codec_id: String,
    pub(crate) codec_private: Option<Vec<u8>>,
    pub(crate) sample_rate: f64,
    pub(crate) channels: u64,
    /// The track's content compression (its first `ContentEncoding`), if the blocks are compressed.
    pub(crate) compression: Option<Compression>,
}

/// How the blocks of a track are compressed (Matroska's `ContentCompression`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Compression {
    /// zlib (algorithm 0): each block is a zlib stream.
    Zlib,
    /// Header stripping (algorithm 3): each block lacks the bytes in `settings`, which go back in front of it.
    HeaderStripping(Vec<u8>),
}

/// The compression of the first content encoding in a `ContentEncodings` payload, if it is one this program reads
/// (zlib, or header stripping); other compressions are an error, since the blocks could not be read.
fn content_compression(encodings: &[u8]) -> io::Result<Option<Compression>> {
    const CONTENT_ENCODING: u32 = 0x6240;
    const CONTENT_COMPRESSION: u32 = 0x5034;
    const CONTENT_COMP_ALGO: u32 = 0x4254;
    const CONTENT_COMP_SETTINGS: u32 = 0x4255;
    let Some((encoding, _)) = next_element(encodings) else { return Ok(None) };
    if encoding.id != CONTENT_ENCODING {
        return Ok(None);
    }
    let mut algorithm = 0u64;
    let mut settings = Vec::new();
    let mut compressed = false;
    let mut rest = encoding.payload.ok_or_else(invalid)?;
    while !rest.is_empty() {
        let (child, after) = next_element(rest).ok_or_else(invalid)?;
        if child.id == CONTENT_COMPRESSION {
            compressed = true;
            let mut fields = child.payload.ok_or_else(invalid)?;
            while !fields.is_empty() {
                let (field, next) = next_element(fields).ok_or_else(invalid)?;
                let value = field.payload.ok_or_else(invalid)?;
                match field.id {
                    CONTENT_COMP_ALGO => algorithm = uint(value),
                    CONTENT_COMP_SETTINGS => settings = value.to_vec(),
                    _ => {}
                }
                fields = next;
            }
        }
        rest = after;
    }
    if !compressed {
        return Ok(None);
    }
    match algorithm {
        0 => Ok(Some(Compression::Zlib)),
        3 => Ok(Some(Compression::HeaderStripping(settings))),
        _ => Err(io::Error::new(io::ErrorKind::Unsupported, "the blocks are compressed in a way this program does not read")),
    }
}

/// The entry of the track with TrackNumber `track_number` in a `Tracks` payload.
pub(crate) fn track_entry(tracks: &[u8], track_number: u64) -> io::Result<Option<TrackEntry>> {
    let mut rest = tracks;
    while !rest.is_empty() {
        let (element, after) = next_element(rest).ok_or_else(invalid)?;
        let body = element.payload.ok_or_else(invalid)?;
        if element.id == TRACK_ENTRY {
            let mut number = None;
            let mut entry = TrackEntry { codec_id: String::new(), codec_private: None, sample_rate: 0.0, channels: 0, compression: None };
            let mut children = body;
            while !children.is_empty() {
                let (child, next) = next_element(children).ok_or_else(invalid)?;
                let value = child.payload.ok_or_else(invalid)?;
                match child.id {
                    TRACK_NUMBER => number = Some(uint(value)),
                    CODEC_ID => entry.codec_id = text(value),
                    CODEC_PRIVATE => entry.codec_private = Some(value.to_vec()),
                    CONTENT_ENCODINGS => entry.compression = content_compression(value)?,
                    AUDIO => {
                        let mut fields = value;
                        while !fields.is_empty() {
                            let (field, after_field) = next_element(fields).ok_or_else(invalid)?;
                            let payload = field.payload.ok_or_else(invalid)?;
                            match field.id {
                                SAMPLING_FREQUENCY => entry.sample_rate = float(payload),
                                CHANNELS => entry.channels = uint(payload),
                                _ => {}
                            }
                            fields = after_field;
                        }
                    }
                    _ => {}
                }
                children = next;
            }
            if number == Some(track_number) {
                return Ok(Some(entry));
            }
        }
        rest = after;
    }
    Ok(None)
}

/// The entry of track `track_number` in an MKV file (its `Tracks` element is read; nothing else).
pub(crate) fn read_track_entry(path: &Path, track_number: u64) -> io::Result<Option<TrackEntry>> {
    let mut file = File::open(path)?;
    let (body, segment_end) = segment_range(&mut file)?;
    match tracks_payload(&mut file, body, segment_end)? {
        Some(payload) => track_entry(&payload, track_number),
        None => Ok(None),
    }
}

/// A float element: four or eight bytes, big endian.
fn float(payload: &[u8]) -> f64 {
    match payload.len() {
        4 => f32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as f64,
        8 => f64::from_be_bytes(payload.try_into().unwrap_or([0; 8])),
        _ => 0.0,
    }
}

/// A size in the file that runs past the end of the file (a corrupt or cut-short file).
pub(crate) fn invalid_range() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "a size in the file runs past its end")
}

pub(crate) fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "not a Matroska file, or its track list cannot be read")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An element with a one byte size, for building test files.
    fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x80 | payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    fn track_entry(number: u8, kind: u8, codec: &str, language: &str, default: u8, name: &str) -> Vec<u8> {
        let mut body = element(&[0xD7], &[number]);
        body.extend(element(&[0x83], &[kind]));
        body.extend(element(&[0x86], codec.as_bytes()));
        body.extend(element(&[0x22, 0xB5, 0x9C], language.as_bytes()));
        body.extend(element(&[0x88], &[default]));
        body.extend(element(&[0x53, 0x6E], name.as_bytes()));
        element(&[0xAE], &body)
    }

    #[test]
    fn ids_and_sizes_are_read_with_their_length_markers() {
        assert_eq!(read_id(&[0x1A, 0x45, 0xDF, 0xA3]), Some((EBML_HEADER, 4)));
        assert_eq!(read_id(&[0xAE]), Some((0xAE, 1)));
        assert_eq!(read_size(&[0x85]), Some((Some(5), 1)));
        assert_eq!(read_size(&[0x40, 0x05]), Some((Some(5), 2)));
        assert_eq!(read_size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), Some((None, 8)));
        assert_eq!(read_size(&[0x7F, 0xFF]), Some((None, 2)), "all ones in two bytes is unknown");
    }

    #[test]
    fn tracks_are_read_in_order_with_their_flags_and_names() {
        let mut payload = track_entry(1, 1, "V_MPEG4/ISO/AVC", "und", 1, "Picture");
        payload.extend(track_entry(2, 2, "A_AAC", "ita", 1, "Italiano"));
        payload.extend(track_entry(3, 2, "A_AC3", "eng", 0, "English"));
        payload.extend(track_entry(4, 0x11, "S_TEXT/UTF8", "eng", 1, "Subs"));
        let tracks = parse_tracks(&payload);
        let summary: Vec<(Kind, usize, &str, &str, bool, &str)> = tracks
            .iter()
            .map(|t| (t.kind, t.number, t.codec.as_str(), t.language.as_str(), t.default, t.name.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (Kind::Video, 1, "h264", "und", true, "Picture"),
                (Kind::Audio, 1, "aac", "ita", true, "Italiano"),
                (Kind::Audio, 2, "ac3", "eng", false, "English"),
                (Kind::Subtitles, 1, "subrip", "eng", true, "Subs"),
            ]
        );
        assert_eq!(tracks[1].id, 1, "the ID is the track number minus one, as mkvmerge numbers them");
    }

    #[test]
    fn a_track_without_a_default_flag_is_default() {
        let mut body = element(&[0xD7], &[1]);
        body.extend(element(&[0x83], &[2]));
        body.extend(element(&[0x86], b"A_AAC"));
        let tracks = parse_tracks(&element(&[0xAE], &body));
        assert!(tracks[0].default, "FlagDefault is 1 when it is not written");
        assert_eq!(tracks[0].language, "und", "no language written is undetermined");
    }

    #[test]
    fn tracks_after_a_cluster_are_still_found() {
        // a file where the track list was moved behind the media, as mkvpropedit leaves it
        let mut track_list = Vec::new();
        track_list.extend(track_entry(1, 1, "V_MPEG4/ISO/AVC", "und", 1, "Picture"));
        track_list.extend(track_entry(2, 2, "A_AAC", "eng", 1, "English"));
        let tracks_element = element(&[0x16, 0x54, 0xAE, 0x6B], &track_list);
        let cluster = element(&[0x1F, 0x43, 0xB6, 0x75], &[0xE7, 0x81, 0x00]);
        let mut segment_body = element(&[0xEC], &[0; 4]);
        segment_body.extend(cluster);
        segment_body.extend(tracks_element);
        let mut file = element(&[0x1A, 0x45, 0xDF, 0xA3], &[0x42, 0x82, 0x81, 0x6D]);
        file.extend(element(&[0x18, 0x53, 0x80, 0x67], &segment_body));
        let path = std::env::temp_dir().join(format!("mau-ebml-after-cluster-{}.mkv", std::process::id()));
        std::fs::write(&path, &file).unwrap();
        let tracks = read_file_tracks(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let names: Vec<(&str, &str)> = tracks.iter().map(|t| (t.codec.as_str(), t.name.as_str())).collect();
        assert_eq!(names, [("h264", "Picture"), ("aac", "English")]);
    }

    #[test]
    fn an_unknown_codec_keeps_its_id() {
        assert_eq!(codec_name("V_UNKNOWN/THING"), "V_UNKNOWN/THING");
    }

    /// The tracks `mkvmerge -J` reports for a file, as the same summary this reader gives.
    fn mkvmerge_summary(path: &Path) -> Option<Vec<(i64, String, String, String, bool, bool, String)>> {
        let out = std::process::Command::new("mkvmerge").arg("-J").arg(path).output().ok()?;
        let data: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let mut rows = Vec::new();
        for raw in data.get("tracks")?.as_array()? {
            let kind = raw.get("type")?.as_str()?;
            if !matches!(kind, "video" | "audio" | "subtitles") {
                continue;
            }
            let props = raw.get("properties")?;
            let codec = codec_name(props.get("codec_id")?.as_str()?);
            let language = props.get("language").and_then(|v| v.as_str()).unwrap_or("und").to_string();
            rows.push((
                raw.get("id")?.as_i64()?,
                kind.to_string(),
                codec,
                language,
                props.get("default_track").and_then(|v| v.as_bool()).unwrap_or(false),
                props.get("forced_track").and_then(|v| v.as_bool()).unwrap_or(false),
                props.get("track_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            ));
        }
        Some(rows)
    }

    /// Compare with mkvmerge on the files named in MAU_EBML_CHECK (separated by `:`). Skipped when
    /// the variable is not set or mkvmerge is missing, so it only runs when someone asks for it.
    #[test]
    fn the_tracks_match_mkvmerge_on_the_files_named_in_mau_ebml_check() {
        let Ok(list) = std::env::var("MAU_EBML_CHECK") else { return };
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let path = Path::new(file);
            let Some(expected) = mkvmerge_summary(path) else { return };
            assert!(!expected.is_empty(), "{file}: mkvmerge reported no tracks");
            let ours: Vec<(i64, String, String, String, bool, bool, String)> = read_file_tracks(path)
                .unwrap_or_else(|e| panic!("{file}: {e}"))
                .iter()
                .map(|t| (t.id, t.kind.name().to_string(), t.codec.clone(), t.language.clone(), t.default, t.forced, t.name.clone()))
                .collect();
            assert_eq!(ours, expected, "{file}");
        }
    }
}
