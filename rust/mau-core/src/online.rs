//! Searching OpenSubtitles (the REST API at api.opensubtitles.com) for subtitles, and downloading one.
//!
//! The requests are made with `curl`, which mau already uses to download the whisper.cpp model.
//! The API key, the user name and the password never go on a command line (where `ps` shows
//! them): each request is a `curl --config` file that only the user can read, removed after.
//!
//! A search needs the API key. A download also needs a login (the free account is enough),
//! because the service counts downloads per user.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::languages::{all, resolve_language};
use crate::model::SUBTITLE_FILES;
use crate::util::{run_command, Ran};

/// The API the requests go to, unless `MAU_OPENSUBTITLES_URL` names another (a test server).
pub const API_URL: &str = "https://api.opensubtitles.com/api/v1";

/// What identifies mau to the service (it asks for the name and version of the application).
fn user_agent() -> String {
    format!("mau v{}", crate::VERSION)
}

/// What is needed to use the service: an API key for searching, and a login for downloading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub api_key: String,
    pub username: String,
    pub password: String,
}

/// Where the credentials are kept: `opensubtitles.json` in mau's data folder.
pub fn credentials_path() -> Option<PathBuf> {
    crate::detect::data_folder().map(|folder| crate::detect::app_folder(&folder).join("opensubtitles.json"))
}

/// The saved credentials; `MAU_OPENSUBTITLES_KEY`, `MAU_OPENSUBTITLES_USER` and
/// `MAU_OPENSUBTITLES_PASSWORD` take the place of what is saved, one by one.
pub fn load_credentials() -> Credentials {
    let mut saved = credentials_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Credentials>(&text).ok())
        .unwrap_or_default();
    let from_env = |name: &str| crate::util::mau_env(name).ok().filter(|v| !v.trim().is_empty());
    if let Some(v) = from_env("OPENSUBTITLES_KEY") { saved.api_key = v; }
    if let Some(v) = from_env("OPENSUBTITLES_USER") { saved.username = v; }
    if let Some(v) = from_env("OPENSUBTITLES_PASSWORD") { saved.password = v; }
    saved
}

/// Save the credentials where only the user can read them (mode 600 on Unix).
pub fn save_credentials(credentials: &Credentials) -> Result<(), String> {
    let path = credentials_path().ok_or("there is no folder to keep the settings in")?;
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(|e| format!("could not make {}: {e}", folder.display()))?;
    }
    let text = serde_json::to_string_pretty(credentials).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    private_file(&temp, text.as_bytes()).map_err(|e| format!("could not save {}: {e}", path.display()))?;
    fs::rename(&temp, &path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("could not save {}: {e}", path.display())
    })
}

/// Write a file that only its owner can read.
fn private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_private(path, bytes, false)
}

/// Write a file that only its owner can read, and only if there is nothing at `path` yet: in a folder that others can
/// write to (the temporary folder) a name that is easy to guess could be a link to a file of the owner's, which
/// writing to would overwrite, and a file that was there already could be readable by others.
fn private_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_private(path, bytes, true)
}

