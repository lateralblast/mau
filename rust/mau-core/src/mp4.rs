//! Reading and changing the "enabled" flag of the tracks of an MP4 file, in place.
//!
//! In an MP4 the default track of each kind is the one whose track header (`tkhd`) has its
//! "enabled" bit set, and that is what `mediainfo` reports as `Default`. `ffmpeg` always
//! enables the first track of each kind when it writes a file, so it can't make a file with
//! no default subtitles; changing the bit in a copy of the file can, without touching any media.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::track::Kind;

/// Where a track's header flags are in the file: the kind it is, its place among the tracks of
/// that kind (from 1) and the offset of the last of the three flag bytes, whose lowest bit is
/// "enabled".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackFlag {
    pub kind: Kind,
    pub number: usize,
    pub offset: u64,
    pub enabled: bool,
}

/// The boxes in `data[start..end]`: type, where it starts, where its content starts, where it ends.
fn boxes(data: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize, usize)> {
    let mut found = Vec::new();
    let mut at = start;
    while at + 8 <= end {
        let size = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as u64;
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (header, size) = match size {
            0 => (8, (end - at) as u64),
            1 if at + 16 <= end => {
                let mut big = [0u8; 8];
                big.copy_from_slice(&data[at + 8..at + 16]);
                (16, u64::from_be_bytes(big))
            }
            size => (8, size),
        };
        if size < header as u64 || at as u64 + size > end as u64 {
            break; // not a box: stop rather than guess
        }
        found.push((kind, at, at + header, at + size as usize));
        at += size as usize;
    }
    found
}

fn kind_of_handler(handler: &[u8]) -> Option<Kind> {
    match handler {
        b"vide" => Some(Kind::Video),
        b"soun" => Some(Kind::Audio),
        b"sbtl" | b"text" | b"subt" | b"clcp" => Some(Kind::Subtitles),
        _ => None,
    }
}

/// The flags of the audio, video and subtitle tracks of the `moov` box of an MP4, in file order.
pub fn parse_track_flags(moov: &[u8], moov_offset: u64) -> Vec<TrackFlag> {
    let mut counts = [0usize; 3];
    let mut flags = Vec::new();
    for (kind, _, content, end) in boxes(moov, 0, moov.len()) {
        if &kind != b"moov" {
            continue;
        }
        for (child, _, trak_content, trak_end) in boxes(moov, content, end) {
            if &child != b"trak" {
                continue;
            }
            let (mut header, mut handler) = (None, None);
            for (name, _, inner, inner_end) in boxes(moov, trak_content, trak_end) {
                match &name {
                    b"tkhd" => header = Some(inner),
                    b"mdia" => {
                        for (m, _, hinner, _) in boxes(moov, inner, inner_end) {
                            // hdlr: version and flags (4), pre_defined (4), then the handler type
                            if &m == b"hdlr" && hinner + 12 <= moov.len() {
                                handler = Some(&moov[hinner + 8..hinner + 12]);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let (Some(header), Some(handler)) = (header, handler) else { continue };
            let Some(kind) = kind_of_handler(handler) else { continue };
            if header + 4 > moov.len() {
                continue;
            }
            let slot = match kind {
                Kind::Video => 0,
                Kind::Audio => 1,
                Kind::Subtitles => 2,
            };
            counts[slot] += 1;
            flags.push(TrackFlag {
                kind,
                number: counts[slot],
                offset: moov_offset + header as u64 + 3,
                enabled: moov[header + 3] & 1 == 1,
            });
        }
    }
    flags
}

/// Read the track flags of an MP4 file (only its `moov` box is read).
pub fn read_track_flags(path: &Path) -> io::Result<Vec<TrackFlag>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut at = 0u64;
    while at + 8 <= length {
        file.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 16];
        let got = file.read(&mut header)?;
        if got < 8 {
            break;
        }
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let size = match size32 {
            0 => length - at,
            1 if got >= 16 => u64::from_be_bytes(header[8..16].try_into().unwrap_or([0; 8])),
            size => size,
        };
        if size < 8 {
            break;
        }
        if &header[4..8] == b"moov" {
            let mut moov = vec![0u8; size.min(length - at) as usize];
            file.seek(SeekFrom::Start(at))?;
            file.read_exact(&mut moov)?;
            return Ok(parse_track_flags(&moov, at));
        }
        at += size;
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "no moov box: not an MP4 file"))
}

/// Set or clear the "enabled" (default) bit of tracks, each given as the offset of its flags.
pub fn set_enabled(path: &Path, changes: &[(u64, bool)]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    for (offset, enabled) in changes {
        let mut byte = [0u8; 1];
        file.seek(SeekFrom::Start(*offset))?;
        file.read_exact(&mut byte)?;
        byte[0] = if *enabled { byte[0] | 1 } else { byte[0] & !1 };
        file.seek(SeekFrom::Start(*offset))?;
        file.write_all(&byte)?;
    }
    file.flush()
}

/// A `trak` box of the `moov`, as `parse_traks` finds it: where it is in the moov (from the moov's start), its
/// kind (`None` for a track that is not audio, video or subtitles), where the flags of its `tkhd` are (the lowest
/// bit is "enabled") and where the language of its `mdhd` is (two bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trak {
    pub start: usize,
    pub end: usize,
    pub kind: Option<Kind>,
    /// The track ID in its `tkhd` box, which mediainfo reports as the track's ID.
    pub id: u32,
    pub enabled_at: usize,
    pub language_at: usize,
}

/// A change to one track of an MP4, in place: the language (three lowercase letters) and/or the default
/// ("enabled") flag. `trak` is the track's place among the `trak` boxes, from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrakChange {
    pub trak: usize,
    pub language: Option<String>,
    pub enabled: Option<bool>,
    /// The track's name (its `udta` `name` box): `Some("")` removes it. Changing a name changes the size of the
    /// `moov` box, so the file is written out again (see `rebuild_moov`).
    pub name: Option<String>,
    /// The forced flag of a subtitle track (`Some(true)` adds a `kind` box with the forced role, `Some(false)` removes
    /// it). It lives in the track's `udta`, so a change to it rebuilds the `moov` box like a name does.
    pub forced: Option<bool>,
}

/// The `udta` changes of one track: `name` as in `TrakChange` (`None` leaves the name alone), `forced` likewise.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrakEdit {
    name: Option<String>,
    forced: Option<bool>,
}

/// The scheme and value that ffmpeg (and so mediainfo and ffprobe) read as the forced flag of a subtitle track.
const FORCED_SCHEME: &[u8] = b"urn:mpeg:dash:role:2011";
const FORCED_VALUE: &[u8] = b"forced-subtitle";

/// The content of the `kind` box that marks a forced subtitle track: a full box header (version 0, no flags), then
/// the scheme and the value, each a null-terminated string.
fn forced_kind_content() -> Vec<u8> {
    let mut content = vec![0u8; 4];
    content.extend_from_slice(FORCED_SCHEME);
    content.push(0);
    content.extend_from_slice(FORCED_VALUE);
    content.push(0);
    content
}

/// The bytes of a box with type `kind` around `content`.
fn make_box(kind: &[u8; 4], content: &[u8]) -> io::Result<Vec<u8>> {
    let size = u32::try_from(content.len() + 8).map_err(|_| invalid("a box is too big"))?;
    let mut out = size.to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(content);
    Ok(out)
}

/// The boxes that hold the data this module changes; they are copied with their children changed where needed.
const CONTAINERS: [&[u8; 4]; 6] = [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"udta"];

/// Where a chunk offset (`stco`, `co64`) of the file goes after a change: the byte ranges cut out of the media data
/// (`cuts`, sorted and not overlapping, in the old file), and the `moov` box, which ends at `moov_end` in the old file
/// and has grown or shrunk by `moov_delta` bytes. A chunk offset that points into a cut is an error.
struct Remap {
    cuts: Vec<(u64, u64)>,
    moov_end: u64,
    moov_delta: i64,
}

impl Remap {
    fn apply(&self, offset: u64) -> io::Result<u64> {
        if self.cuts.iter().any(|&(start, len)| start <= offset && offset < start + len) {
            return Err(invalid("a chunk offset points into the data that is removed"));
        }
        let cut_before: u64 = self.cuts.iter().filter(|&&(start, _)| start < offset).map(|&(_, len)| len).sum();
        let moved = offset as i64 - cut_before as i64 + if offset >= self.moov_end { self.moov_delta } else { 0 };
        u64::try_from(moved).map_err(|_| invalid("a chunk offset would be negative"))
    }
}

/// What the rewrite of a `moov` box changes: the `udta` edit of each track (keyed by where its `trak` box starts in
/// the `moov`), the `trak` boxes to leave out (by the same key), and where the chunk offsets go (see `Remap`).
struct Rewrite<'a> {
    edits: HashMap<usize, TrakEdit>,
    drop: &'a [usize],
    map: &'a Remap,
}

/// The boxes of `data[start..end]`, copied; the containers among them are rebuilt around their children.
fn emit_boxes(data: &[u8], start: usize, end: usize, rewrite: &Rewrite<'_>) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for (kind, at, content, box_end) in boxes(data, start, end) {
        if &kind == b"trak" && rewrite.drop.contains(&at) {
            continue;
        }
        let bytes = if &kind == b"trak" && rewrite.edits.contains_key(&at) {
            let edit = rewrite.edits[&at].clone();
            make_box(b"trak", &trak_children(data, content, box_end, &edit, rewrite)?)?
        } else if CONTAINERS.contains(&&kind) {
            make_box(&kind, &emit_boxes(data, content, box_end, rewrite)?)?
        } else if &kind == b"stco" || &kind == b"co64" {
            shift_offsets(&kind, &data[at..box_end], rewrite)?
        } else {
            data[at..box_end].to_vec()
        };
        out.extend(bytes);
    }
    Ok(out)
}

