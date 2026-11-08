//! Reordering the streams of an AVI file in place, without ffmpeg.
//!
//! An AVI has no flags, names or languages to change, so the only edit it needs is to put its audio streams in
//! another order (`--swap`). That is three changes that keep every size the same: the stream lists (`strl`) in
//! the header list (`hdrl`) are put in the new order, each chunk's stream number (the two digits in `01wb`) is
//! renamed to the stream's new place, and the same renaming is made in the index (`idx1`). The offsets in the
//! index are into the `movi` list, which does not move, so they stay right. A file with an OpenDML index (`indx`)
//! is refused: its offsets are absolute.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::ebml::invalid_range;
use crate::track::{Kind, Track};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// A chunk or list in the file: its ID, where it starts, its payload's start and its payload's length.
#[derive(Debug, Clone, Copy)]
struct Chunk {
    id: [u8; 4],
    start: u64,
    body: u64,
    len: u64,
}

fn read_at(file: &mut File, at: u64, len: usize) -> io::Result<Vec<u8>> {
    // a size read from the file can be anything: nothing past the end of the file is read (or allocated)
    let length = file.metadata()?.len();
    if at.checked_add(len as u64).map_or(true, |end| end > length) {
        return Err(invalid_range());
    }
    let mut buffer = vec![0u8; len];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// The chunks between `start` and `end`, in order. A chunk whose size runs past `end` is refused.
fn chunks(file: &mut File, start: u64, end: u64) -> io::Result<Vec<Chunk>> {
    let mut out = Vec::new();
    let mut at = start;
    while at + 8 <= end {
        let head = read_at(file, at, 8)?;
        let id = [head[0], head[1], head[2], head[3]];
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
        let body = at + 8;
        if body + len > end {
            return Err(invalid("a chunk runs past the end of its list"));
        }
        out.push(Chunk { id, start: at, body, len });
        // chunks are padded to an even length
        at = body + len + (len & 1);
    }
    Ok(out)
}

/// The two digit stream number of a data chunk (`01wb` is stream 1), if the ID is one.
fn stream_of(id: &[u8; 4]) -> Option<usize> {
    if id[0].is_ascii_digit() && id[1].is_ascii_digit() {
        Some(((id[0] - b'0') as usize) * 10 + (id[1] - b'0') as usize)
    } else {
        None
    }
}

/// Put the streams of the AVI file at `path` in the order `order` (`order[k]` is the stream that comes k-th;
/// streams are counted from 0 in the order of their `strl` lists). The file keeps its size. Fails, and changes
/// nothing, for a file without an `idx1` index or with an OpenDML index.
pub fn reorder_streams_in_place(path: &Path, order: &[usize]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let Parts { hdrl, movi, idx1, .. } = parts(&mut file)?;

    // the streams: each strl list of the header list, which must be one run (nothing else between them)
    let header_items = chunks(&mut file, hdrl.body + 4, hdrl.body + hdrl.len)?;
    if header_items.iter().any(|c| &c.id == b"indx") {
        return Err(invalid("an OpenDML index (indx) has absolute offsets, so the file is not changed"));
    }
    let mut streams: Vec<Chunk> = Vec::new();
    for item in &header_items {
        if &item.id == b"LIST" && read_at(&mut file, item.body, 4)? == *b"strl" {
            streams.push(*item);
        }
    }
    if streams.len() != order.len() || streams.is_empty() {
        return Err(invalid("the order does not name every stream"));
    }
    let mut seen = vec![false; order.len()];
    if order.iter().any(|&i| i >= order.len() || std::mem::replace(&mut seen[i], true)) {
        return Err(invalid("the order does not name every stream once"));
    }
    if streams.windows(2).any(|pair| pair[0].start + 8 + pair[0].len + (pair[0].len & 1) != pair[1].start) {
        return Err(invalid("the stream lists are not one run, so they cannot be moved"));
    }
    let first = streams[0];
    let last = streams[streams.len() - 1];
    let mut region = Vec::new();
    for &i in order {
        let s = streams[i];
        region.extend(read_at(&mut file, s.start, (8 + s.len + (s.len & 1)) as usize)?);
    }
    // the region is the same length: the lists are only put in another order
    let region_len = (last.start + 8 + last.len + (last.len & 1) - first.start) as usize;
    if region.len() != region_len {
        return Err(invalid("the stream lists do not fill their run"));
    }

    // the new stream number of each old stream
    let mut renumber = vec![0usize; order.len()];
    for (place, &old) in order.iter().enumerate() {
        renumber[old] = place;
    }
    let renamed = |id: &[u8; 4]| -> Option<[u8; 4]> {
        let old = stream_of(id)?;
        let new = *renumber.get(old)?;
        Some([b'0' + (new / 10) as u8, b'0' + (new % 10) as u8, id[2], id[3]])
    };

    // the index first (it is read and written whole), then the stream lists, then the data chunks
    let mut index = read_at(&mut file, idx1.body, idx1.len as usize)?;
    if index.len() % 16 != 0 {
        return Err(invalid("the index is not a whole number of entries"));
    }
    for entry in index.chunks_exact_mut(16) {
        if let Some(id) = renamed(&[entry[0], entry[1], entry[2], entry[3]]) {
            entry[0..4].copy_from_slice(&id);
        }
    }
    let data_chunks = data_chunks_of(&mut file, movi)?;
    file.seek(SeekFrom::Start(first.start))?;
    file.write_all(&region)?;
    for at in data_chunks {
        let id = read_at(&mut file, at, 4)?;
        if let Some(new) = renamed(&[id[0], id[1], id[2], id[3]]) {
            file.seek(SeekFrom::Start(at))?;
            file.write_all(&new)?;
        }
    }
    file.seek(SeekFrom::Start(idx1.body))?;
    file.write_all(&index)?;
    file.flush()
}

/// The top-level pieces of an AVI file that the changes here need.
struct Parts {
    length: u64,
    top_chunks: Vec<Chunk>,
    hdrl: Chunk,
    movi: Chunk,
    idx1: Chunk,
}

/// Find the header list, the `movi` list and the index in an AVI file (refused if one is missing).
fn parts(file: &mut File) -> io::Result<Parts> {
    let length = file.metadata()?.len();
    let top = read_at(file, 0, 12)?;
    if &top[0..4] != b"RIFF" || &top[8..12] != b"AVI " {
        return Err(invalid("not an AVI file"));
    }
    let riff_end = (8 + u32::from_le_bytes([top[4], top[5], top[6], top[7]]) as u64).min(length);
    let top_chunks = chunks(file, 12, riff_end)?;
    let list_of = |file: &mut File, kind: &[u8; 4]| -> io::Result<Option<Chunk>> {
        for c in &top_chunks {
            if &c.id == b"LIST" && read_at(file, c.body, 4)? == *kind {
                return Ok(Some(*c));
            }
        }
        Ok(None)
    };
    let hdrl = list_of(file, b"hdrl")?.ok_or_else(|| invalid("no header list"))?;
    let movi = list_of(file, b"movi")?.ok_or_else(|| invalid("no movi list"))?;
    let idx1 = top_chunks.iter().find(|c| &c.id == b"idx1").copied().ok_or_else(|| invalid("no idx1 index"))?;
    Ok(Parts { length, top_chunks, hdrl, movi, idx1 })
}

/// The tracks of an AVI file, read from its header in this program: one per stream, in the order of the `strl`
/// lists (which is the order of the stream numbers in the chunks). The kind comes from the stream's `strh` type, the
/// codec from its format chunk (`strf`), the channels from an audio format, and the name from `strn`. An AVI has no
/// language or default flag, so the language is `und`. A stream of another type (not video, audio or text) is left
/// out, as mediainfo leaves it out.
pub fn stream_tracks(path: &Path) -> io::Result<Vec<Track>> {
    let mut file = File::open(path)?;
    let Parts { hdrl, .. } = parts(&mut file)?;
    let mut tracks: Vec<Track> = Vec::new();
    let mut numbers = [0usize; 3];
    let mut stream = 0i64;
    for item in chunks(&mut file, hdrl.body + 4, hdrl.body + hdrl.len)? {
        if &item.id != b"LIST" || read_at(&mut file, item.body, 4)? != *b"strl" {
            continue;
        }
        let this = stream;
        stream += 1;
        let inside = chunks(&mut file, item.body + 4, item.body + item.len)?;
        let child = |id: &[u8; 4]| inside.iter().find(|c| &c.id == id).copied();
        let strh = child(b"strh").ok_or_else(|| invalid("a stream has no header"))?;
        let kind_of = read_at(&mut file, strh.body, 4)?;
        let kind = match &kind_of[..] {
            b"vids" => Kind::Video,
            b"auds" => Kind::Audio,
            b"txts" => Kind::Subtitles,
            _ => continue,
        };
        let format = match child(b"strf") {
            Some(strf) => read_at(&mut file, strf.body, strf.len.min(40) as usize)?,
            None => Vec::new(),
        };
        let name = match child(b"strn") {
            Some(strn) => String::from_utf8_lossy(&read_at(&mut file, strn.body, strn.len as usize)?)
                .trim_end_matches('\0')
                .trim()
                .to_string(),
            None => String::new(),
        };
        let slot = match kind {
            Kind::Video => 0,
            Kind::Audio => 1,
            Kind::Subtitles => 2,
        };
        numbers[slot] += 1;
        let (codec, channels) = match kind {
            Kind::Video => (video_codec(&format), 0),
            Kind::Audio => audio_codec(&format),
            Kind::Subtitles => (String::new(), 0),
        };
        tracks.push(Track {
            id: this,
            kind,
            number: numbers[slot],
            codec,
            language: "und".to_string(),
            default: false,
            forced: false,
            name,
            channels,
        });
    }
    Ok(tracks)
}

/// The codec name of a video stream from its `BITMAPINFOHEADER` (`strf`): the four-character code of the compression,
/// named the way mediainfo names the common ones. A zero code is uncompressed RGB.
fn video_codec(format: &[u8]) -> String {
    if format.len() < 20 {
        return String::new();
    }
    let code = &format[16..20];
    if code == [0, 0, 0, 0] {
        return "RGB".to_string();
    }
    let text = String::from_utf8_lossy(code).to_string();
    match text.to_ascii_uppercase().as_str() {
        "MP4V" | "DIVX" | "DX50" | "XVID" | "FMP4" | "MP42" | "MP43" => "MPEG-4 Visual".to_string(),
        "H264" | "X264" | "AVC1" => "AVC".to_string(),
        "HEVC" | "H265" | "HVC1" => "HEVC".to_string(),
        "MJPG" => "JPEG".to_string(),
        "CVID" => "Cinepak".to_string(),
        "IV31" | "IV32" => "Indeo 3".to_string(),
        "IV41" => "Indeo 4".to_string(),
        "IV50" => "Indeo 5".to_string(),
        "MSVC" | "CRAM" => "Microsoft Video 1".to_string(),
        _ => text.trim().to_string(),
    }
}

/// The codec name and channel count of an audio stream from its `WAVEFORMATEX` (`strf`): the format tag, named the way
/// mediainfo names the common ones, and the number of channels.
fn audio_codec(format: &[u8]) -> (String, usize) {
    if format.len() < 4 {
        return (String::new(), 0);
    }
    let tag = u16::from_le_bytes([format[0], format[1]]);
    let channels = u16::from_le_bytes([format[2], format[3]]) as usize;
    let codec = match tag {
        0x0001 => "PCM".to_string(),
        0x0050 | 0x0055 => "MPEG Audio".to_string(),
        0x2000 => "AC-3".to_string(),
        0x00FF | 0x1600 => "AAC".to_string(),
        0x0161 | 0x0162 => "WMA".to_string(),
        other => format!("0x{other:04X}"),
    };
    (codec, channels)
}

/// Where each data chunk of the `movi` list starts (chunks inside a `rec ` list are included).
fn data_chunks_of(file: &mut File, movi: Chunk) -> io::Result<Vec<u64>> {
    let mut found = Vec::new();
    data_chunks_in(file, movi.body + 4, movi.body + movi.len, &mut found)?;
    Ok(found)
}

/// The data chunks between `start` and `end`, in file order; the lists among them (`rec `) are read in place.
fn data_chunks_in(file: &mut File, start: u64, end: u64, found: &mut Vec<u64>) -> io::Result<()> {
    for chunk in chunks(file, start, end)? {
        if &chunk.id == b"LIST" {
            data_chunks_in(file, chunk.body + 4, chunk.body + chunk.len, found)?;
        } else if stream_of(&chunk.id).is_some() {
            found.push(chunk.start);
        }
    }
    Ok(())
}

/// The `movi` list as it will be written: a kept data chunk (with its new stream number), a chunk copied as it is,
/// or a `rec ` list of chunks (`len` is the payload's length, the kind included).
enum Node {
    Data { body: u64, id: [u8; 4], len: u64 },
    Copy { at: u64, total: u64 },
    List { kind: [u8; 4], children: Vec<Node>, len: u64 },
}

/// The bytes a chunk takes up, its padding included.
fn total_of(chunk: &Chunk) -> u64 {
    8 + chunk.len + (chunk.len & 1)
}

/// What becomes of a chunk whose ID is `id`, given the new number of each stream (`None` for a removed one): `None`
/// if it is not a stream's chunk, `Some(None)` if it is removed, otherwise `Some(Some(new ID))`.
fn stream_fate(id: &[u8; 4], renumber: &[Option<usize>]) -> io::Result<Option<Option<[u8; 4]>>> {
    let Some(old) = stream_of(id) else { return Ok(None) };
    match renumber.get(old) {
        None => Err(invalid("a chunk names a stream that the file does not have")),
        Some(None) => Ok(Some(None)),
        Some(Some(new)) => Ok(Some(Some([b'0' + (new / 10) as u8, b'0' + (new % 10) as u8, id[2], id[3]]))),
    }
}

/// Plan the chunks between `start` and `end` of a `movi` list, and where each kept chunk goes. `pos` is the place of
/// the next chunk in the new list, counted from its `movi` fourcc (as the index counts). `map` takes the absolute place
/// of each kept data chunk, and of each `rec ` list that is kept, to its new place. A `rec ` list whose chunks are all
/// removed is dropped, with its entry in the index.
fn plan_movi(
    file: &mut File,
    start: u64,
    end: u64,
    renumber: &[Option<usize>],
    map: &mut HashMap<u64, u64>,
    sizes: &mut HashMap<u64, u64>,
    pos: &mut u64,
) -> io::Result<Vec<Node>> {
    let mut nodes = Vec::new();
    for chunk in chunks(file, start, end)? {
        if &chunk.id == b"LIST" {
            let kind = read_at(file, chunk.body, 4)?;
            if &kind == b"rec " {
                let at = *pos;
                *pos += 12;
                map.insert(chunk.start, at);
                let children = plan_movi(file, chunk.body + 4, chunk.body + chunk.len, renumber, map, sizes, pos)?;
                if children.is_empty() {
                    *pos = at;
                    map.remove(&chunk.start);
                    continue;
                }
                sizes.insert(chunk.start, *pos - at - 8);
                nodes.push(Node::List { kind: [kind[0], kind[1], kind[2], kind[3]], children, len: *pos - at - 8 });
                continue;
            }
            nodes.push(Node::Copy { at: chunk.start, total: total_of(&chunk) });
            *pos += total_of(&chunk);
            continue;
        }
        match stream_fate(&chunk.id, renumber)? {
            None => {
                nodes.push(Node::Copy { at: chunk.start, total: total_of(&chunk) });
                *pos += total_of(&chunk);
            }
            Some(None) => {}
            Some(Some(id)) => {
                map.insert(chunk.start, *pos);
                nodes.push(Node::Data { body: chunk.body, id, len: chunk.len });
                *pos += total_of(&chunk);
            }
        }
    }
    Ok(nodes)
}

/// Whether the index entry names a chunk at `base + offset`: the bytes there are its ID, or, for a `rec ` list, a `LIST`
/// header whose type is `rec `.
fn points_at(file: &mut File, base: u64, entry: &[u8; 16]) -> io::Result<bool> {
    let target = base + u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as u64;
    if target + 12 > file.metadata()?.len() {
        return Ok(false);
    }
    let bytes = read_at(file, target, 12)?;
    if bytes[0..4] == entry[0..4] {
        return Ok(true);
    }
    Ok(&entry[0..4] == b"rec " && &bytes[0..4] == b"LIST" && &bytes[8..12] == b"rec ")
}

/// Copy `file[from..to]` to `out`.
fn copy_bytes(file: &mut File, out: &mut impl Write, from: u64, to: u64) -> io::Result<()> {
    file.seek(SeekFrom::Start(from))?;
    io::copy(&mut file.take(to - from), out)?;
    Ok(())
}

/// Write planned nodes; `length` is the length of the source, which the last chunk may run past (no padding).
fn write_nodes(file: &mut File, out: &mut impl Write, nodes: &[Node], length: u64) -> io::Result<()> {
    for node in nodes {
        match node {
            Node::Data { body, id, len } => {
                out.write_all(id)?;
                out.write_all(&(*len as u32).to_le_bytes())?;
                copy_bytes(file, out, *body, body + len)?;
                if len & 1 == 1 {
                    out.write_all(&[0])?;
                }
            }
            Node::Copy { at, total } => copy_bytes(file, out, *at, (at + total).min(length))?,
            Node::List { kind, children, len } => {
                out.write_all(b"LIST")?;
                out.write_all(&(*len as u32).to_le_bytes())?;
                out.write_all(kind)?;
                write_nodes(file, out, children, length)?;
            }
        }
    }
    Ok(())
}

/// Remove streams from the AVI file `source`, writing the result to `dest` (the source is not changed). `removed`
/// is the streams to take out, counted from 0 in the order of their `strl` lists, as `reorder_streams_in_place`
/// counts them. The streams after each removed one move down a number, in every chunk ID and index entry; the
/// `movi` list and its `rec ` lists are rebuilt with the chunks that are left, and each index offset is moved to
/// where its chunk now is. The header's stream count is made to match. A file with an OpenDML index is refused.
pub fn remove_streams(source: &Path, dest: &Path, removed: &[usize]) -> io::Result<()> {
    let mut file = File::open(source)?;
    let Parts { length, top_chunks, hdrl, movi, idx1 } = parts(&mut file)?;
    let header_items = chunks(&mut file, hdrl.body + 4, hdrl.body + hdrl.len)?;
    if header_items.iter().any(|c| &c.id == b"indx") {
        return Err(invalid("an OpenDML index (indx) has absolute offsets, so the file is not changed"));
    }
    let mut strls: Vec<Chunk> = Vec::new();
    for item in &header_items {
        if &item.id == b"LIST" && read_at(&mut file, item.body, 4)? == *b"strl" {
            strls.push(*item);
        }
    }
    let count = strls.len();
    if count == 0 || count > 99 {
        return Err(invalid("the file does not have between 1 and 99 streams"));
    }
    let mut gone = vec![false; count];
    for &index in removed {
        if index >= count || std::mem::replace(&mut gone[index], true) {
            return Err(invalid("a stream to remove is not in the file, or is named twice"));
        }
    }
    if removed.len() == count {
        return Err(invalid("every stream would be removed"));
    }
    let renumber: Vec<Option<usize>> = (0..count)
        .map(|old| if gone[old] { None } else { Some(old - gone[..old].iter().filter(|&&g| g).count()) })
        .collect();
    let kept = count - removed.len();

    // the index's offsets: from the movi fourcc, or from the start of the file (older writers did this); each entry must
    // point at a chunk with its ID, in one of the two ways, or the file is refused
    let index = read_at(&mut file, idx1.body, idx1.len as usize)?;
    if index.len() % 16 != 0 {
        return Err(invalid("the index is not a whole number of entries"));
    }
    let entries: Vec<[u8; 16]> = index.chunks_exact(16).map(|e| <[u8; 16]>::try_from(e).unwrap_or([0; 16])).collect();
    let movi_fourcc = movi.start + 8;
    let from_fourcc = |file: &mut File, entry: &[u8; 16]| points_at(file, movi_fourcc, entry);
    let from_start = |file: &mut File, entry: &[u8; 16]| points_at(file, 0, entry);
    let relative = entries.iter().all(|e| from_fourcc(&mut file, e).unwrap_or(false));
    let absolute = !relative && entries.iter().all(|e| from_start(&mut file, e).unwrap_or(false));
    if !relative && !absolute {
        return Err(invalid("the index does not point at the chunks it names, so the file is not changed"));
    }
    let base = if relative { movi_fourcc } else { 0 };

    // the movi list first: its plan gives the new places the index needs
    let mut map = HashMap::new();
    let mut sizes = HashMap::new();
    let mut pos = 4u64; // the fourcc comes first
    let nodes = plan_movi(&mut file, movi.body + 4, movi.body + movi.len, &renumber, &mut map, &mut sizes, &mut pos)?;
    let movi_len = pos; // the fourcc and the chunks

    // the header list: the streams that are left, and the stream count
    let mut hdrl_bytes = b"hdrl".to_vec();
    for item in &header_items {
        let total = total_of(item).min(length - item.start);
        if &item.id == b"avih" {
            let mut payload = read_at(&mut file, item.body, item.len as usize)?;
            if payload.len() < 28 {
                return Err(invalid("the main header is cut short"));
            }
            payload[24..28].copy_from_slice(&(kept as u32).to_le_bytes());
            hdrl_bytes.extend_from_slice(b"avih");
            hdrl_bytes.extend_from_slice(&(item.len as u32).to_le_bytes());
            hdrl_bytes.extend_from_slice(&payload);
            if item.len & 1 == 1 {
                hdrl_bytes.push(0);
            }
        } else if let Some(index) = strls.iter().position(|s| s.start == item.start) {
            if !gone[index] {
                hdrl_bytes.extend(read_at(&mut file, item.start, total as usize)?);
            }
        } else {
            hdrl_bytes.extend(read_at(&mut file, item.start, total as usize)?);
        }
    }

    // where the movi fourcc is in the new file: the RIFF header, then the boxes before movi (the header list is the
    // new one)
    let mut movi_fourcc_out = 12u64;
    for chunk in &top_chunks {
        if chunk.start == movi.start {
            break;
        }
        movi_fourcc_out += if chunk.start == hdrl.start { 8 + hdrl_bytes.len() as u64 } else { total_of(chunk) };
    }
    movi_fourcc_out += 8;
    let new_offset = |moved: u64| if relative { moved } else { movi_fourcc_out + moved };

    // the index: the entries of the streams that are left, and of the rec lists that are kept, each with its new offset
    let mut new_index = Vec::new();
    for entry in &entries {
        let id = [entry[0], entry[1], entry[2], entry[3]];
        let old = base + u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as u64;
        if &id == b"rec " {
            if let (Some(moved), Some(size)) = (map.get(&old), sizes.get(&old)) {
                new_index.extend_from_slice(&entry[0..8]);
                new_index.extend_from_slice(&(new_offset(*moved) as u32).to_le_bytes());
                new_index.extend_from_slice(&(*size as u32).to_le_bytes());
            }
            continue;
        }
        match stream_fate(&id, &renumber)? {
            Some(Some(new_id)) => {
                let moved = *map.get(&old).ok_or_else(|| invalid("an index entry points at a chunk that is not in movi"))?;
                new_index.extend_from_slice(&new_id);
                new_index.extend_from_slice(&entry[4..8]);
                new_index.extend_from_slice(&(new_offset(moved) as u32).to_le_bytes());
                new_index.extend_from_slice(&entry[12..16]);
            }
            Some(None) => {}
            None => return Err(invalid("an index entry is not a stream's chunk")),
        }
    }

    // the file: the same boxes in the same order, with the three that changed written again
    let mut out = BufWriter::new(File::create(dest)?);
    out.write_all(b"RIFF\0\0\0\0AVI ")?;
    for chunk in &top_chunks {
        if chunk.start == hdrl.start {
            out.write_all(b"LIST")?;
            out.write_all(&(hdrl_bytes.len() as u32).to_le_bytes())?;
            out.write_all(&hdrl_bytes)?;
        } else if chunk.start == movi.start {
            out.write_all(b"LIST")?;
            out.write_all(&(movi_len as u32).to_le_bytes())?;
            out.write_all(b"movi")?;
            write_nodes(&mut file, &mut out, &nodes, length)?;
        } else if chunk.start == idx1.start {
            out.write_all(b"idx1")?;
            out.write_all(&(new_index.len() as u32).to_le_bytes())?;
            out.write_all(&new_index)?;
            if new_index.len() % 2 == 1 {
                out.write_all(&[0])?;
            }
        } else {
            copy_bytes(&mut file, &mut out, chunk.start, (chunk.start + total_of(chunk)).min(length))?;
        }
    }
    let written = out.stream_position()?;
    out.seek(SeekFrom::Start(4))?;
    out.write_all(&((written - 8) as u32).to_le_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn list(kind: &[u8; 4], items: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = kind.to_vec();
        payload.extend(items.concat());
        chunk(b"LIST", &payload)
    }

    fn riff(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = b"AVI ".to_vec();
        payload.extend(parts.concat());
        chunk(b"RIFF", &payload)
    }

    /// A stream list: its header (`strh`) with the stream's type, and a format chunk of `format_len` bytes.
    fn stream(kind: &[u8; 4], format_len: usize) -> Vec<u8> {
        list(b"strl", &[chunk(b"strh", &[kind[0], kind[1], kind[2], kind[3], 0, 0, 0, 0]), chunk(b"strf", &vec![7u8; format_len])])
    }

    /// Video (stream 0), two audio (1 and 2), with one data chunk per stream per frame and an index for them.
    fn three_stream_file() -> Vec<u8> {
        let hdrl = list(b"hdrl", &[chunk(b"avih", &[0u8; 56]), stream(b"vids", 40), stream(b"auds", 16), stream(b"auds", 18)]);
        let frames: Vec<Vec<u8>> = (0..2)
            .flat_map(|frame| {
                [chunk(b"00dc", &[frame; 6]), chunk(b"01wb", &[1, frame, 1]), chunk(b"02wb", &[2, frame])]
            })
            .collect();
        let movi = list(b"movi", &frames);
        let mut index = Vec::new();
        let mut offset = 4u32;
        for frame in frames.iter() {
            index.extend_from_slice(&frame[0..4]);
            index.extend_from_slice(&[0x10, 0, 0, 0]);
            index.extend_from_slice(&offset.to_le_bytes());
            index.extend_from_slice(&(u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]])).to_le_bytes());
            offset += frame.len() as u32;
        }
        riff(&[hdrl, movi, chunk(b"idx1", &index)])
    }

    fn read_streams(bytes: &[u8]) -> (Vec<Vec<u8>>, Vec<[u8; 4]>, Vec<[u8; 4]>) {
        // the strl lists' stream types, the chunk IDs in movi, and the index IDs
        let mut file = tempfile(bytes);
        let length = file.metadata().unwrap().len();
        let top = chunks(&mut file, 12, length).unwrap();
        let hdrl = top.iter().find(|c| &c.id == b"LIST" && read_at(&mut file, c.body, 4).unwrap() == *b"hdrl").copied().unwrap();
        let items = chunks(&mut file, hdrl.body + 4, hdrl.body + hdrl.len).unwrap();
        // each stream's type is the first four bytes of its strh chunk
        let kinds: Vec<Vec<u8>> = items.iter().filter(|c| &c.id == b"LIST").map(|strl| {
            let inside = chunks(&mut file, strl.body + 4, strl.body + strl.len).unwrap();
            let strh = inside.iter().find(|c| &c.id == b"strh").unwrap();
            read_at(&mut file, strh.body, 4).unwrap()
        }).collect();
        let movi = top.iter().find(|c| &c.id == b"LIST" && read_at(&mut file, c.body, 4).unwrap() == *b"movi").copied().unwrap();
        let mut ids = Vec::new();
        for at in data_chunks_of(&mut file, movi).unwrap() {
            let id = read_at(&mut file, at, 4).unwrap();
            ids.push([id[0], id[1], id[2], id[3]]);
        }
        let idx = top.iter().find(|c| &c.id == b"idx1").copied().unwrap();
        let index = read_at(&mut file, idx.body, idx.len as usize).unwrap();
        let index_ids = index.chunks_exact(16).map(|e| [e[0], e[1], e[2], e[3]]).collect();
        (kinds, ids, index_ids)
    }

    fn tempfile(bytes: &[u8]) -> File {
        // a name of its own for each call: tests run in parallel, and two files of the same length must not collide
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mau-avi-{}-{n}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        File::open(&path).unwrap()
    }

    #[test]
    fn the_streams_are_put_in_the_new_order_and_the_size_does_not_change() {
        let dir = std::env::temp_dir().join(format!("mau-avi-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("three.avi");
        let before = three_stream_file();
        std::fs::write(&path, &before).unwrap();
        reorder_streams_in_place(&path, &[0, 2, 1]).unwrap();
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after.len(), before.len());
        let (kinds, ids, index_ids) = read_streams(&after);
        // the second and third stream lists have swapped places: the video stays first
        assert_eq!(kinds, [b"vids".to_vec(), b"auds".to_vec(), b"auds".to_vec()]);
        // the data chunks: what was stream 2 is now stream 1, and the other way round
        let first_frame: Vec<[u8; 4]> = ids.iter().take(3).copied().collect();
        assert_eq!(first_frame, [*b"00dc", *b"02wb", *b"01wb"]);
        assert_eq!(index_ids.iter().take(3).copied().collect::<Vec<_>>(), first_frame);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whether each index entry names its chunk: its offset is from the `movi` fourcc or from the start of the file, and
    /// a `rec ` entry names the `LIST` header of a `rec ` list.
    fn index_points_at_its_chunks(bytes: &[u8]) -> bool {
        let movi = bytes.windows(4).position(|w| w == b"movi").unwrap();
        index_points_from(bytes, movi) || index_points_from(bytes, 0)
    }

    /// Whether each index entry names its chunk, with its offset counted from `base`.
    fn index_points_from(bytes: &[u8], base: usize) -> bool {
        let index = bytes.windows(4).rposition(|w| w == b"idx1").unwrap();
        let count = u32::from_le_bytes([bytes[index + 4], bytes[index + 5], bytes[index + 6], bytes[index + 7]]) / 16;
        let entries: Vec<&[u8]> = (0..count as usize).map(|i| &bytes[index + 8 + 16 * i..index + 24 + 16 * i]).collect();
        let names = |base: usize| {
            entries.iter().all(|entry| {
                let at = base + u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
                match bytes.get(at..at + 12) {
                    Some(got) => {
                        got[0..4] == entry[0..4]
                            || (&entry[0..4] == b"rec " && &got[0..4] == b"LIST" && &got[8..12] == b"rec ")
                    }
                    None => false,
                }
            })
        };
        names(base)
    }

    /// The RIFF size is the file's length less the eight bytes of the RIFF header.
    fn riff_size_is_right(bytes: &[u8]) -> bool {
        u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize + 8 == bytes.len()
    }

    fn remove_in(name: &str, before: &[u8], removed: &[usize]) -> io::Result<Vec<u8>> {
        let dir = std::env::temp_dir().join(format!("mau-avi-remove-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.avi");
        let dest = dir.join("dest.avi");
        std::fs::write(&source, before).unwrap();
        let result = remove_streams(&source, &dest, removed);
        let out = if result.is_ok() { std::fs::read(&dest).unwrap() } else { Vec::new() };
        assert_eq!(std::fs::read(&source).unwrap(), before, "the source is not changed");
        let _ = std::fs::remove_dir_all(&dir);
        result.map(|()| out)
    }

    #[test]
    fn a_stream_is_taken_out_and_the_others_keep_their_chunks_index_and_count() {
        let before = three_stream_file();
        let after = remove_in("audio", &before, &[1]).unwrap();
        assert!(riff_size_is_right(&after));
        let (kinds, ids, index_ids) = read_streams(&after);
        assert_eq!(kinds, [b"vids".to_vec(), b"auds".to_vec()]);
        // what was stream 2 is now stream 1; the removed stream's chunks are gone
        assert_eq!(ids, [*b"00dc", *b"01wb", *b"00dc", *b"01wb"]);
        assert_eq!(index_ids, ids);
        assert!(index_points_at_its_chunks(&after));
        // the header's stream count is the one left
        let avih = after.windows(4).position(|w| w == b"avih").unwrap();
        assert_eq!(u32::from_le_bytes([after[avih + 8 + 24], after[avih + 8 + 25], after[avih + 8 + 26], after[avih + 8 + 27]]), 2);
    }

    #[test]
    fn the_video_can_go_and_the_audio_streams_are_renumbered_from_zero() {
        let before = three_stream_file();
        let after = remove_in("video", &before, &[0]).unwrap();
        assert!(riff_size_is_right(&after));
        let (kinds, ids, index_ids) = read_streams(&after);
        assert_eq!(kinds, [b"auds".to_vec(), b"auds".to_vec()]);
        assert_eq!(ids, [*b"00wb", *b"01wb", *b"00wb", *b"01wb"]);
        assert_eq!(index_ids, ids);
        assert!(index_points_at_its_chunks(&after));
    }

    /// The same streams as `three_stream_file`, with each frame's chunks in a `rec ` list, and an index that
    /// points at them (the offsets count from the `movi` fourcc).
    fn three_stream_file_with_rec_lists() -> Vec<u8> {
        let hdrl = list(b"hdrl", &[chunk(b"avih", &[0u8; 56]), stream(b"vids", 40), stream(b"auds", 16), stream(b"auds", 18)]);
        let mut movi_body = b"movi".to_vec();
        let mut index = Vec::new();
        for frame in 0..2u8 {
            let chunks = [chunk(b"00dc", &[frame; 6]), chunk(b"01wb", &[1, frame, 1]), chunk(b"02wb", &[2, frame])];
            let rec_at = movi_body.len() as u32; // from the fourcc
            let mut offset = rec_at + 12; // the first chunk of the rec list, which starts with its own header
            for c in &chunks {
                index.extend_from_slice(&c[0..4]);
                index.extend_from_slice(&[0x10, 0, 0, 0]);
                index.extend_from_slice(&offset.to_le_bytes());
                index.extend_from_slice(&(u32::from_le_bytes([c[4], c[5], c[6], c[7]])).to_le_bytes());
                offset += c.len() as u32;
            }
            movi_body.extend(list(b"rec ", &chunks));
        }
        let movi = chunk(b"LIST", &movi_body);
        riff(&[hdrl, movi, chunk(b"idx1", &index)])
    }

    #[test]
    fn a_stream_is_taken_out_of_a_file_whose_chunks_are_in_rec_lists() {
        let before = three_stream_file_with_rec_lists();
        let after = remove_in("rec", &before, &[1]).unwrap();
        assert!(riff_size_is_right(&after));
        assert!(index_points_at_its_chunks(&after));
        let (kinds, ids, index_ids) = read_streams(&after);
        assert_eq!(kinds, [b"vids".to_vec(), b"auds".to_vec()]);
        assert_eq!(ids, [*b"00dc", *b"01wb", *b"00dc", *b"01wb"]);
        assert_eq!(index_ids, ids);
    }

    /// Run `remove_streams` on a real file named by the environment, for the checks made with ffmpeg (see the
    /// notes in APPSTORE.md): `MAU_AVI_CHECK` is the source, `MAU_AVI_CHECK_OUT` the result, and
    /// `MAU_AVI_CHECK_REMOVE` the streams to take out, as a comma separated list. Does nothing when it is not set.
    #[test]
    fn remove_streams_on_a_real_file_named_in_mau_avi_check() {
        let Ok(source) = std::env::var("MAU_AVI_CHECK") else { return };
        let dest = std::env::var("MAU_AVI_CHECK_OUT").unwrap();
        let removed: Vec<usize> = std::env::var("MAU_AVI_CHECK_REMOVE")
            .unwrap()
            .split(',')
            .map(|n| n.parse().unwrap())
            .collect();
        remove_streams(Path::new(&source), Path::new(&dest), &removed).unwrap();
        assert!(index_points_at_its_chunks(&std::fs::read(&dest).unwrap()));
    }

    #[test]
    fn the_streams_are_listed_as_tracks_in_their_order_with_their_kinds_and_codecs() {
        let dir = std::env::temp_dir().join(format!("mau-avi-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("list.avi");
        std::fs::write(&path, three_stream_file_with_rec_lists()).unwrap();
        let tracks = stream_tracks(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let kinds: Vec<(Kind, usize)> = tracks.iter().map(|t| (t.kind, t.number)).collect();
        assert_eq!(kinds, [(Kind::Video, 1), (Kind::Audio, 1), (Kind::Audio, 2)]);
        assert_eq!(tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(tracks.iter().all(|t| t.language == "und" && !t.default && !t.forced));
    }

    #[test]
    fn a_codec_is_named_as_mediainfo_names_it() {
        assert_eq!(video_codec(&{ let mut f = vec![0u8; 40]; f[16..20].copy_from_slice(b"DIVX"); f }), "MPEG-4 Visual");
        assert_eq!(video_codec(&vec![0u8; 40]), "RGB");
        assert_eq!(audio_codec(&[0x50, 0x00, 0x02, 0x00]), ("MPEG Audio".to_string(), 2));
        assert_eq!(audio_codec(&[0x01, 0x00, 0x01, 0x00]), ("PCM".to_string(), 1));
        assert_eq!(audio_codec(&[0x00, 0x20, 0x06, 0x00]), ("AC-3".to_string(), 6));
    }

    /// The same file, with each index offset counted from the start of the file (as some writers do).
    fn with_absolute_offsets(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        let movi = bytes.windows(4).position(|w| w == b"movi").unwrap();
        let index = bytes.windows(4).rposition(|w| w == b"idx1").unwrap();
        let count = u32::from_le_bytes([bytes[index + 4], bytes[index + 5], bytes[index + 6], bytes[index + 7]]) / 16;
        for i in 0..count as usize {
            let at = index + 8 + 16 * i + 8;
            let relative = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            out[at..at + 4].copy_from_slice(&(relative + movi as u32).to_le_bytes());
        }
        out
    }

    #[test]
    fn index_offsets_counted_from_the_start_of_the_file_are_kept_that_way() {
        let before = with_absolute_offsets(&three_stream_file_with_rec_lists());
        let after = remove_in("absolute", &before, &[1]).unwrap();
        assert!(riff_size_is_right(&after));
        assert!(index_points_from(&after, 0), "the offsets are still counted from the start of the file");
        let (kinds, ids, _) = read_streams(&after);
        assert_eq!(kinds, [b"vids".to_vec(), b"auds".to_vec()]);
        assert_eq!(ids, [*b"00dc", *b"01wb", *b"00dc", *b"01wb"]);
    }

    /// Frames whose video, first audio and second audio chunks are each in a rec list of their own.
    fn three_stream_file_one_rec_list_per_stream() -> Vec<u8> {
        let hdrl = list(b"hdrl", &[chunk(b"avih", &[0u8; 56]), stream(b"vids", 40), stream(b"auds", 16), stream(b"auds", 18)]);
        let mut movi_body = b"movi".to_vec();
        let mut index = Vec::new();
        for frame in 0..2u8 {
            for c in [chunk(b"00dc", &[frame; 6]), chunk(b"01wb", &[1, frame, 1]), chunk(b"02wb", &[2, frame])] {
                let rec_at = movi_body.len() as u32;
                let rec = list(b"rec ", &[c.clone()]);
                index.extend_from_slice(b"rec ");
                index.extend_from_slice(&[0x01, 0, 0, 0]);
                index.extend_from_slice(&rec_at.to_le_bytes());
                index.extend_from_slice(&((rec.len() - 8) as u32).to_le_bytes());
                // the chunk's own entry, at its place in the rec list (12 bytes after the list header's start)
                index.extend_from_slice(&c[0..4]);
                index.extend_from_slice(&[0x10, 0, 0, 0]);
                index.extend_from_slice(&(rec_at + 12).to_le_bytes());
                index.extend_from_slice(&(u32::from_le_bytes([c[4], c[5], c[6], c[7]])).to_le_bytes());
                movi_body.extend(rec);
            }
        }
        let movi = chunk(b"LIST", &movi_body);
        riff(&[hdrl, movi, chunk(b"idx1", &index)])
    }

    #[test]
    fn a_rec_list_whose_only_stream_is_removed_goes_with_its_index_entries() {
        let before = three_stream_file_one_rec_list_per_stream();
        let after = remove_in("emptyrec", &before, &[1]).unwrap();
        assert!(riff_size_is_right(&after));
        let (kinds, ids, _) = read_streams(&after);
        assert_eq!(kinds, [b"vids".to_vec(), b"auds".to_vec()]);
        assert_eq!(ids, [*b"00dc", *b"01wb", *b"00dc", *b"01wb"]);
        // no index entry is left for a list that is gone: every entry names something in the file
        assert!(index_points_at_its_chunks(&after));
        // six rec lists before (two frames, three streams each), and the two of the removed stream are gone
        let recs = after.windows(12).filter(|w| &w[0..4] == b"LIST" && &w[8..12] == b"rec ").count();
        assert_eq!(recs, 4, "the rec lists of the removed stream are dropped");
    }

    #[test]
    fn streams_that_cannot_be_removed_are_refused_and_nothing_changes() {
        let before = three_stream_file();
        assert!(remove_in("all", &before, &[0, 1, 2]).is_err());
        assert!(remove_in("twice", &before, &[1, 1]).is_err());
        assert!(remove_in("missing", &before, &[5]).is_err());
    }

    #[test]
    fn an_order_that_does_not_name_each_stream_once_is_refused_and_nothing_changes() {
        let dir = std::env::temp_dir().join(format!("mau-avi-refuse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("three.avi");
        let before = three_stream_file();
        std::fs::write(&path, &before).unwrap();
        assert!(reorder_streams_in_place(&path, &[0, 1]).is_err());
        assert!(reorder_streams_in_place(&path, &[0, 1, 1]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
