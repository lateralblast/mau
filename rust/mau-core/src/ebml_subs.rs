//! Extracting a text subtitle track (SubRip or SubStationAlpha) from a Matroska file, in this program
//! and not with `mkvextract` or ffmpeg. The output is what `mkvextract` writes: the tests compare it
//! byte for byte with `mkvextract`'s output on real files (see `APPSTORE.md`, option 3).
//!
//! A subtitle is one block (a `SimpleBlock`, or a `Block` in a `BlockGroup`) of its track. Its time is the
//! block's time plus its cluster's `Timecode`, in ticks of the segment's `TimestampScale` (nanoseconds, by
//! default). Its end is the block's `BlockDuration`, when it has one. Laced blocks are refused: text
//! subtitles are not laced in files made by mkvmerge.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::ebml::{invalid, invalid_range, read_id, read_size, segment_range, track_entry, tracks_payload, uint};

const INFO: u32 = 0x1549_A966;
const DURATION: u32 = 0x4489;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const CLUSTER: u32 = 0x1F43_B675;
const TIMECODE: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const BLOCK_DURATION: u32 = 0x9B;
const BLOCK_ADDITIONS: u32 = 0x75A1;
const BLOCK_MORE: u32 = 0xA6;
const BLOCK_ADD_ID: u32 = 0xEE;
const BLOCK_ADDITIONAL: u32 = 0xA5;

/// The byte order mark `mkvextract` writes at the start of an SRT file.
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// One subtitle: its start and end in milliseconds, and its payload. For SubRip the payload is the text; for
/// SubStationAlpha it is the event as the block stores it (`ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub payload: Vec<u8>,
}

/// A subtitle track: the header the codec stores (SubStationAlpha's `[Script Info]` and styles), and its cues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtitles {
    pub codec_private: Option<Vec<u8>>,
    pub cues: Vec<Cue>,
}

/// Read the cues of the track with TrackNumber `track_number` (the mkvmerge ID plus one), and its header.
pub fn read_subtitles(path: &Path, track_number: u64) -> io::Result<Subtitles> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let (body, segment_end) = segment_range(&mut file)?;
    let codec_private = match tracks_payload(&mut file, body, segment_end)? {
        Some(payload) => codec_private_of(&payload, track_number)?,
        None => None,
    };
    let mut cues: Vec<(i64, Option<i64>, Vec<u8>)> = Vec::new();
    let mut scale = 1_000_000u64;
    let mut at = body;
    while at < segment_end.min(file_len) {
        let (id, header_len, size) = element_at(&mut file, at)?;
        let size = size.ok_or_else(invalid)?;
        let payload = at + header_len;
        match id {
            INFO => scale = timestamp_scale(&mut file, payload, size)?.unwrap_or(scale),
            CLUSTER => read_cluster(&mut file, payload, size, track_number, scale, false, &mut cues)?,
            _ => {}
        }
        at = payload + size;
    }
    Ok(Subtitles { codec_private, cues: resolve_ends(cues) })
}

/// Give each cue its end. A cue with no duration lasts until the next cue starts, as `mkvextract` makes it,
/// and the last one, with no next cue, is left out (`mkvextract` leaves it out too).
fn resolve_ends(cues: Vec<(i64, Option<i64>, Vec<u8>)>) -> Vec<Cue> {
    let starts: Vec<i64> = cues.iter().map(|(start, _, _)| *start).collect();
    cues.into_iter()
        .enumerate()
        .filter_map(|(index, (start_ms, end_ms, payload))| {
            let end_ms = match end_ms {
                Some(end) => end,
                None => *starts.get(index + 1)?,
            };
            Some(Cue { start_ms, end_ms, payload })
        })
        .collect()
}

/// A block of a track: its start and duration in milliseconds (the duration is `None` when the block has
/// none), and its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) start_ms: i64,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) payload: Vec<u8>,
}

/// The blocks of track `track_number` that start at or after `from_ms` and before `to_ms`, in file order.
pub(crate) fn read_blocks(path: &Path, track_number: u64, from_ms: i64, to_ms: i64) -> io::Result<Vec<Block>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let (body, segment_end) = segment_range(&mut file)?;
    let mut found: Vec<(i64, Option<i64>, Vec<u8>)> = Vec::new();
    let mut scale = 1_000_000u64;
    let mut at = body;
    while at < segment_end.min(file_len) {
        let (id, header_len, size) = element_at(&mut file, at)?;
        let size = size.ok_or_else(invalid)?;
        let payload = at + header_len;
        match id {
            INFO => scale = timestamp_scale(&mut file, payload, size)?.unwrap_or(scale),
            CLUSTER => read_cluster(&mut file, payload, size, track_number, scale, true, &mut found)?,
            _ => {}
        }
        at = payload + size;
    }
    Ok(found
        .into_iter()
        .filter(|(start, _, _)| *start >= from_ms && *start < to_ms)
        .map(|(start_ms, end_ms, payload)| Block { start_ms, duration_ms: end_ms.map(|end| end - start_ms), payload })
        .collect())
}

/// A block of a track with the additions it carries (`BlockAdditions`): its time, its duration, its payload, and each
/// addition as (BlockAddID, data). Only a block with one frame is read (a subtitle is never laced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddBlock {
    pub(crate) start_ms: i64,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) payload: Vec<u8>,
    pub(crate) additions: Vec<(u64, Vec<u8>)>,
}

/// The blocks of track `track_number` with their additions, in file order.
pub(crate) fn read_add_blocks(path: &Path, track_number: u64) -> io::Result<Vec<AddBlock>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let (body, segment_end) = segment_range(&mut file)?;
    let mut out = Vec::new();
    let mut scale = 1_000_000u64;
    let mut at = body;
    while at < segment_end.min(file_len) {
        let (id, header_len, size) = element_at(&mut file, at)?;
        let size = size.ok_or_else(invalid)?;
        let payload = at + header_len;
        match id {
            INFO => scale = timestamp_scale(&mut file, payload, size)?.unwrap_or(scale),
            CLUSTER => read_cluster_additions(&mut file, payload, size, track_number, scale, &mut out)?,
            _ => {}
        }
        at = payload + size;
    }
    Ok(out)
}

