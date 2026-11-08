//! Small helpers: running commands, copying files, patterns, disk space.

use std::fs::{self, File, FileTimes};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::model::{cancelled, Container};

/// The environment variable `MAU_<name>`, or the `FML_<name>` it was called before the project
/// was renamed, which is still read when the new one is not set.
pub fn mau_env(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(format!("MAU_{name}")).or_else(|_| std::env::var(format!("FML_{name}")))
}

/// As `mau_env`, for a value that may not be text (a list of folders).
pub fn mau_env_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("MAU_{name}")).or_else(|| std::env::var_os(format!("FML_{name}")))
}

/// A command as one line that a shell would read back (for --verbose).
pub fn shell_join(cmd: &[String]) -> String {
    cmd.iter().map(|arg| shell_quote(arg)).collect::<Vec<_>>().join(" ")
}

fn shell_quote(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\"'\"'"))
    }
}

/// The operating system's text for an error ("No such file or directory").
pub fn strerror(error: &io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error") {
        Some(end) => text[..end].to_string(),
        None => text,
    }
}

/// What a finished command printed and how it ended.
#[derive(Debug, Clone)]
pub struct Completed {
    /// The exit code, or minus the signal number if it was ended by a signal.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The program to start for a command name. Windows only adds `.exe` when it searches for a
/// bare name, so `mkvmerge.cmd` or `.bat` would not be found: look it up through `PATHEXT`
/// as `which` does. Elsewhere the name is used as it is.
fn resolve_program(name: &str) -> PathBuf {
    if cfg!(windows) && Path::new(name).components().count() == 1 {
        if let Some(found) = which(name) {
            return found;
        }
    }
    PathBuf::from(name)
}

/// How a command ended.
pub enum Ran {
    Done(Completed),
    /// We were asked to stop, and the command was ended.
    Cancelled,
}

fn drain<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// Run a command and collect its output. If we are asked to stop, the command is killed.
/// An `Err` means the command could not be started.
pub fn run_command(cmd: &[String]) -> io::Result<Ran> {
    // see `which`
    if crate::model::mkvtoolnix_hidden() && cmd[0].starts_with("mkv") {
        return Err(io::Error::new(io::ErrorKind::NotFound, "spike: mkvtoolnix hidden"));
    }
    if crate::model::ffmpeg_hidden() && (cmd[0] == "ffmpeg" || cmd[0] == "ffprobe") {
        let message = format!("{} is not included in this build (--nolgpl), and this change needs it", cmd[0]);
        return Err(io::Error::new(io::ErrorKind::Unsupported, message));
    }
    let mut child = Command::new(resolve_program(&cmd[0]))
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let status = loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Ran::Cancelled);
        }
        match child.try_wait()? {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    let collect = |handle: Option<thread::JoinHandle<Vec<u8>>>| {
        let bytes = handle.and_then(|h| h.join().ok()).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(Ran::Done(Completed {
        code: status.code().unwrap_or_else(|| signal_code(&status)),
        stdout: collect(out),
        stderr: collect(err),
    }))
}

/// Minus the number of the signal that ended a command (there are no signals on Windows).
#[cfg(unix)]
fn signal_code(status: &std::process::ExitStatus) -> i32 {
    -status.signal().unwrap_or(1)
}

#[cfg(not(unix))]
fn signal_code(_status: &std::process::ExitStatus) -> i32 {
    -1
}

/// Whether a file can be run: it has an execute bit (Unix), or exists (Windows, where the
/// extension decides).
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        path.is_file() && fs::metadata(path).map(|m| m.mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The names a command can have in a folder: itself, and on Windows also with each of the
/// extensions in `PATHEXT` (`mkvmerge` is `mkvmerge.exe`).
fn command_names(tool: &str) -> Vec<String> {
    let mut names = vec![tool.to_string()];
    if cfg!(windows) && Path::new(tool).extension().is_none() {
        let extensions =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        names.extend(
            extensions.split(';').filter(|e| !e.is_empty()).map(|e| format!("{tool}{e}")),
        );
    }
    names
}

/// The full path of a command on `PATH`.
pub fn which(tool: &str) -> Option<PathBuf> {
    // without mkvtoolnix (the no-mkvtoolnix feature), its commands are not used, even when installed
    if crate::model::mkvtoolnix_hidden() && tool.starts_with("mkv") {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let names = command_names(tool);
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| is_executable(candidate))
}

/// On macOS a program started from Finder gets a bare `PATH` (`/usr/bin:/bin:...`) with none of
/// the folders Homebrew, MacPorts or the MKVToolNix app put commands in. Add the ones that
/// exist, after what is already there. On Windows it adds the folders the installers use. Call it
/// once, before other threads start; it does nothing on other systems.
pub fn add_tool_folders() {
    // a Mac app built with `build.sh --sandboxed` carries its tools: its own copies come first
    if let Some(bundled) = bundled_tools_folder() {
        let mut first = vec![bundled];
        first.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
        if let Ok(joined) = std::env::join_paths(first) {
            std::env::set_var("PATH", joined);
        }
    }
    let folders: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mut extra: Vec<PathBuf> = if cfg!(target_os = "macos") {
        ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].iter().map(PathBuf::from).collect()
    } else if cfg!(windows) {
        // where the MKVToolNix installer, winget and Chocolatey put commands; a program that was
        // started before an install does not see the change to PATH that they make
        let under = |variable: &str, rest: &str| {
            std::env::var_os(variable).map(|base| PathBuf::from(base).join(rest))
        };
        [
            under("ProgramFiles", "MKVToolNix"),
            under("LOCALAPPDATA", "Microsoft\\WinGet\\Links"),
            under("ProgramData", "chocolatey\\bin"),
        ]
        .into_iter()
        .flatten()
        .collect()
    } else {
        return;
    };
    // the MKVToolNix .dmg installs its commands inside the app: MKVToolNix*.app/Contents/MacOS
    if let Ok(entries) = fs::read_dir("/Applications") {
        let mut apps: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("MKVToolNix") && n.ends_with(".app")))
            .map(|p| p.join("Contents/MacOS"))
            .collect();
        apps.sort();
        extra.extend(apps);
    }
    if let Ok(joined) = std::env::join_paths(append_missing(folders, extra)) {
        std::env::set_var("PATH", joined);
    }
}

