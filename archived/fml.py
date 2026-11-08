#!/usr/bin/env python3

# Name:         fml (Fix Media Language etc)
# Version:      1.6.9
# Release:      1
# License:      Apache-2.0
#               https://www.apache.org/licenses/LICENSE-2.0
# Group:        System
# Source:       N/A
# URL:          https://github.com/lateralblast/fml
# Distribution: UNIX
# Vendor:       UNIX
# Packager:     Richard Spindler <richard@lateralblast.com.au>
# Description:  Fix media language (default audio track) etc for media files

"""Fix Media Language: change the default audio and subtitle tracks and languages of media files.

MKV files are edited in place with mkvpropedit where possible (only the metadata
changes) and rewritten with mkvmerge when tracks are removed. MP4 files are
rewritten with ffmpeg (a copy of the streams, so nothing is re-encoded). AVI files have
no track metadata to edit, so they are only rewritten, to swap or remove audio tracks.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime
import fnmatch
import json
import os
import platform
import shlex
import shutil
import signal
import subprocess
import sys
import threading
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, NamedTuple, NoReturn

__version__ = "1.6.9"

# Commands the script runs and the package that provides each
REQUIRED_TOOLS = {
    "mkvmerge": "mkvtoolnix",
    "mkvpropedit": "mkvtoolnix",
    "mediainfo": "mediainfo",
    "ffmpeg": "ffmpeg",
}

# Tools needed to read, or to change, a file of each kind
READ_TOOLS = {"mkv": ("mkvmerge",), "avi": ("mediainfo",), "mp4": ("mediainfo",)}
WRITE_TOOLS = {"mkv": ("mkvmerge", "mkvpropedit"), "avi": ("mediainfo", "ffmpeg"),
               "mp4": ("mediainfo", "ffmpeg")}

CONTAINERS = {"mkv": "Matroska", "avi": "AVI", "mp4": "MP4"}

# The kinds of track the actions work on, as they can be named on the command line
KINDS = {"audio": "audio", "sub": "subtitles", "subs": "subtitles",
         "subtitle": "subtitles", "subtitles": "subtitles"}
SELECTORS = {"audio": "a", "subtitles": "s"}  # as in mkvpropedit track:a1 and ffmpeg -map 0:a:0
STREAM_SELECTORS = {"video": "v", "audio": "a", "subtitles": "s"}  # every kind, for ffmpeg
MKVMERGE_SELECT = {"audio": "--audio-tracks", "subtitles": "--subtitle-tracks"}

FFMPEG_QUIET = ("-hide_banner", "-loglevel", "error", "-y")

# Temporary files are named after the file they replace, with this in the name
TEMP_MARK = ".fml-tmp"

# With --backup the original is kept beside the result, with this added to its name
BACKUP_MARK = ".fml-bak"

# What mkvmerge exit codes mean: 0 success, 1 success with warnings, 2 error
MKVMERGE_OK = (0, 1)

TASK_CHOICES = {
    "get": ("lang",),
    "set": ("lang", "default", "name", "forced", "unforced"),
    "swap": ("lang",),
    "delete": ("lang", "track"),
    "preserve": ("lang",),
}


# Languages -------------------------------------------------------------------

class Language(NamedTuple):
    """A language with its ISO 639 codes."""

    code: str         # ISO 639-2/B, the code mkvmerge uses
    terminology: str  # ISO 639-2/T
    alpha2: str       # ISO 639-1 ('' if the language has none)
    name: str


LANGUAGES = (
    Language("afr", "afr", "af", "Afrikaans"),
    Language("alb", "sqi", "sq", "Albanian"),
    Language("ara", "ara", "ar", "Arabic"),
    Language("arm", "hye", "hy", "Armenian"),
    Language("baq", "eus", "eu", "Basque"),
    Language("ben", "ben", "bn", "Bengali"),
    Language("bos", "bos", "bs", "Bosnian"),
    Language("bul", "bul", "bg", "Bulgarian"),
    Language("cat", "cat", "ca", "Catalan"),
    Language("chi", "zho", "zh", "Chinese"),
    Language("hrv", "hrv", "hr", "Croatian"),
    Language("cze", "ces", "cs", "Czech"),
    Language("dan", "dan", "da", "Danish"),
    Language("dut", "nld", "nl", "Dutch"),
    Language("eng", "eng", "en", "English"),
    Language("est", "est", "et", "Estonian"),
    Language("fil", "fil", "", "Filipino"),
    Language("fin", "fin", "fi", "Finnish"),
    Language("fre", "fra", "fr", "French"),
    Language("glg", "glg", "gl", "Galician"),
    Language("geo", "kat", "ka", "Georgian"),
    Language("ger", "deu", "de", "German"),
    Language("gre", "ell", "el", "Greek"),
    Language("heb", "heb", "he", "Hebrew"),
    Language("hin", "hin", "hi", "Hindi"),
    Language("hun", "hun", "hu", "Hungarian"),
    Language("ice", "isl", "is", "Icelandic"),
    Language("ind", "ind", "id", "Indonesian"),
    Language("gle", "gle", "ga", "Irish"),
    Language("ita", "ita", "it", "Italian"),
    Language("jpn", "jpn", "ja", "Japanese"),
    Language("kor", "kor", "ko", "Korean"),
    Language("lat", "lat", "la", "Latin"),
    Language("lav", "lav", "lv", "Latvian"),
    Language("lit", "lit", "lt", "Lithuanian"),
    Language("mac", "mkd", "mk", "Macedonian"),
    Language("may", "msa", "ms", "Malay"),
    Language("mal", "mal", "ml", "Malayalam"),
    Language("mar", "mar", "mr", "Marathi"),
    Language("nep", "nep", "ne", "Nepali"),
    Language("nor", "nor", "no", "Norwegian"),
    Language("per", "fas", "fa", "Persian"),
    Language("pol", "pol", "pl", "Polish"),
    Language("por", "por", "pt", "Portuguese"),
    Language("rum", "ron", "ro", "Romanian"),
    Language("rus", "rus", "ru", "Russian"),
    Language("srp", "srp", "sr", "Serbian"),
    Language("sin", "sin", "si", "Sinhala"),
    Language("slo", "slk", "sk", "Slovak"),
    Language("slv", "slv", "sl", "Slovenian"),
    Language("spa", "spa", "es", "Spanish"),
    Language("swa", "swa", "sw", "Swahili"),
    Language("swe", "swe", "sv", "Swedish"),
    Language("tam", "tam", "ta", "Tamil"),
    Language("tel", "tel", "te", "Telugu"),
    Language("tha", "tha", "th", "Thai"),
    Language("tur", "tur", "tr", "Turkish"),
    Language("ukr", "ukr", "uk", "Ukrainian"),
    Language("urd", "urd", "ur", "Urdu"),
    Language("vie", "vie", "vi", "Vietnamese"),
    Language("wel", "cym", "cy", "Welsh"),
    Language("mis", "mis", "", "Uncoded"),
    Language("mul", "mul", "", "Multiple"),
    Language("und", "und", "", "Undetermined"),
    Language("zxx", "zxx", "", "No linguistic content"),
)

LANGUAGE_ALIASES = {
    "castilian": "spa",
    "farsi": "per",
    "flemish": "dut",
    "greek (modern)": "gre",
    "mandarin": "chi",
    "cantonese": "chi",
}


def _build_language_index() -> dict[str, Language]:
    index: dict[str, Language] = {}
    for language in LANGUAGES:
        for key in (language.code, language.terminology, language.alpha2, language.name):
            if key:
                index[key.lower()] = language
    by_code = {language.code: language for language in LANGUAGES}
    for alias, code in LANGUAGE_ALIASES.items():
        index[alias] = by_code[code]
    return index


LANGUAGE_INDEX = _build_language_index()


def resolve_language(text: str) -> str:
    """Convert a language name or ISO 639 code to the ISO 639-2/B code mkvmerge uses.

    Names and two letter codes must be known. Any other three letter code is
    passed through, as the table above does not hold every ISO 639-2 code.
    """
    key = text.strip().lower()
    known = LANGUAGE_INDEX.get(key)
    if known:
        return known.code
    if len(key) == len("eng") and key.isalpha():
        return key
    raise ValueError(
        f'unknown language "{text}" (use an ISO 639 name or code, e.g. English, en or eng)')


def normalise_language(value: object) -> str:
    """ISO 639-2/B code for a language read from a file (``und`` if it has none)."""
    if not isinstance(value, str) or not value.strip():
        return "und"
    try:
        return resolve_language(value)
    except ValueError:
        return value.strip().lower()


# Tracks ----------------------------------------------------------------------

@dataclass(frozen=True)
class Track:
    """A track in a media file."""

    id: int        # track ID (the one mkvmerge uses)
    kind: str      # video, audio or subtitles
    number: int    # position among the tracks of its kind, from 1
    codec: str
    language: str  # ISO 639-2/B
    default: bool
    forced: bool
    name: str

    @property
    def label(self) -> str:
        return f"{kind_word(self.kind)} track {self.number}"

    def to_dict(self) -> dict[str, Any]:
        return {"id": self.id, "type": self.kind, "number": self.number, "codec": self.codec,
                "language": self.language, "default": self.default, "forced": self.forced,
                "name": self.name}


def kind_word(kind: str) -> str:
    """How a kind of track is named in messages."""
    return "subtitle" if kind == "subtitles" else kind


def parse_mkv_tracks(data: dict[str, Any]) -> list[Track]:
    """Tracks from the output of ``mkvmerge -F json -i``."""
    counts: dict[str, int] = {}
    tracks = []
    for index, raw in enumerate(data.get("tracks", [])):
        kind = str(raw.get("type", ""))
        counts[kind] = counts.get(kind, 0) + 1
        props = raw.get("properties", {})
        tracks.append(Track(
            id=int(raw.get("id", index)), kind=kind, number=counts[kind],
            codec=str(raw.get("codec", "")),
            language=normalise_language(props.get("language")),
            default=bool(props.get("default_track", False)),
            forced=bool(props.get("forced_track", False)),
            name=str(props.get("track_name", ""))))
    return tracks


def parse_mediainfo_tracks(data: dict[str, Any]) -> list[Track]:
    """Tracks from the output of ``mediainfo --Output=JSON``."""
    kinds = {"Video": "video", "Audio": "audio", "Text": "subtitles"}
    counts: dict[str, int] = {}
    tracks = []
    for index, raw in enumerate(data.get("media", {}).get("track", [])):
        kind = kinds.get(str(raw.get("@type", "")))
        if kind is None:
            continue
        counts[kind] = counts.get(kind, 0) + 1
        number = int(raw["@typeorder"]) if str(raw.get("@typeorder", "")).isdigit() \
            else counts[kind]
        tracks.append(Track(
            id=int(raw["ID"]) if str(raw.get("ID", "")).isdigit() else index,
            kind=kind, number=number, codec=str(raw.get("Format", "")),
            language=normalise_language(raw.get("Language")),
            default=raw.get("Default") == "Yes", forced=raw.get("Forced") == "Yes",
            name=str(raw.get("Title", ""))))
    return tracks


def tracks_from_dicts(rows: Sequence[dict[str, Any]]) -> list[Track]:
    """Tracks from their ``to_dict`` form."""
    return [Track(id=row["id"], kind=row["type"], number=row["number"], codec=row["codec"],
                  language=row["language"], default=row["default"], forced=row["forced"],
                  name=row["name"]) for row in rows]


def format_tracks(tracks: Sequence[Track]) -> str:
    """A table of tracks."""
    header = ("Track", "ID", "Codec", "Language", "Default", "Forced", "Name")
    rows = [header] + [
        (f"{track.kind}:{track.number}", str(track.id), track.codec, track.language,
         "yes" if track.default else "-", "yes" if track.forced else "-", track.name)
        for track in tracks]
    widths = [max(len(row[column]) for row in rows) for column in range(len(header))]
    lines = ["  ".join(cell.ljust(widths[column]) for column, cell in enumerate(row)).rstrip()
             for row in rows]
    return "\n".join(lines)


def yes_no(value: bool) -> str:
    return "yes" if value else "no"


def human_size(size: float) -> str:
    """A size in bytes as a short string (1.5 GB)."""
    for unit in ("bytes", "KB", "MB", "GB"):
        if size < 1024:
            return f"{size:.0f} {unit}" if unit == "bytes" else f"{size:.1f} {unit}"
        size /= 1024
    return f"{size:.1f} TB"


# Results ---------------------------------------------------------------------

@dataclass
class Result:
    """The outcome of an action on a file."""

    file: str
    action: str
    status: str = "ok"  # ok, changed, unchanged, skipped or error
    message: str = ""
    changes: list[str] = field(default_factory=list)
    commands: list[list[str]] = field(default_factory=list)
    data: Any = None
    dry_run: bool = False
    backup: str = ""  # where the original was kept, with --backup
    moved_from: int = 0  # the place the track that was moved first had (for the log)

    def to_dict(self) -> dict[str, Any]:
        return {"file": self.file, "action": self.action, "status": self.status,
                "message": self.message, "changes": self.changes, "commands": self.commands,
                "dry_run": self.dry_run, "backup": self.backup or None, "data": self.data}


class Task(NamedTuple):
    """An action requested on the command line, with its argument."""

    name: str
    what: str


class Fatal(Exception):
    """An error that stops everything, such as a missing tool."""


@dataclass
class Config:
    """Settings from the command line."""

    file: str = ""
    dir: str = ""
    recursive: bool = False
    lang: str = ""
    default: str = ""  # a language, or "none" for no default subtitle track
    prefer: list[str] = field(default_factory=list)
    track: str = ""
    kind: str = "audio"  # the kind of track the actions work on: audio or subtitles
    forced: bool = False
    verbose: bool = False
    dryrun: bool = False
    install: bool = False
    force: bool = False
    backup: bool = False  # keep the original beside the changed file
    log: str = ""  # a file to record each change in, so that --undo can reverse it
    include: list[str] = field(default_factory=list)  # only the files that match one of these
    exclude: list[str] = field(default_factory=list)  # never the files that match one of these
    jobs: int = 1  # how many files to process at once
    move: bool = False  # also make the chosen track the first of its kind
    name: str | None = None  # a track name, for --set name
    format: str = "text"
    tasks: list[Task] = field(default_factory=list)


@dataclass
class Media:
    """A media file and the kind of container it is."""

    path: str
    kind: str  # mkv, mp4 or avi
    tracks: list[Track] | None = None


def detect_kind(path: str) -> str:
    """Return 'mkv' (Matroska or WebM), 'mp4' (MP4, M4V, MOV), 'avi' or '' from the header."""
    try:
        with open(path, "rb") as handle:
            head = handle.read(64)
    except OSError:
        return ""
    if head[:4] == b"\x1a\x45\xdf\xa3" and (b"matroska" in head or b"webm" in head):
        return "mkv"
    if head[:4] == b"RIFF" and head[8:12] == b"AVI ":
        return "avi"
    if head[4:8] == b"ftyp":
        return "mp4"
    return ""


def detect_distro() -> str:
    """Linux distribution name (e.g. 'Ubuntu'), or '' if it can't be found."""
    try:
        result = subprocess.run(["lsb_release", "-i", "-s"], capture_output=True,
                                text=True, check=False)
        return result.stdout.strip()
    except OSError:
        pass
    try:
        for line in Path("/etc/os-release").read_text().splitlines():
            if line.startswith("ID="):
                return line.split("=", 1)[1].strip('"').capitalize()
    except OSError:
        pass
    return ""


