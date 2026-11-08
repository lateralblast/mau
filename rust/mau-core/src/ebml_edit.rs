//! Changing the default, forced and language flags and the names of an MKV's tracks, without
//! ffmpeg or mkvtoolnix (see `APPSTORE.md`, option 3).
//!
//! The changes are made to the `Tracks` element, which holds those fields. When it changes size,
//! everything after it moves, so the offsets in the `SeekHead` (`SeekPosition`) and the `Cues`
//! (`CueClusterPosition`) that point past it are moved too, and the `Segment`'s size is updated.
//! The media is copied through unchanged. The result is written to a new file, which the caller
//! puts in place of the old one once it has checked it.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::ebml::{
    invalid, invalid_range, next_element, read_id, read_size, uint, CODEC_ID, CODEC_PRIVATE, EBML_HEADER, FLAG_DEFAULT,
    FLAG_FORCED, LANGUAGE, LANGUAGE_IETF, MAX_TRACKS_BYTES, NAME, SEGMENT, TRACKS, TRACK_ENTRY,
    TRACK_NUMBER, TRACK_TYPE,
};

const SEEKHEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_POSITION: u32 = 0x53AC;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;

/// The changes to one track. `number` is the track's TrackNumber in the file (its mkvmerge ID plus one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackEdit {
    pub number: u64,
    pub default: Option<bool>,
    pub forced: Option<bool>,
    /// The ISO 639-2 language code. Any IETF language tag the track has is removed, so that the code is the language.
    pub language: Option<String>,
    /// The name. An empty name removes it.
    pub name: Option<String>,
}

/// The ID bytes of an element ID, as the file writes them (the length marker is part of the ID).
fn id_bytes(id: u32) -> Vec<u8> {
    let len = if id > 0x00FF_FFFF {
        4
    } else if id > 0x00FF_FF {
        3
    } else if id > 0xFF {
        2
    } else {
        1
    };
    (0..len).rev().map(|i| (id >> (8 * i)) as u8).collect()
}

/// A size as the fewest bytes that hold it.
fn size_bytes(value: u64) -> Vec<u8> {
    for len in 1..=8usize {
        if len == 8 || value < (1u64 << (7 * len)) - 1 {
            return sized(value, len).expect("the value fits its own length");
        }
    }
    unreachable!()
}

/// A size in exactly `len` bytes (its length marker set), or `None` when the value does not fit.
fn sized(value: u64, len: usize) -> Option<Vec<u8>> {
    if len == 0 || len > 8 || value >= (1u64 << (7 * len)) - 1 {
        return None;
    }
    let encoded = value | (1u64 << (7 * len));
    Some((0..len).rev().map(|i| (encoded >> (8 * i)) as u8).collect())
}

/// An element: its ID, its size and its payload.
fn element(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend(size_bytes(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

/// The header bytes (ID and size) and the payload of each element in `data`. Every element must
/// have a known size.
fn split(data: &[u8]) -> io::Result<Vec<(u32, &[u8], &[u8])>> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let (element, after) = next_element(rest).ok_or_else(invalid)?;
        let payload = element.payload.ok_or_else(invalid)?;
        let total = rest.len() - after.len();
        let header = &rest[..total - payload.len()];
        out.push((element.id, header, payload));
        rest = after;
    }
    Ok(out)
}

/// The changed `TrackEntry`, from its children.
fn edited_entry(body: &[u8], edit: &TrackEdit) -> io::Result<Vec<u8>> {
    let mut children: Vec<Vec<u8>> = Vec::new();
    let (mut has_default, mut has_forced, mut has_language, mut has_name) = (false, false, false, false);
    for (id, header, payload) in split(body)? {
        match id {
            FLAG_DEFAULT if edit.default.is_some() => {
                children.push(element(FLAG_DEFAULT, &[edit.default.unwrap() as u8]));
                has_default = true;
            }
            FLAG_FORCED if edit.forced.is_some() => {
                children.push(element(FLAG_FORCED, &[edit.forced.unwrap() as u8]));
                has_forced = true;
            }
            LANGUAGE if edit.language.is_some() => {
                children.push(element(LANGUAGE, edit.language.as_deref().unwrap().as_bytes()));
                has_language = true;
            }
            LANGUAGE_IETF if edit.language.is_some() => {}
            NAME if edit.name.is_some() => {
                let name = edit.name.as_deref().unwrap();
                if !name.is_empty() {
                    children.push(element(NAME, name.as_bytes()));
                }
                has_name = true;
            }
            _ => {
                let mut raw = header.to_vec();
                raw.extend_from_slice(payload);
                children.push(raw);
            }
        }
    }
    if let (Some(value), false) = (edit.default, has_default) {
        children.push(element(FLAG_DEFAULT, &[value as u8]));
    }
    if let (Some(value), false) = (edit.forced, has_forced) {
        children.push(element(FLAG_FORCED, &[value as u8]));
    }
    if let (Some(language), false) = (&edit.language, has_language) {
        children.push(element(LANGUAGE, language.as_bytes()));
    }
    if let (Some(name), false) = (&edit.name, has_name) {
        if !name.is_empty() {
            children.push(element(NAME, name.as_bytes()));
        }
    }
    Ok(element(TRACK_ENTRY, &children.concat()))
}

/// The `Tracks` payload with the edits applied; entries that no edit names are kept as they are.
fn edited_tracks(payload: &[u8], edits: &[TrackEdit]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for (id, header, body) in split(payload)? {
        if id == TRACK_ENTRY {
            let number = split(body)?
                .into_iter()
                .find(|(child, _, _)| *child == TRACK_NUMBER)
                .map(|(_, _, value)| uint(value));
            if let Some(edit) = edits.iter().find(|e| Some(e.number) == number) {
                out.extend(edited_entry(body, edit)?);
                continue;
            }
        }
        out.extend_from_slice(header);
        out.extend_from_slice(body);
    }
    Ok(out)
}

/// Where an element is in the file, and where its payload starts.
struct Top {
    id: u32,
    start: u64,
    header_len: u64,
    size: Option<u64>,
}

/// The `Segment`'s header, and where its body starts.
struct Segment {
    start: u64,
    id_len: u64,
    size_width: usize,
    size: Option<u64>,
    body: u64,
}

fn segment_header(file: &mut File) -> io::Result<Segment> {
    let mut start = [0u8; 12];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut start).map_err(|_| invalid())?;
    let (id, id_len) = read_id(&start).ok_or_else(invalid)?;
    let (size, size_len) = read_size(&start[id_len..]).ok_or_else(invalid)?;
    if id != EBML_HEADER {
        return Err(invalid());
    }
    let position = (id_len + size_len) as u64 + size.ok_or_else(invalid)?;
    file.seek(SeekFrom::Start(position))?;
    let mut header = [0u8; 16];
    let read = file.read(&mut header)?;
    let (segment_id, segment_id_len) = read_id(&header[..read]).ok_or_else(invalid)?;
    let (segment_size, segment_size_len) = read_size(&header[segment_id_len..read]).ok_or_else(invalid)?;
    if segment_id != SEGMENT {
        return Err(invalid());
    }
    Ok(Segment {
        start: position,
        id_len: segment_id_len as u64,
        size_width: segment_size_len,
        size: segment_size,
        body: position + (segment_id_len + segment_size_len) as u64,
    })
}

/// The children of the `Segment`, in the order they are in the file. Stops at an unknown size.
fn top_level(file: &mut File, segment: &Segment, file_len: u64) -> io::Result<Vec<Top>> {
    let end = segment.size.map(|s| segment.body + s).unwrap_or(file_len).min(file_len);
    let mut tops = Vec::new();
    let mut position = segment.body;
    while position < end {
        file.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 16];
        let read = file.read(&mut header)?;
        if read == 0 {
            break;
        }
        let (id, id_len) = read_id(&header[..read]).ok_or_else(invalid)?;
        let (size, size_len) = read_size(&header[id_len..read]).ok_or_else(invalid)?;
        let header_len = (id_len + size_len) as u64;
        tops.push(Top { id, start: position, header_len, size });
        match size {
            Some(size) => position += header_len + size,
            None => break,
        }
    }
    Ok(tops)
}