fn read_cluster_additions(file: &mut File, start: u64, len: u64, track_number: u64, scale: u64, out: &mut Vec<AddBlock>) -> io::Result<()> {
    let end = start + len;
    let mut at = start;
    let mut cluster_time = 0i64;
    while at < end {
        let (id, header_len, size) = element_at(file, at)?;
        let size = size.ok_or_else(invalid)?;
        let payload = at + header_len;
        match id {
            TIMECODE => cluster_time = uint(&read_bytes(file, payload, size)?) as i64,
            BLOCK_GROUP => {
                let mut block = None;
                let mut duration = None;
                let mut additions = Vec::new();
                let mut inner = payload;
                while inner < payload + size {
                    let (child, child_header, child_size) = element_at(file, inner)?;
                    let child_size = child_size.ok_or_else(invalid)?;
                    let child_at = inner + child_header;
                    match child {
                        BLOCK => block = Some(read_bytes(file, child_at, child_size)?),
                        BLOCK_DURATION => duration = Some(uint(&read_bytes(file, child_at, child_size)?)),
                        BLOCK_ADDITIONS => additions = read_more(file, child_at, child_size)?,
                        _ => {}
                    }
                    inner = child_at + child_size;
                }
                if let Some(block) = block {
                    if block_track(&block)? == track_number {
                        let (time, frames) = parse_block(&block)?;
                        if frames.len() != 1 {
                            return Err(io::Error::new(io::ErrorKind::Unsupported, "a laced subtitle block"));
                        }
                        let start_ms = ticks_to_ms(cluster_time + time, scale);
                        let end_ms = duration.map(|d| ticks_to_ms(cluster_time + time + d as i64, scale));
                        out.push(AddBlock {
                            start_ms,
                            duration_ms: end_ms.map(|end| end - start_ms),
                            payload: frames[0].to_vec(),
                            additions,
                        });
                    }
                }
            }
            _ => {}
        }
        at = payload + size;
    }
    Ok(())
}

/// The (BlockAddID, data) pairs of a `BlockAdditions` payload.
fn read_more(file: &mut File, start: u64, len: u64) -> io::Result<Vec<(u64, Vec<u8>)>> {
    let mut more_list = Vec::new();
    let mut at = start;
    while at < start + len {
        let (id, header_len, size) = element_at(file, at)?;
        let size = size.ok_or_else(invalid)?;
        if id == BLOCK_MORE {
            let mut add_id = 1u64;
            let mut data = Vec::new();
            let mut inner = at + header_len;
            while inner < at + header_len + size {
                let (child, child_header, child_size) = element_at(file, inner)?;
                let child_size = child_size.ok_or_else(invalid)?;
                let child_at = inner + child_header;
                match child {
                    BLOCK_ADD_ID => add_id = uint(&read_bytes(file, child_at, child_size)?),
                    BLOCK_ADDITIONAL => data = read_bytes(file, child_at, child_size)?,
                    _ => {}
                }
                inner = child_at + child_size;
            }
            more_list.push((add_id, data));
        }
        at += header_len + size;
    }
    Ok(more_list)
}

/// The length of an MKV file in seconds, from the `Duration` of its `Info` element (`None` when it has none).
pub(crate) fn read_duration(path: &Path) -> io::Result<Option<f64>> {
    let mut file = File::open(path)?;
    let (body, segment_end) = segment_range(&mut file)?;
    let file_len = file.metadata()?.len();
    let mut at = body;
    while at < segment_end.min(file_len) {
        let (id, header_len, size) = element_at(&mut file, at)?;
        let size = size.ok_or_else(invalid)?;
        let payload = at + header_len;
        if id == INFO {
            let scale = timestamp_scale(&mut file, payload, size)?.unwrap_or(1_000_000);
            let mut inner = payload;
            while inner < payload + size {
                let (child, child_header, child_size) = element_at(&mut file, inner)?;
                let child_size = child_size.ok_or_else(invalid)?;
                if child == DURATION {
                    let value = read_bytes(&mut file, inner + child_header, child_size)?;
                    let ticks = match value.len() {
                        4 => f32::from_be_bytes([value[0], value[1], value[2], value[3]]) as f64,
                        8 => f64::from_be_bytes(value.as_slice().try_into().unwrap_or([0; 8])),
                        _ => return Ok(None),
                    };
                    return Ok(Some(ticks * scale as f64 / 1e9));
                }
                inner += child_header + child_size;
            }
            return Ok(None);
        }
        at = payload + size;
    }
    Ok(None)
}

/// The `TimestampScale` of the `Info` element, if it has one.
fn timestamp_scale(file: &mut File, start: u64, len: u64) -> io::Result<Option<u64>> {
    let mut at = start;
    while at < start + len {
        let (id, header_len, size) = element_at(file, at)?;
        let size = size.ok_or_else(invalid)?;
        if id == TIMESTAMP_SCALE {
            return Ok(Some(uint(&read_bytes(file, at + header_len, size)?)));
        }
        at += header_len + size;
    }
    Ok(None)
}

/// The cues of `track_number` in one cluster, in the order the blocks are in the file.
fn read_cluster(file: &mut File, start: u64, len: u64, track_number: u64, scale: u64, laced: bool, out: &mut Vec<(i64, Option<i64>, Vec<u8>)>) -> io::Result<()> {
    let end = start + len;
    let mut at = start;
    let mut cluster_time = 0i64;
    // (the block's time relative to the cluster, its duration, its payload)
    let mut blocks: Vec<(i64, Option<u64>, Vec<u8>)> = Vec::new();
    while at < end {
        let (id, header_len, size) = element_at(file, at)?;
        let size = size.ok_or_else(invalid)?;
        let payload = at + header_len;
        match id {
            TIMECODE => cluster_time = uint(&read_bytes(file, payload, size)?) as i64,
            SIMPLE_BLOCK => {
                // only the header is read first, so the blocks of other tracks are skipped without reading them
                let head = read_bytes(file, payload, size.min(10))?;
                if block_track(&head)? == track_number {
                    let block = read_bytes(file, payload, size)?;
                    let (time, frames) = parse_block(&block)?;
                    push_frames(&mut blocks, time, None, frames, laced)?;
                }
            }
            BLOCK_GROUP => {
                let mut block = None;
                let mut duration = None;
                let mut inner = payload;
                while inner < payload + size {
                    let (child, child_header, child_size) = element_at(file, inner)?;
                    let child_size = child_size.ok_or_else(invalid)?;
                    let child_at = inner + child_header;
                    match child {
                        BLOCK => block = Some(read_bytes(file, child_at, child_size)?),
                        BLOCK_DURATION => duration = Some(uint(&read_bytes(file, child_at, child_size)?)),
                        _ => {}
                    }
                    inner = child_at + child_size;
                }
                if let Some(block) = block {
                    if block_track(&block)? == track_number {
                        let (time, frames) = parse_block(&block)?;
                        push_frames(&mut blocks, time, duration, frames, laced)?;
                    }
                }
            }
            _ => {}
        }
        at = payload + size;
    }
    for (time, duration, payload) in blocks {
        let start_ms = ticks_to_ms(cluster_time + time, scale);
        let end_ms = duration.map(|d| ticks_to_ms(cluster_time + time + d as i64, scale));
        out.push((start_ms, end_ms, payload));
    }
    Ok(())
}