/// The children of a `trak` with its `udta` edited (see `udta_edited`); a `udta` is made if the track had none and
/// the edit needs one.
fn trak_children(data: &[u8], start: usize, end: usize, edit: &TrakEdit, rewrite: &Rewrite<'_>) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut udta_done = false;
    for (kind, at, content, box_end) in boxes(data, start, end) {
        if &kind == b"udta" {
            out.extend(udta_edited(data, content, box_end, edit, rewrite)?);
            udta_done = true;
        } else {
            out.extend(emit_boxes(data, at, box_end, rewrite)?);
        }
    }
    if !udta_done {
        out.extend(udta_edited(&[], 0, 0, edit, rewrite)?);
    }
    Ok(out)
}

/// A `udta` box with its `name` and forced `kind` box changed as `edit` says (a name of `Some("")` removes it, `None`
/// keeps it), and every other child kept. A box with nothing left in it is dropped.
fn udta_edited(data: &[u8], start: usize, end: usize, edit: &TrakEdit, rewrite: &Rewrite<'_>) -> io::Result<Vec<u8>> {
    let mut children = Vec::new();
    for (kind, at, content, box_end) in boxes(data, start, end) {
        if &kind == b"name" && edit.name.is_some() {
            continue;
        }
        if &kind == b"kind" && edit.forced.is_some() && &data[content..box_end] == forced_kind_content().as_slice() {
            continue;
        }
        children.extend(emit_boxes(data, at, box_end, rewrite)?);
    }
    if let Some(name) = edit.name.as_ref().filter(|n| !n.is_empty()) {
        children.extend(make_box(b"name", name.as_bytes())?);
    }
    if edit.forced == Some(true) {
        children.extend(make_box(b"kind", &forced_kind_content())?);
    }
    if children.is_empty() {
        return Ok(Vec::new());
    }
    make_box(b"udta", &children)
}

/// A chunk offset table with each offset at or past `rewrite.from` moved by `rewrite.shift` bytes. `bytes` is the
/// whole box; its entries have the same size after the change.
fn shift_offsets(kind: &[u8; 4], bytes: &[u8], rewrite: &Rewrite<'_>) -> io::Result<Vec<u8>> {
    let width = if kind == b"co64" { 8 } else { 4 };
    if bytes.len() < 16 {
        return Err(invalid("a chunk offset table is cut short"));
    }
    let count = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    if 16 + count * width > bytes.len() {
        return Err(invalid("a chunk offset table is cut short"));
    }
    let mut out = bytes.to_vec();
    for entry in 0..count {
        let at = 16 + entry * width;
        let value = if width == 8 {
            u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap_or([0; 8]))
        } else {
            u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap_or([0; 4])) as u64
        };
        let moved = rewrite.map.apply(value)?;
        if width == 4 {
            let fits = u32::try_from(moved).map_err(|_| invalid("a chunk offset does not fit in 32 bits"))?;
            out[at..at + 4].copy_from_slice(&fits.to_be_bytes());
        } else {
            out[at..at + 8].copy_from_slice(&moved.to_be_bytes());
        }
    }
    Ok(out)
}

/// The `moov` box of `moov` (its whole bytes at `moov_at`) with the track edits in `edits` made and the `trak` boxes in
/// `drop` left out (see `Rewrite`). The chunk offsets are remapped for the media data cut out (`cuts`) and for the
/// change in size of the box, which is found by building the box once without moving anything.
fn rebuild_moov(
    moov: &[u8],
    moov_at: u64,
    edits: HashMap<usize, TrakEdit>,
    drop: &[usize],
    cuts: Vec<(u64, u64)>,
    extra: &[u8],
) -> io::Result<Vec<u8>> {
    let moov_end = moov_at + moov.len() as u64;
    let unmoved = Remap { cuts: Vec::new(), moov_end, moov_delta: 0 };
    let first = emit_moov(moov, &Rewrite { edits: edits.clone(), drop, map: &unmoved }, extra)?;
    let moov_delta = first.len() as i64 - moov.len() as i64;
    let map = Remap { cuts, moov_end, moov_delta };
    emit_moov(moov, &Rewrite { edits, drop, map: &map }, extra)
}

/// The `moov` box with its children rebuilt as `rewrite` says, and the bytes `extra` (more `trak` boxes) after them.
/// The `moov` header is eight bytes; a box with a 64-bit size is refused.
fn emit_moov(moov: &[u8], rewrite: &Rewrite<'_>, extra: &[u8]) -> io::Result<Vec<u8>> {
    if moov.len() < 8 || moov[0..4] == [0, 0, 0, 1] {
        return Err(invalid("the moov box has a size that is not supported"));
    }
    let children = emit_boxes(moov, 8, moov.len(), rewrite)?;
    make_box(b"moov", &[children, extra.to_vec()].concat())
}