/// The folder of the tools a Mac app carries (`Contents/Resources/bin`), when this program runs from
/// inside such an app (`Contents/MacOS/`). `None` anywhere else.
pub fn bundled_tools_folder() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let folder = macos.parent()?.join("Resources").join("bin");
    folder.is_dir().then_some(folder)
}

/// `folders` followed by each of `extra` that is a folder and not already in it.
fn append_missing(mut folders: Vec<PathBuf>, extra: Vec<PathBuf>) -> Vec<PathBuf> {
    for folder in extra {
        if folder.is_dir() && !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    folders
}

/// Copy a file's permissions and times onto another.
pub fn copy_stat(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::metadata(from)?;
    fs::set_permissions(to, meta.permissions())?;
    let mut times = FileTimes::new();
    if let Ok(accessed) = meta.accessed() {
        times = times.set_accessed(accessed);
    }
    if let Ok(modified) = meta.modified() {
        times = times.set_modified(modified);
    }
    File::options().write(true).open(to)?.set_times(times)
}

/// Copy a file with its permissions and times, stopping early (with `Interrupted`) if
/// we are asked to.
pub fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = File::open(from)?;
    let mut target = File::create(to)?;
    let mut buffer = vec![0u8; 4 << 20];
    loop {
        if cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
        }
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        target.write_all(&buffer[..count])?;
    }
    drop(target);
    copy_stat(from, to)
}

/// Free space on the disk a folder is on, for a user who is not root, and the disk's id.
#[cfg(unix)]
pub fn disk_free(folder: &Path) -> io::Result<(u64, u64)> {
    let device = fs::metadata(folder)?.dev();
    let c_path = std::ffi::CString::new(folder.as_os_str().as_encoded_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c_path is a valid C string and stats is a valid, writable statvfs.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)]
    let free = (stats.f_bavail as u64) * (stats.f_frsize as u64);
    Ok((free, device))
}

/// Free space on the disk a folder is on, for the user, and an id for the disk (its drive).
#[cfg(windows)]
pub fn disk_free(folder: &Path) -> io::Result<(u64, u64)> {
    use std::hash::{Hash, Hasher};
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }

    let wide: Vec<u16> = folder.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    // SAFETY: wide is a NUL-terminated UTF-16 string and the three outputs are valid u64s.
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // two folders are on the same disk if they have the same drive (or share) prefix
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    folder.components().next().hash(&mut hasher);
    Ok((free, hasher.finish()))
}

/// Whether a text matches a shell pattern (`*`, `?` and `[abc]` or `[!abc]`), case
/// sensitively, like Python's `fnmatch.fnmatchcase`.
pub fn fnmatch(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    matches_from(&pattern, &text)
}