/// A block's track number, from the start of its payload.
fn block_track(data: &[u8]) -> io::Result<u64> {
    let (track, _) = read_size(data).ok_or_else(invalid)?;
    track.ok_or_else(invalid)
}

/// Add the frames of a block, all at the block's time. A laced block is refused unless `laced` (the audio
/// reader takes them; a subtitle is one frame).
fn push_frames(blocks: &mut Vec<(i64, Option<u64>, Vec<u8>)>, time: i64, duration: Option<u64>, frames: Vec<&[u8]>, laced: bool) -> io::Result<()> {
    if frames.len() > 1 && !laced {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "laced subtitle blocks are not supported"));
    }
    for frame in frames {
        blocks.push((time, duration, frame.to_vec()));
    }
    Ok(())
}

/// A block's time (relative to its cluster, in ticks) and its frames, once the track number is known to be
/// right. A block with one frame has that frame; a laced block has each of its frames (see `lace_frames`).
fn parse_block(block: &[u8]) -> io::Result<(i64, Vec<&[u8]>)> {
    let (_, track_len) = read_size(block).ok_or_else(invalid)?;
    if block.len() < track_len + 3 {
        return Err(invalid());
    }
    let time = i16::from_be_bytes([block[track_len], block[track_len + 1]]) as i64;
    let flags = block[track_len + 2];
    let frames = lace_frames(flags, &block[track_len + 3..])?;
    Ok((time, frames))
}