/// Read `len` bytes at `position`.
fn read_at(file: &mut File, position: u64, len: u64) -> io::Result<Vec<u8>> {
    // a size read from the file can be anything: nothing past the end of the file is read (or allocated)
    let length = file.metadata()?.len();
    if position.checked_add(len).map_or(true, |end| end > length) {
        return Err(invalid_range());
    }
    let mut buffer = vec![0u8; len as usize];
    file.seek(SeekFrom::Start(position))?;
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// Note a position that points past the `Tracks` element, to be moved by `delta`. Its value is kept
/// at the same width. `base` is where the element's bytes start, so the offset can be found later.
fn note_position(value: &[u8], base: usize, tracks_at: u64, delta: i64, out: &mut Vec<(usize, usize)>) -> io::Result<()> {
    let old = uint(value);
    if old > tracks_at {
        let new = old as i64 + delta;
        if new < 0 || sized_uint(new as u64, value.len()).is_none() {
            return Err(invalid());
        }
        out.push((value.as_ptr() as usize - base, value.len()));
    }
    Ok(())
}

/// Move the `SeekPosition`s and `CueClusterPosition`s that point past the `Tracks` element by `delta`.
/// `raw` is a whole `SeekHead` or `Cues` element, with a known size.
fn move_offsets(raw: &mut [u8], tracks_at: u64, delta: i64) -> io::Result<()> {
    let base = raw.as_ptr() as usize;
    let mut targets: Vec<(usize, usize)> = Vec::new();
    {
        let (root_id, _, root_body) = split(raw)?.into_iter().next().ok_or_else(invalid)?;
        match root_id {
            SEEKHEAD => {
                for (child, _, seek) in split(root_body)? {
                    if child == SEEK {
                        for (inner, _, value) in split(seek)? {
                            if inner == SEEK_POSITION {
                                note_position(value, base, tracks_at, delta, &mut targets)?;
                            }
                        }
                    }
                }
            }
            CUES => {
                for (child, _, point) in split(root_body)? {
                    if child != CUE_POINT {
                        continue;
                    }
                    for (inner, _, track_positions) in split(point)? {
                        if inner != CUE_TRACK_POSITIONS {
                            continue;
                        }
                        for (leaf, _, value) in split(track_positions)? {
                            if leaf == CUE_CLUSTER_POSITION {
                                note_position(value, base, tracks_at, delta, &mut targets)?;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (offset, width) in targets {
        let old = uint(&raw[offset..offset + width]);
        let bytes = sized_uint((old as i64 + delta) as u64, width).ok_or_else(invalid)?;
        raw[offset..offset + width].copy_from_slice(&bytes);
    }
    Ok(())
}

/// An unsigned integer in exactly `width` bytes, or `None` when it does not fit.
fn sized_uint(value: u64, width: usize) -> Option<Vec<u8>> {
    if width == 0 || width > 8 {
        return None;
    }
    if width < 8 && value >= (1u64 << (8 * width)) {
        return None;
    }
    Some((0..width).rev().map(|i| (value >> (8 * i)) as u8).collect())
}

/// Write `source` to `dest` with the track changes made. `dest` is a new file; `source` is not changed.
pub fn rewrite_tracks(source: &Path, dest: &Path, edits: &[TrackEdit]) -> io::Result<()> {
    rewrite_tracks_stoppable(source, dest, edits, &crate::model::cancelled)
}

/// `rewrite_tracks`, but the copy stops (with an `Interrupted` error) as soon as `stop` says so.
pub fn rewrite_tracks_stoppable(source: &Path, dest: &Path, edits: &[TrackEdit], stop: &dyn Fn() -> bool) -> io::Result<()> {
    let mut file = File::open(source)?;
    let file_len = file.metadata()?.len();
    let segment = segment_header(&mut file)?;
    let tops = top_level(&mut file, &segment, file_len)?;
    let tracks = tops.iter().find(|t| t.id == TRACKS).ok_or_else(invalid)?;
    let tracks_size = tracks.size.ok_or_else(invalid)?;
    if tracks_size > MAX_TRACKS_BYTES {
        return Err(invalid());
    }
    let old_tracks = read_at(&mut file, tracks.start, tracks.header_len + tracks_size)?;
    let new_tracks_payload = edited_tracks(&old_tracks[tracks.header_len as usize..], edits)?;
    write_with_tracks(&mut file, &segment, &tops, &new_tracks_payload, &[], dest, stop)
}

/// Write the file as it is, but with the payload of its `Tracks` element replaced by `new_tracks_payload`,
/// and `appended` (whole elements) added at the end of the segment. The offsets in the seek index and the cues
/// that point past the tracks are moved, and the segment's size is made to match.
fn write_with_tracks(
    file: &mut File,
    segment: &Segment,
    tops: &[Top],
    new_tracks_payload: &[u8],
    appended: &[u8],
    dest: &Path,
    stop: &dyn Fn() -> bool,
) -> io::Result<()> {
    let file_len = file.metadata()?.len();
    let tracks = tops.iter().find(|t| t.id == TRACKS).ok_or_else(invalid)?;
    let tracks_size = tracks.size.ok_or_else(invalid)?;
    let old_tracks_len = tracks.header_len + tracks_size;
    let new_tracks = element(TRACKS, new_tracks_payload);
    // the offsets move with the Tracks element only: the appended elements come after everything they point at
    let tracks_delta = new_tracks.len() as i64 - old_tracks_len as i64;
    let delta = tracks_delta + appended.len() as i64;
    let tracks_at = tracks.start - segment.body;

    let mut out = BufWriter::new(File::create(dest)?);
    // everything before the segment's size, which is written next
    let size_at = segment.start + segment.id_len;
    out.write_all(&read_at(file, 0, size_at)?)?;
    match segment.size {
        Some(size) => {
            let new_size = (size as i64 + delta) as u64;
            let bytes = sized(new_size, segment.size_width).ok_or_else(invalid)?;
            out.write_all(&bytes)?;
        }
        None => out.write_all(&read_at(file, size_at, segment.size_width as u64)?)?,
    }
    let mut written_to = segment.body;
    for top in tops {
        // a cancelled run stops between elements (the copy is then removed by the caller)
        if stop() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
        }
        // copy the bytes between the elements (there are none in a file that is laid out as it should be)
        if top.start > written_to {
            out.write_all(&read_at(file, written_to, top.start - written_to)?)?;
        }
        let total = match top.size {
            Some(size) => top.header_len + size,
            None => file_len - top.start,
        };
        match top.id {
            TRACKS => out.write_all(&new_tracks)?,
            SEEKHEAD | CUES if top.size.is_some() => {
                let mut raw = read_at(file, top.start, total)?;
                move_offsets(&mut raw, tracks_at, tracks_delta)?;
                out.write_all(&raw)?;
            }
            _ => out.write_all(&read_at(file, top.start, total)?)?,
        }
        written_to = top.start + total;
    }
    // the new elements go after the last element of the segment, and before anything after the segment
    out.write_all(appended)?;
    let end = segment.size.map(|s| segment.body + s).unwrap_or(file_len).min(file_len);
    if written_to < file_len && written_to >= end {
        out.write_all(&read_at(file, written_to, file_len - written_to)?)?;
    }
    out.flush()?;
    Ok(())
}

const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const CLUSTER: u32 = 0x1F43_B675;
const CUE_TRACK: u32 = 0xF7;

/// Reorder the tracks of a file in place, without changing its size: the tracks are renumbered
/// `1..`, in the order given. `order` lists the current TrackNumbers in the new order, and must name
/// every track. The `Tracks` element, the track numbers of the blocks and of the `Cues` are changed to
/// match, and each keeps its width, so nothing moves. A track number of 127 or more is refused: its
/// block header would need another byte.
pub fn reorder_tracks_in_place(path: &Path, order: &[u64]) -> io::Result<()> {
    if order.len() > 126 {
        return Err(invalid());
    }
    let mut seen = std::collections::HashSet::new();
    if !order.iter().all(|n| seen.insert(*n)) {
        return Err(invalid());
    }
    let renumber: std::collections::HashMap<u64, u64> =
        order.iter().enumerate().map(|(index, old)| (*old, index as u64 + 1)).collect();
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let file_len = file.metadata()?.len();
    let segment = segment_header(&mut file)?;
    let tops = top_level(&mut file, &segment, file_len)?;

    // the Tracks element: the entries in the new order, each with its new TrackNumber (same width)
    let tracks = tops.iter().find(|t| t.id == TRACKS).ok_or_else(invalid)?;
    let tracks_size = tracks.size.ok_or_else(invalid)?;
    let old_payload = read_at(&mut file, tracks.start + tracks.header_len, tracks_size)?;
    let mut entries: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut other = Vec::new();
    // a CRC-32 (the spec wants it first in its element) is kept first, and its value is worked out again below
    let mut crc: Option<Vec<u8>> = None;
    for (id, header, body) in split(&old_payload)? {
        if id == CRC_32 {
            crc = Some([header, body].concat());
            continue;
        }
        if id != TRACK_ENTRY {
            other.push(header.to_vec());
            other.push(body.to_vec());
            continue;
        }
        let mut children = Vec::new();
        let mut number = None;
        for (child, child_header, child_body) in split(body)? {
            if child == TRACK_NUMBER {
                let old = uint(child_body);
                let new = *renumber.get(&old).ok_or_else(invalid)?;
                number = Some(old);
                let bytes = sized_uint(new, child_body.len()).ok_or_else(invalid)?;
                children.extend_from_slice(child_header);
                children.extend_from_slice(&bytes);
            } else {
                children.extend_from_slice(child_header);
                children.extend_from_slice(child_body);
            }
        }
        let old = number.ok_or_else(invalid)?;
        // the entry keeps its own header: its body has the same length, so the size is the same
        let mut entry = header.to_vec();
        entry.extend(children);
        entries.push((old, entry));
    }
    if entries.len() != order.len() {
        return Err(invalid());
    }
    let mut new_payload = crc.clone().unwrap_or_default();
    for old in order {
        let (_, entry) = entries.iter().find(|(n, _)| n == old).ok_or_else(invalid)?;
        new_payload.extend_from_slice(entry);
    }
    // whatever else is in the element (nothing, in a file made by a muxer) stays after the entries
    for part in other {
        new_payload.extend_from_slice(&part);
    }
    if new_payload.len() != old_payload.len() {
        // the entries were given a fixed 8 byte size, so the payload only has the same length when the
        // original sizes were 8 bytes too: refuse otherwise rather than move the rest of the file
        return Err(invalid());
    }
    // the CRC-32 covers everything after it in the element, and its value is stored little endian
    if let Some(crc) = &crc {
        let value_at = crc.len() - 4;
        let sum = crc32(&new_payload[crc.len()..]);
        new_payload[value_at..crc.len()].copy_from_slice(&sum.to_le_bytes());
    }
    file.seek(SeekFrom::Start(tracks.start + tracks.header_len))?;
    file.write_all(&new_payload)?;

    // the track numbers of the blocks, in each cluster (one byte each, so the width is kept)
    for top in tops.iter().filter(|t| t.id == CLUSTER) {
        let Some(size) = top.size else { return Err(invalid()) };
        let end = top.start + top.header_len + size;
        let mut at = top.start + top.header_len;
        while at < end {
            let header = read_at(&mut file, at, 16.min(end - at))?;
            let (id, id_len) = read_id(&header).ok_or_else(invalid)?;
            let (child_size, size_len) = read_size(&header[id_len..]).ok_or_else(invalid)?;
            let child_size = child_size.ok_or_else(invalid)?;
            let body = at + (id_len + size_len) as u64;
            match id {
                SIMPLE_BLOCK | BLOCK => renumber_block(&mut file, body, &renumber)?,
                BLOCK_GROUP => {
                    let mut inner = body;
                    while inner < body + child_size {
                        let h = read_at(&mut file, inner, 16.min(body + child_size - inner))?;
                        let (inner_id, inner_len) = read_id(&h).ok_or_else(invalid)?;
                        let (inner_size, inner_size_len) = read_size(&h[inner_len..]).ok_or_else(invalid)?;
                        let inner_size = inner_size.ok_or_else(invalid)?;
                        let inner_body = inner + (inner_len + inner_size_len) as u64;
                        if inner_id == BLOCK {
                            renumber_block(&mut file, inner_body, &renumber)?;
                        }
                        inner = inner_body + inner_size;
                    }
                }
                _ => {}
            }
            at = body + child_size;
        }
    }

    // the track numbers in the cues
    for top in tops.iter().filter(|t| t.id == CUES) {
        let Some(size) = top.size else { continue };
        let mut raw = read_at(&mut file, top.start, top.header_len + size)?;
        let base = raw.as_ptr() as usize;
        let mut patches: Vec<(usize, usize, u64)> = Vec::new();
        let (_, _, cues_body) = split(&raw)?.into_iter().next().ok_or_else(invalid)?;
        for (point_id, _, point) in split(cues_body)? {
            if point_id != CUE_POINT {
                continue;
            }
            for (positions_id, _, positions) in split(point)? {
                if positions_id != CUE_TRACK_POSITIONS {
                    continue;
                }
                for (leaf, _, value) in split(positions)? {
                    if leaf == CUE_TRACK {
                        let old = uint(value);
                        let new = *renumber.get(&old).ok_or_else(invalid)?;
                        patches.push((value.as_ptr() as usize - base, value.len(), new));
                    }
                }
            }
        }
        for (offset, width, new) in patches {
            let bytes = sized_uint(new, width).ok_or_else(invalid)?;
            raw[offset..offset + width].copy_from_slice(&bytes);
        }
        file.seek(SeekFrom::Start(top.start))?;
        file.write_all(&raw)?;
    }
    file.sync_all()?;
    Ok(())
}

/// Change the track number of one block, whose payload starts at `at`. The number is a one byte
/// variable size integer, so the byte is replaced by its new value with the length marker kept.
fn renumber_block(file: &mut File, at: u64, renumber: &std::collections::HashMap<u64, u64>) -> io::Result<()> {
    let mut first = [0u8; 1];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut first)?;
    if first[0] & 0x80 == 0 {
        // a track number of 127 or more: not one byte (refused when the tracks are renumbered)
        return Err(invalid());
    }
    let old = (first[0] & 0x7F) as u64;
    if let Some(new) = renumber.get(&old) {
        if *new > 126 {
            return Err(invalid());
        }
        file.seek(SeekFrom::Start(at))?;
        file.write_all(&[0x80 | *new as u8])?;
    }
    Ok(())
}

const CUE_RELATIVE_POSITION: u32 = 0xF0;
const CRC_32: u32 = 0xBF;
const VOID: u32 = 0xEC;

/// The CRC-32 of Matroska (the IEEE polynomial, as in zlib).
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// The track a block belongs to, from the first byte of its payload (a track number under 127).
fn block_track(payload: &[u8]) -> io::Result<u64> {
    let first = *payload.first().ok_or_else(invalid)?;
    if first & 0x80 == 0 {
        return Err(invalid());
    }
    Ok((first & 0x7F) as u64)
}

/// The offset of `part` inside `whole`, which contains it.
fn offset_in(part: &[u8], whole: &[u8]) -> u64 {
    (part.as_ptr() as usize - whole.as_ptr() as usize) as u64
}

/// The elements of a cluster that go when the tracks in `gone` are removed: each one's offset and length,
/// in the cluster's payload. A block group goes whole, with its block.
fn cluster_removals(payload: &[u8], gone: &[u64]) -> io::Result<Vec<(u64, u64)>> {
    let mut removed = Vec::new();
    for (id, header, body) in split(payload)? {
        let start = offset_in(header, payload);
        let whole = (header.len() + body.len()) as u64;
        match id {
            SIMPLE_BLOCK if gone.contains(&block_track(body)?) => removed.push((start, whole)),
            BLOCK_GROUP => {
                for (inner, _, inner_body) in split(body)? {
                    if inner == BLOCK && gone.contains(&block_track(inner_body)?) {
                        removed.push((start, whole));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(removed)
}

/// The cluster's payload without the removed elements, with the blocks renumbered.
fn rebuild_cluster(payload: &[u8], gone: &[u64], renumber: &std::collections::HashMap<u64, u64>) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(payload.len());
    for (id, header, body) in split(payload)? {
        match id {
            SIMPLE_BLOCK => {
                let track = block_track(body)?;
                if gone.contains(&track) {
                    continue;
                }
                let new = *renumber.get(&track).ok_or_else(invalid)?;
                out.extend_from_slice(header);
                out.push(0x80 | new as u8);
                out.extend_from_slice(&body[1..]);
            }
            BLOCK_GROUP => {
                let mut inner_out = Vec::new();
                let mut drop = false;
                for (inner, inner_header, inner_body) in split(body)? {
                    if inner == BLOCK {
                        let track = block_track(inner_body)?;
                        if gone.contains(&track) {
                            drop = true;
                            break;
                        }
                        let new = *renumber.get(&track).ok_or_else(invalid)?;
                        inner_out.extend_from_slice(inner_header);
                        inner_out.push(0x80 | new as u8);
                        inner_out.extend_from_slice(&inner_body[1..]);
                    } else {
                        inner_out.extend_from_slice(inner_header);
                        inner_out.extend_from_slice(inner_body);
                    }
                }
                if !drop {
                    out.extend_from_slice(header);
                    out.extend(inner_out);
                }
            }
            _ => {
                out.extend_from_slice(header);
                out.extend_from_slice(body);
            }
        }
    }
    Ok(out)
}

/// The new position of a cluster (given its old position), and the removals inside it (see `cluster_removals`).
type ClusterMap<'a> = &'a dyn Fn(u64) -> io::Result<(u64, Vec<(u64, u64)>)>;

/// The `Cues` with the points of the removed tracks dropped, the tracks renumbered, and every position moved.
fn rebuild_cues(
    payload: &[u8],
    gone: &[u64],
    renumber: &std::collections::HashMap<u64, u64>,
    cluster: ClusterMap,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for (id, header, body) in split(payload)? {
        if id != CUE_POINT {
            out.extend_from_slice(header);
            out.extend_from_slice(body);
            continue;
        }
        let mut point = Vec::new();
        let mut kept_any = false;
        let mut had_positions = false;
        for (pid, pheader, pbody) in split(body)? {
            if pid != CUE_TRACK_POSITIONS {
                point.extend_from_slice(pheader);
                point.extend_from_slice(pbody);
                continue;
            }
            had_positions = true;
            let fields = split(pbody)?;
            let track = fields.iter().find(|(f, _, _)| *f == CUE_TRACK).map(|(_, _, v)| uint(v)).ok_or_else(invalid)?;
            if gone.contains(&track) {
                continue;
            }
            kept_any = true;
            let cluster_at = fields.iter().find(|(f, _, _)| *f == CUE_CLUSTER_POSITION).map(|(_, _, v)| uint(v)).ok_or_else(invalid)?;
            let (new_cluster, removals) = cluster(cluster_at)?;
            let relative = fields.iter().find(|(f, _, _)| *f == CUE_RELATIVE_POSITION).map(|(_, _, v)| uint(v));
            let mut tp = Vec::new();
            for (fid, fheader, fbody) in &fields {
                match *fid {
                    CUE_TRACK => {
                        let new = *renumber.get(&track).ok_or_else(invalid)?;
                        tp.extend_from_slice(fheader);
                        tp.extend(sized_uint(new, fbody.len()).ok_or_else(invalid)?);
                    }
                    CUE_CLUSTER_POSITION => {
                        tp.extend_from_slice(fheader);
                        tp.extend(sized_uint(new_cluster, fbody.len()).ok_or_else(invalid)?);
                    }
                    CUE_RELATIVE_POSITION => {
                        // measured from the start of the cluster's payload: the removed bytes before it come off
                        let old = relative.unwrap_or(0);
                        let before: u64 = removals.iter().filter(|(start, _)| *start < old).map(|(_, len)| len).sum();
                        tp.extend_from_slice(fheader);
                        tp.extend(sized_uint(old - before, fbody.len()).ok_or_else(invalid)?);
                    }
                    _ => {
                        tp.extend_from_slice(fheader);
                        tp.extend_from_slice(fbody);
                    }
                }
            }
            point.extend(element(CUE_TRACK_POSITIONS, &tp));
        }
        if !had_positions || kept_any {
            out.extend(element(CUE_POINT, &point));
        }
    }
    Ok(out)
}

/// Remove the tracks whose numbers are in `gone` from a file, writing the result to `dest` (`source` is not
/// changed). The blocks of those tracks go, the remaining tracks are renumbered `1..`, and the seek index,
/// the cues and the sizes are all made to match. Fails for a file whose layout it does not expect (an
/// unknown size on a cluster, or a gap between the elements of the segment). `promote` is the TrackNumber
/// (before the change) of a kept track to make the default, which the caller does when the default track
/// was removed; `None` leaves every flag as it was.
pub fn remove_tracks(source: &Path, dest: &Path, gone: &[u64], promote: Option<u64>) -> io::Result<()> {
    let mut file = File::open(source)?;
    let file_len = file.metadata()?.len();
    let segment = segment_header(&mut file)?;
    let tops = top_level(&mut file, &segment, file_len)?;
    let segment_size = segment.size.ok_or_else(invalid)?;
    // the elements must be laid out end to end, with nothing between them
    let mut expected = segment.body;
    for top in &tops {
        if top.start != expected {
            return Err(invalid());
        }
        expected = top.start + top.header_len + top.size.ok_or_else(invalid)?;
    }
    if expected != segment.body + segment_size {
        return Err(invalid());
    }

    // the tracks that are kept, in their order, with their new numbers
    let tracks = tops.iter().find(|t| t.id == TRACKS).ok_or_else(invalid)?;
    let tracks_size = tracks.size.ok_or_else(invalid)?;
    let old_payload = read_at(&mut file, tracks.start + tracks.header_len, tracks_size)?;
    let mut kept_entries: Vec<(u64, Vec<u8>)> = Vec::new();
    for (id, header, body) in split(&old_payload)? {
        if id != TRACK_ENTRY {
            // a CRC-32 or Void is optional and is left out of the rebuilt element (a kept CRC would be
            // wrong, since the entries around it change); anything else is refused
            if id == CRC_32 || id == VOID {
                continue;
            }
            return Err(invalid());
        }
        let mut children = Vec::new();
        let mut number = None;
        for (child, child_header, child_body) in split(body)? {
            if child == TRACK_NUMBER {
                number = Some(uint(child_body));
            }
            children.extend_from_slice(child_header);
            children.extend_from_slice(child_body);
        }
        let number = number.ok_or_else(invalid)?;
        if !gone.contains(&number) {
            let entry = if promote == Some(number) {
                // the default flag is set on the entry (edited_entry writes the whole element)
                let edit = TrackEdit { number, default: Some(true), forced: None, language: None, name: None };
                edited_entry(&children, &edit)?
            } else {
                let mut entry = header.to_vec();
                entry.extend(children);
                entry
            };
            kept_entries.push((number, entry));
        }
    }
    let renumber: std::collections::HashMap<u64, u64> =
        kept_entries.iter().enumerate().map(|(index, (old, _))| (*old, index as u64 + 1)).collect();
    if renumber.len() > 126 {
        return Err(invalid());
    }
    // the entries, with the TrackNumber of each set to its new number (the same width)
    let mut new_tracks_payload = Vec::new();
    for (old, entry) in &kept_entries {
        let mut bytes = Vec::new();
        let (header, body) = split_header(entry)?;
        for (child, child_header, child_body) in split(body)? {
            bytes.extend_from_slice(child_header);
            if child == TRACK_NUMBER {
                let new = renumber[old];
                bytes.extend(sized_uint(new, child_body.len()).ok_or_else(invalid)?);
            } else {
                bytes.extend_from_slice(child_body);
            }
        }
        new_tracks_payload.extend_from_slice(header);
        new_tracks_payload.extend(bytes);
    }
    let new_tracks = element(TRACKS, &new_tracks_payload);

    // the clusters: what goes out of each, and its new size
    let mut plans: Vec<(usize, u64, Vec<(u64, u64)>, u64)> = Vec::new(); // (top index, old cluster payload len, removals, new payload len)
    let mut cluster_by_old: std::collections::HashMap<u64, (usize, Vec<(u64, u64)>)> = std::collections::HashMap::new();
    for (index, top) in tops.iter().enumerate() {
        if top.id != CLUSTER {
            continue;
        }
        let size = top.size.ok_or_else(invalid)?;
        let payload = read_at(&mut file, top.start + top.header_len, size)?;
        let removals = cluster_removals(&payload, gone)?;
        let removed_total: u64 = removals.iter().map(|(_, len)| len).sum();
        let new_len = size - removed_total;
        cluster_by_old.insert(top.start - segment.body, (index, removals.clone()));
        plans.push((index, size, removals, new_len));
    }

    // the sizes of everything, so that each element's new position is known
    let mut new_sizes: Vec<u64> = Vec::with_capacity(tops.len());
    let mut new_bytes_for: Vec<Option<Vec<u8>>> = vec![None; tops.len()];
    for (index, top) in tops.iter().enumerate() {
        let (new_len, bytes) = match top.id {
            TRACKS => {
                let bytes = new_tracks.clone();
                (bytes.len() as u64, Some(bytes))
            }
            CLUSTER => {
                let (_, _, _, new_len) = plans.iter().find(|p| p.0 == index).ok_or_else(invalid)?;
                let header = id_bytes(CLUSTER).len() as u64 + size_bytes(*new_len).len() as u64;
                (header + new_len, None)
            }
            CUES | SEEKHEAD => {
                let payload = read_at(&mut file, top.start + top.header_len, top.size.ok_or_else(invalid)?)?;
                let rebuilt = if top.id == CUES {
                    rebuild_cues(&payload, gone, &renumber, &|_| Ok((0, Vec::new())))?
                } else {
                    rebuild_seekhead(&payload, &|_| Ok(0))?
                };
                let bytes = element(top.id, &rebuilt);
                (bytes.len() as u64, Some(bytes))
            }
            _ => (top.header_len + top.size.ok_or_else(invalid)?, None),
        };
        new_sizes.push(new_len);
        new_bytes_for[index] = bytes;
    }
    // new starts, relative to the segment body, from the new sizes
    let mut new_start: Vec<u64> = Vec::with_capacity(tops.len());
    let mut at = 0u64;
    for size in &new_sizes {
        new_start.push(at);
        at += size;
    }
    let total = at;
    let map_top = |old: u64| -> io::Result<u64> {
        tops.iter()
            .position(|t| t.start - segment.body == old)
            .map(|i| new_start[i])
            .ok_or_else(invalid)
    };
    let cluster_map = |old: u64| -> io::Result<(u64, Vec<(u64, u64)>)> {
        let (_, removals) = cluster_by_old.get(&old).ok_or_else(invalid)?;
        Ok((map_top(old)?, removals.clone()))
    };

    // pass two: write the file (a cancelled run stops between elements, and the caller removes the copy)
    let mut out = BufWriter::new(File::create(dest)?);
    let size_at = segment.start + segment.id_len;
    out.write_all(&read_at(&mut file, 0, size_at)?)?;
    let bytes = sized(total, segment.size_width).ok_or_else(invalid)?;
    out.write_all(&bytes)?;
    for (index, top) in tops.iter().enumerate() {
        if crate::model::cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
        }
        match top.id {
            CLUSTER => {
                let (_, _, _, new_len) = plans.iter().find(|p| p.0 == index).ok_or_else(invalid)?;
                let payload = read_at(&mut file, top.start + top.header_len, top.size.ok_or_else(invalid)?)?;
                let rebuilt = rebuild_cluster(&payload, gone, &renumber)?;
                debug_assert_eq!(rebuilt.len() as u64, *new_len);
                out.write_all(&id_bytes(CLUSTER))?;
                out.write_all(&size_bytes(rebuilt.len() as u64))?;
                out.write_all(&rebuilt)?;
            }
            CUES | SEEKHEAD => {
                let payload = read_at(&mut file, top.start + top.header_len, top.size.ok_or_else(invalid)?)?;
                let rebuilt = if top.id == CUES {
                    rebuild_cues(&payload, gone, &renumber, &cluster_map)?
                } else {
                    rebuild_seekhead(&payload, &map_top)?
                };
                out.write_all(&element(top.id, &rebuilt))?;
            }
            TRACKS => out.write_all(new_bytes_for[index].as_ref().ok_or_else(invalid)?)?,
            _ => out.write_all(&read_at(&mut file, top.start, top.header_len + top.size.ok_or_else(invalid)?)?)?,
        }
    }
    out.flush()?;
    Ok(())
}

/// The `SeekHead` payload with each `SeekPosition` moved by `map` (the old position gives the new one).
fn rebuild_seekhead(payload: &[u8], map: &dyn Fn(u64) -> io::Result<u64>) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for (id, header, body) in split(payload)? {
        if id != SEEK {
            out.extend_from_slice(header);
            out.extend_from_slice(body);
            continue;
        }
        let mut seek = Vec::new();
        for (fid, fheader, fbody) in split(body)? {
            seek.extend_from_slice(fheader);
            if fid == SEEK_POSITION {
                let new = map(uint(fbody))?;
                seek.extend(sized_uint(new, fbody.len()).ok_or_else(invalid)?);
            } else {
                seek.extend_from_slice(fbody);
            }
        }
        out.extend(element(SEEK, &seek));
    }
    Ok(out)
}

/// The header of an element (its ID and size bytes) and its body, for an element of known size.
fn split_header(element: &[u8]) -> io::Result<(&[u8], &[u8])> {
    let (_, id_len) = read_id(element).ok_or_else(invalid)?;
    let (size, size_len) = read_size(&element[id_len..]).ok_or_else(invalid)?;
    let header = id_len + size_len;
    let size = size.ok_or_else(invalid)? as usize;
    Ok((&element[..header], &element[header..header + size]))
}

/// A subtitle track to add to an MKV: its language and name, whether it is forced, and its cues. It is a
/// text track (`S_TEXT/UTF8`) with no default flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSubtitleTrack {
    pub language: String,
    pub name: String,
    pub forced: bool,
    pub cues: Vec<NewCue>,
    /// The codec ID: `S_TEXT/UTF8` for SubRip, `S_TEXT/ASS` for an ASS track (with its header as `private`).
    pub codec: &'static str,
    pub private: Option<Vec<u8>>,
}

/// One cue to write: its start and end in milliseconds, its text, and its block additions as (BlockAddID, data) (a
/// WebVTT cue's identifier and settings are one, see `text_subs::read_vtt_for_mkv`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: Vec<u8>,
    pub additions: Vec<(u64, Vec<u8>)>,
}

const BLOCK_ADDITIONS: u32 = 0x75A1;
const BLOCK_MORE: u32 = 0xA6;
const BLOCK_ADD_ID: u32 = 0xEE;
const BLOCK_ADDITIONAL: u32 = 0xA5;
const INFO_ELEMENT: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const TIMECODE: u32 = 0xE7;
const BLOCK_DURATION: u32 = 0x9B;
const TRACK_UID: u32 = 0x73C5;
/// The track type of a subtitle track.
const SUBTITLE_TYPE: u8 = 0x11;
/// The most time one cluster covers, so that a block's time from the cluster fits in 16 bits (in milliseconds).
const CLUSTER_SPAN_MS: i64 = 30_000;

/// Write `source` to `dest` with the subtitle tracks added after its existing tracks, each with its cues in
/// clusters at the end of the segment. The file's timestamp scale must be the default (1 ms), and its segment
/// must have a known size. `source` is not changed.
pub fn add_subtitle_tracks(source: &Path, dest: &Path, tracks: &[NewSubtitleTrack]) -> io::Result<()> {
    let mut file = File::open(source)?;
    let file_len = file.metadata()?.len();
    let segment = segment_header(&mut file)?;
    let tops = top_level(&mut file, &segment, file_len)?;
    segment.size.ok_or_else(invalid)?;
    if let Some(info) = tops.iter().find(|t| t.id == INFO_ELEMENT) {
        let size = info.size.ok_or_else(invalid)?;
        for (id, _, body) in split(&read_at(&mut file, info.start + info.header_len, size)?)? {
            if id == TIMESTAMP_SCALE && uint(body) != 1_000_000 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "only a file with a 1 ms timestamp scale can be written"));
            }
        }
    }
    let old_tracks = tops.iter().find(|t| t.id == TRACKS).ok_or_else(invalid)?;
    let tracks_size = old_tracks.size.ok_or_else(invalid)?;
    if tracks_size > MAX_TRACKS_BYTES {
        return Err(invalid());
    }
    let old = read_at(&mut file, old_tracks.start + old_tracks.header_len, tracks_size)?;
    let mut payload = Vec::new();
    let mut highest = 0u64;
    for (id, header, body) in split(&old)? {
        match id {
            TRACK_ENTRY => {
                for (child, _, value) in split(body)? {
                    if child == TRACK_NUMBER {
                        highest = highest.max(uint(value));
                    }
                }
                payload.extend_from_slice(header);
                payload.extend_from_slice(body);
            }
            // a CRC-32 or Void is left out, as in remove_tracks (a checksum would be stale)
            CRC_32 | VOID => {}
            _ => return Err(invalid()),
        }
    }
    let mut appended = Vec::new();
    for track in tracks {
        highest += 1;
        if highest > 126 {
            return Err(invalid());
        }
        payload.extend(text_track_entry(highest, track));
        appended.extend(clusters_for(highest, &track.cues));
    }
    write_with_tracks(&mut file, &segment, &tops, &payload, &appended, dest, &crate::model::cancelled)
}

/// The TrackEntry of a new subtitle track with TrackNumber `number`.
fn text_track_entry(number: u64, track: &NewSubtitleTrack) -> Vec<u8> {
    let mut body = [
        element(TRACK_NUMBER, &uint_bytes(number)),
        element(TRACK_UID, &uint_bytes(new_track_uid(number))),
        element(TRACK_TYPE, &[SUBTITLE_TYPE]),
        element(CODEC_ID, track.codec.as_bytes()),
        element(FLAG_DEFAULT, &[0]),
    ]
    .concat();
    if let Some(private) = &track.private {
        body.extend(element(CODEC_PRIVATE, private));
    }
    if track.forced {
        body.extend(element(FLAG_FORCED, &[1]));
    }
    body.extend(element(LANGUAGE, track.language.as_bytes()));
    if !track.name.is_empty() {
        body.extend(element(NAME, track.name.as_bytes()));
    }
    element(TRACK_ENTRY, &body)
}

/// A TrackUID for a new track, which the spec requires (mkvmerge warns without one). It is not zero, and it
/// differs for each track number and each moment it is made.
fn new_track_uid(number: u64) -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    (nanos ^ number.wrapping_mul(0x9E37_79B9_7F4A_7C15)).max(1)
}

/// The clusters that hold the cues of track `number`: a cluster starts at the first cue that is out of
/// reach of the last one's cluster, and each cue is a BlockGroup with its duration.
fn clusters_for(number: u64, cues: &[NewCue]) -> Vec<u8> {
    let mut sorted: Vec<&NewCue> = cues.iter().collect();
    sorted.sort_by_key(|cue| cue.start_ms);
    let mut out = Vec::new();
    let mut current: Option<(i64, Vec<u8>)> = None;
    for cue in sorted {
        let at = cue.start_ms.max(0);
        let reachable = current.as_ref().is_some_and(|(base, _)| at - base <= CLUSTER_SPAN_MS);
        if !reachable {
            if let Some((base, blocks)) = current.take() {
                out.extend(cluster(base, &blocks));
            }
            current = Some((at, Vec::new()));
        }
        if let Some((base, blocks)) = current.as_mut() {
            let relative = (at - *base) as i16;
            let duration = (cue.end_ms - cue.start_ms).max(0) as u64;
            let block = [&[0x80 | number as u8][..], &relative.to_be_bytes(), &[0x00], &cue.text[..]].concat();
            let mut group = [element(BLOCK, &block), element(BLOCK_DURATION, &uint_bytes(duration))].concat();
            if !cue.additions.is_empty() {
                // BlockAdditions > BlockMore (BlockAddID, BlockAdditional) for each addition
                let mut more = Vec::new();
                for (add_id, data) in &cue.additions {
                    let body = [element(BLOCK_ADD_ID, &uint_bytes(*add_id)), element(BLOCK_ADDITIONAL, data)].concat();
                    more.extend(element(BLOCK_MORE, &body));
                }
                group.extend(element(BLOCK_ADDITIONS, &more));
            }
            blocks.extend(element(BLOCK_GROUP, &group));
        }
    }
    if let Some((base, blocks)) = current {
        out.extend(cluster(base, &blocks));
    }
    out
}

fn cluster(base: i64, blocks: &[u8]) -> Vec<u8> {
    element(CLUSTER, &[element(TIMECODE, &uint_bytes(base as u64)), blocks.to_vec()].concat())
}

/// An unsigned integer in the fewest bytes (at least one).
fn uint_bytes(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    bytes[first..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ebml::{read_file_tracks, CODEC_ID};

    fn track_entry(number: u8, kind: u8, codec: &str, language: &str, default: bool, name: &str) -> Vec<u8> {
        let mut body = element(TRACK_NUMBER, &[number]);
        body.extend(element(0x83, &[kind]));
        body.extend(element(CODEC_ID, codec.as_bytes()));
        body.extend(element(LANGUAGE, language.as_bytes()));
        body.extend(element(FLAG_DEFAULT, &[default as u8]));
        if !name.is_empty() {
            body.extend(element(NAME, name.as_bytes()));
        }
        element(TRACK_ENTRY, &body)
    }

    const CLUSTER: u32 = 0x1F43_B675;
    const CUES_ID: u32 = 0x1C53_BB6B;

    /// A position as an 8 byte unsigned integer, so that the SeekHead and Cues keep their size.
    fn fixed(value: u64) -> Vec<u8> {
        value.to_be_bytes().to_vec()
    }

    /// A small Matroska file, laid out as ffmpeg writes one: a SeekHead that points at the Cluster and the
    /// Cues, a Void, the Tracks, a Cluster, and Cues that point at the Cluster. The positions are measured
    /// from the start of the Segment's body, as the format defines them.
    fn sample_file(cluster_at: u64, cues_at: u64) -> Vec<u8> {
        let seek = |id: u32, position: u64| {
            let mut body = element(0x53AB, &id_bytes(id));
            body.extend(element(SEEK_POSITION, &fixed(position)));
            element(SEEK, &body)
        };
        let seekhead = element(SEEKHEAD, &[seek(CUES_ID, cues_at), seek(CLUSTER, cluster_at)].concat());
        let void = element(0xEC, &[0; 16]);
        let mut tracks_list = track_entry(1, 1, "V_MPEG4/ISO/AVC", "und", true, "Picture");
        tracks_list.extend(track_entry(2, 2, "A_AAC", "ita", true, "Italiano"));
        tracks_list.extend(track_entry(3, 2, "A_AAC", "eng", false, ""));
        let tracks = element(TRACKS, &tracks_list);
        let cluster = element(CLUSTER, &[0xE7, 0x81, 0x00, 0xA3, 0x84, 0x81, 0x00, 0x00, 0x00]);
        let cue_point = element(
            CUE_POINT,
            &[element(0xB3, &[0x81]), element(CUE_TRACK_POSITIONS, &element(CUE_CLUSTER_POSITION, &fixed(cluster_at)))].concat(),
        );
        let cues = element(CUES_ID, &cue_point);
        let mut body = seekhead;
        body.extend(void);
        body.extend(tracks);
        body.extend(cluster);
        body.extend(cues);
        let mut segment = id_bytes(SEGMENT);
        segment.push(0x01);
        // an 8 byte size: the marker is the top bit of the first byte, so seven bytes of value follow it
        segment.extend(&(body.len() as u64).to_be_bytes()[1..]);
        segment.extend(body);
        let mut file = element(EBML_HEADER, &[0x42, 0x82, 0x81, 0x6D]);
        file.extend(segment);
        file
    }

    /// The layout of a sample: where the Cluster and the Cues are, measured from the Segment's body.
    fn layout_of(bytes: &[u8]) -> (u64, u64) {
        let segment_body = find_segment_body(bytes);
        let mut at = (segment_body) as usize;
        let mut cluster = None;
        let mut cues = None;
        while at < bytes.len() {
            let (id, id_len) = read_id(&bytes[at..]).unwrap();
            let (size, size_len) = read_size(&bytes[at + id_len..]).unwrap();
            let size = size.unwrap();
            let rel = at as u64 - segment_body;
            if id == CLUSTER {
                cluster = Some(rel);
            }
            if id == CUES_ID {
                cues = Some(rel);
            }
            at += id_len + size_len + size as usize;
        }
        (cluster.unwrap(), cues.unwrap())
    }

    fn find_segment_body(bytes: &[u8]) -> u64 {
        let (_, id_len) = read_id(bytes).unwrap();
        let (size, size_len) = read_size(&bytes[id_len..]).unwrap();
        let header = (id_len + size_len) as u64 + size.unwrap();
        let (_, seg_id_len) = read_id(&bytes[header as usize..]).unwrap();
        let (_, seg_size_len) = read_size(&bytes[header as usize + seg_id_len..]).unwrap();
        header + (seg_id_len + seg_size_len) as u64
    }

    /// The position a SeekHead entry gives for an element ID.
    fn seek_position(bytes: &[u8], id: u32) -> u64 {
        let body = find_segment_body(bytes) as usize;
        let mut at = body;
        loop {
            let (top, top_id_len) = read_id(&bytes[at..]).unwrap();
            let (size, size_len) = read_size(&bytes[at + top_id_len..]).unwrap();
            let size = size.unwrap() as usize;
            let payload = &bytes[at + top_id_len + size_len..at + top_id_len + size_len + size];
            if top == SEEKHEAD {
                for (child, _, seek) in split(payload).unwrap() {
                    if child == SEEK {
                        let mut target = None;
                        let mut position = None;
                        for (leaf, _, value) in split(seek).unwrap() {
                            if leaf == 0x53AB {
                                target = Some(uint(value) as u32);
                            } else if leaf == SEEK_POSITION {
                                position = Some(uint(value));
                            }
                        }
                        if target == Some(id) {
                            return position.unwrap();
                        }
                    }
                }
                panic!("no SeekHead entry for {id:x}");
            }
            at += top_id_len + size_len + size;
        }
    }

    fn write_sample(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("mau-ebml-edit-{}-{name}.mkv", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn the_flags_names_and_languages_change_and_the_offsets_follow_the_moved_tracks() {
        // the layout is measured once with placeholder positions (the same size), then filled in
        let probe = sample_file(0, 0);
        let (cluster_at, cues_at) = layout_of(&probe);
        let bytes = sample_file(cluster_at, cues_at);
        let source = write_sample("source", &bytes);
        let dest = std::env::temp_dir().join(format!("mau-ebml-edit-{}-dest.mkv", std::process::id()));
        let edits = [
            TrackEdit { number: 2, default: Some(false), ..TrackEdit::default() },
            TrackEdit { number: 3, default: Some(true), forced: Some(true), name: Some("Forced".into()), ..TrackEdit::default() },
            TrackEdit { number: 1, language: Some("fre".into()), name: Some(String::new()), ..TrackEdit::default() },
        ];
        rewrite_tracks(&source, &dest, &edits).unwrap();
        let out = std::fs::read(&dest).unwrap();
        let tracks = read_file_tracks(&dest).unwrap();
        let summary: Vec<(&str, &str, bool, bool, &str)> = tracks
            .iter()
            .map(|t| (t.codec.as_str(), t.language.as_str(), t.default, t.forced, t.name.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("h264", "fre", true, false, ""),
                ("aac", "ita", false, false, "Italiano"),
                ("aac", "eng", true, true, "Forced"),
            ]
        );
        // the Tracks element grew, so the Cluster and the Cues moved, and the SeekHead and Cues say so
        let (moved_cluster, moved_cues) = layout_of(&out);
        assert!(moved_cluster > cluster_at, "the edit made the tracks bigger");
        assert_eq!(seek_position(&out, CLUSTER), moved_cluster, "the SeekHead points at the moved Cluster");
        assert_eq!(seek_position(&out, CUES_ID), moved_cues, "the SeekHead points at the moved Cues");
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&dest);
    }

    /// The edits used on real files: the first audio track becomes the default and the second is
    /// marked French, the first subtitle track is renamed and marked forced. Returns them with the
    /// expected (type, language, name, default, forced) of each track after the edits.
    fn real_file_edits(tracks: &[crate::track::Track]) -> (Vec<TrackEdit>, Vec<(String, String, String, bool, bool)>) {
        let mut edits = Vec::new();
        let mut expected = Vec::new();
        for track in tracks {
            let number = track.id as u64 + 1;
            let mut edit = TrackEdit { number, ..TrackEdit::default() };
            let (kind, language, name, default, forced) = match (track.kind, track.number) {
                (crate::track::Kind::Audio, 1) => {
                    edit.default = Some(true);
                    ("audio", track.language.clone(), track.name.clone(), true, track.forced)
                }
                (crate::track::Kind::Audio, 2) => {
                    edit.language = Some("fre".into());
                    edit.default = Some(false);
                    ("audio", "fre".to_string(), track.name.clone(), false, track.forced)
                }
                (crate::track::Kind::Subtitles, 1) => {
                    edit.name = Some("Renamed subtitles".into());
                    edit.forced = Some(true);
                    ("subtitles", track.language.clone(), "Renamed subtitles".to_string(), track.default, true)
                }
                _ => ("", track.language.clone(), track.name.clone(), track.default, track.forced),
            };
            if !kind.is_empty() || edit.default.is_some() || edit.language.is_some() || edit.name.is_some() || edit.forced.is_some() {
                edits.push(edit);
            }
            expected.push((track.kind.name().to_string(), language, name, default, forced));
        }
        (edits, expected)
    }

    /// The tracks `mkvmerge -J` reports for a file, in the same form as `real_file_edits` returns.
    fn mkvmerge_tracks(path: &Path) -> Option<(Vec<(String, String, String, bool, bool)>, usize, usize)> {
        let out = std::process::Command::new("mkvmerge").arg("-J").arg(path).output().ok()?;
        let data: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let mut rows = Vec::new();
        for raw in data.get("tracks")?.as_array()? {
            let kind = raw.get("type")?.as_str()?;
            if !matches!(kind, "video" | "audio" | "subtitles") {
                continue;
            }
            let props = raw.get("properties")?;
            rows.push((
                kind.to_string(),
                props.get("language").and_then(|v| v.as_str()).unwrap_or("und").to_string(),
                props.get("track_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                props.get("default_track").and_then(|v| v.as_bool()).unwrap_or(false),
                props.get("forced_track").and_then(|v| v.as_bool()).unwrap_or(false),
            ));
        }
        Some((rows, data.get("attachments")?.as_array()?.len(), data.get("chapters")?.as_array().map(|c| c.len()).unwrap_or(0)))
    }

    /// Run the edits on the real files named in MAU_EBML_EDIT_CHECK (separated by `:`), and check each
    /// result with mkvmerge. Skipped when the variable is not set or mkvmerge is missing. The files are
    /// read, and the results are written to temporary files that are removed.
    #[test]
    fn the_edits_are_right_on_the_files_named_in_mau_ebml_edit_check() {
        let Ok(list) = std::env::var("MAU_EBML_EDIT_CHECK") else { return };
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let source = Path::new(file);
            let (before, attachments_before, chapters_before) = mkvmerge_tracks(source).expect("mkvmerge reads the source");
            assert!(!before.is_empty(), "{file}: mkvmerge reported no tracks");
            let tracks = crate::ebml::read_file_tracks(source).unwrap();
            let (edits, expected) = real_file_edits(&tracks);
            let dest = std::env::temp_dir().join(format!("mau-ebml-edit-real-{}.mkv", std::process::id()));
            rewrite_tracks(source, &dest, &edits).unwrap_or_else(|e| panic!("{file}: {e}"));
            let (after, attachments_after, chapters_after) = mkvmerge_tracks(&dest).expect("mkvmerge reads the result");
            let _ = std::fs::remove_file(&dest);
            assert_eq!(after, expected, "{file}: the tracks after the edits");
            assert_eq!(attachments_after, attachments_before, "{file}: attachments");
            assert_eq!(chapters_after, chapters_before, "{file}: chapters");
        }
    }

    /// A file with two tracks, a cluster holding a block for each, and cues for each. The layout is
    /// measured once with the same sizes, so the cluster and cue positions are right.
    fn two_track_file() -> Vec<u8> {
        // the cues point at the cluster, so its position is measured from a first version with the same sizes
        let probe = two_track_file_at(0, 0);
        let (cluster_at, cues_at) = layout_of(&probe);
        two_track_file_at(cluster_at, cues_at)
    }

    fn two_track_file_at(cluster_at: u64, cues_at: u64) -> Vec<u8> {
        let seek = |id: u32, position: u64| {
            let mut body = element(0x53AB, &id_bytes(id));
            body.extend(element(SEEK_POSITION, &fixed(position)));
            element(SEEK, &body)
        };
        let seekhead = element(SEEKHEAD, &[seek(CUES_ID, cues_at), seek(CLUSTER, cluster_at)].concat());
        let mut tracks_list = track_entry(1, 1, "V_MPEG4/ISO/AVC", "und", true, "Picture");
        tracks_list.extend(track_entry(2, 2, "A_AAC", "ita", true, "Italiano"));
        let tracks = element(TRACKS, &tracks_list);
        // a SimpleBlock: track vint, a 16 bit timecode, flags, then the data
        let block = |track: u8| element(SIMPLE_BLOCK, &[0x80 | track, 0, 0, 0x80, 1, 2, 3]);
        let cluster = element(CLUSTER, &[element(0xE7, &[0]), block(1), block(2)].concat());
        let cue = |track: u8| {
            element(CUE_POINT, &[element(0xB3, &[1]), element(CUE_TRACK_POSITIONS, &[element(CUE_TRACK, &[track]), element(CUE_CLUSTER_POSITION, &fixed(cluster_at))].concat())].concat())
        };
        let cues = element(CUES_ID, &[cue(1), cue(2)].concat());
        let mut body = seekhead;
        body.extend(tracks);
        body.extend(cluster);
        body.extend(cues);
        let mut segment = id_bytes(SEGMENT);
        segment.push(0x01);
        segment.extend(&(body.len() as u64).to_be_bytes()[1..]);
        segment.extend(body);
        let mut file = element(EBML_HEADER, &[0x42, 0x82, 0x81, 0x6D]);
        file.extend(segment);
        file
    }

    #[test]
    fn reordering_renumbers_the_tracks_and_their_blocks_and_cues_without_moving_anything() {
        let bytes = two_track_file();
        let path = std::env::temp_dir().join(format!("mau-ebml-reorder-{}.mkv", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        reorder_tracks_in_place(&path, &[2, 1]).unwrap();
        let out = std::fs::read(&path).unwrap();
        assert_eq!(out.len(), bytes.len(), "the size does not change");
        let tracks = read_file_tracks(&path).unwrap();
        let order: Vec<(&str, &str)> = tracks.iter().map(|t| (t.codec.as_str(), t.name.as_str())).collect();
        assert_eq!(order, [("aac", "Italiano"), ("h264", "Picture")]);
        // the blocks: the one that was track 2 is now track 1
        let segment = find_segment_body(&out) as usize;
        let mut block_tracks = Vec::new();
        for (id, _, body) in split(&out[segment..]).unwrap() {
            if id == CLUSTER {
                for (child, _, payload) in split(body).unwrap() {
                    if child == SIMPLE_BLOCK {
                        block_tracks.push(payload[0] & 0x7F);
                    }
                }
            }
        }
        assert_eq!(block_tracks, [2, 1], "the blocks keep their order, with new numbers");
        let _ = std::fs::remove_file(&path);
    }

    /// Reorder the real files named in MAU_EBML_REORDER_CHECK (separated by `:`) on copies in the temporary
    /// folder, and check with mkvmerge: the new order, and that the file still remuxes with no error.
    #[test]
    fn the_reorder_is_right_on_the_files_named_in_mau_ebml_reorder_check() {
        let Ok(list) = std::env::var("MAU_EBML_REORDER_CHECK") else { return };
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let source = Path::new(file);
            let tracks = crate::ebml::read_file_tracks(source).unwrap();
            // the last audio track first, then the rest in their order
            let mut numbers: Vec<u64> = tracks.iter().map(|t| t.id as u64 + 1).collect();
            let last_audio = tracks.iter().rev().find(|t| t.kind == crate::track::Kind::Audio).map(|t| t.id as u64 + 1);
            if let Some(last) = last_audio {
                numbers.retain(|n| *n != last);
                numbers.insert(0, last);
            }
            let copy = std::env::temp_dir().join(format!("mau-ebml-reorder-real-{}.mkv", std::process::id()));
            std::fs::copy(source, &copy).unwrap();
            reorder_tracks_in_place(&copy, &numbers).unwrap_or_else(|e| panic!("{file}: {e}"));
            let reread = crate::ebml::read_file_tracks(&copy).unwrap();
            let expected: Vec<i64> = numbers.iter().map(|n| *n as i64 - 1).collect();
            let got: Vec<i64> = reread.iter().map(|t| t.id).collect();
            let _ = got;
            let names: Vec<&str> = reread.iter().map(|t| t.codec.as_str()).collect();
            let want: Vec<&str> = numbers.iter().map(|n| tracks.iter().find(|t| t.id as u64 + 1 == *n).unwrap().codec.as_str()).collect();
            assert_eq!(names, want, "{file}: the order of the tracks");
            let _ = expected;
            // mkvmerge reads the result, and remuxes it with no error
            let out = std::process::Command::new("mkvmerge").arg("-J").arg(&copy).output();
            if let Ok(out) = out {
                let data: serde_json::Value = serde_json::from_slice(&out.stdout).expect("mkvmerge reads the result");
                let mkv_codecs: Vec<String> = data["tracks"].as_array().unwrap().iter()
                    .filter(|t| matches!(t["type"].as_str(), Some("video" | "audio" | "subtitles")))
                    .map(|t| t["codec"].as_str().unwrap_or("").to_string()).collect();
                assert_eq!(mkv_codecs.len(), numbers.len(), "{file}: mkvmerge's track count");
            }
            let remux = std::env::temp_dir().join(format!("mau-ebml-reorder-remux-{}.mkv", std::process::id()));
            let status = std::process::Command::new("mkvmerge").arg("-o").arg(&remux).arg(&copy).status();
            if let Ok(status) = status {
                assert!(status.code() == Some(0) || status.code() == Some(1), "{file}: mkvmerge remuxes the result");
            }
            let _ = std::fs::remove_file(&remux);
            let _ = std::fs::remove_file(&copy);
        }
    }

    /// Reorder the tracks of a file that has a CRC-32 in its Tracks element, and check the CRC-32 still
    /// matches the element (it is worked out again after the entries move). Uses MAU_EBML_CRC_CHECK, a
    /// file list like the others. Its stored value was first checked against zlib's CRC-32 by hand.
    #[test]
    fn the_crc_of_the_tracks_element_matches_after_a_reorder() {
        let Ok(list) = std::env::var("MAU_EBML_CRC_CHECK") else { return };
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let source = Path::new(file);
            let tracks = crate::ebml::read_file_tracks(source).unwrap();
            assert!(tracks.len() >= 2, "{file}: needs two tracks to reorder");
            let mut numbers: Vec<u64> = tracks.iter().map(|t| t.id as u64 + 1).collect();
            numbers.reverse();
            let copy = std::env::temp_dir().join(format!("mau-ebml-crc-real-{}.mkv", std::process::id()));
            std::fs::copy(source, &copy).unwrap();
            reorder_tracks_in_place(&copy, &numbers).unwrap_or_else(|e| panic!("{file}: {e}"));
            let mut f = File::open(&copy).unwrap();
            let len = f.metadata().unwrap().len();
            let segment = segment_header(&mut f).unwrap();
            let tops = top_level(&mut f, &segment, len).unwrap();
            let element = tops.iter().find(|t| t.id == TRACKS).unwrap();
            let payload = read_at(&mut f, element.start + element.header_len, element.size.unwrap()).unwrap();
            let _ = std::fs::remove_file(&copy);
            assert!(payload.starts_with(&[0xBF, 0x84]), "{file}: the CRC-32 is still first in the Tracks element");
            let stored = u32::from_le_bytes(payload[2..6].try_into().unwrap());
            assert_eq!(stored, crc32(&payload[6..]), "{file}: the CRC-32 of the Tracks element after a reorder");
        }
    }

    /// The cue cluster position of the first cue point in a file, from its Cues element.
    /// The position of the first Cluster, from the Segment's body.
    fn first_cluster_of(bytes: &[u8]) -> u64 {
        let segment = find_segment_body(bytes) as usize;
        let mut at = segment;
        loop {
            let (id, id_len) = read_id(&bytes[at..]).unwrap();
            let (size, size_len) = read_size(&bytes[at + id_len..]).unwrap();
            if id == CLUSTER {
                return (at - segment) as u64;
            }
            at += id_len + size_len + size.unwrap() as usize;
        }
    }

    fn first_cue_cluster(bytes: &[u8]) -> u64 {
        let segment = find_segment_body(bytes) as usize;
        for (id, _, body) in split(&bytes[segment..]).unwrap() {
            if id == CUES_ID {
                let point = split(body).unwrap().into_iter().find(|(i, _, _)| *i == CUE_POINT).unwrap().2;
                let positions = split(point).unwrap().into_iter().find(|(i, _, _)| *i == CUE_TRACK_POSITIONS).unwrap().2;
                let value = split(positions).unwrap().into_iter().find(|(i, _, _)| *i == CUE_CLUSTER_POSITION).unwrap().2;
                return uint(value);
            }
        }
        panic!("no cues")
    }

    /// The value of the segment's size, which the writer keeps in its 8 byte form.
    fn segment_size_value(bytes: &[u8]) -> u64 {
        let (_, id_len) = read_id(bytes).unwrap();
        let (size, size_len) = read_size(&bytes[id_len..]).unwrap();
        let at = (id_len + size_len) as u64 + size.unwrap();
        let (_, seg_id_len) = read_id(&bytes[at as usize..]).unwrap();
        let (value, _) = read_size(&bytes[at as usize + seg_id_len..]).unwrap();
        value.unwrap()
    }

    #[test]
    fn removing_a_track_takes_its_blocks_and_cues_and_moves_the_index_to_match() {
        let bytes = two_track_file();
        let path = std::env::temp_dir().join(format!("mau-ebml-remove-{}.mkv", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let dest = std::env::temp_dir().join(format!("mau-ebml-remove-dest-{}.mkv", std::process::id()));
        remove_tracks(&path, &dest, &[1], None).unwrap();
        let out = std::fs::read(&dest).unwrap();
        let tracks = read_file_tracks(&dest).unwrap();
        let names: Vec<(&str, &str, i64)> = tracks.iter().map(|t| (t.codec.as_str(), t.name.as_str(), t.id)).collect();
        assert_eq!(names, [("aac", "Italiano", 0)], "one track, numbered from 1");
        let segment = find_segment_body(&out) as usize;
        let mut block_tracks = Vec::new();
        for (id, _, body) in split(&out[segment..]).unwrap() {
            if id == CLUSTER {
                for (child, _, payload) in split(body).unwrap() {
                    if child == SIMPLE_BLOCK {
                        block_tracks.push(payload[0] & 0x7F);
                    }
                }
            }
        }
        assert_eq!(block_tracks, [1], "the block of the kept track is now track 1");
        let (cluster_now, cues_now) = layout_of(&out);
        assert_eq!(seek_position(&out, CLUSTER), cluster_now, "the seek index points at the cluster");
        assert_eq!(seek_position(&out, CUES_ID), cues_now, "the seek index points at the cues");
        assert_eq!(first_cue_cluster(&out), cluster_now, "the cue points at the cluster");
        assert_eq!(segment_size_value(&out), out.len() as u64 - find_segment_body(&out), "the segment size is right");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&dest);
    }

    /// A rewrite that is told to stop part way gives an `Interrupted` error, and the source is not changed. The same
    /// rewrite without the stop succeeds (so the stop, not something else, is what failed it).
    #[test]
    fn a_rewrite_told_to_stop_is_interrupted_and_the_source_is_unchanged() {
        let dir = std::env::temp_dir().join(format!("mau-ebml-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("a.mkv");
        let dest = dir.join("out.mkv");
        std::fs::write(&source, two_track_file()).unwrap();
        let before = std::fs::read(&source).unwrap();
        let edit = TrackEdit { number: 2, default: Some(true), forced: None, language: None, name: None };
        let error = rewrite_tracks_stoppable(&source, &dest, std::slice::from_ref(&edit), &|| true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(std::fs::read(&source).unwrap(), before, "the source is not changed");
        rewrite_tracks_stoppable(&source, &dest, std::slice::from_ref(&edit), &|| false).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A subtitle track added to a file with a seek index and cues: the new track reads back with its language,
    /// forced flag and cues, the old tracks are as they were, and the seek index, cues and segment size point at
    /// the right places.
    #[test]
    fn a_subtitle_track_is_added_with_its_cues_and_the_index_still_points_right() {
        let source = std::env::temp_dir().join(format!("mau-ebml-add-src-{}.mkv", std::process::id()));
        let dest = std::env::temp_dir().join(format!("mau-ebml-add-{}.mkv", std::process::id()));
        std::fs::write(&source, two_track_file()).unwrap();
        let track = NewSubtitleTrack {
            language: "eng".to_string(),
            name: "Forced".to_string(),
            forced: true,
            cues: vec![
                NewCue { start_ms: 0, end_ms: 1000, text: b"hello".to_vec(), additions: vec![] },
                NewCue { start_ms: 2000, end_ms: 3500, text: b"world\nline two".to_vec(), additions: vec![] },
            ],
            codec: "S_TEXT/UTF8",
            private: None,
        };
        add_subtitle_tracks(&source, &dest, &[track]).unwrap();
        let out = std::fs::read(&dest).unwrap();

        let tracks = read_file_tracks(&dest).unwrap();
        assert_eq!(tracks.len(), 3, "the two tracks and the new one");
        let new = &tracks[2];
        assert_eq!(new.kind, crate::track::Kind::Subtitles);
        assert_eq!(new.language, "eng");
        assert!(new.forced, "the forced flag is set");
        assert!(!new.default, "the new track is not the default");
        assert_eq!(new.name, "Forced");
        assert_eq!(new.codec, "subrip");
        let subtitles = crate::ebml_subs::read_subtitles(&dest, 3).unwrap();
        let cues: Vec<(i64, i64, &[u8])> = subtitles.cues.iter().map(|c| (c.start_ms, c.end_ms, &c.payload[..])).collect();
        assert_eq!(cues, [(0, 1000, &b"hello"[..]), (2000, 3500, &b"world\nline two"[..])]);

        let first_cluster = first_cluster_of(&out);
        assert_eq!(seek_position(&out, CLUSTER), first_cluster, "the seek index points at the first cluster");
        assert_eq!(first_cue_cluster(&out), first_cluster, "the cue points at the first cluster");
        assert_eq!(seek_position(&out, CUES_ID), first_cluster + 26, "the seek index points at the cues (moved with the tracks)");
        assert_eq!(segment_size_value(&out), out.len() as u64 - find_segment_body(&out), "the segment size is right");
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&dest);
    }

    /// Each real file named in MAU_SUBS_ADD_CHECK (separated by `:`) gets a copy of one of its SubRip tracks, as
    /// a new track, on a copy of the file. `mkvmerge` must read the result and see the new track with its language
    /// and name, `mkvextract` must write the new track as the SRT it was made from, and `mkvmerge` must remux it
    /// with no error. Files with no SubRip track are reported and skipped.
    #[test]
    fn the_added_subtitles_are_read_back_as_they_were_written_on_the_files_named_in_mau_subs_add_check() {
        let Ok(list) = std::env::var("MAU_SUBS_ADD_CHECK") else { return };
        let dir = std::env::temp_dir().join(format!("mau-subs-add-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut failures = Vec::new();
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let source = Path::new(file);
            let tracks = read_file_tracks(source).unwrap();
            let Some(srt_track) = tracks.iter().find(|t| t.codec == "subrip") else {
                eprintln!("{file}: no SubRip track, not checked");
                continue;
            };
            let srt = dir.join("source.srt");
            let status = std::process::Command::new("mkvextract")
                .arg("tracks").arg(source).arg(format!("{}:{}", srt_track.id, srt.display()))
                .output().expect("mkvextract runs");
            assert!(status.status.success(), "{file}: mkvextract failed");
            let cues = crate::ebml_subs::read_srt(&srt).unwrap();
            let copy = dir.join("added.mkv");
            let track = NewSubtitleTrack {
                language: "fre".to_string(),
                name: "Added".to_string(),
                forced: false,
                cues,
                codec: "S_TEXT/UTF8",
                private: None,
            };
            add_subtitle_tracks(source, &copy, &[track]).unwrap_or_else(|e| panic!("{file}: {e}"));

            let read = read_file_tracks(&copy).unwrap();
            let new = read.last().unwrap();
            let mut problems = Vec::new();
            if read.len() != tracks.len() + 1 { problems.push(format!("{} tracks, expected {}", read.len(), tracks.len() + 1)); }
            if new.language != "fre" || new.name != "Added" || new.forced || new.default {
                problems.push(format!("the new track is {} {:?} forced={} default={}", new.language, new.name, new.forced, new.default));
            }
            let reference = dir.join("again.srt");
            let status = std::process::Command::new("mkvextract")
                .arg("tracks").arg(&copy).arg(format!("{}:{}", new.id, reference.display()))
                .output().expect("mkvextract runs");
            let want = std::fs::read(&srt).unwrap();
            let got = std::fs::read(&reference).unwrap_or_default();
            if !status.status.success() || got != want {
                problems.push(format!("mkvextract wrote {} bytes for the new track, the source was {} bytes", got.len(), want.len()));
            }
            let remux = dir.join("remux.mkv");
            let status = std::process::Command::new("mkvmerge").arg("-o").arg(&remux).arg(&copy).output().expect("mkvmerge runs");
            // mkvmerge's own warnings count here too (a missing TrackUID, for one)
            let stderr = String::from_utf8_lossy(&status.stderr);
            if status.status.code() != Some(0) || stderr.contains("Warning") || stderr.contains("rror") {
                problems.push(format!("mkvmerge remux exit {:?}: {stderr}", status.status.code()));
            }
            if problems.is_empty() {
                eprintln!("{file}: a SubRip track of {} cues added and read back as written", crate::ebml_subs::read_srt(&srt).unwrap().len());
            } else {
                failures.push(format!("{file}: {}", problems.join("; ")));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Remove the first subtitle track of each real file named in MAU_EBML_REMOVE_CHECK (separated by `:`),
    /// writing the result to a temporary file, and check it with mkvmerge: the tracks that are left, in
    /// order and renumbered, the attachments and chapters, and that mkvmerge remuxes it without an error.
    #[test]
    fn the_removal_is_right_on_the_files_named_in_mau_ebml_remove_check() {
        let Ok(list) = std::env::var("MAU_EBML_REMOVE_CHECK") else { return };
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let source = Path::new(file);
            let (before, attachments_before, chapters_before) = mkvmerge_tracks(source).expect("mkvmerge reads the source");
            let tracks = crate::ebml::read_file_tracks(source).unwrap();
            // the first subtitle track, or else the first audio track (even the only one: refusing to remove
            // the last audio track is the engine's rule, not the writer's, and needs --force)
            let subtitle = tracks.iter().find(|t| t.kind == crate::track::Kind::Subtitles).or_else(|| {
                tracks.iter().find(|t| t.kind == crate::track::Kind::Audio)
            });
            let Some(subtitle) = subtitle else {
                eprintln!("{file}: no subtitle or audio track, not checked");
                continue;
            };
            let number = subtitle.id as u64 + 1;
            let dest = std::env::temp_dir().join(format!("mau-ebml-remove-real-{}.mkv", std::process::id()));
            remove_tracks(source, &dest, &[number], None).unwrap_or_else(|e| panic!("{file}: {e}"));
            let mut expected = before.clone();
            let index = tracks.iter().position(|t| t.id == subtitle.id).unwrap();
            expected.remove(index);
            let (after, attachments_after, chapters_after) = mkvmerge_tracks(&dest).expect("mkvmerge reads the result");
            let status = std::process::Command::new("mkvmerge").arg("-o").arg(std::env::temp_dir().join("mau-ebml-remux-check.mkv")).arg(&dest).output().unwrap();
            let _ = std::fs::remove_file(&dest);
            assert!(status.status.code() == Some(0) || status.status.code() == Some(1), "{file}: mkvmerge remuxes the result");
            assert!(!String::from_utf8_lossy(&status.stderr).contains("rror"), "{file}: {}", String::from_utf8_lossy(&status.stderr));
            assert_eq!(after, expected, "{file}: the tracks left");
            assert_eq!(attachments_after, attachments_before, "{file}: attachments");
            assert_eq!(chapters_after, chapters_before, "{file}: chapters");
        }
    }
}