# Application -----------------------------------------------------------------

# Messages are printed under this lock, so that threads can't mix up their lines
OUTPUT_LOCK = threading.Lock()

@dataclass
class Edit:
    """A change to a track: its default flag, language, name or forced flag, or its place."""

    track: Track
    default: bool | None = None
    language: str | None = None
    move_to: int | None = None  # the place to move it to among tracks of its kind (1 is first)
    name: str | None = None
    forced: bool | None = None

    def describe(self) -> str:
        parts = []
        if self.default is not None:
            parts.append(f"default {yes_no(self.track.default)} -> {yes_no(self.default)}")
        if self.language is not None:
            parts.append(f"language {self.track.language} -> {self.language}")
        if self.name is not None:
            parts.append(f'name "{self.track.name}" -> "{self.name}"')
        if self.forced is not None:
            parts.append(f"forced {yes_no(self.track.forced)} -> {yes_no(self.forced)}")
        if self.move_to is not None:
            parts.append(f"moved to the first {kind_word(self.track.kind)} track"
                         if self.move_to == 1
                         else f"moved to {kind_word(self.track.kind)} track {self.move_to}")
        named = (self.default is not None or self.move_to is not None or self.name is not None
                 or self.forced is not None)
        name = self.track.label + (f" ({self.track.language})" if named else "")
        return f"{name}: {', '.join(parts)}"