/// The frames in a block's data, from its lacing (bits 1 and 2 of the flags): none (one frame), Xiph (255 runs),
/// fixed size, or EBML (the first size, then signed differences). The sizes are checked against the data.
pub(crate) fn lace_frames(flags: u8, data: &[u8]) -> io::Result<Vec<&[u8]>> {
    let lacing = (flags >> 1) & 0x03;
    if lacing == 0 {
        return Ok(vec![data]);
    }
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "a laced block's sizes do not fit its data");
    let count = *data.first().ok_or_else(bad)? as usize + 1;
    let mut at = 1usize;
    let mut sizes: Vec<usize> = Vec::with_capacity(count);
    match lacing {
        // Xiph: each size is a run of 255s and a last byte
        1 => {
            for _ in 0..count - 1 {
                let mut size = 0usize;
                loop {
                    let byte = *data.get(at).ok_or_else(bad)?;
                    at += 1;
                    size += byte as usize;
                    if byte != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        // fixed size: the data divides into equal frames
        2 => {
            let rest = data.len() - at;
            if rest % count != 0 {
                return Err(bad());
            }
            sizes = vec![rest / count; count - 1];
        }
        // EBML: the first size is an unsigned vint, the others are signed differences from the one before
        _ => {
            let (first, used) = unsigned_vint(&data[at..]).ok_or_else(bad)?;
            at += used;
            sizes.push(first as usize);
            let mut previous = first as i64;
            for _ in 0..count.saturating_sub(2) {
                let (raw, used) = unsigned_vint(&data[at..]).ok_or_else(bad)?;
                at += used;
                let bias = (1i64 << (7 * used as i64 - 1)) - 1;
                previous += raw as i64 - bias;
                if previous < 0 {
                    return Err(bad());
                }
                sizes.push(previous as usize);
            }
        }
    }
    let used: usize = sizes.iter().sum();
    if at + used > data.len() {
        return Err(bad());
    }
    let mut frames = Vec::with_capacity(count);
    let mut start = at;
    for size in sizes {
        frames.push(&data[start..start + size]);
        start += size;
    }
    frames.push(&data[start..]);
    Ok(frames)
}

/// An unsigned EBML variable size integer (its length marker removed), and how many bytes it took.
fn unsigned_vint(data: &[u8]) -> Option<(u64, usize)> {
    let (value, len) = read_size(data)?;
    Some((value?, len))
}

/// Ticks (of `scale` nanoseconds each) as whole milliseconds, rounded down.
fn ticks_to_ms(ticks: i64, scale: u64) -> i64 {
    ((ticks as i128 * scale as i128) / 1_000_000) as i64
}

/// The CodecPrivate of the track with TrackNumber `track_number`, if it has one.
fn codec_private_of(tracks: &[u8], track_number: u64) -> io::Result<Option<Vec<u8>>> {
    Ok(track_entry(tracks, track_number)?.and_then(|entry| entry.codec_private))
}

/// The ID, the header length and the size of the element at `at` in the file (`None` for an unknown size).
fn element_at(file: &mut File, at: u64) -> io::Result<(u32, u64, Option<u64>)> {
    let file_len = file.metadata()?.len();
    let head = read_bytes(file, at, 12.min(file_len.saturating_sub(at)))?;
    let (id, id_len) = read_id(&head).ok_or_else(invalid)?;
    let (size, size_len) = read_size(&head[id_len..]).ok_or_else(invalid)?;
    Ok((id, (id_len + size_len) as u64, size))
}

/// Read `len` bytes at `at`.
fn read_bytes(file: &mut File, at: u64, len: u64) -> io::Result<Vec<u8>> {
    // a size read from the file can be anything: nothing past the end of the file is read (or allocated)
    let length = file.metadata()?.len();
    if at.checked_add(len).map_or(true, |end| end > length) {
        return Err(invalid_range());
    }
    let mut buffer = vec![0u8; len as usize];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// `hh:mm:ss,mmm`, as SubRip writes a time.
fn srt_time(ms: i64) -> String {
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// `h:mm:ss.cc`, as SubStationAlpha writes a time (hundredths of a second, rounded down).
fn ass_time(ms: i64) -> String {
    format!("{}:{:02}:{:02}.{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000 / 10)
}

/// The SubRip file for the cues, as `mkvextract` writes it: a byte order mark, then each cue numbered, with
/// its times and text and a blank line. Empty when there are no cues.
pub fn srt_bytes(cues: &[Cue]) -> Vec<u8> {
    if cues.is_empty() {
        return Vec::new();
    }
    let mut out = BOM.to_vec();
    out.extend(srt_text(cues));
    out
}

/// The same SubRip text without the byte order mark, as ffmpeg writes it (the MP4 extraction matches ffmpeg's).
pub fn srt_text(cues: &[Cue]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, cue) in cues.iter().enumerate() {
        let end = cue.end_ms;
        out.extend(format!("{}\n{} --> {}\n", index + 1, srt_time(cue.start_ms), srt_time(end)).as_bytes());
        out.extend(lf(trim_nuls(&cue.payload)));
        out.extend_from_slice(b"\n\n");
    }
    out
}

/// The SubStationAlpha file, as `mkvextract` writes it: a byte order mark, the track's header with its line
/// endings made `\n` and its NUL padding removed (and an `[Events]` section if the header has none), then one
/// `Dialogue:` line per cue (its layer, its times, then the rest of the event). Empty when there are no cues.
pub fn ass_bytes(subtitles: &Subtitles) -> Vec<u8> {
    if subtitles.cues.is_empty() {
        return Vec::new();
    }
    let mut out = BOM.to_vec();
    // the header's last line break is written by the loop below, so it is taken off first
    let header = trim_nuls(subtitles.codec_private.as_deref().unwrap_or(&[]));
    let header = header.strip_suffix(b"\n").unwrap_or(header);
    let header = header.strip_suffix(b"\r").unwrap_or(header);
    if !header.is_empty() {
        for line in header.split(|b| *b == b'\n') {
            out.extend_from_slice(line.strip_suffix(b"\r").unwrap_or(line));
            out.push(b'\n');
        }
    }
    let has_events = header.split(|b| *b == b'\n').any(|line| line.strip_suffix(b"\r").unwrap_or(line) == b"[Events]");
    if !has_events {
        // the section starts with a blank line, which is not wanted at the very start of the file
        let events = if header.is_empty() { EVENTS_HEADER.strip_prefix("\n").unwrap_or(EVENTS_HEADER) } else { EVENTS_HEADER };
        out.extend_from_slice(events.as_bytes());
    }
    for cue in &subtitles.cues {
        // the payload is ReadOrder,Layer,...: the read order is dropped, and the layer comes first
        let event = cue.payload.iter().position(|b| *b == b',').map(|i| &cue.payload[i + 1..]).unwrap_or(&[]);
        let layer_end = event.iter().position(|b| *b == b',').unwrap_or(event.len());
        let end = cue.end_ms;
        out.extend(format!("Dialogue: {},{},{},", String::from_utf8_lossy(&event[..layer_end]), ass_time(cue.start_ms), ass_time(end)).as_bytes());
        out.extend(lf(trim_nuls(&event[(layer_end + 1).min(event.len())..])));
        out.push(b'\n');
    }
    out
}

/// The `[Events]` section `mkvextract` adds to a header that has none.
const EVENTS_HEADER: &str = "\n[Events]\nFormat: Layer, Start, End, Style, Actor, MarginL, MarginR, MarginV, Effect, Text\n";

/// The text with each CRLF made LF, as `mkvextract` writes a line break.
fn lf(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for (i, byte) in text.iter().enumerate() {
        if *byte == b'\r' && text.get(i + 1) == Some(&b'\n') {
            continue;
        }
        out.push(*byte);
    }
    out
}

/// The bytes without the NULs at their end (a block or a header can be padded with them).
fn trim_nuls(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    &data[..end]
}

/// Read a SubRip file: its cues, in the order they are in the file. A byte order mark, CRLF line ends and a
/// cue without its number are accepted. Anything else that is not SubRip is an error.
pub fn read_srt(path: &Path) -> io::Result<Vec<crate::ebml_edit::NewCue>> {
    parse_srt(&std::fs::read(path)?)
}

fn not_srt() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "not a SubRip (SRT) file")
}

fn parse_srt(data: &[u8]) -> io::Result<Vec<crate::ebml_edit::NewCue>> {
    let data = data.strip_prefix(BOM).unwrap_or(data);
    let text = String::from_utf8(data.to_vec()).map_err(|_| not_srt())?.replace("\r\n", "\n");
    let mut cues = Vec::new();
    // a blank line ends each cue (more than one blank line in a row is allowed)
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.trim_matches('\n').lines().collect();
        if lines.is_empty() {
            continue;
        }
        let lines = if lines[0].contains("-->") { &lines[..] } else { &lines[1..] };
        let (timing, text_lines) = lines.split_first().ok_or_else(not_srt)?;
        let (start_ms, end_ms) = srt_timing(timing)?;
        cues.push(crate::ebml_edit::NewCue { start_ms, end_ms, text: text_lines.join("\n").into_bytes(), additions: vec![] });
    }
    Ok(cues)
}

/// The start and end of a cue, from `hh:mm:ss,mmm --> hh:mm:ss,mmm` (anything after the end is ignored).
fn srt_timing(line: &str) -> io::Result<(i64, i64)> {
    let (start, end) = line.split_once("-->").ok_or_else(not_srt)?;
    let end = end.split_whitespace().next().ok_or_else(not_srt)?;
    Ok((srt_millis(start.trim())?, srt_millis(end)?))
}

/// `hh:mm:ss,mmm` (or with a full stop) as milliseconds.
fn srt_millis(time: &str) -> io::Result<i64> {
    let time = time.replace('.', ",");
    let (clock, millis) = time.split_once(',').ok_or_else(not_srt)?;
    let parts: Vec<i64> = clock.split(':').map(|p| p.parse::<i64>()).collect::<Result<_, _>>().map_err(|_| not_srt())?;
    if parts.len() != 3 || millis.len() != 3 {
        return Err(not_srt());
    }
    let millis: i64 = millis.parse().map_err(|_| not_srt())?;
    Ok(((parts[0] * 60 + parts[1]) * 60 + parts[2]) * 1000 + millis)
}

/// The blocks of a track with their content decoded: a zlib block is inflated, and a header-stripped block gets its
/// stripped bytes back in front of it (see `ebml::Compression`).
pub(crate) fn decoded_blocks(path: &Path, track_number: u64) -> io::Result<Vec<Block>> {
    use crate::ebml::Compression;
    let compression = crate::ebml::read_track_entry(path, track_number)?.and_then(|entry| entry.compression);
    let mut blocks = read_blocks(path, track_number, 0, i64::MAX)?;
    for block in &mut blocks {
        block.payload = match &compression {
            None => continue,
            Some(Compression::Zlib) => miniz_oxide::inflate::decompress_to_vec_zlib(&block.payload).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "a compressed block is not a zlib stream")
            })?,
            Some(Compression::HeaderStripping(stripped)) => [stripped.as_slice(), &block.payload].concat(),
        };
    }
    Ok(blocks)
}

/// The PGS (`.sup`) file for the blocks of a track, as `mkvextract` writes it: each block's segments, each with its
/// `PG` header (the presentation time is the block's time in 90 kHz ticks, and the decoding time is zero, as
/// Matroska has none). A block is a run of segments, each a type byte, a two byte size and its data.
pub(crate) fn pgs_bytes(blocks: &[Block]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for block in blocks {
        let pts = u32::try_from(block.start_ms * 90).map_err(|_| invalid())?;
        let mut at = 0usize;
        while at < block.payload.len() {
            let header = block.payload.get(at..at + 3).ok_or_else(not_pgs)?;
            let size = u16::from_be_bytes([header[1], header[2]]) as usize;
            let data = block.payload.get(at + 3..at + 3 + size).ok_or_else(not_pgs)?;
            out.extend_from_slice(b"PG");
            out.extend_from_slice(&pts.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(header);
            out.extend_from_slice(data);
            at += 3 + size;
        }
    }
    Ok(out)
}

fn not_pgs() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "a block is not PGS segments")
}