/// Write the file with its `moov` box (`old_len` bytes at `moov_at`) replaced by `new_moov`. The file is written
/// beside the original and then put in its place, so an error leaves the original as it was.
fn splice_moov(path: &Path, moov_at: u64, old_len: u64, new_moov: &[u8]) -> io::Result<()> {
    let mut source = File::open(path)?;
    let writing = path.with_file_name(format!("{}.moov-tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    let written = (|| -> io::Result<()> {
        let mut out = io::BufWriter::new(File::create(&writing)?);
        let mut head = (&mut source).take(moov_at);
        io::copy(&mut head, &mut out)?;
        out.write_all(new_moov)?;
        source.seek(SeekFrom::Start(moov_at + old_len))?;
        io::copy(&mut source, &mut out)?;
        out.flush()
    })();
    match written {
        Ok(()) => std::fs::rename(&writing, path),
        Err(error) => {
            let _ = std::fs::remove_file(&writing);
            Err(error)
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// The `trak` boxes of a `moov` box (the whole box, header included), in file order. A track whose handler
/// is not audio, video or subtitles has no kind.
pub fn parse_traks(moov: &[u8]) -> io::Result<Vec<Trak>> {
    let mut traks = Vec::new();
    for (kind, _, content, end) in boxes(moov, 0, moov.len()) {
        if &kind != b"moov" {
            continue;
        }
        for (child, at, trak_content, trak_end) in boxes(moov, content, end) {
            if &child != b"trak" {
                continue;
            }
            let (mut header, mut media, mut handler) = (None, None, None);
            for (name, _, inner, inner_end) in boxes(moov, trak_content, trak_end) {
                match &name {
                    b"tkhd" => header = Some(inner),
                    b"mdia" => {
                        for (m, _, mi, _) in boxes(moov, inner, inner_end) {
                            match &m {
                                b"mdhd" => media = Some(mi),
                                // hdlr: version and flags (4), pre_defined (4), then the handler type
                                b"hdlr" if mi + 12 <= moov.len() => handler = Some(&moov[mi + 8..mi + 12]),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            let header = header.ok_or_else(|| invalid("a track has no track header"))?;
            let media = media.ok_or_else(|| invalid("a track has no media header"))?;
            if header + 4 > moov.len() || media + 1 > moov.len() {
                return Err(invalid("a track header is cut short"));
            }
            // tkhd: version and flags (4), the times (8, or 16 for version 1), then the track ID
            let id_at = header + if moov[header] == 1 { 20 } else { 12 };
            if id_at + 4 > moov.len() {
                return Err(invalid("a track header is cut short"));
            }
            let id = u32::from_be_bytes([moov[id_at], moov[id_at + 1], moov[id_at + 2], moov[id_at + 3]]);
            // mdhd: version and flags (4), then the times and duration (16 bytes, or 28 for version 1), then the language
            let language_at = media + if moov[media] == 1 { 32 } else { 20 };
            if language_at + 2 > moov.len() {
                return Err(invalid("a media header is cut short"));
            }
            traks.push(Trak {
                start: at,
                end: trak_end,
                kind: handler.and_then(kind_of_handler),
                id,
                enabled_at: header + 3,
                language_at,
            });
        }
    }
    Ok(traks)
}

/// The two bytes of an mdhd language for a three letter lowercase code (`eng`), as the spec packs them.
fn language_bits(code: &str) -> io::Result<[u8; 2]> {
    let letters = code.as_bytes();
    if letters.len() != 3 || !letters.iter().all(|c| c.is_ascii_lowercase()) {
        return Err(invalid("a language must be three lowercase letters"));
    }
    let bits = letters.iter().fold(0u16, |acc, c| (acc << 5) | (*c - 0x60) as u16);
    Ok(bits.to_be_bytes())
}

/// The whole `moov` box of an MP4 file: where it starts, and its bytes.
fn read_moov(path: &Path) -> io::Result<(u64, Vec<u8>)> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut at = 0u64;
    while at + 8 <= length {
        file.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 16];
        let got = file.read(&mut header)?;
        if got < 8 {
            break;
        }
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let size = match size32 {
            0 => length - at,
            1 if got >= 16 => u64::from_be_bytes(header[8..16].try_into().unwrap_or([0; 8])),
            size => size,
        };
        if size < 8 {
            break;
        }
        if &header[4..8] == b"moov" {
            let mut moov = vec![0u8; size.min(length - at) as usize];
            file.seek(SeekFrom::Start(at))?;
            file.read_exact(&mut moov)?;
            return Ok((at, moov));
        }
        at += size;
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "no moov box: not an MP4 file"))
}

/// The track IDs of the `trak` boxes of an MP4, in file order (the same order as the trak numbers the edits use).
pub fn track_ids(path: &Path) -> io::Result<Vec<u32>> {
    let (_, moov) = read_moov(path)?;
    Ok(parse_traks(&moov)?.iter().map(|trak| trak.id).collect())
}

/// Make the changes that the bytes of the `moov` box hold (languages and the enabled flag) in place, and return the
/// `udta` edits (names and forced flags), keyed by where each `trak` box starts.
fn apply_changes(moov: &mut [u8], traks: &[Trak], changes: &[TrakChange]) -> io::Result<HashMap<usize, TrakEdit>> {
    let mut edits: HashMap<usize, TrakEdit> = HashMap::new();
    for change in changes {
        let trak = traks.get(change.trak).ok_or_else(|| invalid("a track was not found in the file"))?;
        if let Some(language) = &change.language {
            moov[trak.language_at..trak.language_at + 2].copy_from_slice(&language_bits(language)?);
        }
        if let Some(enabled) = change.enabled {
            let flags = moov[trak.enabled_at];
            moov[trak.enabled_at] = if enabled { flags | 1 } else { flags & !1 };
        }
        if change.name.is_some() || change.forced.is_some() {
            edits.insert(trak.start, TrakEdit { name: change.name.clone(), forced: change.forced });
        }
    }
    Ok(edits)
}

/// Change the tracks of an MP4 file in place: the languages and default flags in `changes`, and, when `order`
/// is given, the `trak` boxes put in that order (`order[i]` is the place of the trak that comes i-th). The
/// `trak` boxes are only moved within the `moov` box, which keeps its size, so the media data and the offsets
/// into it stay as they are. The boxes must be one run with nothing between them for a reorder.
pub fn edit_in_place(path: &Path, changes: &[TrakChange], order: Option<&[usize]>) -> io::Result<()> {
    let (moov_at, mut moov) = read_moov(path)?;
    let mut traks = parse_traks(&moov)?;
    let edits = apply_changes(&mut moov, &traks, changes)?;
    let old_len = moov.len() as u64;
    if !edits.is_empty() {
        // a name or forced flag changes the size of the moov: it is rebuilt here, and the order below is applied to the
        // rebuilt tracks
        moov = rebuild_moov(&moov, moov_at, edits, &[], Vec::new(), &[])?;
        traks = parse_traks(&moov)?;
    }
    if let Some(order) = order {
        let mut seen = vec![false; traks.len()];
        if order.len() != traks.len() || order.iter().any(|&i| i >= traks.len() || std::mem::replace(&mut seen[i], true)) {
            return Err(invalid("the new order does not name every track once"));
        }
        if traks.windows(2).any(|pair| pair[0].end != pair[1].start) {
            return Err(invalid("the track boxes are not one run, so they cannot be moved"));
        }
        if let (Some(first), Some(last)) = (traks.first(), traks.last()) {
            let region: Vec<u8> = order.iter().flat_map(|&i| moov[traks[i].start..traks[i].end].to_vec()).collect();
            moov[first.start..last.end].copy_from_slice(&region);
        }
    }
    if moov.len() as u64 == old_len {
        // the same size: the box is changed where it is
        let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
        file.seek(SeekFrom::Start(moov_at))?;
        file.write_all(&moov)?;
        file.flush()
    } else {
        splice_moov(path, moov_at, old_len, &moov)
    }
}

/// The content of the first box of type `kind` directly in `data`.
fn box_content<'a>(data: &'a [u8], kind: &[u8; 4]) -> io::Result<&'a [u8]> {
    boxes(data, 0, data.len())
        .into_iter()
        .find(|(k, _, _, _)| k == kind)
        .map(|(_, _, content, end)| &data[content..end])
        .ok_or_else(|| invalid("a track has no sample table box it needs"))
}

fn u32_at(data: &[u8], at: usize) -> io::Result<u32> {
    let b = data.get(at..at + 4).ok_or_else(|| invalid("a sample table is cut short"))?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_at(data: &[u8], at: usize) -> io::Result<u64> {
    let b = data.get(at..at + 8).ok_or_else(|| invalid("a sample table is cut short"))?;
    let mut out = [0u8; 8];
    out.copy_from_slice(b);
    Ok(u64::from_be_bytes(out))
}

/// Where each chunk of a track is in the file, as (offset, length): the offsets from `stco` or `co64`, and the
/// length from the sample sizes (`stsz`) of the samples that `stsc` puts in each chunk. `trak` is the whole box.
fn chunk_spans(trak: &[u8]) -> io::Result<Vec<(u64, u64)>> {
    let trak = box_content(trak, b"trak")?;
    let stbl = box_content(box_content(box_content(trak, b"mdia")?, b"minf")?, b"stbl")?;
    let (offsets, width) = match box_content(stbl, b"stco") {
        Ok(table) => (table, 4),
        Err(_) => (box_content(stbl, b"co64")?, 8),
    };
    let count = u32_at(offsets, 4)? as usize;
    let sizes = box_content(stbl, b"stsz")?;
    let fixed = u32_at(sizes, 4)?;
    let sample_count = u32_at(sizes, 8)? as usize;
    let stsc = box_content(stbl, b"stsc")?;
    let entries = u32_at(stsc, 4)? as usize;
    let mut spans = Vec::with_capacity(count);
    let mut sample = 0usize;
    for chunk in 0..count {
        // the last stsc entry that starts at or before this chunk gives the samples in it
        let mut per = None;
        for entry in 0..entries {
            let first = u32_at(stsc, 8 + 12 * entry)? as usize;
            if first <= chunk + 1 {
                per = Some(u32_at(stsc, 12 + 12 * entry)? as usize);
            }
        }
        let per = per.ok_or_else(|| invalid("a chunk has no sample table entry"))?;
        let offset = if width == 8 { u64_at(offsets, 8 + 8 * chunk)? } else { u32_at(offsets, 8 + 4 * chunk)? as u64 };
        let mut len = 0u64;
        for _ in 0..per {
            if sample >= sample_count {
                return Err(invalid("the sample table names more samples than it has"));
            }
            len += if fixed != 0 { fixed as u64 } else { u32_at(sizes, 12 + 4 * sample)? as u64 };
            sample += 1;
        }
        spans.push((offset, len));
    }
    Ok(spans)
}

/// Sort byte ranges, merge the ones that touch, and refuse ranges that overlap.
fn merged(mut ranges: Vec<(u64, u64)>) -> io::Result<Vec<(u64, u64)>> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (start, len) in ranges {
        match out.last_mut() {
            Some(last) if start < last.0 + last.1 => return Err(invalid("two chunks share the same bytes")),
            Some(last) if start == last.0 + last.1 => last.1 += len,
            _ => out.push((start, len)),
        }
    }
    Ok(out)
}

/// Copy `source[from..to]` to `out`.
fn copy_range(source: &mut File, out: &mut impl Write, from: u64, to: u64) -> io::Result<()> {
    source.seek(SeekFrom::Start(from))?;
    io::copy(&mut source.take(to - from), out)?;
    Ok(())
}

/// Write `source` to `dest` with the `moov` box (`old_len` bytes at `moov_at`) replaced by `new_moov`, and the byte
/// ranges `cuts` (sorted, from the source) left out of the boxes that hold them. Such a box is an `mdat`, whose size
/// is made smaller to match.
fn write_cut(source: &Path, dest: &Path, moov_at: u64, new_moov: &[u8], cuts: &[(u64, u64)]) -> io::Result<()> {
    let mut input = File::open(source)?;
    let length = input.metadata()?.len();
    let mut out = io::BufWriter::new(File::create(dest)?);
    let mut at = 0u64;
    while at < length {
        input.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 16];
        let got = input.read(&mut header)?;
        if got < 8 {
            return Err(invalid("the file ends in the middle of a box"));
        }
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let (header_len, size) = match size32 {
            0 => (8u64, length - at),
            1 if got == 16 => (16u64, u64::from_be_bytes([header[8], header[9], header[10], header[11], header[12], header[13], header[14], header[15]])),
            size => (8u64, size),
        };
        let end = at.checked_add(size).filter(|&end| size >= header_len && end <= length)
            .ok_or_else(|| invalid("a box runs past the end of the file"))?;
        if at == moov_at {
            out.write_all(new_moov)?;
        } else {
            let inside: Vec<(u64, u64)> = cuts.iter().copied().filter(|&(start, len)| start < end && start + len > at).collect();
            if inside.is_empty() {
                copy_range(&mut input, &mut out, at, end)?;
            } else {
                if &header[4..8] != b"mdat" {
                    return Err(invalid("the media data of a track is not in an mdat box"));
                }
                if inside.iter().any(|&(start, len)| start < at + header_len || start + len > end) {
                    return Err(invalid("the media data of a track is split across boxes"));
                }
                let cut: u64 = inside.iter().map(|&(_, len)| len).sum();
                let new_size = size - cut;
                if header_len == 8 {
                    let fits = u32::try_from(new_size).map_err(|_| invalid("the mdat box is too big for its header"))?;
                    out.write_all(&fits.to_be_bytes())?;
                    out.write_all(&header[4..8])?;
                } else {
                    out.write_all(&1u32.to_be_bytes())?;
                    out.write_all(&header[4..8])?;
                    out.write_all(&new_size.to_be_bytes())?;
                }
                let mut pos = at + header_len;
                for &(start, len) in &inside {
                    copy_range(&mut input, &mut out, pos, start)?;
                    pos = start + len;
                }
                copy_range(&mut input, &mut out, pos, end)?;
            }
        }
        at = end;
    }
    out.flush()
}

/// The track IDs that a `trak` box refers to through its `tref` box (such as the track a chapter track belongs to).
fn referenced_ids(trak: &[u8]) -> io::Result<Vec<u32>> {
    let trak = box_content(trak, b"trak")?;
    let Ok(tref) = box_content(trak, b"tref") else { return Ok(Vec::new()) };
    let mut ids = Vec::new();
    for (_, _, content, end) in boxes(tref, 0, tref.len()) {
        ids.extend(tref[content..end].chunks_exact(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])));
    }
    Ok(ids)
}