class Fml:
    """Runs the requested actions against media files."""

    def __init__(self, config: Config) -> None:
        self.config = config
        self.results: list[Result] = []
        self.lock = threading.Lock()  # for writing the log
        self.install_lock = threading.Lock()  # so that threads don't all install a command
        self.space_lock = threading.Lock()  # for the free space that jobs have set aside
        self.reserved: dict[int, int] = {}  # bytes set aside on each disk by the jobs running
        self.holds: dict[str, list[tuple[int, int]]] = {}  # what each file has set aside
        self.pending: list[str] = []  # temporary files to remove if we are stopped
        self.os_name = platform.system()
        self.distro = detect_distro() if self.os_name == "Linux" else ""

    @property
    def kind(self) -> str:
        """The kind of track the actions work on (audio or subtitles)."""
        return self.config.kind

    @property
    def word(self) -> str:
        return kind_word(self.config.kind)

    # Messages and commands --------------------------------------------------

    @staticmethod
    def warn(message: str) -> None:
        with OUTPUT_LOCK:
            print(f"Warning: {message}", file=sys.stderr)

    def note(self, message: str) -> None:
        """A message shown with --verbose."""
        if self.config.verbose:
            with OUTPUT_LOCK:
                print(message, file=sys.stderr)

    def query(self, cmd: list[str]) -> subprocess.CompletedProcess[str] | None:
        """Run a read-only command (also during a dry run). None if it can't be run."""
        self.note(f"Executing:\t{shlex.join(cmd)}")
        try:
            return subprocess.run(cmd, capture_output=True, text=True, check=False)
        except OSError:
            return None

    def apply(self, result: Result, cmd: list[str], ok_codes: Sequence[int] = (0,)) -> bool:
        """Run a command that changes things, unless this is a dry run.

        The command is recorded in the result. On failure the result becomes an error.
        """
        result.commands.append(cmd)
        self.note(f"Executing:\t{shlex.join(cmd)}")
        if self.config.dryrun:
            return True
        sys.stdout.flush()
        try:
            completed = subprocess.run(cmd, capture_output=True, text=True, check=False)
        except OSError as error:
            result.status, result.message = "error", f"{cmd[0]} failed: {error.strerror}"
            return False
        if completed.returncode not in ok_codes:
            detail = (completed.stderr or completed.stdout).strip().splitlines()
            result.status = "error"
            result.message = f"{cmd[0]} failed with exit code {completed.returncode}" + (
                f": {detail[-1]}" if detail else "")
            return False
        return True

    # Environment ------------------------------------------------------------

    def install_package(self, package: str) -> None:
        if self.os_name == "Linux" and self.distro == "Ubuntu":
            cmd = ["sudo", "apt", "install", "-y", package]
        elif self.os_name == "Darwin":
            cmd = ["brew", "install", package]
        else:
            self.warn(f'don\'t know how to install "{package}" on this OS')
            return
        self.note(f"Executing:\t{shlex.join(cmd)}")
        if not self.config.dryrun:
            sys.stdout.flush()
            subprocess.run(cmd, check=False)

    def install_missing(self, missing: Sequence[str]) -> None:
        for package in dict.fromkeys(REQUIRED_TOOLS[tool] for tool in missing):
            self.install_package(package)

    def check_tools(self) -> Result:
        """Report which of the required commands are installed."""
        found = {tool: shutil.which(tool) for tool in REQUIRED_TOOLS}
        missing = [tool for tool, path in found.items() if path is None]
        if missing and self.config.install:
            self.install_missing(missing)
            if not self.config.dryrun:
                found = {tool: shutil.which(tool) for tool in REQUIRED_TOOLS}
                missing = [tool for tool, path in found.items() if path is None]
        result = Result("", "check", dry_run=self.config.dryrun, data=[
            {"tool": tool, "path": path, "package": REQUIRED_TOOLS[tool]}
            for tool, path in found.items()])
        if missing and not (self.config.install and self.config.dryrun):
            result.status = "error"
            result.message = "missing: " + ", ".join(missing)
            if not self.config.install:
                result.message += " (use --install to install them)"
        return result

    def require(self, tools: Sequence[str]) -> None:
        """Make sure commands are installed, installing them if --install was given."""
        missing = [tool for tool in tools if shutil.which(tool) is None]
        if not missing:
            return
        if self.config.install:
            with self.install_lock:  # the first thread installs, the others then find them
                missing = [tool for tool in tools if shutil.which(tool) is None]
                if missing:
                    self.install_missing(missing)
            if self.config.dryrun:
                return
            missing = [tool for tool in tools if shutil.which(tool) is None]
        if missing:
            raise Fatal(f"command not found: {', '.join(missing)} "
                        "(use --install to install, or --check to see what is missing)")

    # Files ------------------------------------------------------------------

    @staticmethod
    def pattern_hit(relative: Path, patterns: Sequence[str]) -> bool:
        """Whether a path (below the directory) matches one of the patterns, ignoring case.

        A pattern with no / matches the file name or the name of any folder above it, so
        ``Extras`` skips everything in a folder of that name. A pattern with a / is
        matched against the whole path, as in ``Season 1/*``.
        """
        parts = [part.lower() for part in relative.parts]
        text = relative.as_posix().lower()
        for pattern in (item.lower() for item in patterns):
            if "/" in pattern:
                if fnmatch.fnmatchcase(text, pattern):
                    return True
            elif any(fnmatch.fnmatchcase(part, pattern) for part in parts):
                return True
        return False

    def files(self) -> Iterator[str]:
        """The file, or the files in the directory (that --include and --exclude allow)."""
        if self.config.file:
            yield self.config.file
            return
        base = Path(self.config.dir)
        if not base.is_dir():
            raise Fatal(f'directory "{self.config.dir}" does not exist')
        paths = base.rglob("*") if self.config.recursive else base.glob("*")
        for path in sorted(paths):
            if not path.is_file() or TEMP_MARK in path.name or BACKUP_MARK in path.name:
                continue
            relative = path.relative_to(base)
            if self.config.include and not self.pattern_hit(relative, self.config.include):
                continue
            if self.pattern_hit(relative, self.config.exclude):
                continue
            yield str(path)

    def read_tracks(self, media: Media) -> list[Track]:
        """Read the tracks of a file."""
        if media.kind == "mkv":
            done = self.query(["mkvmerge", "-F", "json", "-i", media.path])
            parse = parse_mkv_tracks
        else:
            done = self.query(["mediainfo", "--Output=JSON", media.path])
            parse = parse_mediainfo_tracks
        if done is None:
            return []
        try:
            return parse(json.loads(done.stdout))
        except (ValueError, AttributeError, TypeError):
            self.warn(f"could not read the tracks of {media.path}")
            return []

    def tracks(self, media: Media) -> list[Track]:
        """The tracks of a file (read once, then remembered)."""
        if media.tracks is None:
            media.tracks = self.read_tracks(media)
        return media.tracks

    def selected(self, media: Media) -> list[Track]:
        """The tracks of the kind being worked on."""
        return [track for track in self.tracks(media) if track.kind == self.kind]

    def result(self, media: Media, action: str) -> Result:
        return Result(media.path, action, dry_run=self.config.dryrun)

    @staticmethod
    def skip(result: Result, message: str) -> Result:
        result.status, result.message = "skipped", message
        return result

    def resolve_track(self, media: Media, result: Result) -> Track | None:
        """The track chosen with --track (the first if none was)."""
        tracks = self.selected(media)
        wanted = self.config.track or "1"
        number = len(tracks) if wanted == "last" else int(wanted)
        if not 1 <= number <= len(tracks):
            self.skip(result, f"has no {self.word} track {wanted} "
                              f"({len(tracks)} {self.word} tracks)")
            return None
        return tracks[number - 1]

    def finish(self, result: Result) -> Result:
        """Mark a result that made changes as changed, unless it failed."""
        if result.status == "ok" and result.changes:
            result.status = "changed"
        return result

    @staticmethod
    def temp_path(path: str) -> str:
        """Where ffmpeg writes a rewritten file (it needs the real extension)."""
        return f"{Path(path).with_suffix('')}{TEMP_MARK}{Path(path).suffix}"

    @staticmethod
    def ffmpeg_output(media: Media, temp_file: str) -> list[str]:
        """The output options of an ffmpeg rewrite."""
        if media.kind == "mp4":
            return ["-movflags", "+faststart", temp_file]
        return [temp_file]

    def check_space(self, result: Result, path: str, what: str) -> bool:
        """Check there is room beside a file to work on a copy of it as big as the file.

        The room is set aside until the file is done (release_space), so that several
        jobs at once can't each find enough and then use it up between them.
        """
        need = os.path.getsize(path)
        folder = Path(path).resolve().parent
        device = os.stat(folder).st_dev
        with self.space_lock:
            free = shutil.disk_usage(folder).free - self.reserved.get(device, 0)
            if free >= need:
                self.reserved[device] = self.reserved.get(device, 0) + need
                self.holds.setdefault(path, []).append((device, need))
                return True
        result.changes.clear()
        result.status = "error"
        result.message = (f"not enough free space to {what}: needs {human_size(need)}, "
                          f"{human_size(max(free, 0))} free")
        return False

    def release_space(self, path: str) -> None:
        """Give back the room a file set aside."""
        with self.space_lock:
            for device, need in self.holds.pop(path, []):
                self.reserved[device] = self.reserved.get(device, 0) - need

    def start_copy(self, result: Result, path: str, temp_file: str) -> bool:
        """Copy a file, so the copy can be changed and swapped in if that works."""
        if not self.check_space(result, path, "copy the file"):
            return False
        self.pending.append(temp_file)
        self.note(f"Copying:\t{path} -> {temp_file}")
        if self.config.dryrun:
            return True
        try:
            shutil.copy2(path, temp_file)
        except OSError as error:
            Path(temp_file).unlink(missing_ok=True)
            result.status, result.message = "error", f"could not copy the file: {error.strerror}"
            return False
        return True

    @staticmethod
    def backup_path(path: str) -> str:
        """Where the original is kept: the file's name and .fml-bak, or a numbered one if
        that is taken."""
        backup = f"{path}{BACKUP_MARK}"
        number = 0
        while os.path.lexists(backup):
            number += 1
            backup = f"{path}{BACKUP_MARK}.{number}"
        return backup

    def keep_original(self, result: Result, path: str, link: bool) -> bool:
        """Keep the original as a backup. A hard link costs nothing, and is only possible
        while the file is going to be replaced, not edited; if it fails, or a copy is
        needed, the file is copied. Returns False (and an error result) if it can't be."""
        backup = self.backup_path(path)
        result.backup = backup
        result.changes.append(
            f"the original {'would be' if self.config.dryrun else 'is'} kept as {backup}")
        if self.config.dryrun:
            return True
        try:
            if not link:
                raise OSError("a copy is needed")
            os.link(path, backup)
        except OSError:
            if not self.check_space(result, path, "keep a backup"):
                return False
            try:
                shutil.copy2(path, backup)
            except OSError as error:
                Path(backup).unlink(missing_ok=True)
                result.status = "error"
                result.message = f"could not keep a backup: {error.strerror}"
                return False
        return True

    def replace_file(self, result: Result, temp_file: str, path: str) -> None:
        """Replace a file with its changed copy, keeping its permissions and times.

        On an error the copy is removed and the original is left as it was. With --backup
        the original is kept first, and if that fails nothing is replaced.
        """
        if self.config.backup and result.status != "error":
            self.keep_original(result, path, link=True)
        if self.config.dryrun:
            return
        if result.status == "error":
            Path(temp_file).unlink(missing_ok=True)
        else:
            shutil.copystat(path, temp_file)
            os.replace(temp_file, path)
        if temp_file in self.pending:
            self.pending.remove(temp_file)

    def cleanup(self) -> None:
        """Remove any temporary file left when we are stopped part way."""
        for temp_file in self.pending:
            Path(temp_file).unlink(missing_ok=True)
        self.pending.clear()

    # Choosing tracks --------------------------------------------------------

    def pick_track(self, tracks: Sequence[Track], lang: str) -> Track | None:
        """The track to use for a language: with --forced the first forced track,
        otherwise the first one (preferring a track that is not forced, for subtitles)."""
        candidates = [track for track in tracks if track.language == lang]
        if self.config.forced:
            candidates = [track for track in candidates if track.forced]
        elif self.kind == "subtitles":
            candidates = [track for track in candidates if not track.forced] or candidates
        return candidates[0] if candidates else None

    def languages(self, single: str) -> list[str]:
        """The languages to look for, in order: --prefer, or the single language."""
        return self.config.prefer or ([single] if single else [])

    def choose(self, tracks: Sequence[Track], languages: Sequence[str]) -> tuple[str, Track] | None:
        """The first language that has a track, with that track."""
        for lang in languages:
            track = self.pick_track(tracks, lang)
            if track is not None:
                return lang, track
        return None

    def missing_message(self, languages: Sequence[str]) -> str:
        if len(languages) == 1:
            return f'has no {self.word} track with language "{languages[0]}"'
        return f"has no {self.word} track in any of: {', '.join(languages)}"

    # Editing tracks ---------------------------------------------------------

    @staticmethod
    def default_edits(tracks: Sequence[Track], chosen: Track | None, language: str,
                      move: bool = False) -> list[Edit]:
        """The edits that make one track the only default (none if chosen is None),
        give it a language if one is given, and move it first if asked. Tracks that are
        right are left alone."""
        edits = []
        for track in tracks:
            is_chosen = chosen is not None and track.number == chosen.number
            default = None if track.default == is_chosen else is_chosen
            new_language = language if is_chosen and language != track.language else None
            moving = move and is_chosen and track.number != tracks[0].number
            if default is not None or new_language or moving:
                edits.append(Edit(track, default, new_language or None, 1 if moving else None))
        return edits

    def reordered(self, media: Media, moving: Track | None, to: int = 1) -> list[Track]:
        """The tracks of the file, with the one being moved put at a place among its kind."""
        tracks = list(self.tracks(media))
        if moving is not None:
            tracks.remove(moving)
            rest = [track for track in tracks if track.kind == moving.kind]
            if to - 1 < len(rest):
                tracks.insert(tracks.index(rest[to - 1]), moving)
            else:
                tracks.insert(tracks.index(rest[-1]) + 1, moving)
        return tracks

    @staticmethod
    def moving_edit(edits: Sequence[Edit]) -> Edit | None:
        """The edit that moves a track, if there is one."""
        return next((edit for edit in edits if edit.move_to is not None), None)

    def expected_layout(self, media: Media, edits: Sequence[Edit]) -> list[Track]:
        """The tracks the file should have after the edits."""
        mover = self.moving_edit(edits)
        by_track = {(edit.track.kind, edit.track.number): edit for edit in edits}
        expected = []
        for track in self.reordered(media, mover.track if mover else None,
                                    mover.move_to or 1 if mover else 1):
            edit = by_track.get((track.kind, track.number))
            if edit is not None:
                track = replace(track,
                                default=track.default if edit.default is None else edit.default,
                                language=edit.language or track.language,
                                name=track.name if edit.name is None else edit.name,
                                forced=track.forced if edit.forced is None else edit.forced)
            expected.append(track)
        return expected

    def kept_tracks(self, media: Media,
                    replaced: Mapping[str, list[Track]]) -> dict[str, list[Track]]:
        """The tracks of each kind in the order they will have in the new file; replaced
        gives the new list for a kind that changes."""
        kept: dict[str, list[Track]] = {}
        for track in self.tracks(media):
            kept.setdefault(track.kind, []).append(track)
        kept.update(replaced)
        return kept

    @staticmethod
    def name_args(kept: Mapping[str, list[Track]],
                  renamed: Mapping[tuple[str, int], str] | None = None) -> list[str]:
        """ffmpeg options that write the track names back into a rewritten MP4.

        ffmpeg drops the names it cannot read from an MP4 (the ones mediainfo reports as
        the title), so each is written again, at the track's place in the new file. renamed
        gives new names for tracks being renamed (an empty name removes it).
        """
        args: list[str] = []
        for kind, tracks in kept.items():
            selector = STREAM_SELECTORS.get(kind)
            for index, track in enumerate(tracks):
                name = (renamed or {}).get((track.kind, track.number), track.name)
                if selector and name:
                    args += [f"-metadata:s:{selector}:{index}", f"title={name}"]
        return args

    def verify_layout(self, result: Result, rewritten: Media, expected: Sequence[Track],
                      tool: str) -> None:
        """Check a changed copy has the expected tracks, in order, with the expected
        languages and names and (for the kind being edited) default flags. A tool can exit
        successfully without applying a change, or can lose something, so this is checked
        before the copy replaces the original."""
        actual = self.read_tracks(rewritten)
        if len(actual) != len(expected) or any(
                got.kind != want.kind or got.language != want.language
                or got.name.strip() != want.name.strip()
                or (rewritten.kind == "mkv" and got.forced != want.forced)
                or (want.kind == self.kind and got.default != want.default)
                for got, want in ((actual[i], expected[i]) for i in range(len(actual)))):
            result.status = "error"
            result.message = f"{tool} did not make the change to the file"

    def edit_tracks(self, media: Media, result: Result, edits: list[Edit],
                    unchanged: str) -> Result:
        """Apply edits to a file.

        An MKV is edited with mkvpropedit, on a copy that replaces the file if it worked.
        If a track has to be moved, or the file is an MP4,
        the file is rewritten instead, with mkvmerge or ffmpeg. Either way the result is
        read back and checked before it replaces the original.
        """
        if not edits:
            result.status, result.message = "unchanged", unchanged
            return result
        result.changes.extend(edit.describe() for edit in edits)
        mover = self.moving_edit(edits)
        moving = mover.track if mover else None
        if mover is not None:
            result.moved_from = mover.track.number
        if media.kind == "mkv" and moving is None:
            return self.edit_mkv(media, result, edits)
        if not self.check_space(result, media.path, "rewrite the file"):
            return result
        expected = self.expected_layout(media, edits)
        if media.kind == "mkv":
            temp_file = f"{media.path}{TEMP_MARK}"
            self.pending.append(temp_file)
            done = self.apply(result, self.mkvmerge_command(media, edits, moving, temp_file),
                              MKVMERGE_OK)
            tool = "mkvmerge"
        else:
            temp_file = self.temp_path(media.path)
            self.pending.append(temp_file)
            done = self.apply(result, self.ffmpeg_command(media, edits, moving, temp_file))
            tool = "ffmpeg"
        if done and not self.config.dryrun:
            self.verify_layout(result, Media(temp_file, media.kind), expected, tool)
        self.replace_file(result, temp_file, media.path)
        return self.finish(result)

    def edit_mkv(self, media: Media, result: Result, edits: list[Edit]) -> Result:
        """Change flags and languages of an MKV with mkvpropedit, on a copy."""
        selector = SELECTORS[self.kind]
        args: list[str] = []
        for edit in edits:
            args += ["--edit", f"track:{selector}{edit.track.number}"]
            if edit.default is not None:
                args += ["--set", f"flag-default={int(edit.default)}"]
            if edit.language is not None:
                args += ["--set", f"language={edit.language}"]
            if edit.name is not None:
                args += ["--set", f"name={edit.name}"] if edit.name else ["--delete", "name"]
            if edit.forced is not None:
                args += ["--set", f"flag-forced={int(edit.forced)}"]
        temp_file = f"{media.path}{TEMP_MARK}"
        if not self.start_copy(result, media.path, temp_file):
            return result
        if self.apply(result, ["mkvpropedit", temp_file, *args]) and not self.config.dryrun:
            self.verify_layout(result, Media(temp_file, media.kind),
                               self.expected_layout(media, edits), "mkvpropedit")
        self.replace_file(result, temp_file, media.path)
        return self.finish(result)

    def mkvmerge_command(self, media: Media, edits: Sequence[Edit], moving: Track | None,
                         temp_file: str) -> list[str]:
        """Rewrite an MKV with the tracks in a new order and new flags and languages."""
        mover = self.moving_edit(edits)
        order = self.reordered(media, moving, mover.move_to or 1 if mover else 1)
        cmd = ["mkvmerge", "-o", temp_file, "--track-order",
               ",".join(f"0:{track.id}" for track in order)]
        for edit in edits:
            if edit.default is not None:
                cmd += ["--default-track-flag", f"{edit.track.id}:{int(edit.default)}"]
            if edit.language is not None:
                cmd += ["--language", f"{edit.track.id}:{edit.language}"]
            if edit.name is not None:
                cmd += ["--track-name", f"{edit.track.id}:{edit.name}"]
            if edit.forced is not None:
                cmd += ["--forced-display-flag", f"{edit.track.id}:{int(edit.forced)}"]
        return [*cmd, media.path]

    def ffmpeg_command(self, media: Media, edits: Sequence[Edit], moving: Track | None,
                       temp_file: str) -> list[str]:
        """Rewrite a file with ffmpeg, copying the streams, with new flags and languages
        and, if a track is being moved, the streams of its kind in the new order."""
        selector = SELECTORS[self.kind]
        tracks = self.selected(media)
        order = tracks
        maps = ["-map", "0"]
        if moving is not None:
            mover = self.moving_edit(edits)
            order = [track for track in self.reordered(
                media, moving, mover.move_to or 1 if mover else 1) if track.kind == self.kind]
            groups = {"video": ["-map", "0:v?"], "audio": ["-map", "0:a?"],
                      "subtitles": ["-map", "0:s?"]}
            groups[self.kind] = [arg for track in order
                                 for arg in ("-map", f"0:{selector}:{track.number - 1}")]
            maps = [*groups["video"], *groups["audio"], *groups["subtitles"],
                    "-map", "0:d?", "-map", "0:t?"]
        renamed = {(edit.track.kind, edit.track.number): edit.name
                   for edit in edits if edit.name is not None}
        cmd = ["ffmpeg", *FFMPEG_QUIET, "-i", media.path, *maps, "-c", "copy",
               *self.name_args(self.kept_tracks(media, {self.kind: order}), renamed)]
        position = {track.number: index for index, track in enumerate(order)}
        for edit in edits:
            index = position[edit.track.number]
            if edit.default is not None:
                cmd += [f"-disposition:{selector}:{index}",
                        "+default" if edit.default else "-default"]
            if edit.language is not None:
                cmd += [f"-metadata:s:{selector}:{index}", f"language={edit.language}"]
        return cmd + self.ffmpeg_output(media, temp_file)

    @staticmethod
    def promotion(media: Media, survivors: Sequence[Track]) -> Track | None:
        """The track that becomes the default when the default is removed.

        If none of the tracks left has the default flag, the first one gets it. That is
        always done for audio, so a file isn't left without a default track, and for an
        MP4, where the muxer insists on a default track for every kind. An MKV may have no
        default subtitle track, so none is made. AVI has no flags.
        """
        if media.kind == "avi" or not survivors or any(t.default for t in survivors):
            return None
        return survivors[0] if survivors[0].kind == "audio" or media.kind == "mp4" else None

    def remove_tracks(self, media: Media, result: Result, remove: list[Track]) -> Result:
        """Rewrite a file without the given tracks."""
        tracks = self.selected(media)
        if not remove:
            return self.skip(result, f"has no matching {self.word} tracks to remove")
        if self.kind == "audio" and len(remove) == len(tracks) and not self.config.force:
            return self.skip(result, "this would remove every audio track (use --force to allow)")
        if not self.check_space(result, media.path, "rewrite the file"):
            return result
        removed = {(track.kind, track.number) for track in remove}
        survivors = [track for track in tracks if (track.kind, track.number) not in removed]
        promote = self.promotion(media, survivors)
        for track in remove:
            result.changes.append(f"removed {track.label} ({track.language})")
        if promote is not None:
            result.changes.append(f"{promote.label} ({promote.language}): default no -> yes")
        expected = [replace(track, default=True) if promote is not None and track == promote
                    else track for track in self.tracks(media)
                    if (track.kind, track.number) not in removed]
        if media.kind == "mkv":
            ids = ",".join(str(track.id) for track in remove)
            temp_file = f"{media.path}{TEMP_MARK}"
            self.pending.append(temp_file)
            cmd = ["mkvmerge", "-o", temp_file, MKVMERGE_SELECT[self.kind], f"!{ids}"]
            if promote is not None:
                cmd += ["--default-track-flag", f"{promote.id}:1"]
            done = self.apply(result, [*cmd, media.path], MKVMERGE_OK)
        else:
            temp_file = self.temp_path(media.path)
            self.pending.append(temp_file)
            cmd = ["ffmpeg", *FFMPEG_QUIET, "-i", media.path, "-map", "0"]
            for track in remove:
                cmd += ["-map", f"-0:{SELECTORS[self.kind]}:{track.number - 1}"]
            cmd += ["-c", "copy", *self.name_args(self.kept_tracks(media, {self.kind: survivors}))]
            if promote is not None:
                cmd += [f"-disposition:{SELECTORS[self.kind]}:0", "+default"]
            done = self.apply(result, cmd + self.ffmpeg_output(media, temp_file))
        if done and not self.config.dryrun:
            self.verify_layout(result, Media(temp_file, media.kind), expected,
                               "mkvmerge" if media.kind == "mkv" else "ffmpeg")
        self.replace_file(result, temp_file, media.path)
        return self.finish(result)

    # Operations -------------------------------------------------------------

    def do_info(self, media: Media) -> Result:
        result = self.result(media, "info")
        result.data = {"container": CONTAINERS[media.kind],
                       "tracks": [track.to_dict() for track in self.tracks(media)]}
        return result

    def do_get(self, media: Media) -> Result:
        result = self.result(media, "get")
        tracks = self.selected(media)
        if self.config.lang:
            matches = [track for track in tracks if track.language == self.config.lang]
            if not matches:
                return self.skip(result, self.missing_message([self.config.lang]))
            result.data = {"tracks": [track.to_dict() for track in matches]}
        else:
            chosen = [track for track in tracks if track.default]
            if not chosen and self.kind == "audio":
                chosen = tracks[:1]  # AVI has no default flag, so use the first audio track
            result.data = {f"default_{self.kind}": [track.language for track in chosen]}
        return result

    def do_set(self, media: Media, what: str) -> Result:
        result = self.result(media, "set")
        if media.kind == "avi":
            return self.skip(result, "set is only supported for MKV and MP4 files")
        tracks = self.selected(media)
        if what in ("name", "forced", "unforced"):
            return self.set_track_property(media, result, what)
        if what == "default":
            track = self.resolve_track(media, result)
            if track is None:
                return result
            lang = self.config.lang
            return self.edit_tracks(
                media, result, self.default_edits(tracks, track, lang, self.config.move),
                f"{track.label} is already the default" + (f" and {lang}" if lang else "")
                + (" and the first track" if self.config.move else ""))
        if self.config.default or self.config.prefer:
            return self.set_default_language(media, result, tracks)
        track = self.resolve_track(media, result)
        if track is None:
            return result
        edits = [Edit(track, language=self.config.lang)] \
            if track.language != self.config.lang else []
        return self.edit_tracks(media, result, edits,
                                f"{track.label} is already {track.language}")

    def set_track_property(self, media: Media, result: Result, what: str) -> Result:
        """Set the name or the forced flag of the track chosen with --track."""
        if what != "name" and media.kind != "mkv":
            return self.skip(result, "the forced flag can only be set for MKV files (mediainfo "
                                     "cannot read it from an MP4 to check the change)")
        track = self.resolve_track(media, result)
        if track is None:
            return result
        if what == "name":
            wanted = self.config.name or ""
            edits = [Edit(track, name=wanted)] if track.name != wanted else []
            unchanged = (f'{track.label} is already named "{wanted}"' if wanted
                         else f"{track.label} has no name")
        else:
            forced = what == "forced"
            edits = [Edit(track, forced=forced)] if track.forced != forced else []
            unchanged = f"{track.label} is already {'forced' if forced else 'not forced'}"
        return self.edit_tracks(media, result, edits, unchanged)

    def set_default_language(self, media: Media, result: Result,
                             tracks: Sequence[Track]) -> Result:
        """Make the first track in the (preferred) language the only default track."""
        if self.config.default == "none":
            if media.kind == "mp4":
                return self.skip(result, "MP4 files always have a default subtitle track")
            return self.edit_tracks(media, result, self.default_edits(tracks, None, ""),
                                    f"there is already no default {self.word} track")
        languages = self.languages(self.config.default)
        choice = self.choose(tracks, languages)
        if choice is None:
            return self.skip(result, self.missing_message(languages))
        lang, track = choice
        return self.edit_tracks(
            media, result, self.default_edits(tracks, track, "", self.config.move),
            f"{track.label} ({lang}) is already the default"
            + (" and the first track" if self.config.move else ""))

    def do_swap(self, media: Media) -> Result:
        result = self.result(media, "swap")
        audio = self.selected(media)
        if len(audio) < 2:
            return self.skip(result, "needs at least two audio tracks to swap")
        if media.kind in ("mkv", "mp4"):
            return self.edit_tracks(
                media, result, self.default_edits(audio, audio[1], ""),
                f"{audio[1].label} ({audio[1].language}) is already the default")
        if not self.check_space(result, media.path, "rewrite the file"):
            return result
        temp_file = self.temp_path(media.path)
        self.pending.append(temp_file)
        result.changes.append("audio tracks 1 and 2 swapped")
        expected = list(self.tracks(media))
        first, second = [index for index, track in enumerate(expected)
                         if track.kind == "audio"][:2]
        expected[first], expected[second] = expected[second], expected[first]
        done = self.apply(result, [
            "ffmpeg", *FFMPEG_QUIET, "-i", media.path,
            "-map", "0:v?", "-map", "0:a:1", "-map", "0:a:0", "-map", "0:a:2?", "-map", "0:s?",
            "-disposition:a:0", "default", "-disposition:a:1", "none", "-c", "copy", temp_file])
        if done and not self.config.dryrun:
            self.verify_layout(result, Media(temp_file, media.kind), expected, "ffmpeg")
        self.replace_file(result, temp_file, media.path)
        return self.finish(result)

    def do_delete(self, media: Media, what: str) -> Result:
        result = self.result(media, "delete")
        if what == "track":
            track = self.resolve_track(media, result)
            if track is None:
                return result
            return self.remove_tracks(media, result, [track])
        if media.kind == "avi":
            return self.skip(result, "deleting by language is only supported for MKV and "
                                     "MP4 files")
        remove = [track for track in self.selected(media) if track.language == self.config.lang]
        return self.remove_tracks(media, result, remove)

    def do_preserve(self, media: Media) -> Result:
        result = self.result(media, "preserve")
        if media.kind == "avi":
            return self.skip(result, "preserving by language is only supported for MKV and "
                                     "MP4 files")
        tracks = self.selected(media)
        languages = self.languages(self.config.lang)
        keep = next((lang for lang in languages
                     if any(track.language == lang for track in tracks)), None)
        if keep is None:
            if not self.config.force:
                return self.skip(result, f"{self.missing_message(languages)}, so nothing "
                                         "would be left (use --force to remove them all)")
            return self.remove_tracks(media, result, tracks)
        return self.remove_tracks(media, result,
                                  [track for track in tracks if track.language != keep])

    # Dispatch ---------------------------------------------------------------

    @staticmethod
    def tools_for(task: Task, kind: str) -> Sequence[str]:
        if task.name in ("info", "get"):
            return READ_TOOLS[kind]
        return WRITE_TOOLS[kind]

    def process(self, task: Task, path: str) -> Result | None:
        """Run a task on one file."""
        if not os.path.isfile(path):
            return Result(path, task.name, "error", "does not exist")
        kind = detect_kind(path)
        if not kind:
            if self.config.file:
                return Result(path, task.name, "error", "is not a supported media file")
            self.note(f"Skipping {path}: not a supported media file")
            return None
        self.require(self.tools_for(task, kind))
        media = Media(path, kind)
        try:
            outcome = self.dispatch(task, media)
            if self.config.log and outcome.status == "changed" and not self.config.dryrun:
                self.write_log(media, outcome)
        finally:
            self.release_space(path)
        return outcome

    def dispatch(self, task: Task, media: Media) -> Result:
        if task.name == "info":
            return self.do_info(media)
        if task.name == "get":
            return self.do_get(media)
        if task.name == "set":
            return self.do_set(media, task.what)
        if task.name == "swap":
            return self.do_swap(media)
        if task.name == "delete":
            return self.do_delete(media, task.what)
        return self.do_preserve(media)

    # The log and undoing ----------------------------------------------------

    def write_log(self, media: Media, result: Result) -> None:
        """Add a line to the log about a change: what the tracks were and are now."""
        entry = {
            "time": datetime.datetime.now().isoformat(timespec="seconds"),
            "version": __version__, "file": os.path.abspath(media.path),
            "action": result.action, "kind": self.kind, "changes": result.changes,
            "moved_from": result.moved_from or None,
            "backup": os.path.abspath(result.backup) if result.backup else None,
            "before": [track.to_dict() for track in media.tracks or []],
            "after": [track.to_dict() for track in self.read_tracks(Media(media.path, media.kind))],
        }
        try:
            with self.lock, open(self.config.log, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry, ensure_ascii=False) + "\n")
        except OSError as error:
            self.warn(f"could not write the log {self.config.log}: {error.strerror}")

    def read_log(self, path: str) -> list[dict[str, Any]]:
        """The entries of a log, oldest first."""
        entries = []
        with open(path, encoding="utf-8") as handle:
            for number, line in enumerate(handle, start=1):
                if not line.strip():
                    continue
                try:
                    entries.append(json.loads(line))
                except ValueError:
                    self.warn(f"{path}: line {number} is not valid JSON, so it was ignored")
        return entries

    def in_scope(self, file: str) -> bool:
        """Whether a file in the log is one --file or --dir (if given) asks about."""
        if self.config.file:
            return os.path.abspath(self.config.file) == file
        if self.config.dir:
            base, folder = os.path.abspath(self.config.dir), os.path.dirname(file)
            return folder == base or (self.config.recursive and folder.startswith(base + os.sep))
        return True

    @staticmethod
    def same_state(first: Sequence[Track], second: Sequence[Track]) -> bool:
        """Whether two lists of tracks agree in order, kind, language, flags and names."""
        return len(first) == len(second) and all(
            (a.kind, a.language, a.default, a.forced, a.name)
            == (b.kind, b.language, b.default, b.forced, b.name) for a, b in zip(first, second))

    def restore_edits(self, media: Media, before: Sequence[Track],
                      moved_from: int | None) -> list[Edit]:
        """The edits that put the tracks of the kind back as they were."""
        current = [track for track in self.tracks(media) if track.kind == self.kind]
        wanted = [track for track in before if track.kind == self.kind]
        moving = current[0] if moved_from and moved_from != 1 else None
        restored = list(current)
        if moving is not None and moved_from:
            restored.remove(moving)
            restored.insert(moved_from - 1, moving)
        edits = []
        for now, old in zip(restored, wanted):
            default = old.default if now.default != old.default else None
            language = old.language if now.language != old.language else None
            name = old.name if now.name != old.name else None
            # the forced flag of an MP4 can't be read back to check, so it is not touched
            forced = old.forced if now.forced != old.forced and media.kind == "mkv" else None
            to = moved_from if now is moving else None
            if any(change is not None for change in (default, language, name, forced, to)):
                edits.append(Edit(now, default, language, to, name, forced))
        return edits

    def restore_backup(self, result: Result, media: Media, backup: str,
                       before: Sequence[Track]) -> Result:
        """Put the kept original back, once it is checked to be what was logged."""
        if not self.same_state(self.read_tracks(Media(backup, media.kind)), before):
            return self.skip(result, f"the backup {backup} is not what was logged, so it was "
                                     "not used")
        result.changes.append(f"restored from {backup}")
        if not self.config.dryrun:
            os.replace(backup, media.path)
        result.status = "changed"
        return result

    def undo_log(self, path: str) -> None:
        """Undo the changes in a log, newest first."""
        planned: set[str] = set()  # in a dry run, files with an undo that was only planned
        for entry in reversed(self.read_log(path)):
            file = str(entry.get("file", ""))
            if not self.in_scope(file):
                continue
            if self.config.dryrun and file in planned:
                result = Result(file, "undo", "unchanged", dry_run=True,
                                message="an older change: it can be checked once the newer one "
                                        "above has really been undone")
                self.report(result)
                continue
            result = self.undo_entry(entry)
            if self.config.dryrun and result.status == "changed":
                planned.add(file)
            self.report(result)

    def undo_entry(self, entry: dict[str, Any]) -> Result:
        """Reverse one change from the log, if the file is as that change left it."""
        path = str(entry.get("file", ""))
        result = Result(path, "undo", dry_run=self.config.dryrun)
        try:
            before = tracks_from_dicts(entry["before"])
            after = tracks_from_dicts(entry["after"])
            kind = str(entry["kind"])
        except (KeyError, TypeError):
            return self.skip(result, "this log entry is not understood")
        if not os.path.isfile(path):
            return self.skip(result, "no longer exists")
        container = detect_kind(path)
        if not container:
            return self.skip(result, "is not a supported media file")
        self.require(WRITE_TOOLS[container])
        media = Media(path, container)
        current = self.tracks(media)
        if self.same_state(current, before) and not self.same_state(current, after):
            result.status, result.message = "unchanged", "is already as it was before this change"
            return result
        if not self.same_state(current, after):
            return self.skip(result, "has changed since it was logged, so it was left alone")
        backup = entry.get("backup")
        if backup and os.path.isfile(backup):
            return self.restore_backup(result, media, str(backup), before)
        if len(before) != len(after) or container == "avi":
            return self.skip(result, "can't be undone without the backup it would need (the "
                                     "change removed tracks or rewrote an AVI; use --backup)")
        previous, self.config.kind = self.config.kind, kind
        try:
            edits = self.restore_edits(media, before, entry.get("moved_from"))
            return self.edit_tracks(media, result, edits, "is already as it was")
        finally:
            self.config.kind = previous

    def report(self, result: Result) -> None:
        """Record a result, and print it unless the output is JSON."""
        self.results.append(result)
        if self.config.format == "json":
            return
        name = f"{result.file}: " if result.file else ""
        if result.action == "check":
            for entry in result.data:
                if entry["path"]:
                    print(f"Found:\t\t{entry['tool']} ({entry['path']})")
                else:
                    print(f"Missing:\t{entry['tool']}")
        if result.status in ("error", "skipped"):
            self.warn(f"{name}{result.message}")
        elif result.action == "check":
            pass
        elif result.action == "info":
            print(f"{result.file} ({result.data['container']})")
            if result.data["tracks"]:
                print(format_tracks(tracks_from_dicts(result.data["tracks"])))
            else:
                print("no tracks found")
        elif result.action == "get":
            self.print_get(result)
        elif result.changes:
            prefix = "[dry-run] " if result.dry_run else ""
            for change in result.changes:
                print(f"{prefix}{name}{change}")
        else:
            print(f"{name}{result.message}")

    def print_get(self, result: Result) -> None:
        if "tracks" in result.data:
            print(result.file)
            print(format_tracks(tracks_from_dicts(result.data["tracks"])))
            return
        languages = next(value for key, value in result.data.items() if key.startswith("default_"))
        for language in languages or ["none"]:
            print(f"{result.file}: {language}" if self.config.dir else language)

    def process_in_parallel(self, task: Task, paths: Sequence[str]) -> None:
        """Process several files at once, reporting them in the order they were found, so
        the output doesn't depend on which finished first."""
        with concurrent.futures.ThreadPoolExecutor(max_workers=self.config.jobs) as pool:
            futures = [pool.submit(self.process, task, path) for path in paths]
            try:
                for future in futures:
                    outcome = future.result()
                    if outcome is not None:
                        self.report(outcome)
            except BaseException:  # a stop or a fatal error: start no more, then let it go on
                for future in futures:
                    future.cancel()
                raise

    def run(self) -> int:
        """Run the tasks; return the exit status."""
        try:
            for task in self.config.tasks:
                if task.name == "check":
                    self.report(self.check_tools())
                    continue
                if task.name == "undo":
                    self.undo_log(task.what)
                    continue
                paths = list(self.files())
                if self.config.jobs > 1 and len(paths) > 1:
                    self.process_in_parallel(task, paths)
                    continue
                for path in paths:
                    outcome = self.process(task, path)
                    if outcome is not None:
                        self.report(outcome)
        except Fatal as stop:
            self.warn(str(stop))
            return 1
        except KeyboardInterrupt:
            self.warn("interrupted")
            return 130
        finally:
            self.cleanup()
            if self.config.format == "json":
                print(json.dumps({"version": __version__, "dry_run": self.config.dryrun,
                                  "results": [result.to_dict() for result in self.results]},
                                 indent=2, ensure_ascii=False))
        return 1 if any(result.status == "error" for result in self.results) else 0