fn write_private(path: &Path, bytes: &[u8], new_only: bool) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if new_only {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

/// The OpenSubtitles hash of a video file: its size plus the sum, as 64-bit little endian words,
/// of its first and last 64 KB. It names the exact file without sending any of it. A file of
/// less than 128 KB has none.
pub fn movie_hash(path: &Path) -> Result<String, String> {
    const CHUNK: u64 = 65536;
    let mut file = fs::File::open(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size < CHUNK * 2 {
        return Err("the file is too small to have a hash".into());
    }
    let mut hash = size;
    let mut buffer = vec![0u8; CHUNK as usize];
    for start in [0, size - CHUNK] {
        file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        file.read_exact(&mut buffer).map_err(|e| e.to_string())?;
        for word in buffer.chunks_exact(8) {
            hash = hash.wrapping_add(u64::from_le_bytes(word.try_into().unwrap()));
        }
    }
    Ok(format!("{hash:016x}"))
}

/// What a file name says about what it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guess {
    pub title: String,
    pub year: Option<u32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

/// Guess the title, the year and the season and episode from a file name: `Show.Name.S01E03.1080p.mkv`
/// is `Show Name`, season 1, episode 3; `Film.Name.2019.BluRay.mkv` is `Film Name` and 2019.
pub fn guess_query(file_name: &str) -> Guess {
    let stem = Path::new(file_name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let words: Vec<String> = stem
        .split(|c: char| matches!(c, '.' | '_' | ' ' | '-' | '[' | ']' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();
    let mut guess = Guess::default();
    let mut end = words.len();
    for (index, word) in words.iter().enumerate() {
        if let Some((season, episode)) = season_episode(word) {
            guess.season = Some(season);
            guess.episode = Some(episode);
            end = index;
            break;
        }
        let year = word.parse::<u32>().ok().filter(|y| (1900..=2099).contains(y) && word.len() == 4);
        if year.is_some() && index > 0 {
            guess.year = year;
            end = index;
            break;
        }
        let lower = word.to_lowercase();
        let junk = ["480p", "576p", "720p", "1080p", "2160p", "4k", "web", "webrip", "bluray", "brrip",
                    "bdrip", "hdtv", "dvdrip", "x264", "x265", "hevc", "remux"];
        if index > 0 && junk.contains(&lower.as_str()) {
            end = index;
            break;
        }
    }
    guess.title = words[..end].join(" ");
    guess
}

/// `S01E03` or `1x03`, as (1, 3).
fn season_episode(word: &str) -> Option<(u32, u32)> {
    let lower = word.to_lowercase();
    let digits = |text: &str| -> Option<u32> {
        if !text.is_empty() && text.len() <= 4 && text.bytes().all(|b| b.is_ascii_digit()) { text.parse().ok() } else { None }
    };
    if let Some(rest) = lower.strip_prefix('s') {
        let (season, episode) = rest.split_once('e')?;
        return Some((digits(season)?, digits(episode)?));
    }
    let (season, episode) = lower.split_once('x')?;
    Some((digits(season)?, digits(episode)?))
}

/// A subtitle the service offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub file_id: i64,
    /// ISO 639-2/B, or what the service said when that is not a known language.
    pub language: String,
    pub release: String,
    pub file_name: String,
    pub downloads: i64,
    pub hearing_impaired: bool,
    /// Subtitles for the foreign speech only.
    pub forced: bool,
    pub machine_translated: bool,
    /// Made for this very file (the hash matches).
    pub exact: bool,
}

/// The subtitles in a search response, the ones made for this file first, then the most downloaded.
pub fn parse_search(data: &Value) -> Vec<Hit> {
    let mut hits = Vec::new();
    for item in data.get("data").and_then(Value::as_array).into_iter().flatten() {
        let attr = &item["attributes"];
        let Some(file) = attr["files"].as_array().and_then(|files| files.first()) else { continue };
        let Some(file_id) = file["file_id"].as_i64() else { continue };
        let code = attr["language"].as_str().unwrap_or("").split('-').next().unwrap_or("").to_string();
        let flag = |name: &str| attr[name].as_bool().unwrap_or(false);
        hits.push(Hit {
            file_id,
            language: resolve_language(&code).unwrap_or(code),
            release: attr["release"].as_str().unwrap_or("").to_string(),
            file_name: file["file_name"].as_str().unwrap_or("").to_string(),
            downloads: attr["download_count"].as_i64().unwrap_or(0),
            hearing_impaired: flag("hearing_impaired"),
            forced: flag("foreign_parts_only"),
            machine_translated: flag("machine_translated") || flag("ai_translated"),
            exact: flag("moviehash_match"),
        });
    }
    hits.sort_by(|a, b| b.exact.cmp(&a.exact).then(b.downloads.cmp(&a.downloads)));
    hits
}

fn urlencode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The two letter codes the service takes for languages (ISO 639-2/B in, ISO 639-1 out).
fn two_letter(languages: &[String]) -> Vec<&'static str> {
    languages
        .iter()
        .filter_map(|lang| all().iter().find(|row| row.0 == lang && !row.2.is_empty()).map(|row| row.2))
        .collect()
}

/// A curl config string: in quotes, with the escapes curl reads.
fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

static CONFIGS: AtomicUsize = AtomicUsize::new(0);

/// One request to the API: the answer as JSON, or why there is none.
fn api(base: &str, credentials: &Credentials, path: &str, body: Option<&Value>, token: Option<&str>) -> Result<Value, String> {
    if credentials.api_key.trim().is_empty() {
        return Err("there is no OpenSubtitles API key: save one under \"OpenSubtitles account\" in the app's subtitle search, or set MAU_OPENSUBTITLES_KEY".into());
    }
    let mut config = format!("url = {}\nsilent\nshow-error\nmax-time = 30\n", quoted(&format!("{base}/{path}")));
    let mut header = |text: String| config.push_str(&format!("header = {}\n", quoted(&text)));
    header(format!("Api-Key: {}", credentials.api_key.trim()));
    header(format!("User-Agent: {}", user_agent()));
    header("Accept: application/json".into());
    if let Some(token) = token {
        header(format!("Authorization: Bearer {token}"));
    }
    if let Some(body) = body {
        header("Content-Type: application/json".into());
        config.push_str(&format!("data = {}\n", quoted(&body.to_string())));
    }
    config.push_str("write-out = \"\\n%{http_code}\"\n");
    let file = std::env::temp_dir()
        .join(format!("mau-curl-{}-{}.conf", std::process::id(), CONFIGS.fetch_add(1, Ordering::SeqCst)));
    private_new_file(&file, config.as_bytes()).map_err(|e| format!("could not prepare the request: {e}"))?;
    let ran = run_command(&["curl".to_string(), "--config".to_string(), file.to_string_lossy().into_owned()]);
    let _ = fs::remove_file(&file);
    let done = match ran {
        Ok(Ran::Done(done)) => done,
        Ok(Ran::Cancelled) => return Err("stopped".into()),
        Err(error) => return Err(format!("could not run curl (it is needed to reach OpenSubtitles): {error}")),
    };
    if done.code != 0 {
        return Err(format!("could not reach OpenSubtitles: {}", done.stderr.trim()));
    }
    let (text, status) = done.stdout.rsplit_once('\n').unwrap_or((&done.stdout, ""));
    let status: u32 = status.trim().parse().unwrap_or(0);
    let json: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    if status >= 400 || status == 0 {
        let why = json["message"].as_str().or_else(|| json["errors"][0].as_str()).unwrap_or("").to_string();
        return Err(match status {
            401 => format!("OpenSubtitles did not accept the login or API key{}", suffix(&why)),
            406 => format!("the daily download limit of this OpenSubtitles account is used up{}", suffix(&why)),
            429 => "OpenSubtitles asked for fewer requests: try again in a minute".to_string(),
            _ => format!("OpenSubtitles answered {status}{}", suffix(&why)),
        });
    }
    Ok(json)
}

fn suffix(why: &str) -> String {
    if why.is_empty() { String::new() } else { format!(": {why}") }
}

fn base_url() -> String {
    crate::util::mau_env("OPENSUBTITLES_URL").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| API_URL.to_string())
}

/// Search for subtitles for a media file: by its hash when it has one, and by `query` (the title,
/// guessed from the name when empty), in `languages` (ISO 639-2/B; none means every language).
pub fn search(credentials: &Credentials, media: &Path, query: &str, languages: &[String]) -> Result<Vec<Hit>, String> {
    search_at(&base_url(), credentials, media, query, languages)
}

fn search_at(base: &str, credentials: &Credentials, media: &Path, query: &str, languages: &[String]) -> Result<Vec<Hit>, String> {
    let name = media.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let guess = guess_query(&name);
    let query = if query.trim().is_empty() { guess.title.clone() } else { query.trim().to_string() };
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Ok(hash) = movie_hash(media) {
        params.push(("moviehash", hash));
    }
    if !query.is_empty() {
        params.push(("query", query.to_lowercase()));
    }
    // the season and the episode belong to the title that was guessed, not to a title typed in
    if query.eq_ignore_ascii_case(&guess.title) {
        if let (Some(season), Some(episode)) = (guess.season, guess.episode) {
            params.push(("season_number", season.to_string()));
            params.push(("episode_number", episode.to_string()));
            params.push(("type", "episode".into()));
        } else if let Some(year) = guess.year {
            params.push(("year", year.to_string()));
            params.push(("type", "movie".into()));
        }
    }
    if params.is_empty() {
        return Err("there is nothing to search for: type a title".into());
    }
    let codes = two_letter(languages);
    if !codes.is_empty() {
        params.push(("languages", codes.join(",")));
    }
    params.sort_by(|a, b| a.0.cmp(b.0));
    let text: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect();
    let json = api(base, credentials, &format!("subtitles?{}", text.join("&")), None, None)?;
    Ok(parse_search(&json))
}

/// The best subtitle in a language among search results (already sorted, the ones made for the file
/// first): a normal one, else a hearing impaired one. Machine translated and forced subtitles are
/// never picked for you.
pub fn best_hit<'a>(hits: &'a [Hit], language: &str) -> Option<&'a Hit> {
    let here = |hit: &&Hit| language.is_empty() || hit.language == language;
    let usable = |hit: &&Hit| !hit.forced && !hit.machine_translated;
    hits.iter().filter(here).filter(usable).find(|h| !h.hearing_impaired)
        .or_else(|| hits.iter().filter(here).filter(usable).next())
}