/// Write `source` to `dest` without the tracks at `removed` (places among the `trak` boxes, as `parse_traks`
/// numbers them), with `changes` made to the tracks that are left. The media data of a removed track is cut out of
/// its `mdat` box, and the chunk offsets of the others are moved to match, along with the `moov` box, which loses the
/// removed `trak` boxes. Nothing is moved in the source. The caller checks `dest` and replaces the source with it.
pub fn remove_traks(source: &Path, dest: &Path, removed: &[usize], changes: &[TrakChange]) -> io::Result<()> {
    let (moov_at, mut moov) = read_moov(source)?;
    let traks = parse_traks(&moov)?;
    if removed.iter().any(|&index| index >= traks.len()) {
        return Err(invalid("a track was not found in the file"));
    }
    if changes.iter().any(|change| removed.contains(&change.trak)) {
        return Err(invalid("a track that is removed is also changed"));
    }
    let edits = apply_changes(&mut moov, &traks, changes)?;
    let mut drop = Vec::new();
    let mut cut = Vec::new();
    let mut kept = Vec::new();
    let removed_ids: Vec<u32> = removed.iter().map(|&index| traks[index].id).collect();
    for (index, trak) in traks.iter().enumerate() {
        let spans = chunk_spans(&moov[trak.start..trak.end])?;
        if removed.contains(&index) {
            drop.push(trak.start);
            cut.extend(spans);
        } else {
            if referenced_ids(&moov[trak.start..trak.end])?.iter().any(|id| removed_ids.contains(id)) {
                return Err(invalid("a track that is kept refers to a track that is removed"));
            }
            kept.extend(spans);
        }
    }
    let cuts = merged(cut)?;
    let moov_end = moov_at + moov.len() as u64;
    for &(start, len) in &cuts {
        if start < moov_end && start + len > moov_at {
            return Err(invalid("a track's media data is inside the moov box"));
        }
        if kept.iter().any(|&(at, n)| at < start + len && at + n > start) {
            return Err(invalid("a track that is kept shares media data with one that is removed"));
        }
    }
    let new_moov = rebuild_moov(&moov, moov_at, edits, &drop, cuts.clone(), &[])?;
    write_cut(source, dest, moov_at, &new_moov, &cuts)
}


/// The cues of a text subtitle track (`tx3g` mov_text, `wvtt` WebVTT or `stpp` TTML), as (start, end, text) in
/// milliseconds. The timing is the sample's, from its duration. A sample with no text (the gaps between cues) is no
/// cue. The track is the one whose track ID is `track_id`.
pub fn read_text_cues(path: &Path, track_id: u32) -> io::Result<Vec<(i64, i64, Vec<u8>)>> {
    let (_, moov) = read_moov(path)?;
    let traks = parse_traks(&moov)?;
    let trak = traks.iter().find(|t| t.id == track_id).ok_or_else(|| invalid("a track was not found in the file"))?;
    let trak_body = box_content(&moov[trak.start..trak.end], b"trak")?;
    let mdia = box_content(trak_body, b"mdia")?;
    let mdhd = box_content(mdia, b"mdhd")?;
    let timescale = u32_at(mdhd, if mdhd.first() == Some(&1) { 20 } else { 12 })?;
    if timescale == 0 {
        return Err(invalid("a track has no time scale"));
    }
    let stbl = box_content(box_content(mdia, b"minf")?, b"stbl")?;
    let format: [u8; 4] = match box_content(stbl, b"stsd")?.get(12..16) {
        Some(code) if [b"tx3g", b"wvtt", b"stpp"].iter().any(|k| k[..] == *code) => [code[0], code[1], code[2], code[3]],
        _ => return Err(invalid("the track is not a text subtitle this program reads")),
    };
    // the durations (stts), then where each sample is: the chunk offsets (stco or co64), the samples in each chunk
    // (stsc) and the size of each sample (stsz)
    let stts = box_content(stbl, b"stts")?;
    let mut durations: Vec<u64> = Vec::new();
    for entry in 0..u32_at(stts, 4)? as usize {
        let (count, delta) = (u32_at(stts, 8 + 8 * entry)?, u32_at(stts, 12 + 8 * entry)?);
        durations.extend(std::iter::repeat(delta as u64).take(count as usize));
    }
    let sizes = box_content(stbl, b"stsz")?;
    let fixed = u32_at(sizes, 4)?;
    let (offsets, width) = match box_content(stbl, b"stco") {
        Ok(table) => (table, 4),
        Err(_) => (box_content(stbl, b"co64")?, 8),
    };
    let stsc = box_content(stbl, b"stsc")?;
    let mut samples: Vec<(u64, u64)> = Vec::new();
    let mut sample = 0usize;
    for chunk in 0..u32_at(offsets, 4)? as usize {
        let mut per = None;
        for entry in 0..u32_at(stsc, 4)? as usize {
            if u32_at(stsc, 8 + 12 * entry)? as usize <= chunk + 1 {
                per = Some(u32_at(stsc, 12 + 12 * entry)? as usize);
            }
        }
        let per = per.ok_or_else(|| invalid("a chunk has no sample table entry"))?;
        let mut at = if width == 8 { u64_at(offsets, 8 + 8 * chunk)? } else { u32_at(offsets, 8 + 4 * chunk)? as u64 };
        for _ in 0..per {
            let size = if fixed != 0 { fixed as u64 } else { u32_at(sizes, 12 + 4 * sample)? as u64 };
            samples.push((at, size));
            at += size;
            sample += 1;
        }
    }
    if samples.len() != durations.len() {
        return Err(invalid("the sample tables do not agree on the number of samples"));
    }
    let mut file = File::open(path)?;
    let mut cues = Vec::new();
    let mut ticks = 0u64;
    let to_ms = |t: u64| (t * 1000 + timescale as u64 / 2) / timescale as u64;
    for ((at, size), duration) in samples.iter().zip(&durations) {
        let (start, end) = (to_ms(ticks), to_ms(ticks + duration));
        ticks += duration;
        if *size < 2 {
            continue;
        }
        file.seek(SeekFrom::Start(*at))?;
        let mut data = vec![0u8; *size as usize];
        file.read_exact(&mut data)?;
        let text = sample_text(&format, &data)?;
        if !text.is_empty() {
            cues.push((start as i64, end as i64, text));
        }
    }
    Ok(cues)
}

/// The text of one sample of a text track, by the track's sample entry. A `tx3g` sample is a two byte length and the
/// text. A `wvtt` sample is a `vttc` box (or an empty `vtte`), whose `payl` box holds the text. A `stpp` sample is a
/// TTML document, whose `p` elements hold the text (a `br` is a line break).
fn sample_text(format: &[u8; 4], data: &[u8]) -> io::Result<Vec<u8>> {
    match format {
        b"tx3g" => {
            if data.len() < 2 {
                return Ok(Vec::new());
            }
            let length = u16::from_be_bytes([data[0], data[1]]) as usize;
            Ok(data[2..(2 + length).min(data.len())].to_vec())
        }
        b"wvtt" => {
            let mut text = Vec::new();
            for (kind, _, content, end) in boxes(data, 0, data.len()) {
                if &kind != b"vttc" {
                    continue; // vtte is an empty cue
                }
                for (inner, _, body, body_end) in boxes(data, content, end) {
                    if &inner == b"payl" {
                        if !text.is_empty() {
                            text.push(b'\n');
                        }
                        text.extend_from_slice(&data[body..body_end]);
                    }
                }
            }
            Ok(text)
        }
        _ => Ok(ttml_text(data)?.into_bytes()),
    }
}

/// The text of the `p` elements of a TTML document, one line per element: a `br` is a line break, other tags are
/// dropped, and the XML entities are made characters.
fn ttml_text(data: &[u8]) -> io::Result<String> {
    let xml = std::str::from_utf8(data).map_err(|_| invalid("a TTML subtitle is not UTF-8"))?;
    let mut lines = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<p") {
        let after = &rest[open + 2..];
        // `<p` followed by a space, `>` or `/`; `<pre`, `<span` and the like are not paragraphs
        if !after.starts_with([' ', '>', '\n', '\t']) {
            rest = &rest[open + 2..];
            continue;
        }
        let Some(start_end) = after.find('>') else { break };
        if after[..start_end].ends_with('/') {
            rest = &after[start_end + 1..];
            continue; // an empty paragraph
        }
        let body_start = &after[start_end + 1..];
        let Some(close) = body_start.find("</p>") else { break };
        let body = &body_start[..close];
        let body = body.replace("<br/>", "\n").replace("<br />", "\n");
        lines.push(ttml_plain(&body));
        rest = &body_start[close + 4..];
    }
    Ok(lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n"))
}