/// The sector size of a VobSub `.sub` file: each block is one sector, its pack and packet first, then zeros.
const VOBSUB_SECTOR: usize = 2048;

/// The VobSub files for the blocks of a track, as `mkvextract` writes them: the `.idx` (the track's header, then one
/// `timestamp` line for each block, with its place in the `.sub` in hexadecimal) and the `.sub` (one sector for each
/// block, holding a pack header and a packet with the subpicture). A subpicture too big for one sector is refused.
/// Returns the `.idx` text and the `.sub` bytes.
pub(crate) fn vobsub_files(blocks: &[Block], header: &[u8], language: &str) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut sub = Vec::new();
    let mut index = b"# VobSub index file, v7 (do not modify this line!)\n".to_vec();
    index.extend_from_slice(header);
    // the track's header has no language index line, which the index has (a single language is index 0)
    if !header.windows(8).any(|w| w == b"langidx:") {
        index.extend_from_slice(b"langidx: 0\n");
    }
    index.extend_from_slice(format!("\nid: {language}, index: 0\n").as_bytes());
    for block in blocks {
        let at = sub.len();
        let pts = (block.start_ms * 90) as u64;
        // the subpicture goes into one sector, or into several when it is bigger: the first packet has the time
        // stamp, and each later one (in a sector of its own) has none
        let mut sent = 0usize;
        let mut first = true;
        loop {
            let (header, room): (&[u8], usize) = if first {
                (&[0x81, 0x80, 0x05], VOBSUB_SECTOR - 14 - 6 - 3 - 5 - 1)
            } else {
                (&[0x81, 0x00, 0x00], VOBSUB_SECTOR - 14 - 6 - 3 - 1)
            };
            let take = room.min(block.payload.len() - sent);
            let sector = sub.len();
            sub.extend_from_slice(&[0, 0, 1, 0xBA]);
            sub.extend_from_slice(&scr_bytes(pts));
            sub.extend_from_slice(&[0x01, 0x89, 0xC3, 0xF8]);
            sub.extend_from_slice(&[0, 0, 1, 0xBD]);
            let length = header.len() + if first { 5 } else { 0 } + 1 + take;
            sub.extend_from_slice(&(length as u16).to_be_bytes());
            sub.extend_from_slice(header);
            if first {
                sub.extend_from_slice(&pts_bytes(pts));
            }
            sub.push(0x20);
            sub.extend_from_slice(&block.payload[sent..sent + take]);
            sent += take;
            // the rest of the sector is a padding packet (stream 0xBE) of 0xFF bytes, as mkvextract writes it
            let remaining = VOBSUB_SECTOR - (sub.len() - sector);
            if remaining >= 6 {
                sub.extend_from_slice(&[0, 0, 1, 0xBE]);
                sub.extend_from_slice(&((remaining - 6) as u16).to_be_bytes());
                sub.resize(sector + VOBSUB_SECTOR, 0xFF);
            } else {
                sub.resize(sector + VOBSUB_SECTOR, 0);
            }
            first = false;
            if sent == block.payload.len() {
                break;
            }
        }
        index.extend_from_slice(format!("timestamp: {}, filepos: {:09x}\n", vobsub_time(block.start_ms), at).as_bytes());
    }
    Ok((index, sub))
}

/// The six bytes of a pack header's system clock reference for `scr` (90 kHz ticks), with no extension.
fn scr_bytes(scr: u64) -> [u8; 6] {
    let bits = [
        (0b01, 2),
        ((scr >> 30) & 0x7, 3),
        (1, 1),
        ((scr >> 15) & 0x7FFF, 15),
        (1, 1),
        (scr & 0x7FFF, 15),
        (1, 1),
        (0, 9),
        (1, 1),
    ];
    let bytes = pack_bits(&bits);
    [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]]
}

/// The five bytes of a presentation time stamp in a packet: its prefix `0010`, then the time with a marker bit
/// after each part.
fn pts_bytes(pts: u64) -> [u8; 5] {
    let bits = [
        (0b0010, 4),
        ((pts >> 30) & 0x7, 3),
        (1, 1),
        ((pts >> 15) & 0x7FFF, 15),
        (1, 1),
        (pts & 0x7FFF, 15),
        (1, 1),
    ];
    let bytes = pack_bits(&bits);
    [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]
}

/// The fields, most significant bit first (each a value and its width in bits), packed into whole bytes.
fn pack_bits(fields: &[(u64, u32)]) -> Vec<u8> {
    let total: u32 = fields.iter().map(|(_, width)| width).sum();
    let mut out = vec![0u8; total.div_ceil(8) as usize];
    let mut at = 0u32;
    for &(value, width) in fields {
        for bit in (0..width).rev() {
            if (value >> bit) & 1 == 1 {
                out[(at / 8) as usize] |= 0x80 >> (at % 8);
            }
            at += 1;
        }
    }
    out
}

