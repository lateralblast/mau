//! Reading the text subtitle formats that are added to an MP4 in this program: SubStationAlpha (`ass` and `ssa`) and
//! WebVTT (`vtt`). Each is read as cues (start, end, text) in milliseconds, which are then written as `mov_text`.
//! The styling is not kept (an MP4 subtitle here is plain text): override blocks, tags and positions are dropped, and
//! line breaks and entities are made plain text.

use std::io;
use std::path::Path;

/// A cue: its start and end in milliseconds, and its text (plain, with `\n` between lines).
pub type Cue = (i64, i64, Vec<u8>);

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// The text of a file: a byte order mark is removed and CRLF line ends are made `\n`.
fn text_of(data: &[u8]) -> io::Result<String> {
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(data);
    Ok(String::from_utf8(data.to_vec()).map_err(|_| invalid("the subtitle file is not UTF-8 text"))?.replace("\r\n", "\n"))
}

/// The cues of a SubStationAlpha (`.ass` or `.ssa`) file, from its `[Events]` section.
pub fn read_ass(path: &Path) -> io::Result<Vec<Cue>> {
    parse_ass(&std::fs::read(path)?)
}

/// An ASS file as an MKV's ASS track holds it: the header (every line before the first event, with CRLF line ends, as
/// mkvmerge writes the codec private data) and the cues. A cue's payload is `ReadOrder,Layer,Style,Name,MarginL,
/// MarginR,MarginV,Effect,Text`, with the text as it is in the file (override tags kept): the times are the cue's
/// start and end. `mkvextract` rebuilds the file from these.
pub fn read_ass_for_mkv(path: &Path) -> io::Result<(Vec<u8>, Vec<Cue>)> {
    read_styled_for_mkv(path, false)
}

/// An SSA file as an MKV's `S_TEXT/SSA` track holds it: as `read_ass_for_mkv`, but the column after the times is
/// `Marked` (in the Dialogue lines written as `Marked=0`), and its value is stored without the `Marked=` part.
pub fn read_ssa_for_mkv(path: &Path) -> io::Result<(Vec<u8>, Vec<Cue>)> {
    read_styled_for_mkv(path, true)
}

fn read_styled_for_mkv(path: &Path, ssa: bool) -> io::Result<(Vec<u8>, Vec<Cue>)> {
    let text = text_of(&std::fs::read(path)?)?;
    let mut header = String::new();
    let mut cues = Vec::new();
    let mut in_events = false;
    let mut columns: Vec<String> = ASS_COLUMNS.iter().map(|c| c.to_string()).collect();
    let mut started = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_events = trimmed.eq_ignore_ascii_case("[events]");
        }
        if !started && strip_key(trimmed, "dialogue:").is_none() {
            header.push_str(line);
            header.push_str("\r\n");
            if in_events {
                if let Some(format) = strip_key(trimmed, "format:") {
                    columns = format.split(',').map(|c| c.trim().to_ascii_lowercase()).collect();
                }
            }
            continue;
        }
        started = true;
        if !in_events {
            continue;
        }
        let Some(dialogue) = strip_key(trimmed, "dialogue:") else { continue };
        let at = |name: &str| columns.iter().position(|c| c == name).ok_or_else(|| invalid("a Dialogue line has no such column"));
        let (start_at, end_at, text_at) = (at("start")?, at("end")?, at("text")?);
        let fields: Vec<&str> = dialogue.splitn(columns.len(), ',').collect();
        if fields.len() < columns.len() {
            return Err(invalid("a Dialogue line is missing its fields"));
        }
        let (start, end) = (ass_time(fields[start_at])?, ass_time(fields[end_at])?);
        // every column but the times, in file order, with the text last
        let mut payload = format!("{},", cues.len());
        for (index, _name) in columns.iter().enumerate() {
            if index == start_at || index == end_at {
                continue;
            }
            // an SSA `Marked=0` is stored as its value alone
            let value = match fields[index].trim().strip_prefix("Marked=") {
                Some(marked) if ssa && columns[index] == "marked" => marked,
                _ => fields[index].trim(),
            };
            if index == text_at {
                payload.push_str(fields[index]);
            } else {
                payload.push_str(value);
                payload.push(',');
            }
        }
        cues.push((start, end, payload.into_bytes()));
    }
    Ok((header.into_bytes(), cues))
}