/// The login of this run, so a folder of files logs in once (the service limits logins).
static SESSION: std::sync::Mutex<Option<(String, String, String)>> = std::sync::Mutex::new(None);

/// Download a subtitle to the temporary folder, and say where it is. This needs a login.
pub fn download(credentials: &Credentials, file_id: i64) -> Result<PathBuf, String> {
    download_at(&base_url(), credentials, file_id)
}

fn download_at(base: &str, credentials: &Credentials, file_id: i64) -> Result<PathBuf, String> {
    if credentials.username.trim().is_empty() || credentials.password.is_empty() {
        return Err("downloading needs an OpenSubtitles login (a free account is enough): save the user name and \
                    password under \"OpenSubtitles account\" in the app's subtitle search, or set MAU_OPENSUBTITLES_USER and \
                    MAU_OPENSUBTITLES_PASSWORD"
            .into());
    }
    let who = (credentials.username.trim().to_string(), base.to_string());
    let known = SESSION.lock().unwrap().clone().filter(|s| (s.0.clone(), s.1.clone()) == who).map(|s| s.2);
    let log_in = || -> Result<String, String> {
        let login = api(
            base,
            credentials,
            "login",
            Some(&json!({"username": credentials.username.trim(), "password": credentials.password})),
            None,
        )?;
        let token = login["token"].as_str().ok_or("OpenSubtitles gave no login token")?.to_string();
        *SESSION.lock().unwrap() = Some((who.0.clone(), who.1.clone(), token.clone()));
        Ok(token)
    };
    let body = json!({"file_id": file_id, "sub_format": "srt"});
    let answer = match known {
        // a token that has run out is replaced once
        Some(token) => match api(base, credentials, "download", Some(&body), Some(&token)) {
            Err(e) if e.contains("did not accept") => {
                let token = log_in()?;
                api(base, credentials, "download", Some(&body), Some(&token))?
            }
            other => other?,
        },
        None => {
            let token = log_in()?;
            api(base, credentials, "download", Some(&body), Some(&token))?
        }
    };
    let link = answer["link"].as_str().ok_or("OpenSubtitles gave no download link")?;
    let extension = answer["file_name"]
        .as_str()
        .and_then(|name| Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()))
        .filter(|e| SUBTITLE_FILES.contains(&e.as_str()))
        .unwrap_or_else(|| "srt".into());
    let target = std::env::temp_dir().join(format!("{TEMP_PREFIX}{file_id}.{extension}"));
    // a name that others could have made a link to a file of ours is cleared first (removing a link removes only the link)
    let _ = fs::remove_file(&target);
    let cmd: Vec<String> = ["curl", "--fail", "--location", "--silent", "--show-error", "--max-time", "60", "--output"]
        .iter()
        .map(|s| s.to_string())
        .chain([target.to_string_lossy().into_owned(), link.to_string()])
        .collect();
    match run_command(&cmd) {
        Ok(Ran::Done(done)) if done.code == 0 => {}
        Ok(Ran::Done(done)) => {
            let _ = fs::remove_file(&target);
            return Err(format!("could not download the subtitle: {}", done.stderr.trim()));
        }
        Ok(Ran::Cancelled) => return Err("stopped".into()),
        Err(error) => return Err(format!("could not run curl: {error}")),
    }
    if fs::metadata(&target).map(|m| m.len()).unwrap_or(0) == 0 {
        let _ = fs::remove_file(&target);
        return Err("the downloaded subtitle is empty".into());
    }
    Ok(target)
}