/// A VobSub time stamp, `hh:mm:ss:mmm`.
fn vobsub_time(ms: i64) -> String {
    format!("{:02}:{:02}:{:02}:{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// The WebVTT file for the blocks of a `D_WEBVTT/SUBTITLES` track: the header, then each block as a cue. A block is its
/// cue identifier, its settings and its text, one per line (an empty line when there is none); its timing is the
/// block's, from its time and duration. A block with no duration lasts until the next block (the last one to its
/// own start).
pub(crate) fn webvtt_bytes(blocks: &[Block]) -> io::Result<Vec<u8>> {
    let mut out = b"WEBVTT\n\n".to_vec();
    for (index, block) in blocks.iter().enumerate() {
        let end = match block.duration_ms {
            Some(duration) => block.start_ms + duration,
            None => blocks.get(index + 1).map_or(block.start_ms, |next| next.start_ms),
        };
        let payload = String::from_utf8(block.payload.clone()).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a WebVTT block is not UTF-8"))?;
        let mut lines = payload.splitn(3, '\n');
        let identifier = lines.next().unwrap_or("");
        let settings = lines.next().unwrap_or("");
        let text = lines.next().unwrap_or("");
        if !identifier.is_empty() {
            out.extend_from_slice(identifier.as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(format!("{} --> {}", webvtt_time(block.start_ms), webvtt_time(end)).as_bytes());
        if !settings.is_empty() {
            out.push(b' ');
            out.extend_from_slice(settings.as_bytes());
        }
        out.push(b'\n');
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\n\n");
    }
    Ok(out)
}

/// A WebVTT time, `hh:mm:ss.mmm`.
fn webvtt_time(ms: i64) -> String {
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// The WebVTT file for a `S_TEXT/WEBVTT` track, as `mkvextract` writes it: a byte order mark, the track's header (its
/// codec private data), then each cue. A cue's identifier and settings are in its block's addition 1, which holds the
/// settings, a line break, the identifier and a line break (nothing when the cue has neither). A cue is its
/// identifier (if any), its timing with the settings, and its text; cues are separated by a blank line, and the last
/// one ends with a single line break.
pub(crate) fn webvtt_text_bytes(header: &[u8], blocks: &[AddBlock]) -> Vec<u8> {
    let mut out = BOM.to_vec();
    out.extend_from_slice(header);
    out.extend_from_slice(b"\n\n");
    for (index, block) in blocks.iter().enumerate() {
        let end = match block.duration_ms {
            Some(duration) => block.start_ms + duration,
            None => blocks.get(index + 1).map_or(block.start_ms, |next| next.start_ms),
        };
        let (settings, identifier) = block
            .additions
            .iter()
            .find(|(add_id, _)| *add_id == 1)
            .map(|(_, data)| {
                let data = String::from_utf8_lossy(data).into_owned();
                let (settings, rest) = data.split_once('\n').unwrap_or((data.as_str(), ""));
                (settings.to_string(), rest.strip_suffix('\n').unwrap_or(rest).to_string())
            })
            .unwrap_or_default();
        if !identifier.is_empty() {
            out.extend_from_slice(identifier.as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(format!("{} --> {}", webvtt_time(block.start_ms), webvtt_time(end)).as_bytes());
        if !settings.is_empty() {
            out.push(b' ');
            out.extend_from_slice(settings.as_bytes());
        }
        out.push(b'\n');
        out.extend_from_slice(&block.payload);
        out.extend_from_slice(if index + 1 == blocks.len() { b"\n" } else { b"\n\n" });
    }
    out
}

/// Write the WebVTT file of a `S_TEXT/WEBVTT` track with TrackNumber `track_number` of `source` to `dest` (see
/// `webvtt_text_bytes`). Returns the number of cues.
pub fn write_webvtt_text(source: &Path, track_number: u64, dest: &Path) -> io::Result<usize> {
    let header = read_subtitles(source, track_number)?.codec_private.unwrap_or_else(|| b"WEBVTT".to_vec());
    let blocks = read_add_blocks(source, track_number)?;
    std::fs::write(dest, webvtt_text_bytes(&header, &blocks))?;
    Ok(blocks.len())
}

/// Write the WebVTT file of a `D_WEBVTT/SUBTITLES` track with TrackNumber `track_number` of `source` to `dest` (see
/// `webvtt_bytes`). Returns the number of blocks.
pub fn write_webvtt(source: &Path, track_number: u64, dest: &Path) -> io::Result<usize> {
    let blocks = decoded_blocks(source, track_number)?;
    std::fs::write(dest, webvtt_bytes(&blocks)?)?;
    Ok(blocks.len())
}

/// Write the PGS file of the track with TrackNumber `track_number` of `source` to `dest` (see `pgs_bytes`). Returns
/// the number of blocks.
pub fn write_pgs(source: &Path, track_number: u64, dest: &Path) -> io::Result<usize> {
    let blocks = decoded_blocks(source, track_number)?;
    std::fs::write(dest, pgs_bytes(&blocks)?)?;
    Ok(blocks.len())
}

/// Write the VobSub files of the track with TrackNumber `track_number` of `source`: the `.idx` to `idx_dest` and the
/// `.sub` to `sub_dest` (see `vobsub_files`). Returns the number of blocks.
pub fn write_vobsub(source: &Path, track_number: u64, language: &str, idx_dest: &Path, sub_dest: &Path) -> io::Result<usize> {
    let header = read_subtitles(source, track_number)?.codec_private.unwrap_or_default();
    let blocks = decoded_blocks(source, track_number)?;
    let (index, sub) = vobsub_files(&blocks, &header, language)?;
    std::fs::write(idx_dest, index)?;
    std::fs::write(sub_dest, sub)?;
    Ok(blocks.len())
}

/// Write the track with TrackNumber `track_number` of `source` to `dest`, as SubRip (`srt`) or as
/// SubStationAlpha (`ass`). `dest` is written in full; `source` is not changed.
pub fn write_subtitles(source: &Path, track_number: u64, extension: &str, dest: &Path) -> io::Result<usize> {
    let subtitles = read_subtitles(source, track_number)?;
    let bytes = match extension {
        "srt" => srt_bytes(&subtitles.cues),
        "ass" => ass_bytes(&subtitles),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a text subtitle format")),
    };
    let mut file = File::create(dest)?;
    file.write_all(&bytes)?;
    Ok(subtitles.cues.len())
}

#[cfg(test)]
mod bitmap_and_webvtt_tests {
    use super::*;

    fn block(start_ms: i64, payload: &[u8]) -> Block {
        Block { start_ms, duration_ms: None, payload: payload.to_vec() }
    }

    #[test]
    fn a_pgs_block_is_its_segments_each_with_a_pg_header_at_its_time() {
        // one segment of type 0x80 (END, no data) and one of type 0x16 (PCS) with two bytes of data
        let payload = [0x80, 0, 0, 0x16, 0, 2, 0xAB, 0xCD];
        let out = pgs_bytes(&[block(2000, &payload)]).unwrap();
        let pts = (180000u32).to_be_bytes();
        let mut want = b"PG".to_vec();
        want.extend_from_slice(&pts);
        want.extend_from_slice(&[0, 0, 0, 0]);
        want.extend_from_slice(&[0x80, 0, 0]);
        want.extend_from_slice(b"PG");
        want.extend_from_slice(&pts);
        want.extend_from_slice(&[0, 0, 0, 0]);
        want.extend_from_slice(&[0x16, 0, 2, 0xAB, 0xCD]);
        assert_eq!(out, want);
    }

    #[test]
    fn the_clock_and_time_stamp_fields_are_the_ones_mkvextract_writes() {
        // 2000 ms is 180000 ticks: mkvextract's pack header and packet time stamp for it
        assert_eq!(scr_bytes(180000), [0x44, 0x00, 0x2D, 0xF9, 0x04, 0x01]);
        assert_eq!(pts_bytes(180000), [0x21, 0x00, 0x0B, 0x7E, 0x41]);
        assert_eq!(scr_bytes(0), [0x44, 0x00, 0x04, 0x00, 0x04, 0x01]);
    }

    #[test]
    fn a_small_subpicture_is_one_sector_with_a_padding_packet_after_it() {
        let (index, sub) = vobsub_files(&[block(0, &[7u8; 10])], b"size: 1920x1080\n", "und").unwrap();
        assert_eq!(sub.len(), VOBSUB_SECTOR);
        assert_eq!(&sub[14..18], &[0, 0, 1, 0xBD]);
        // the sub-stream number follows the time stamp: 14 (pack) + 6 (packet) + 3 (flags) + 5 (time) bytes in
        assert_eq!(sub[28], 0x20, "the sub-stream number is 0x20");
        // after the 10 bytes of picture (from 29), a padding packet of 0xFF bytes fills the sector
        let padding = 29 + 10;
        assert_eq!(&sub[padding..padding + 4], &[0, 0, 1, 0xBE]);
        assert!(sub[padding + 6..].iter().all(|b| *b == 0xFF));
        let text = String::from_utf8(index).unwrap();
        assert_eq!(text, "# VobSub index file, v7 (do not modify this line!)\nsize: 1920x1080\nlangidx: 0\n\nid: und, index: 0\ntimestamp: 00:00:00:000, filepos: 000000000\n");
    }

    #[test]
    fn a_big_subpicture_is_split_over_sectors_and_the_later_ones_have_no_time_stamp() {
        let payload = vec![3u8; 2030];
        let (index, sub) = vobsub_files(&[block(2000, &payload)], b"size: 1920x1080\nlangidx: 0\n", "eng").unwrap();
        assert_eq!(sub.len(), 2 * VOBSUB_SECTOR);
        // the second sector starts a packet with no time stamp (flags 81 00 00) and the rest of the picture
        assert_eq!(&sub[VOBSUB_SECTOR + 14..VOBSUB_SECTOR + 14 + 6], &[0, 0, 1, 0xBD, 0, 15]);
        assert_eq!(&sub[VOBSUB_SECTOR + 20..VOBSUB_SECTOR + 23], &[0x81, 0x00, 0x00]);
        // 2030 bytes: 2019 in the first sector and 11 after them
        assert_eq!(&sub[VOBSUB_SECTOR + 14 + 6 + 3 + 1..VOBSUB_SECTOR + 14 + 6 + 3 + 1 + 11], &[3u8; 11]);
        assert!(String::from_utf8(index).unwrap().contains("timestamp: 00:00:02:000, filepos: 000000000"));
    }

    #[test]
    fn a_d_webvtt_block_is_its_id_settings_and_text_with_the_block_timing() {
        let blocks = [
            Block { start_ms: 500, duration_ms: Some(1500), payload: b"intro\nalign:start\n<i>Ciao</i>".to_vec() },
            Block { start_ms: 3000, duration_ms: Some(1250), payload: b"\n\nSeconda\nche".to_vec() },
        ];
        let out = String::from_utf8(webvtt_bytes(&blocks).unwrap()).unwrap();
        assert_eq!(out, "WEBVTT\n\nintro\n00:00:00.500 --> 00:00:02.000 align:start\n<i>Ciao</i>\n\n00:00:03.000 --> 00:00:04.250\nSeconda\nche\n\n");
    }

    #[test]
    fn an_s_text_webvtt_cue_takes_its_identifier_and_settings_from_addition_one() {
        let blocks = [AddBlock {
            start_ms: 1000,
            duration_ms: Some(1000),
            payload: b"Ciao".to_vec(),
            additions: vec![(1, b"line:90%\na\n".to_vec())],
        }];
        let out = String::from_utf8(webvtt_text_bytes(b"WEBVTT", &blocks)).unwrap();
        assert_eq!(out, "\u{feff}WEBVTT\n\na\n00:00:01.000 --> 00:00:02.000 line:90%\nCiao\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::Kind;

    /// An element with an 8 byte size, for building test files.
    fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x01);
        out.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(payload);
        out
    }

    /// A Matroska file with one SubRip track (number 1) and one cluster holding a single block with the
    /// Xiph lacing flag set (two frames, "abc" and "defg"). Nothing else is in it.
    fn laced_subtitle_file() -> Vec<u8> {
        let info = element(&[0x15, 0x49, 0xA9, 0x66], &element(&[0x2A, 0xD7, 0xB1], &[0x00, 0x0F, 0x42, 0x40]));
        let entry = [
            element(&[0xD7], &[1]),
            element(&[0x83], &[0x11]),
            element(&[0x86], b"S_TEXT/UTF8"),
        ]
        .concat();
        let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &element(&[0xAE], &entry));
        // the block: track 1, time 0, flags 0x02 (Xiph lacing), then the frame count minus one, the size of
        // the first frame, and the two frames
        let block = [&[0x81, 0x00, 0x00, 0x02, 0x01, 0x03][..], b"abc", b"defg"].concat();
        let cluster = element(&[0x1F, 0x43, 0xB6, 0x75], &[element(&[0xE7], &[0]), element(&[0xA3], &block)].concat());
        let segment = element(&[0x18, 0x53, 0x80, 0x67], &[info, tracks, cluster].concat());
        [element(&[0x1A, 0x45, 0xDF, 0xA3], &[]), segment].concat()
    }

    #[test]
    fn a_laced_subtitle_block_is_refused_rather_than_read_as_text() {
        let path = std::env::temp_dir().join(format!("mau-subs-laced-{}.mkv", std::process::id()));
        std::fs::write(&path, laced_subtitle_file()).unwrap();
        let result = read_subtitles(&path, 1);
        let _ = std::fs::remove_file(&path);
        let error = result.expect_err("a laced block is refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "laced subtitle blocks are not supported");
    }

    /// A Matroska file with one SubRip track (number 1) and one cluster of three cues: two plain blocks and
    /// a block in a BlockGroup, none of them with a duration.
    fn file_without_durations() -> Vec<u8> {
        let info = element(&[0x15, 0x49, 0xA9, 0x66], &element(&[0x2A, 0xD7, 0xB1], &[0x00, 0x0F, 0x42, 0x40]));
        let entry = [element(&[0xD7], &[1]), element(&[0x83], &[0x11]), element(&[0x86], b"S_TEXT/UTF8")].concat();
        let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &element(&[0xAE], &entry));
        // a block: track 1, its time (in ms, as the scale is 1 ms), flags 0, the text
        let block = |time: i16, text: &[u8]| [&[0x81][..], &time.to_be_bytes(), &[0x00], text].concat();
        let cluster = element(
            &[0x1F, 0x43, 0xB6, 0x75],
            &[
                element(&[0xE7], &[0]),
                element(&[0xA3], &block(1000, b"first")),
                element(&[0xA3], &block(3000, b"second")),
                element(&[0xA0], &element(&[0xA1], &block(5000, b"third"))),
            ]
            .concat(),
        );
        let segment = element(&[0x18, 0x53, 0x80, 0x67], &[info, tracks, cluster].concat());
        [element(&[0x1A, 0x45, 0xDF, 0xA3], &[]), segment].concat()
    }

    /// Cues with no BlockDuration are written as mkvextract writes them (checked against mkvextract on a
    /// file made for the purpose). Skipped without mkvextract.
    #[test]
    fn a_cue_without_a_duration_is_written_as_mkvextract_writes_it() {
        if std::process::Command::new("mkvextract").arg("--version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("mau-subs-nodur-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("nodur.mkv");
        std::fs::write(&source, file_without_durations()).unwrap();
        let ours = dir.join("ours.srt");
        let reference = dir.join("reference.srt");
        write_subtitles(&source, 1, "srt", &ours).unwrap();
        let status = std::process::Command::new("mkvextract")
            .arg("tracks").arg(&source).arg(format!("0:{}", reference.display()))
            .output().expect("mkvextract runs");
        let want = std::fs::read(&reference).unwrap_or_default();
        let got = std::fs::read(&ours).unwrap();
        let text = format!("mkvextract: {:?}\nwant:\n{}\ngot:\n{}",
            status.status, String::from_utf8_lossy(&want), String::from_utf8_lossy(&got));
        assert!(status.status.success(), "{text}");
        assert_eq!(got, want, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_srt_file_is_read_with_its_byte_order_mark_crlf_and_missing_numbers() {
        let data = b"\xEF\xBB\xBF1\r\n00:00:01,000 --> 00:00:02,500\r\nBonjour\r\n\r\n\
00:00:03,000 --> 00:00:04,000 X1:0\nDeux lignes\nici\n\n\n3\n01:00:00.250 --> 01:00:01,000\nLast";
        let cues = parse_srt(data).unwrap();
        let read: Vec<(i64, i64, &[u8])> = cues.iter().map(|c| (c.start_ms, c.end_ms, &c.text[..])).collect();
        assert_eq!(read, [
            (1000, 2500, &b"Bonjour"[..]),
            (3000, 4000, &b"Deux lignes\nici"[..]),
            (3_600_250, 3_601_000, &b"Last"[..]),
        ]);
        assert!(parse_srt(b"not a subtitle file\n").is_err(), "text with no timing is refused");
        assert!(parse_srt(b"1\n00:00:01 --> 00:00:02,000\nx\n").is_err(), "a time with no milliseconds is refused");
    }

    #[test]
    fn each_kind_of_lacing_gives_its_frames() {
        // Xiph (flags 0x02): three frames, of 3 and 260 bytes and the rest (5 bytes); 260 is 255 then 5
        let mut xiph = vec![2u8, 3, 255, 5];
        xiph.extend_from_slice(b"abc");
        xiph.extend(std::iter::repeat(b'x').take(260));
        xiph.extend_from_slice(b"fives");
        let frames = lace_frames(0x02, &xiph).unwrap();
        assert_eq!(frames.iter().map(|f| f.len()).collect::<Vec<_>>(), [3, 260, 5]);
        // fixed (flags 0x04): two frames of 4 bytes each
        let fixed = [&[1u8][..], b"aaaa", b"bbbb"].concat();
        let frames = lace_frames(0x04, &fixed).unwrap();
        assert_eq!(frames, [&b"aaaa"[..], &b"bbbb"[..]]);
        // EBML (flags 0x06): three frames, the first 3 bytes, then a difference of +1 (in one byte signed form,
        // 63 is no change, so 64 is +1), then the rest (4 bytes)
        let ebml = [&[2u8, 0x83, 0xC0][..], b"abc", b"defg", b"hijk"].concat();
        let frames = lace_frames(0x06, &ebml).unwrap();
        assert_eq!(frames.iter().map(|f| f.len()).collect::<Vec<_>>(), [3, 4, 4]);
        // a size that runs past the data is refused, not read past the end
        assert!(lace_frames(0x02, &[1u8, 200, b'a']).is_err());
        assert!(lace_frames(0x04, &[1u8, b'a', b'b', b'c']).is_err());
    }

    /// The duration each real MKV named in MAU_DURATION_CHECK (separated by `:`) has, from its Info element, must
    /// be within a tenth of a second of what ffprobe says (ffprobe reads the same element, and the streams, too).
    #[test]
    fn the_duration_read_from_the_header_is_ffprobes_on_the_files_named_in_mau_duration_check() {
        let Ok(list) = std::env::var("MAU_DURATION_CHECK") else { return };
        let mut failures = Vec::new();
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let ours = read_duration(Path::new(file)).unwrap();
            let out = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
                .arg(file)
                .output()
                .expect("ffprobe runs");
            let theirs: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(f64::NAN);
            match ours {
                Some(seconds) if (seconds - theirs).abs() < 0.1 => {}
                _ => failures.push(format!("{file}: {ours:?} here, {theirs} from ffprobe")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Each text subtitle track of each real file named in MAU_SUBS_CHECK (separated by `:`) is written in
    /// this program and by `mkvextract`, and the two files must be the same bytes. Skipped without the
    /// variable, and for a file with no text subtitle track it says so.
    #[test]
    fn the_extracted_subtitles_are_the_same_bytes_as_mkvextract_writes() {
        let Ok(list) = std::env::var("MAU_SUBS_CHECK") else { return };
        let mut failures = Vec::new();
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let source = Path::new(file);
            let tracks = crate::ebml::read_file_tracks(source).unwrap();
            let text: Vec<_> = tracks
                .iter()
                .filter(|t| t.kind == Kind::Subtitles && matches!(t.codec.as_str(), "subrip" | "ass"))
                .collect();
            if text.is_empty() {
                eprintln!("{file}: no text subtitle track, not checked");
                continue;
            }
            for track in text {
                let extension = if track.codec == "subrip" { "srt" } else { "ass" };
                let ours = std::env::temp_dir().join(format!("mau-subs-ours-{}.{extension}", std::process::id()));
                let reference = std::env::temp_dir().join(format!("mau-subs-ref-{}.{extension}", std::process::id()));
                write_subtitles(source, track.id as u64 + 1, extension, &ours).unwrap_or_else(|e| panic!("{file}: {e}"));
                let status = std::process::Command::new("mkvextract")
                    .arg("tracks").arg(source).arg(format!("{}:{}", track.id, reference.display()))
                    .output().expect("mkvextract runs");
                assert!(status.status.success(), "{file}: mkvextract failed");
                let want = std::fs::read(&reference).unwrap();
                let got = std::fs::read(&ours).unwrap();
                let _ = std::fs::remove_file(&reference);
                if want == got {
                    let _ = std::fs::remove_file(&ours);
                    eprintln!("{file} track {} ({extension}): {} bytes, the same", track.id, got.len());
                } else {
                    // the outputs are kept, so that the difference can be looked at
                    let at = want.iter().zip(got.iter()).position(|(a, b)| a != b).unwrap_or(want.len().min(got.len()));
                    failures.push(format!("{file} track {} ({extension}): {} bytes from mkvextract, {} from this program, first difference at byte {at} (kept: {}, {})",
                        track.id, want.len(), got.len(), reference.display(), ours.display()));
                }
            }
        }
        assert!(failures.is_empty(), "{} track(s) differ:\n{}", failures.len(), failures.join("\n"));
    }
}