# Command line ----------------------------------------------------------------

class QueueAction(argparse.Action):
    """Queue a task, so several can be given and run in the order they were."""

    def __init__(self, option_strings: Sequence[str], dest: str, task: str,
                 **kwargs: Any) -> None:
        super().__init__(option_strings, dest, **kwargs)
        self.task = task

    def __call__(self, parser: argparse.ArgumentParser, namespace: argparse.Namespace,
                 values: str | Sequence[Any] | None, option_string: str | None = None) -> None:
        queue: list[Task] = list(getattr(namespace, self.dest, None) or [])
        queue.append(Task(self.task, values if isinstance(values, str) else ""))
        setattr(namespace, self.dest, queue)


def language_arg(text: str) -> str:
    """argparse type: a language name or code, as an ISO 639-2/B code."""
    try:
        return resolve_language(text)
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from None


def default_arg(text: str) -> str:
    """argparse type: a language, or ``none`` (no default subtitle track)."""
    return "none" if text.strip().lower() == "none" else language_arg(text)


def prefer_arg(text: str) -> list[str]:
    """argparse type: a comma separated list of languages."""
    languages = [language_arg(item) for item in text.split(",") if item.strip()]
    if not languages:
        raise argparse.ArgumentTypeError("give at least one language")
    return list(dict.fromkeys(languages))