/// A WebVTT file as an MKV's WebVTT track holds it: the header (the blocks before the first cue, as mkvmerge stores
/// them in the codec private data) and the cues, each with its text as written (tags kept), its times, and its
/// addition 1 (`settings`, a line break, the identifier, a line break; none when the cue has neither), as mkvmerge
/// writes it. A NOTE, STYLE or REGION block after the first cue is refused: it has no place in the track.
pub fn read_vtt_for_mkv(path: &Path) -> io::Result<(Vec<u8>, Vec<(i64, i64, Vec<u8>, Vec<(u64, Vec<u8>)>)>)> {
    let text = text_of(&std::fs::read(path)?)?;
    let first = text.lines().next().unwrap_or("");
    if !first.trim_start_matches('\u{feff}').starts_with("WEBVTT") {
        return Err(invalid("not a WebVTT file"));
    }
    let blocks: Vec<&str> = text.split("\n\n").collect();
    let first_cue = blocks.iter().position(|b| b.contains("-->")).unwrap_or(blocks.len());
    let header = blocks[..first_cue].join("\n\n").trim_end().to_string();
    let mut cues = Vec::new();
    for block in &blocks[first_cue..] {
        let lines: Vec<&str> = block.lines().skip_while(|l| l.trim().is_empty()).collect();
        if lines.is_empty() {
            continue;
        }
        let at = if lines[0].contains("-->") { 0 } else { 1 };
        let Some(timing) = lines.get(at).filter(|l| l.contains("-->")) else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a block after the first cue is not a cue (a NOTE, STYLE or REGION block)",
            ));
        };
        let identifier = if at == 1 { lines[0] } else { "" };
        let (before, after) = timing.split_once("-->").ok_or_else(|| invalid("a cue has no end time"))?;
        let end_token = after.split_whitespace().next().ok_or_else(|| invalid("a cue has no end time"))?;
        let settings = after.trim_start().strip_prefix(end_token).unwrap_or("").trim();
        let (start, end) = (vtt_time(before.trim())?, vtt_time(end_token)?);
        let body = lines[at + 1..].join("\n");
        let additions = if identifier.is_empty() && settings.is_empty() {
            Vec::new()
        } else {
            vec![(1, format!("{settings}\n{identifier}\n").into_bytes())]
        };
        cues.push((start, end, body.into_bytes(), additions));
    }
    Ok((header.into_bytes(), cues))
}

/// The cues of a WebVTT (`.vtt`) file.
pub fn read_vtt(path: &Path) -> io::Result<Vec<Cue>> {
    parse_vtt(&std::fs::read(path)?)
}

const ASS_COLUMNS: [&str; 10] = ["layer", "start", "end", "style", "name", "marginl", "marginr", "marginv", "effect", "text"];

fn parse_ass(data: &[u8]) -> io::Result<Vec<Cue>> {
    let text = text_of(data)?;
    let mut in_events = false;
    let mut columns: Option<Vec<String>> = None;
    let mut cues = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_events = line.eq_ignore_ascii_case("[events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(format) = strip_key(line, "format:") {
            columns = Some(format.split(',').map(|c| c.trim().to_ascii_lowercase()).collect());
            continue;
        }
        let Some(dialogue) = strip_key(line, "dialogue:") else { continue };
        let names: Vec<String> = match &columns {
            Some(columns) => columns.clone(),
            None => ASS_COLUMNS.iter().map(|c| c.to_string()).collect(),
        };
        let at = |name: &str| names.iter().position(|c| c == name).ok_or_else(|| invalid("a Dialogue line has no such column"));
        let (start_at, end_at, text_at) = (at("start")?, at("end")?, at("text")?);
        // the text is the last column, so it may hold commas: only the columns before it are split off
        let fields: Vec<&str> = dialogue.splitn(names.len(), ',').collect();
        if fields.len() < names.len() {
            return Err(invalid("a Dialogue line is missing its fields"));
        }
        let (start, end) = (ass_time(fields[start_at])?, ass_time(fields[end_at])?);
        let body = plain_ass(fields[text_at]);
        if !body.is_empty() {
            cues.push((start, end, body.into_bytes()));
        }
    }
    Ok(cues)
}

/// The rest of a line after `key` (matched without regard to case), if the line starts with it.
fn strip_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    if line.len() >= key.len() && line[..key.len()].eq_ignore_ascii_case(key) {
        Some(line[key.len()..].trim_start())
    } else {
        None
    }
}

/// An ASS time, `h:mm:ss.cc` (hundredths; a three digit fraction is milliseconds), as milliseconds.
fn ass_time(time: &str) -> io::Result<i64> {
    let time = time.trim();
    let (clock, fraction) = time.split_once('.').ok_or_else(|| invalid("a time has no fraction"))?;
    let parts: Vec<i64> = clock.split(':').map(|p| p.parse::<i64>()).collect::<Result<_, _>>().map_err(|_| invalid("a time is not a number"))?;
    if parts.len() != 3 || fraction.is_empty() || fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("a time is not h:mm:ss.cc"));
    }
    let fraction: i64 = fraction.parse().map_err(|_| invalid("a time is not a number"))?;
    let millis = match fraction_digits(time) {
        2 => fraction * 10,
        3 => fraction,
        1 => fraction * 100,
        _ => fraction,
    };
    Ok(((parts[0] * 60 + parts[1]) * 60 + parts[2]) * 1000 + millis)
}

fn fraction_digits(time: &str) -> usize {
    time.rsplit_once('.').map_or(0, |(_, f)| f.len())
}