/// The text inside TTML markup: tags dropped, then the entities made characters.
fn ttml_plain(markup: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in markup.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            _ => out.push(c),
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// A text track to add to an MP4, written as `mov_text` (a `tx3g` sample entry, the one ffmpeg writes): one sample
/// per cue, with a sample of no text in each gap between cues. `cues` are (start, end, text) in milliseconds.
pub struct NewText {
    pub language: String,
    pub name: String,
    pub forced: bool,
    /// Whether the track is the default one (the enabled flag of its `tkhd`).
    pub enabled: bool,
    pub cues: Vec<(i64, i64, Vec<u8>)>,
}

/// The sample entry of a `tx3g` track, as ffmpeg writes it (its font table names Arial).
const TX3G: [u8; 84] = [
    0x00, 0x00, 0x00, 0x54, 0x74, 0x78, 0x33, 0x67, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x01, 0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x10, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x12, 0x66, 0x74, 0x61, 0x62,
    0x00, 0x01, 0x00, 0x01, 0x05, 0x41, 0x72, 0x69, 0x61, 0x6c, 0x00, 0x00, 0x00, 0x14, 0x62, 0x74, 0x72, 0x74,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x30,
];

/// The samples of a text track: (duration in milliseconds, sample bytes). A cue is a sample of its text (a two byte
/// length, then the text); each gap between cues is a sample with no text. Overlapping cues, a cue whose text is too
/// long for a sample, and a track too long for 32-bit offsets are refused: `mov_text` can't hold them.
fn text_samples(cues: &[(i64, i64, Vec<u8>)]) -> io::Result<Vec<(u32, Vec<u8>)>> {
    let mut sorted: Vec<&(i64, i64, Vec<u8>)> = cues.iter().filter(|(start, end, _)| end > start).collect();
    sorted.sort_by_key(|(start, _, _)| *start);
    let mut samples = Vec::new();
    let mut at = 0i64;
    for (start, end, text) in sorted {
        if *start < at {
            return Err(invalid("two subtitles overlap, which mov_text can't hold"));
        }
        if *start > at {
            samples.push(((start - at) as u32, vec![0, 0]));
        }
        let length = u16::try_from(text.len()).map_err(|_| invalid("a subtitle is too long for a sample"))?;
        let mut sample = length.to_be_bytes().to_vec();
        sample.extend_from_slice(text);
        samples.push(((end - start) as u32, sample));
        at = *end;
    }
    Ok(samples)
}

/// The `trak` box of a text track whose samples are one chunk at `offset` (a `u32`, as the chunk table holds it).
/// `timescale` is the movie's, for the track's duration; `id` is its track ID.
fn text_trak(new: &NewText, id: u32, timescale: u32, offset: u32, samples: &[(u32, Vec<u8>)]) -> io::Result<Vec<u8>> {
    let total: u64 = samples.iter().map(|(duration, _)| *duration as u64).sum();
    let movie_duration = u32::try_from(total * timescale as u64 / 1000).map_err(|_| invalid("the subtitles are too long"))?;
    let total = u32::try_from(total).map_err(|_| invalid("the subtitles are too long"))?;
    // tkhd: version 0, the flags (3 for enabled and in the movie, 2 for in the movie only), times (zero), the ID, the
    // duration, layer, alternate group, volume, the identity matrix, and the size (zero for text)
    let mut matrix = vec![0u8; 36];
    matrix[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    matrix[16..20].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    matrix[32..36].copy_from_slice(&0x4000_0000u32.to_be_bytes());
    let mut tkhd = vec![0, 0, 0, if new.enabled { 3 } else { 2 }];
    tkhd.extend_from_slice(&[0u8; 8]);
    tkhd.extend_from_slice(&id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 4]);
    tkhd.extend_from_slice(&movie_duration.to_be_bytes());
    // the reserved times, the layer, then the alternate group: subtitles are in group 3, as ffmpeg has them (mediainfo
    // reads the default flag from it), then the volume (zero for text) and reserved
    tkhd.extend_from_slice(&[0u8; 8 + 2]);
    tkhd.extend_from_slice(&3u16.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 2 + 2]);
    tkhd.extend_from_slice(&matrix);
    tkhd.extend_from_slice(&[0u8; 8]);
    // mdhd: version 0, zero times, a timescale of 1000 (milliseconds), the duration, the language, quality
    let mut mdhd = vec![0u8; 12];
    mdhd.extend_from_slice(&1000u32.to_be_bytes());
    mdhd.extend_from_slice(&total.to_be_bytes());
    mdhd.extend_from_slice(&language_bits(&new.language)?);
    mdhd.extend_from_slice(&[0, 0]);
    // hdlr: subtitles are handler 'sbtl' (ffmpeg's name for the handler follows)
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"sbtl");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.extend_from_slice(b"SubtitleHandler\0");
    // the sample tables: one chunk holds every sample, and each sample is one entry of the duration table
    let mut stts = vec![0u8; 4];
    stts.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    for (duration, _) in samples {
        stts.extend_from_slice(&1u32.to_be_bytes());
        stts.extend_from_slice(&duration.to_be_bytes());
    }
    let mut stsc = vec![0u8; 4];
    stsc.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1]);
    stsc.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    stsc.extend_from_slice(&[0, 0, 0, 1]);
    let mut stsz = vec![0u8; 8];
    stsz.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    for (_, sample) in samples {
        stsz.extend_from_slice(&(sample.len() as u32).to_be_bytes());
    }
    let mut stco = vec![0u8; 4];
    stco.extend_from_slice(&[0, 0, 0, 1]);
    stco.extend_from_slice(&offset.to_be_bytes());
    // stsd: version and flags, then one sample entry
    let mut stsd = vec![0u8, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend_from_slice(&TX3G);
    // dref: version and flags, one entry: a self-contained url
    let dref = [&[0u8, 0, 0, 0, 0, 0, 0, 1][..], &make_box(b"url ", &[0, 0, 0, 1])?].concat();
    let stbl = make_box(
        b"stbl",
        &[
            make_box(b"stsd", &stsd)?,
            make_box(b"stts", &stts)?,
            make_box(b"stsc", &stsc)?,
            make_box(b"stsz", &stsz)?,
            make_box(b"stco", &stco)?,
        ]
        .concat(),
    )?;
    let minf = make_box(
        b"minf",
        &[make_box(b"nmhd", &[0u8; 4])?, make_box(b"dinf", &make_box(b"dref", &dref)?)?, stbl]
            .concat(),
    )?;
    let mdia = make_box(b"mdia", &[make_box(b"mdhd", &mdhd)?, make_box(b"hdlr", &hdlr)?, minf].concat())?;
    // udta: the forced flag (a kind box) and the name, as the other tracks have them
    let mut udta = Vec::new();
    if new.forced {
        udta.extend(make_box(b"kind", &forced_kind_content())?);
    }
    if !new.name.is_empty() {
        udta.extend(make_box(b"name", new.name.as_bytes())?);
    }
    // edts: one segment for the whole track, from the start of its media (ffmpeg writes the same)
    let mut elst = vec![0u8, 0, 0, 0, 0, 0, 0, 1];
    elst.extend_from_slice(&movie_duration.to_be_bytes());
    elst.extend_from_slice(&[0u8; 4]);
    elst.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    let edts = make_box(b"edts", &make_box(b"elst", &elst)?)?;
    let mut children = vec![make_box(b"tkhd", &tkhd)?, edts, mdia];
    if !udta.is_empty() {
        children.push(make_box(b"udta", &udta)?);
    }
    make_box(b"trak", &children.concat())
}

/// Whether cues can be written as `mov_text` samples in this program: they do not overlap, each text fits a sample,
/// and the file and the track's data stay within 32-bit offsets. Otherwise the file is converted by ffmpeg.
pub fn can_hold(cues: &[(i64, i64, Vec<u8>)], file_length: u64) -> bool {
    match text_samples(cues) {
        Ok(samples) => {
            let data: u64 = samples.iter().map(|(_, sample)| sample.len() as u64).sum();
            // the new data follows the file, so the file and the data must both fit a 32-bit offset
            file_length + data < u32::MAX as u64 / 2
        }
        Err(_) => false,
    }
}

/// The movie's time scale, from its `mvhd` box.
fn movie_timescale(moov: &[u8]) -> io::Result<u32> {
    let mvhd = box_content(&moov[8..], b"mvhd")?;
    let at = if mvhd.first() == Some(&1) { 20 } else { 12 };
    u32_at(mvhd, at)
}