def kind_arg(text: str) -> str:
    """argparse type: the kind of track, audio or subtitles (sub works too)."""
    kind = KINDS.get(text.strip().lower())
    if kind is None:
        raise argparse.ArgumentTypeError(f'"{text}" is not audio or sub (subtitles)')
    return kind


def jobs_arg(text: str) -> int:
    """argparse type: a number of jobs, 1 or more."""
    if text.isdigit() and int(text) >= 1:
        return int(text)
    raise argparse.ArgumentTypeError(f'"{text}" is not a number of jobs (1 or more)')


def track_arg(text: str) -> str:
    """argparse type: a track number (from 1) or ``last``."""
    if text == "last" or (text.isdigit() and int(text) >= 1):
        return text
    raise argparse.ArgumentTypeError(f'"{text}" is not a track number (from 1) or "last"')


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="fml",
        description="Fix the default audio and subtitle languages, and the tracks, of media files.",
        epilog="""examples:
  fml --set lang --default English --file video.mkv     make English the default audio
  fml --set lang --default English --move -i video.mkv  ... and move it to be the first audio track
  fml --set lang --prefer eng,jpn,fre --dir films       the best of those languages in each file
  fml --set default --track 2 --lang English -i a.mkv   make audio track 2 the default and English
  fml --set lang --default English --kind sub -i a.mkv  make English the default subtitles
  fml --set lang --default English --kind sub --forced  ... using the forced subtitle track
  fml --set lang --default none --kind sub -i a.mkv     turn the default subtitles off
  fml --get lang --dir /media/films --recursive         default audio language of each film
  fml --preserve lang --lang eng --file video.mkv       remove the audio that isn't English
  fml --delete track --track last --file video.mp4      remove the last audio track
  fml --set lang --default French --dir films --dryrun  show what would change
""",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("-V", "--version", action="version", version=f"%(prog)s {__version__}")

    target = parser.add_argument_group("files", "MKV (and WebM), MP4 (M4V, MOV) and AVI files")
    exclusive = target.add_mutually_exclusive_group()
    exclusive.add_argument("-i", "--file", metavar="FILE", help="file to process")
    exclusive.add_argument("-d", "--dir", metavar="DIR", help="directory to process")
    target.add_argument("-r", "--recursive", action="store_true",
                        help="include subdirectories of --dir")
    target.add_argument("--include", action="append", metavar="PATTERN",
                        help="with --dir, only process files that match PATTERN (a file name or "
                             "folder name such as '*.mkv', or a path such as 'Season 1/*'); "
                             "can be given more than once")
    target.add_argument("--exclude", action="append", metavar="PATTERN",
                        help="with --dir, skip files that match PATTERN (for example Extras "
                             "skips every folder of that name); can be given more than once")

    actions = parser.add_argument_group("actions", "may be combined; they run in the order given")
    actions.add_argument("-C", "--check", action=QueueAction, task="check", dest="tasks",
                         nargs=0, help="check the required commands are installed")
    actions.add_argument("-I", "--info", action=QueueAction, task="info", dest="tasks",
                         nargs=0, help="list the tracks of the file")
    actions.add_argument("-u", "--undo", action=QueueAction, task="undo", dest="tasks",
                         metavar="LOG", help="undo the changes recorded in a --log file, newest "
                                             "first, for the files that are still as they were "
                                             "left (limited to --file or --dir if given)")
    for flag, short, name, help_text in (
            ("--get", "-g", "get", "get the default language, or with --lang the tracks "
                                   "in a language"),
            ("--set", "-c", "set", "name: set the name of track --track to --name; forced, "
                                   "unforced: set or clear its forced flag (MKV); "
                                   "lang: make --default (or the first of --prefer) the "
                                   "default, or with --lang set the language of track "
                                   "--track (1); default: make track --track the default, "
                                   "and with --lang set its language"),
            ("--swap", "-s", "swap", "make the second audio track the default (the order "
                                     "is unchanged: see --move)"),
            ("--delete", "-z", "delete", "remove the tracks in --lang, or track --track"),
            ("--preserve", "-p", "preserve", "remove all tracks that are not in --lang "
                                             "(or the first of --prefer that is present)")):
        actions.add_argument(short, flag, action=QueueAction, task=name, dest="tasks",
                             choices=TASK_CHOICES[name], metavar="{" + ",".join(
                                 TASK_CHOICES[name]) + "}", help=help_text)

    options = parser.add_argument_group("settings")
    options.add_argument("-k", "--kind", type=kind_arg, default="audio", metavar="{audio,sub}",
                         help="the kind of track the actions work on (default audio)")
    options.add_argument("-l", "--lang", type=language_arg, metavar="LANG",
                         help="language, by name or ISO 639 code (English, en, eng)")
    options.add_argument("-D", "--default", type=default_arg, metavar="LANG",
                         help="language to make the default (none turns the default "
                              "subtitles off)")
    options.add_argument("--prefer", type=prefer_arg, metavar="LANGS",
                         help="languages in order of preference, e.g. eng,jpn,fre: the first "
                              "that a file has is used, so one command suits a mixed library")
    options.add_argument("--forced", action="store_true",
                         help="with --kind sub, pick the forced subtitle track")
    options.add_argument("-t", "--track", type=track_arg, metavar="N",
                         help="track number from 1 (among tracks of the kind), or last")
    options.add_argument("-f", "--force", action="store_true",
                         help="allow removing every audio track")
    options.add_argument("--name", metavar="TEXT",
                         help="the track name for --set name (an empty name removes it)")
    options.add_argument("--move", action="store_true",
                         help="with --set, also move the chosen track to be the first track of "
                              "its kind, for players that play the first track whatever its "
                              "flag; the file is rewritten")
    options.add_argument("-j", "--jobs", type=jobs_arg, default=1, metavar="N",
                         help="process N files at once (default 1; rewriting is limited by "
                              "the disk, so more than a few rarely helps)")
    options.add_argument("--log", metavar="FILE",
                         help="record each change in FILE (one JSON line per file), so that "
                              "--undo can reverse it")
    options.add_argument("--backup", action="store_true",
                         help="keep the original beside the changed file, as NAME.fml-bak "
                              "(a hard link where possible, so it costs no space)")
    options.add_argument("-T", "--dryrun", action="store_true",
                         help="show what would change without changing anything")
    options.add_argument("-v", "--verbose", action="store_true",
                         help="show the commands that are run")
    options.add_argument("-F", "--format", choices=("text", "json"), default="text",
                         help="output format (default text)")
    options.add_argument("--install", action="store_true",
                         help="install missing commands (apt on Ubuntu, brew on macOS)")
    return parser