/// What the names of downloaded subtitles start with.
pub const TEMP_PREFIX: &str = "mau-subtitle-";

/// Remove a subtitle that `download` made. Anything else is left alone.
pub fn forget_download(path: &Path) {
    let ours = path.parent() == Some(std::env::temp_dir().as_path())
        && path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(TEMP_PREFIX));
    if ours {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_file_is_never_written_over_something_that_is_there() {
        let dir = std::env::temp_dir().join(format!("mau-private-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target.txt");
        fs::write(&target, "keep").unwrap();
        let link = dir.join("request.conf");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(not(unix))]
        fs::write(&link, "there").unwrap();
        assert!(private_new_file(&link, b"secret").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        let free = dir.join("free.conf");
        private_new_file(&free, b"secret").unwrap();
        assert_eq!(fs::read_to_string(&free).unwrap(), "secret");
        let _ = fs::remove_dir_all(&dir);
    }
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mau-online-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_hash_is_the_size_plus_the_words_of_both_ends() {
        let dir = scratch("hash");
        let file = dir.join("a.mkv");
        let mut bytes = vec![0u8; 200_000];
        fs::write(&file, &bytes).unwrap();
        assert_eq!(movie_hash(&file).unwrap(), format!("{:016x}", 200_000u64));
        bytes[0] = 1; // the first word
        bytes[200_000 - 8] = 2; // the last word
        bytes[100_000] = 99; // the middle is not read
        fs::write(&file, &bytes).unwrap();
        assert_eq!(movie_hash(&file).unwrap(), format!("{:016x}", 200_000u64 + 1 + 2));
        fs::write(&file, vec![0u8; 1000]).unwrap();
        assert!(movie_hash(&file).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn titles_seasons_and_years_are_guessed_from_names() {
        let g = guess_query("Example.Show.S01E03.1080p.WEB-DL.mkv");
        assert_eq!((g.title.as_str(), g.season, g.episode, g.year), ("Example Show", Some(1), Some(3), None));
        let g = guess_query("Some_Film.2019.BluRay.x264.mp4");
        assert_eq!((g.title.as_str(), g.year, g.season), ("Some Film", Some(2019), None));
        let g = guess_query("Show Name 2x07 - Title.mkv");
        assert_eq!((g.title.as_str(), g.season, g.episode), ("Show Name", Some(2), Some(7)));
        // a title that is a year, and a title with no marker at all
        assert_eq!(guess_query("1917.2019.1080p.mkv").title, "1917");
        assert_eq!(guess_query("home movie.mkv").title, "home movie");
    }

    #[test]
    fn a_search_answer_is_listed_exact_matches_first() {
        let data = json!({"data": [
            {"attributes": {"language": "it", "download_count": 900, "release": "A", "files": [{"file_id": 1, "file_name": "a.srt"}]}},
            {"attributes": {"language": "pt-BR", "download_count": 5, "moviehash_match": true, "hearing_impaired": true,
                            "release": "B", "files": [{"file_id": 2, "file_name": "b.srt"}]}},
            {"attributes": {"language": "en", "download_count": 50, "foreign_parts_only": true, "ai_translated": true,
                            "files": [{"file_id": 3}]}},
            {"attributes": {"language": "en", "files": []}}]});
        let hits = parse_search(&data);
        assert_eq!(hits.iter().map(|h| h.file_id).collect::<Vec<_>>(), [2, 1, 3]);
        assert_eq!((hits[0].language.as_str(), hits[0].exact, hits[0].hearing_impaired), ("por", true, true));
        assert_eq!(hits[1].language, "ita");
        assert!(hits[2].forced && hits[2].machine_translated);
        assert_eq!(hits[2].language, "eng");
    }

    #[test]
    fn the_best_hit_is_a_normal_one_in_the_language() {
        let hit = |id, lang: &str, hi: bool, forced: bool, machine: bool| Hit {
            file_id: id, language: lang.into(), release: String::new(), file_name: String::new(), downloads: 0,
            hearing_impaired: hi, forced, machine_translated: machine, exact: false,
        };
        let hits = vec![
            hit(1, "eng", false, false, false),
            hit(2, "ita", false, false, true),
            hit(3, "ita", false, true, false),
            hit(4, "ita", true, false, false),
            hit(5, "ita", false, false, false),
        ];
        assert_eq!(best_hit(&hits, "ita").map(|h| h.file_id), Some(5));
        assert_eq!(best_hit(&hits[..4], "ita").map(|h| h.file_id), Some(4));
        assert_eq!(best_hit(&hits[..3], "ita").map(|h| h.file_id), None);
        assert_eq!(best_hit(&hits, "").map(|h| h.file_id), Some(1));
        assert_eq!(best_hit(&hits, "fre").map(|h| h.file_id), None);
    }

    #[test]
    fn curl_config_strings_are_escaped() {
        assert_eq!(quoted("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(urlencode("a b&c/é"), "a%20b%26c%2F%C3%A9");
        assert_eq!(two_letter(&["ita".into(), "eng".into(), "xxx".into()]), ["it", "en"]);
    }

    #[test]
    fn without_a_key_or_login_nothing_is_sent() {
        let none = Credentials::default();
        assert!(search_at("http://127.0.0.1:1", &none, Path::new("a.mkv"), "x", &[]).unwrap_err().contains("API key"));
        let key = Credentials { api_key: "k".into(), ..Credentials::default() };
        assert!(download_at("http://127.0.0.1:1", &key, 1).unwrap_err().contains("login"));
    }

    /// A one-thread web server that answers like the service, and keeps what it was asked.
    fn serve(requests: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let root = base.clone();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut headers = String::new();
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    headers.push_str(&header);
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                seen.push(format!("{}|{}|{}", line.trim(), headers.replace("\r\n", ";"), String::from_utf8_lossy(&body)));
                let reply = if line.contains("/subtitles?") {
                    json!({"data": [{"attributes": {"language": "it", "download_count": 3, "release": "R",
                                    "files": [{"file_id": 7, "file_name": "x.srt"}]}}]})
                    .to_string()
                } else if line.contains("/login") {
                    json!({"token": "tok", "status": 200}).to_string()
                } else if line.contains("/download") {
                    json!({"link": format!("{root}/file/x.srt"), "file_name": "x.srt", "remaining": 4}).to_string()
                } else {
                    "1\n00:00:00,000 --> 00:00:01,000\nCiao\n".to_string()
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len())
                    .unwrap();
            }
            seen
        });
        (base, handle)
    }

    #[test]
    fn a_search_and_a_download_against_a_stand_in_server() {
        if crate::util::which("curl").is_none() {
            return;
        }
        let dir = scratch("server");
        let media = dir.join("Example.Show.S01E03.1080p.mkv");
        fs::write(&media, vec![0u8; 200_000]).unwrap();
        let creds = Credentials { api_key: "secret-key".into(), username: "me".into(), password: "p\"w".into() };
        let (base, server) = serve(4);
        let hits = search_at(&base, &creds, &media, "", &["ita".into()]).unwrap();
        assert_eq!((hits[0].file_id, hits[0].language.as_str()), (7, "ita"));
        let target = download_at(&base, &creds, 7).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap().lines().nth(2), Some("Ciao"));
        assert!(target.file_name().unwrap().to_string_lossy().starts_with(TEMP_PREFIX));
        forget_download(&target);
        assert!(!target.exists());
        let seen = server.join().unwrap();
        // the search: sorted parameters, the hash, the guessed episode, the key and the agent in headers
        assert!(
            seen[0].starts_with(
                "GET /subtitles?episode_number=3&languages=it&moviehash=0000000000030d40&query=example%20show\
                 &season_number=1&type=episode "
            ),
            "{}",
            seen[0]
        );
        assert!(seen[0].to_lowercase().contains("api-key: secret-key"));
        assert!(seen[0].contains(&format!("mau v{}", crate::VERSION)));
        // the login carries the password in the body, and the download the token
        assert!(seen[1].contains("POST /login") && seen[1].contains(r#""password":"p\"w""#), "{}", seen[1]);
        assert!(seen[2].contains("POST /download") && seen[2].contains("Authorization: Bearer tok"), "{}", seen[2]);
        let _ = fs::remove_dir_all(dir);
    }
}