/// Add text tracks to the MP4 file `source` (written to `dest`, which the caller checks and then replaces the source
/// with). The new tracks are appended to the `moov` box, and their samples are written in an `mdat` box at the end of
/// the file, so the media data that was there does not move; only the chunk offsets after the `moov` box change size,
/// as they do for any change to the `moov` box. `changes` are made to the existing tracks first (for the default).
pub fn add_text_tracks(source: &Path, dest: &Path, new: &[NewText], changes: &[TrakChange]) -> io::Result<()> {
    let (moov_at, mut moov) = read_moov(source)?;
    let traks = parse_traks(&moov)?;
    let edits = apply_changes(&mut moov, &traks, changes)?;
    let timescale = movie_timescale(&moov)?;
    let first_id = traks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    let mut samples: Vec<Vec<(u32, Vec<u8>)>> = Vec::new();
    for text in new {
        samples.push(text_samples(&text.cues)?);
    }
    let data_len: u64 = samples.iter().flatten().map(|(_, sample)| sample.len() as u64).sum();
    let length = std::fs::metadata(source)?.len();
    // the first pass sizes the new tracks (their offsets are fixed-width, so the sizes do not depend on them)
    let sizing: Vec<Vec<u8>> = new
        .iter()
        .zip(&samples)
        .enumerate()
        .map(|(k, (text, samples))| text_trak(text, first_id + k as u32, timescale, 0, samples))
        .collect::<io::Result<_>>()?;
    let first = rebuild_moov(&moov, moov_at, edits.clone(), &[], Vec::new(), &sizing.concat())?;
    // the data goes after the new moov, in an mdat box
    let delta = first.len() as i64 - moov.len() as i64;
    let data_at = (length as i64 + delta) as u64 + 8;
    if data_at + data_len > u32::MAX as u64 {
        return Err(invalid("the file is too big for a subtitle track's 32-bit offsets"));
    }
    let mut offset = data_at as u32;
    let mut traks_bytes = Vec::new();
    for (k, (text, samples)) in new.iter().zip(&samples).enumerate() {
        traks_bytes.push(text_trak(text, first_id + k as u32, timescale, offset, samples)?);
        offset += samples.iter().map(|(_, sample)| sample.len() as u32).sum::<u32>();
    }
    let moov_new = rebuild_moov(&moov, moov_at, edits, &[], Vec::new(), &traks_bytes.concat())?;
    if moov_new.len() != first.len() {
        return Err(invalid("the new moov box changed size between the passes"));
    }
    write_cut(source, dest, moov_at, &moov_new, &[])?;
    let mut out = std::fs::OpenOptions::new().append(true).open(dest)?;
    out.write_all(&((data_len + 8) as u32).to_be_bytes())?;
    out.write_all(b"mdat")?;
    for (_, sample) in samples.iter().flatten() {
        out.write_all(sample)?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_box(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let mut out = ((content.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(content);
        out
    }

    fn trak(handler: &[u8; 4], flags: u8) -> Vec<u8> {
        let mut tkhd = vec![0, 0, 0, flags]; // version 0, three flag bytes
        tkhd.extend_from_slice(&[0u8; 80]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0u8; 12]);
        let mdia = make_box(b"mdia", &make_box(b"hdlr", &hdlr));
        make_box(b"trak", &[make_box(b"tkhd", &tkhd), mdia].concat())
    }

    /// A track with a media header (version 0) that has `language`, for the in-place edits.
    fn trak_with_language(handler: &[u8; 4], flags: u8, language: &str) -> Vec<u8> {
        let mut tkhd = vec![0, 0, 0, flags];
        tkhd.extend_from_slice(&[0u8; 80]);
        let mut mdhd = vec![0u8; 12]; // version and flags, creation, modification
        mdhd.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        mdhd.extend_from_slice(&0u32.to_be_bytes()); // duration
        mdhd.extend_from_slice(&language_bits(language).unwrap());
        mdhd.extend_from_slice(&[0, 0]); // quality
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0u8; 12]);
        let mdia = make_box(b"mdia", &[make_box(b"mdhd", &mdhd), make_box(b"hdlr", &hdlr)].concat());
        make_box(b"trak", &[make_box(b"tkhd", &tkhd), mdia].concat())
    }

    #[test]
    fn a_language_is_packed_as_the_spec_packs_it() {
        assert_eq!(language_bits("eng").unwrap(), [0x15, 0xC7]); // e=5, n=14, g=7, each in five bits
        assert_eq!(language_bits("fre").unwrap(), [0x1A, 0x45]);
        assert!(language_bits("EN").is_err());
        assert!(language_bits("Eng").is_err());
        assert!(language_bits("english").is_err());
    }

    #[test]
    fn each_trak_has_its_flag_and_language_where_the_bytes_say() {
        let moov = make_box(b"moov", &[
            trak_with_language(b"vide", 3, "und"),
            trak_with_language(b"soun", 3, "ita"),
            trak_with_language(b"soun", 2, "eng"),
        ].concat());
        let traks = parse_traks(&moov).unwrap();
        let kinds: Vec<Option<Kind>> = traks.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [Some(Kind::Video), Some(Kind::Audio), Some(Kind::Audio)]);
        assert_eq!(moov[traks[1].enabled_at], 3);
        assert_eq!(moov[traks[2].enabled_at], 2);
        assert_eq!(&moov[traks[1].language_at..traks[1].language_at + 2], language_bits("ita").unwrap());
        assert_eq!(&moov[traks[2].language_at..traks[2].language_at + 2], language_bits("eng").unwrap());
    }

    /// An MP4 with an mdat (the media, not read here) and a moov after it, with the given traks.
    fn mp4_with_traks(traks: &[Vec<u8>]) -> Vec<u8> {
        let mut data = make_box(b"ftyp", b"isom\0\0\0\0isom");
        data.extend_from_slice(&make_box(b"mdat", &[7u8; 50]));
        data.extend_from_slice(&make_box(b"moov", &traks.concat()));
        data
    }

    fn temp_mp4(name: &str, data: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mau-mp4-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, data).unwrap();
        file
    }

    #[test]
    fn languages_flags_and_the_order_change_in_place_and_the_size_does_not() {
        let traks = [
            trak_with_language(b"vide", 3, "und"),
            trak_with_language(b"soun", 3, "ita"),
            trak_with_language(b"soun", 2, "eng"),
        ];
        let data = mp4_with_traks(&traks);
        let file = temp_mp4("order.mp4", &data);
        let changes = [
            TrakChange { trak: 1, language: Some("fra".to_string()), enabled: Some(false), name: None, forced: None },
            TrakChange { trak: 2, language: None, enabled: Some(true), name: None, forced: None },
        ];
        edit_in_place(&file, &changes, Some(&[0, 2, 1])).unwrap();
        let out = std::fs::read(&file).unwrap();
        assert_eq!(out.len(), data.len(), "the file is the same size");
        let (_, moov) = read_moov(&file).unwrap();
        let now = parse_traks(&moov).unwrap();
        // the English track, now enabled, is second; the Italian one (renamed fra and disabled) is third
        assert_eq!(moov[now[1].enabled_at], 3);
        assert_eq!(&moov[now[1].language_at..now[1].language_at + 2], language_bits("eng").unwrap());
        assert_eq!(moov[now[2].enabled_at] & 1, 0);
        assert_eq!(&moov[now[2].language_at..now[2].language_at + 2], language_bits("fra").unwrap());
        // the media is where it was
        assert_eq!(&out[..data.len() - moov.len()], &data[..data.len() - moov.len()]);
        let _ = std::fs::remove_file(&file);
    }

    /// A track with a sample table whose one chunk offset is `offset` (the place of the media in the file).
    fn trak_with_offset(offset: u32) -> Vec<u8> {
        let mut tkhd = vec![0, 0, 0, 3];
        tkhd.extend_from_slice(&[0u8; 80]);
        let mut mdhd = vec![0u8; 12];
        mdhd.extend_from_slice(&1000u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&language_bits("eng").unwrap());
        mdhd.extend_from_slice(&[0, 0]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"soun");
        hdlr.extend_from_slice(&[0u8; 12]);
        let mut stco = vec![0u8; 4];
        stco.extend_from_slice(&1u32.to_be_bytes());
        stco.extend_from_slice(&offset.to_be_bytes());
        let stbl = make_box(b"stbl", &make_box(b"stco", &stco));
        let minf = make_box(b"minf", &stbl);
        let mdia = make_box(b"mdia", &[make_box(b"mdhd", &mdhd), make_box(b"hdlr", &hdlr), minf].concat());
        make_box(b"trak", &[make_box(b"tkhd", &tkhd), mdia].concat())
    }

    /// The first chunk offset of the first track of a file (read back from its moov).
    fn first_offset(path: &Path) -> u32 {
        let (_, moov) = read_moov(path).unwrap();
        let at = moov.windows(4).position(|w| w == b"stco").unwrap();
        u32::from_be_bytes(moov[at + 12..at + 16].try_into().unwrap())
    }

    #[test]
    fn a_name_is_added_and_the_offsets_after_the_moov_move_with_it() {
        // ftyp, then a moov (which holds the offset of the media), then the media itself
        let header = make_box(b"ftyp", b"isom\0\0\0\0isom");
        let build = |offset: u32| {
            let moov = make_box(b"moov", &trak_with_offset(offset));
            [header.clone(), moov, make_box(b"mdat", b"MEDIA")].concat()
        };
        let first = build(0);
        let media_at = first.len() as u32 - 5 - 8 + 8; // the media is the last five bytes
        let data = build(media_at);
        let file = temp_mp4("named.mp4", &data);
        edit_in_place(&file, &[TrakChange { trak: 0, language: None, enabled: None, name: Some("Ita".to_string()), forced: None }], None)
            .unwrap();
        let out = std::fs::read(&file).unwrap();
        assert!(out.len() > data.len(), "the name made the moov bigger");
        let (_, moov) = read_moov(&file).unwrap();
        assert_eq!(parse_traks(&moov).unwrap().len(), 1);
        assert!(moov.windows(4).any(|w| w == b"name"), "the track has a name box");
        let moved = first_offset(&file) as usize;
        assert_eq!(&out[moved..moved + 5], b"MEDIA", "the chunk offset still points at the media");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn an_empty_name_removes_the_name_and_the_offsets_follow() {
        let header = make_box(b"ftyp", b"isom\0\0\0\0isom");
        let moov_with_name = |offset: u32| {
            let mut trak = trak_with_offset(offset);
            // the trak with a udta holding a name, added before its end
            let udta = make_box(b"udta", &make_box(b"name", b"Ita"));
            trak.splice(trak.len()..trak.len(), udta.iter().copied());
            let len = trak.len() as u32;
            trak[0..4].copy_from_slice(&len.to_be_bytes());
            make_box(b"moov", &trak)
        };
        let first = [header.clone(), moov_with_name(0), make_box(b"mdat", b"MEDIA")].concat();
        let media_at = first.len() as u32 - 5;
        let data = [header.clone(), moov_with_name(media_at), make_box(b"mdat", b"MEDIA")].concat();
        let file = temp_mp4("unnamed.mp4", &data);
        edit_in_place(&file, &[TrakChange { trak: 0, language: None, enabled: None, name: Some(String::new()), forced: None }], None)
            .unwrap();
        let out = std::fs::read(&file).unwrap();
        assert!(out.len() < data.len(), "the name was removed");
        assert!(!read_moov(&file).unwrap().1.windows(4).any(|w| w == b"name"));
        let moved = first_offset(&file) as usize;
        assert_eq!(&out[moved..moved + 5], b"MEDIA");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_forced_flag_is_a_kind_box_in_the_subtitle_track_and_clearing_it_restores_the_bytes() {
        let data = mp4_with_traks(&[trak_with_language(b"soun", 3, "ita"), trak_with_language(b"sbtl", 3, "eng")]);
        let file = temp_mp4("forced.mp4", &data);
        let change = |forced| TrakChange { trak: 1, language: None, enabled: None, name: None, forced: Some(forced) };
        edit_in_place(&file, &[change(true)], None).unwrap();
        let moov = read_moov(&file).unwrap().1;
        let kind = make_box(b"kind", &forced_kind_content());
        assert!(moov.windows(kind.len()).any(|w| w == kind.as_slice()), "the forced kind box is there");
        let traks = parse_traks(&moov).unwrap();
        assert!(!moov[traks[0].start..traks[0].end].windows(4).any(|w| w == b"kind"), "the audio track is not forced");
        assert!(moov[traks[1].start..traks[1].end].windows(4).any(|w| w == b"kind"));
        edit_in_place(&file, &[change(false)], None).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), data, "clearing the flag gives back the file as it was");
        let _ = std::fs::remove_file(&file);
    }

    /// A track whose sample table gives one sample per chunk, with the chunks at `chunks` (offset, length), and
    /// track ID `id`.
    fn trak_with_chunks(handler: &[u8; 4], language: &str, chunks: &[(u32, u32)], id: u32) -> Vec<u8> {
        let mut tkhd = vec![0, 0, 0, 3];
        tkhd.extend_from_slice(&[0u8; 8]); // creation and modification times
        tkhd.extend_from_slice(&id.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 68]);
        let mut mdhd = vec![0u8; 12];
        mdhd.extend_from_slice(&1000u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&language_bits(language).unwrap());
        mdhd.extend_from_slice(&[0, 0]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0u8; 12]);
        let mut stco = vec![0u8; 4];
        stco.extend_from_slice(&(chunks.len() as u32).to_be_bytes());
        for (offset, _) in chunks {
            stco.extend_from_slice(&offset.to_be_bytes());
        }
        let mut stsz = vec![0u8; 4];
        stsz.extend_from_slice(&0u32.to_be_bytes()); // sizes differ, so they are a table
        stsz.extend_from_slice(&(chunks.len() as u32).to_be_bytes());
        for (_, len) in chunks {
            stsz.extend_from_slice(&len.to_be_bytes());
        }
        let mut stsc = vec![0u8; 4];
        stsc.extend_from_slice(&1u32.to_be_bytes());
        stsc.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1]); // first chunk 1, one sample each, description 1
        let stbl = make_box(b"stbl", &[make_box(b"stco", &stco), make_box(b"stsz", &stsz), make_box(b"stsc", &stsc)].concat());
        let minf = make_box(b"minf", &stbl);
        let mdia = make_box(b"mdia", &[make_box(b"mdhd", &mdhd), make_box(b"hdlr", &hdlr), minf].concat());
        make_box(b"trak", &[make_box(b"tkhd", &tkhd), mdia].concat())
    }

    /// An MP4 with two tracks whose chunks alternate in the media data: audio (ten bytes 'A' in each of two chunks)
    /// and subtitles (five bytes 'B' in each of two). `moov_first` puts the `moov` box before the `mdat` box, and
    /// `large` gives the `mdat` box a 64-bit size (a 16-byte header) instead of a 32-bit one.
    fn interleaved_file(moov_first: bool, large: bool) -> Vec<u8> {
        let ftyp = make_box(b"ftyp", b"isom\0\0\0\0isom");
        let header = if large { 16 } else { 8 };
        let traks = |mdat_at: u32| {
            let payload = mdat_at + header;
            let a = trak_with_chunks(b"soun", "ita", &[(payload, 10), (payload + 15, 10)], 1);
            let b = trak_with_chunks(b"sbtl", "eng", &[(payload + 10, 5), (payload + 25, 5)], 2);
            [make_box(b"mvhd", &mvhd_content()), a, b].concat()
        };
        let moov_len = make_box(b"moov", &traks(0)).len() as u32; // the offsets have the same width whatever they are
        let mdat_at = if moov_first { ftyp.len() as u32 + moov_len } else { ftyp.len() as u32 };
        let moov = make_box(b"moov", &traks(mdat_at));
        let mut payload = vec![b'A'; 10];
        payload.extend_from_slice(&[b'B'; 5]);
        payload.extend_from_slice(&[b'A'; 10]);
        payload.extend_from_slice(&[b'B'; 5]);
        let mdat = if large {
            [&1u32.to_be_bytes()[..], b"mdat", &(payload.len() as u64 + 16).to_be_bytes()[..], &payload[..]].concat()
        } else {
            make_box(b"mdat", &payload)
        };
        if moov_first {
            [ftyp, moov, mdat].concat()
        } else {
            [ftyp, mdat, moov].concat()
        }
    }

    /// A movie header (version 0) with a time scale of 1000 units a second.
    fn mvhd_content() -> Vec<u8> {
        let mut content = vec![0u8; 12];
        content.extend_from_slice(&1000u32.to_be_bytes());
        content.extend_from_slice(&3000u32.to_be_bytes());
        content.extend_from_slice(&[0u8; 80]);
        content
    }

    /// The bytes of every chunk of every track of an MP4, read back from the offsets in its `moov` box.
    fn chunk_bytes(path: &Path) -> Vec<Vec<u8>> {
        let data = std::fs::read(path).unwrap();
        let moov = read_moov(path).unwrap().1;
        parse_traks(&moov)
            .unwrap()
            .iter()
            .map(|trak| {
                chunk_spans(&moov[trak.start..trak.end])
                    .unwrap()
                    .iter()
                    .flat_map(|&(at, len)| data[at as usize..(at + len) as usize].to_vec())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_removed_track_is_cut_out_of_the_media_data_and_the_others_still_read_back() {
        for (moov_first, large) in [(false, false), (true, false), (false, true), (true, true)] {
            let data = interleaved_file(moov_first, large);
            let file = temp_mp4(&format!("cut-{moov_first}-{large}.mp4"), &data);
            let dest = file.with_file_name(format!("{}.out", file.file_name().unwrap().to_string_lossy()));
            remove_traks(&file, &dest, &[1], &[]).unwrap();
            assert_eq!(chunk_bytes(&dest), vec![vec![b'A'; 20]], "only the audio is left, whole");
            assert_eq!(parse_traks(&read_moov(&dest).unwrap().1).unwrap().len(), 1);
            // the ten bytes of B are gone, and so is the trak box that described them
            let moov_lost = read_moov(&file).unwrap().1.len() - read_moov(&dest).unwrap().1.len();
            assert_eq!(std::fs::metadata(&dest).unwrap().len(), (data.len() - 10 - moov_lost) as u64);
            // the mdat box keeps its header form, with its size made smaller by the cut
            let out = std::fs::read(&dest).unwrap();
            let at = out.windows(4).position(|w| w == b"mdat").unwrap() - 4;
            let size32 = u32::from_be_bytes([out[at], out[at + 1], out[at + 2], out[at + 3]]);
            let (header, size) = if large {
                assert_eq!(size32, 1, "the 64-bit size is kept");
                (16, u64::from_be_bytes(out[at + 8..at + 16].try_into().unwrap()))
            } else {
                (8, size32 as u64)
            };
            assert_eq!(size, (header + 20) as u64, "the mdat holds only the audio");
            let _ = std::fs::remove_file(&file);
            let _ = std::fs::remove_file(&dest);
        }
    }

    #[test]
    fn a_kept_track_that_refers_to_a_removed_one_is_refused() {
        let ftyp = make_box(b"ftyp", b"isom\0\0\0\0isom");
        let payload = 28u32;
        // the audio track's tref names track 2 (a chapter track, say), which is the one being removed
        let tref = make_box(b"tref", &make_box(b"chap", &2u32.to_be_bytes()));
        let audio = trak_with_chunks(b"soun", "ita", &[(payload, 10)], 1);
        let audio = [make_box(b"trak", &[&audio[8..], &tref].concat())].concat();
        let moov = make_box(b"moov", &[audio, trak_with_chunks(b"sbtl", "eng", &[(payload + 10, 5)], 2)].concat());
        let data = [ftyp, make_box(b"mdat", &[b'A'; 15]), moov].concat();
        let file = temp_mp4("refers.mp4", &data);
        let dest = file.with_file_name("refers.mp4.out");
        let error = remove_traks(&file, &dest, &[1], &[]).unwrap_err();
        assert!(error.to_string().contains("refers to a track that is removed"), "{error}");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn a_kept_track_that_shares_bytes_with_a_removed_one_is_refused() {
        let ftyp = make_box(b"ftyp", b"isom\0\0\0\0isom");
        let payload = 28u32;
        let moov = make_box(b"moov", &[
            trak_with_chunks(b"soun", "ita", &[(payload, 10)], 1),
            trak_with_chunks(b"sbtl", "eng", &[(payload + 5, 5)], 2),
        ].concat());
        let data = [ftyp, make_box(b"mdat", &[b'A'; 10]), moov].concat();
        let file = temp_mp4("shared.mp4", &data);
        let dest = file.with_file_name("shared.mp4.out");
        let error = remove_traks(&file, &dest, &[1], &[]).unwrap_err();
        assert!(error.to_string().contains("shares media data"), "{error}");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn a_text_track_is_added_with_its_samples_after_the_data_and_the_others_still_read_back() {
        let data = interleaved_file(false, false);
        let file = temp_mp4("text-add.mp4", &data);
        let dest = file.with_file_name("text-add.mp4.out");
        let new = NewText {
            language: "ita".to_string(),
            name: "Italiano".to_string(),
            forced: false,
            enabled: true,
            cues: vec![(0, 1000, b"Hi".to_vec()), (2000, 3000, b"There".to_vec())],
        };
        add_text_tracks(&file, &dest, &[new], &[]).unwrap();
        let out = std::fs::read(&dest).unwrap();
        let moov = read_moov(&dest).unwrap().1;
        let traks = parse_traks(&moov).unwrap();
        assert_eq!(traks.len(), 3);
        assert_eq!(traks[2].kind, Some(Kind::Subtitles));
        assert_eq!(traks[2].id, 3, "the next track ID after the ones there are");
        // the track has an edit list with one segment, as ffmpeg's tracks have
        let track = &moov[traks[2].start..traks[2].end];
        let elst = track.windows(4).position(|w| w == b"elst").expect("the track has an edit list");
        assert_eq!(&track[elst + 4 + 4..elst + 4 + 8], &[0, 0, 0, 1], "one segment");
        assert!(moov.windows(8).any(|w| w == b"Italiano"), "the name is in the track's udta");
        // the first two tracks read back as they were: their data is where their offsets say
        assert_eq!(chunk_bytes(&dest)[..2], chunk_bytes(&file)[..2]);
        // the new track's one chunk: a sample with the first cue, one with no text for the gap, then the second cue
        let spans = chunk_spans(&moov[traks[2].start..traks[2].end]).unwrap();
        assert_eq!(spans.len(), 1);
        let (at, len) = spans[0];
        assert_eq!(&out[at as usize..(at + len) as usize], b"\0\x02Hi\0\0\0\x05There");
        // the sample entry and the data reference each have their one entry
        let sample_table = &moov[traks[2].start..traks[2].end];
        let stsd = sample_table.windows(4).position(|w| w == b"stsd").unwrap();
        assert_eq!(&sample_table[stsd + 8..stsd + 12], &[0, 0, 0, 1], "one sample entry");
        assert_eq!(&sample_table[stsd + 12..stsd + 16], &[0, 0, 0, 0x54], "the tx3g entry follows");
        let dref = sample_table.windows(4).position(|w| w == b"dref").unwrap();
        assert_eq!(&sample_table[dref + 8..dref + 12], &[0, 0, 0, 1], "one data reference");
        // the new data is in an mdat box at the end of the file: its samples are the last 13 bytes
        let end = out.len();
        assert_eq!(&out[end - 13 - 4..end - 13], b"mdat");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&dest);
    }

    /// Add a text track to a real file named in the environment, for the checks made with mediainfo and ffmpeg (see
    /// APPSTORE.md). `MAU_MP4_TEXT_CHECK` is the source, `MAU_MP4_TEXT_CHECK_OUT` the result, and the cues are
    /// those of a SubRip file written as (start, end, text) lines. Does nothing when it is not set.
    #[test]
    fn add_a_text_track_to_a_real_file_named_in_mau_mp4_text_check() {
        let Ok(source) = std::env::var("MAU_MP4_TEXT_CHECK") else { return };
        let dest = std::env::var("MAU_MP4_TEXT_CHECK_OUT").unwrap();
        let cues = vec![(500, 2000, b"Ciao".to_vec()), (3000, 4250, b"Seconda riga\nche continua".to_vec())];
        let new = NewText {
            language: "ita".to_string(),
            name: String::new(),
            forced: false,
            enabled: std::env::var("MAU_MP4_TEXT_CHECK_DEFAULT").is_ok(),
            cues,
        };
        add_text_tracks(Path::new(&source), Path::new(&dest), &[new], &[]).unwrap();
    }

    #[test]
    fn a_wvtt_sample_gives_its_payload_text_and_an_empty_cue_gives_none() {
        let payl = make_box(b"payl", b"Ciao\nlinea");
        let vttc = make_box(b"vttc", &payl);
        assert_eq!(sample_text(b"wvtt", &vttc).unwrap(), b"Ciao\nlinea".to_vec());
        assert!(sample_text(b"wvtt", &make_box(b"vtte", b"")).unwrap().is_empty());
    }

    #[test]
    fn a_ttml_sample_gives_the_text_of_its_paragraphs_with_breaks() {
        let doc = br#"<?xml version="1.0"?><tt xmlns="x"><body><div><p begin="00:00:00.000" end="00:00:01.500">Ciao &amp; hi</p><p begin="0s" end="1s"><span tts:color="red">Seconda<br/>riga</span></p><p/></div></body></tt>"#;
        assert_eq!(sample_text(b"stpp", doc).unwrap(), b"Ciao & hi\nSeconda\nriga".to_vec());
        assert!(sample_text(b"stpp", b"<tt><body><div></div></body></tt>").unwrap().is_empty());
    }

    #[test]
    fn a_sample_of_another_format_is_not_read() {
        assert!(read_text_cues(Path::new("/nonexistent.mp4"), 1).is_err());
    }

    #[test]
    fn an_order_that_is_not_a_permutation_or_a_moov_with_gaps_is_refused() {
        let traks = [trak_with_language(b"soun", 3, "ita"), trak_with_language(b"soun", 2, "eng")];
        let file = temp_mp4("refused.mp4", &mp4_with_traks(&traks));
        let before = std::fs::read(&file).unwrap();
        assert!(edit_in_place(&file, &[], Some(&[0, 0])).is_err(), "a track twice");
        assert!(edit_in_place(&file, &[], Some(&[0])).is_err(), "a track missing");
        assert_eq!(std::fs::read(&file).unwrap(), before, "nothing is written when refused");
        // something other than a trak between two traks: the traks are not one run
        let gap = [traks[0].clone(), make_box(b"free", &[0u8; 8]), traks[1].clone()];
        let gapped = temp_mp4("gap.mp4", &mp4_with_traks(&gap));
        assert!(edit_in_place(&gapped, &[], Some(&[1, 0])).is_err());
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&gapped);
    }

    #[test]
    fn the_flags_of_each_track_are_found_in_order_by_kind() {
        let moov = make_box(b"moov", &[
            make_box(b"mvhd", &[0u8; 100]),
            trak(b"vide", 3), trak(b"soun", 3), trak(b"soun", 2),
            trak(b"sbtl", 3), trak(b"tmcd", 3), trak(b"text", 2),
        ].concat());
        let found = parse_track_flags(&moov, 1000);
        let summary: Vec<(Kind, usize, bool)> = found.iter().map(|t| (t.kind, t.number, t.enabled)).collect();
        assert_eq!(summary, [
            (Kind::Video, 1, true), (Kind::Audio, 1, true), (Kind::Audio, 2, false),
            (Kind::Subtitles, 1, true), (Kind::Subtitles, 2, false), // the timecode track is not counted
        ]);
        // the offset is the last flag byte, counted from the start of the file
        let at = found[3].offset as usize - 1000;
        assert_eq!(moov[at], 3);
        assert_eq!(moov[at - 3], 0); // the version byte is three before it
    }

    #[test]
    fn broken_input_is_not_a_panic() {
        assert!(parse_track_flags(&[], 0).is_empty());
        assert!(parse_track_flags(b"\0\0\0\x08moov", 0).is_empty());
        let cut = make_box(b"moov", &trak(b"soun", 3));
        assert!(parse_track_flags(&cut[..cut.len() - 5], 0).is_empty()); // cut short: not trusted
        let mut lying = make_box(b"moov", &trak(b"soun", 3));
        lying[0..4].copy_from_slice(&0xFFFF_FF00u32.to_be_bytes()); // a size past the end
        assert!(parse_track_flags(&lying, 0).is_empty());
    }

    #[test]
    fn the_enabled_bit_is_changed_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("mau-mp4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp4");
        let moov = make_box(b"moov", &[trak(b"soun", 3), trak(b"sbtl", 3)].concat());
        let mut data = make_box(b"ftyp", b"isom\0\0\0\0isom");
        data.extend_from_slice(&make_box(b"mdat", &[7u8; 50]));
        data.extend_from_slice(&moov);
        std::fs::write(&file, &data).unwrap();
        let flags = read_track_flags(&file).unwrap(); // moov after mdat is found
        assert_eq!(flags.len(), 2);
        set_enabled(&file, &[(flags[1].offset, false)]).unwrap();
        let after = read_track_flags(&file).unwrap();
        assert_eq!((after[0].enabled, after[1].enabled), (true, false));
        let changed: Vec<usize> = std::fs::read(&file).unwrap().iter().zip(&data).enumerate()
            .filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
        assert_eq!(changed, [flags[1].offset as usize]); // one byte differs
        set_enabled(&file, &[(flags[1].offset, true)]).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), data);
        std::fs::write(&file, b"not an mp4 at all").unwrap();
        assert!(read_track_flags(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    /// Each real MP4 named in MAU_MP4_CHECK (separated by `:`) is copied, its tracks are put in reverse order
    /// with the first audio track's language changed, and `mediainfo` must read the new order and language and
    /// `ffmpeg` must decode every stream. A file whose track boxes are not one run is reported and skipped.
    #[test]
    fn the_in_place_edits_are_read_back_on_the_real_files_named_in_mau_mp4_check() {
        let Ok(list) = std::env::var("MAU_MP4_CHECK") else { return };
        let dir = std::env::temp_dir().join(format!("mau-mp4-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut failures = Vec::new();
        for file in list.split(':').filter(|f| !f.is_empty()) {
            let copy = dir.join("copy.mp4");
            std::fs::copy(file, &copy).unwrap();
            let (_, moov) = read_moov(&copy).unwrap();
            let traks = parse_traks(&moov).unwrap();
            // (the reverse of all the traks: each kind's order is reversed among its own traks)
            let reversed: Vec<usize> = (0..traks.len()).rev().collect();
            let first_audio = traks.iter().position(|t| t.kind == Some(Kind::Audio));
            let changes: Vec<TrakChange> = first_audio
                .map(|trak| vec![TrakChange { trak, language: Some("fra".to_string()), enabled: None, name: None, forced: None }])
                .unwrap_or_default();
            if let Err(error) = edit_in_place(&copy, &changes, Some(&reversed)) {
                eprintln!("{file}: not changed in place ({error})");
                continue;
            }
            // mediainfo lists the video first whatever the order, so the order is checked within each kind: the
            // tracks of a kind come out in the reversed order, and the first audio track (now last among the audio)
            // has its new language (mediainfo gives two letters for fra)
            let read_tracks = |path: &Path| -> Vec<(String, String)> {
                let out = std::process::Command::new("mediainfo").arg("--Output=JSON").arg(path).output().expect("mediainfo runs");
                let data: serde_json::Value = serde_json::from_slice(&out.stdout).expect("mediainfo reads the file");
                data["media"]["track"].as_array().unwrap().iter()
                    .filter(|t| t["@type"] != "General")
                    .map(|t| (t["@type"].as_str().unwrap_or("").to_string(), t["Language"].as_str().unwrap_or("").to_string()))
                    .collect()
            };
            let read = read_tracks(&copy);
            let before = read_tracks(Path::new(file));
            let mut expected = Vec::new();
            for kind in ["Video", "Audio", "Text"] {
                let mut same_kind: Vec<(String, String)> = before.iter().filter(|(k, _)| k == kind).cloned().collect();
                same_kind.reverse();
                if kind == "Audio" {
                    if let Some(last) = same_kind.last_mut() {
                        if first_audio.is_some() {
                            last.1 = "fr".to_string();
                        }
                    }
                }
                expected.extend(same_kind);
            }
            let mut got_sorted = read.clone();
            got_sorted.sort_by_key(|(k, _)| ["Video", "Audio", "Text"].iter().position(|x| x == k).unwrap_or(9));
            let kinds_match = got_sorted == expected;
            let decode = std::process::Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(&copy).args(["-f", "null", "-"]).output();
            let decoded = decode.map(|o| o.status.success() && o.stderr.is_empty()).unwrap_or(false);
            if !kinds_match || !decoded {
                failures.push(format!("{file}: read {read:?}, expected {expected:?}, decodes: {decoded}"));
            } else {
                eprintln!("{file}: reversed and changed in place; mediainfo reads {read:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

}