def check_arguments(parser: argparse.ArgumentParser, args: argparse.Namespace) -> None:
    """Reject combinations of arguments that make no sense."""
    tasks: list[Task] = args.tasks or []
    if not tasks:
        parser.error("no action given (use --check, --info, --get, --set, --swap, --delete "
                     "or --preserve)")
    if any(task.name not in ("check", "undo") for task in tasks) and not (args.file or args.dir):
        parser.error("no file given (use --file or --dir)")
    for task in tasks:
        if task.name == "undo" and not os.path.isfile(task.what):
            parser.error(f'--undo: "{task.what}" is not a file')
    if (args.include or args.exclude) and not args.dir:
        parser.error("--include and --exclude only work with --dir")
    if args.log and any(task.name == "undo" for task in tasks):
        parser.error("--undo reads a log and can't be used with --log")
    if args.forced and args.kind != "subtitles":
        parser.error("--forced only applies to subtitles (use --kind sub)")
    if args.name is not None and not any(t.name == "set" and t.what == "name" for t in tasks):
        parser.error("--name only works with --set name")
    if args.move and not any(
            task.name == "set" and (task.what == "default" or args.default or args.prefer)
            for task in tasks):
        parser.error("--move only works with --set lang --default (or --prefer), "
                     "or with --set default --track")
    if args.move and args.default == "none":
        parser.error("--move can't be used with --default none")
    for task in tasks:
        if task.name == "set" and task.what == "lang":
            given = [name for name, value in (("--default", args.default),
                                              ("--prefer", args.prefer),
                                              ("--lang", args.lang)) if value]
            if len(given) != 1:
                parser.error("--set lang needs one of --default (a language), --prefer "
                             "(languages in order) or --lang (to set the language of a track)"
                             + (f", not {' and '.join(given)}" if given else ""))
        if task.name == "set" and task.what in ("name", "forced", "unforced"):
            if not args.track:
                parser.error(f"--set {task.what} needs --track")
            if args.default or args.prefer or args.move or args.forced:
                parser.error(f"--set {task.what} takes --track (and --name for a name), "
                             "not --default, --prefer, --move or --forced")
        if task.name == "set" and task.what == "name" and args.name is None:
            parser.error("--set name needs --name")
        if task.name == "set" and task.what == "default":
            if not args.track:
                parser.error("--set default needs --track")
            if args.default or args.prefer:
                parser.error("--set default takes --track (and --lang to set its language), "
                             "not --default or --prefer")
        if args.default == "none" and not (
                task.name == "set" and task.what == "lang" and args.kind == "subtitles"):
            parser.error("--default none only works with --set lang --kind sub")
        if args.prefer and not (task.name == "preserve" or (
                task.name == "set" and task.what == "lang")):
            parser.error("--prefer only works with --set lang and --preserve")
        if task.name == "swap" and args.kind != "audio":
            parser.error("--swap only works on audio")
        needs_lang = task.name == "preserve" or (task.name == "delete" and task.what == "lang")
        if needs_lang and not (args.lang or (task.name == "preserve" and args.prefer)):
            parser.error(f"--{task.name} {task.what} needs --lang"
                         + (" or --prefer" if task.name == "preserve" else ""))
        if task.name == "delete" and task.what == "track" and not args.track:
            parser.error("--delete track needs --track")


def terminate(signum: int, frame: object) -> NoReturn:
    """Handle being asked to stop like Ctrl-C, so temporary files are removed."""
    raise KeyboardInterrupt


def main(argv: Sequence[str] | None = None) -> int:
    signal.signal(signal.SIGTERM, terminate)
    parser = build_parser()
    if argv is None:
        argv = sys.argv[1:]
    if not argv:
        parser.print_help()
        return 0
    args = parser.parse_args(argv)
    check_arguments(parser, args)
    config = Config(
        file=args.file or "", dir=args.dir or "", recursive=args.recursive,
        lang=args.lang or "", default=args.default or "", track=args.track or "",
        kind=args.kind, forced=args.forced, prefer=args.prefer or [],
        verbose=args.verbose, dryrun=args.dryrun, install=args.install, force=args.force,
        backup=args.backup, log=args.log or "", move=args.move,
        name=args.name, include=args.include or [], exclude=args.exclude or [], jobs=args.jobs,
        format=args.format,
        tasks=args.tasks)
    return Fml(config).run()


if __name__ == "__main__":
    sys.exit(main())