fn matches_from(pattern: &[char], text: &[char]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some('*') => (0..=text.len()).any(|skip| matches_from(&pattern[1..], &text[skip..])),
        Some('?') => !text.is_empty() && matches_from(&pattern[1..], &text[1..]),
        Some('[') => {
            if let Some(end) = class_end(pattern) {
                let Some(&c) = text.first() else { return false };
                let mut body = &pattern[1..end];
                let negate = matches!(body.first(), Some('!'));
                if negate {
                    body = &body[1..];
                }
                let mut hit = false;
                let mut i = 0;
                while i < body.len() {
                    if i + 2 < body.len() && body[i + 1] == '-' {
                        hit |= body[i] <= c && c <= body[i + 2];
                        i += 3;
                    } else {
                        hit |= body[i] == c;
                        i += 1;
                    }
                }
                hit != negate && matches_from(&pattern[end + 1..], &text[1..])
            } else {
                text.first() == Some(&'[') && matches_from(&pattern[1..], &text[1..])
            }
        }
        Some(&c) => text.first() == Some(&c) && matches_from(&pattern[1..], &text[1..]),
    }
}

/// The index of the `]` that closes a class that starts at the first character.
fn class_end(pattern: &[char]) -> Option<usize> {
    let mut i = 1;
    if matches!(pattern.get(i), Some('!')) {
        i += 1;
    }
    if matches!(pattern.get(i), Some(']')) {
        i += 1;
    }
    (i..pattern.len()).find(|&j| pattern[j] == ']')
}

/// Which kind of media file this is, from its header: `Mkv` (Matroska or WebM), `Mp4`
/// (MP4, M4V, MOV), `Avi`, or `None`.
pub fn detect_kind(path: &str) -> Option<Container> {
    let mut head = Vec::new();
    File::open(path).ok()?.take(64).read_to_end(&mut head).ok()?;
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) && (has(b"matroska") || has(b"webm")) {
        return Some(Container::Mkv);
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"AVI " {
        return Some(Container::Avi);
    }
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return Some(Container::Mp4);
    }
    None
}

/// The Linux distribution name (`Ubuntu`), or empty if it can't be found.
pub fn detect_distro() -> String {
    if let Ok(done) = Command::new("lsb_release").args(["-i", "-s"]).output() {
        return String::from_utf8_lossy(&done.stdout).trim().to_string();
    }
    if let Ok(text) = fs::read_to_string("/etc/os-release") {
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("ID=") {
                let id = value.trim_matches('"');
                let mut chars = id.chars();
                return match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                };
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_tools_are_only_looked_for_inside_a_mac_app() {
        // the test program is not in a Contents/MacOS folder, so there is no bundle to look in
        assert_eq!(bundled_tools_folder(), None);
    }

    #[test]
    fn patterns_match_like_fnmatch() {
        assert!(fnmatch("*.mkv", "a.mkv"));
        assert!(fnmatch("season 1/*", "season 1/a/b.mkv"));
        assert!(fnmatch("e?tras", "extras"));
        assert!(fnmatch("[ab]*", "bar"));
        assert!(!fnmatch("[!ab]*", "bar"));
        assert!(fnmatch("[a-c]x", "bx"));
        assert!(!fnmatch("*.mkv", "a.mp4"));
        assert!(fnmatch("[", "["));
    }

    #[test]
    fn commands_are_quoted_for_a_shell() {
        let cmd: Vec<String> = ["mkvmerge", "-o", "my file's.mkv", ""].map(String::from).to_vec();
        assert_eq!(shell_join(&cmd), "mkvmerge -o 'my file'\"'\"'s.mkv' ''");
    }

    #[test]
    fn tool_folders_are_added_once_and_only_if_they_exist() {
        let here = std::env::temp_dir();
        let missing = here.join("mau-no-such-folder");
        let have = vec![PathBuf::from("/usr/bin")];
        let merged = append_missing(have.clone(), vec![here.clone(), missing, PathBuf::from("/usr/bin"), here.clone()]);
        assert_eq!(merged, vec![PathBuf::from("/usr/bin"), here]);
    }

    #[test]
    fn an_fml_variable_is_read_when_the_mau_one_is_not_set() {
        // names no other test or program uses, so setting them cannot disturb a parallel test
        std::env::set_var("FML_TEST_ONLY_OLD", "old");
        assert_eq!(mau_env("TEST_ONLY_OLD").as_deref(), Ok("old"));
        assert_eq!(mau_env_os("TEST_ONLY_OLD"), Some("old".into()));
        std::env::set_var("MAU_TEST_ONLY_OLD", "new");
        assert_eq!(mau_env("TEST_ONLY_OLD").as_deref(), Ok("new"), "MAU_ wins over FML_");
        assert_eq!(mau_env_os("TEST_ONLY_OLD"), Some("new".into()));
        assert!(mau_env("TEST_ONLY_NEITHER").is_err());
        assert_eq!(mau_env_os("TEST_ONLY_NEITHER"), None);
    }
}