/// The plain text of an ASS line: the override blocks (`{...}`) are dropped, `\N` and `\n` are line breaks, `\h` is a
/// space.
fn plain_ass(text: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => in_block = true,
            '}' => in_block = false,
            _ if in_block => {}
            '\\' => match chars.peek() {
                Some('N') | Some('n') => {
                    chars.next();
                    out.push('\n');
                }
                Some('h') => {
                    chars.next();
                    out.push(' ');
                }
                _ => out.push('\\'),
            },
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

fn parse_vtt(data: &[u8]) -> io::Result<Vec<Cue>> {
    let text = text_of(data)?;
    let first = text.lines().next().unwrap_or("");
    if !first.trim_start_matches('\u{feff}').starts_with("WEBVTT") {
        return Err(invalid("not a WebVTT file"));
    }
    let mut cues = Vec::new();
    // a blank line ends each block; NOTE, STYLE and REGION blocks have no timing line and are skipped
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.lines().skip_while(|l| l.trim().is_empty()).collect();
        let Some(at) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let (before, after) = lines[at].split_once("-->").ok_or_else(|| invalid("a cue has no end time"))?;
        let end = after.split_whitespace().next().ok_or_else(|| invalid("a cue has no end time"))?;
        let (start, end) = (vtt_time(before.trim())?, vtt_time(end)?);
        let body = plain_vtt(&lines[at + 1..].join("\n"));
        if !body.is_empty() {
            cues.push((start, end, body.into_bytes()));
        }
    }
    Ok(cues)
}

/// A WebVTT time, `[hh:]mm:ss.mmm`, as milliseconds.
fn vtt_time(time: &str) -> io::Result<i64> {
    let (clock, millis) = time.split_once('.').ok_or_else(|| invalid("a cue time has no milliseconds"))?;
    let parts: Vec<i64> = clock.split(':').map(|p| p.parse::<i64>()).collect::<Result<_, _>>().map_err(|_| invalid("a cue time is not a number"))?;
    if !(2..=3).contains(&parts.len()) || millis.len() != 3 || !millis.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("a cue time is not [hh:]mm:ss.mmm"));
    }
    let (hours, minutes, seconds) = if parts.len() == 3 { (parts[0], parts[1], parts[2]) } else { (0, parts[0], parts[1]) };
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis.parse::<i64>().map_err(|_| invalid("a cue time is not a number"))?)
}

/// The plain text of a cue: tags (`<i>`, `<c.colour>`, voice spans) are dropped and the entities made characters.
fn plain_vtt(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            _ => out.push(c),
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_dialogue_lines_become_cues_with_plain_text() {
        let data = b"[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,{\\an8}Ciao\\Nsecond line\nDialogue: 0,0:00:03.00,0:00:04.25,Default,,0,0,0,,{\\b1}Bold{\\b0} text, with comma\nComment: 0,0:00:05.00,0:00:06.00,Default,,0,0,0,,not a cue\n";
        let cues = parse_ass(data).unwrap();
        assert_eq!(cues, vec![
            (500, 2000, b"Ciao\nsecond line".to_vec()),
            (3000, 4250, b"Bold text, with comma".to_vec()),
        ]);
    }

    #[test]
    fn an_ass_file_gives_the_header_and_each_event_as_an_mkv_block() {
        let data = b"[Script Info]\r\nTitle: x\r\n\r\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.50,0:00:02.00,Default,Anna,10,10,5,,Hello, {\\i1}world{\\i0}\nDialogue: 1,0:00:03.00,0:00:04.00,Top,,0,0,0,,x\n";
        let dir = std::env::temp_dir().join(format!("mau-ass-block-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("block.ass");
        std::fs::write(&path, data).unwrap();
        let (header, cues) = read_ass_for_mkv(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(String::from_utf8(header.clone()).unwrap().ends_with("Effect, Text\r\n"));
        assert_eq!(cues, vec![
            (500, 2000, b"0,0,Default,Anna,10,10,5,,Hello, {\\i1}world{\\i0}".to_vec()),
            (3000, 4000, b"1,1,Top,,0,0,0,,x".to_vec()),
        ]);
    }

    #[test]
    fn ass_times_are_centiseconds_or_milliseconds() {
        assert_eq!(ass_time("1:02:03.45").unwrap(), 3723450);
        assert_eq!(ass_time("0:00:01.234").unwrap(), 1234);
        assert!(ass_time("0:00:01").is_err());
    }

    #[test]
    fn webvtt_cues_skip_the_header_notes_and_tags() {
        let data = b"WEBVTT\n\nNOTE a comment\n\nintro\n00:00:00.500 --> 00:00:02.000 align:start\n<i>Ciao</i> &amp; hi\n\nSTYLE\n::cue { color: red }\n\n00:03.000 --> 00:04.250\nSeconda riga\nche continua\n";
        let cues = parse_vtt(data).unwrap();
        assert_eq!(cues, vec![
            (500, 2000, b"Ciao & hi".to_vec()),
            (3000, 4250, b"Seconda riga\nche continua".to_vec()),
        ]);
    }

    #[test]
    fn a_file_that_is_not_webvtt_is_refused() {
        assert!(parse_vtt(b"1\n00:00:00,000 --> 00:00:01,000\nnot webvtt\n").is_err());
    }
}
