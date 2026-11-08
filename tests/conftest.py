"""Shared test helpers.

Real media files are generated with ffmpeg and inspected with mkvmerge, mediainfo and ffprobe
directly (not with fml's own parsing), so the tests check what was really written. Tests that
need those tools are marked ``real`` and are skipped if they are missing. Everything else uses
stub tools or no tools at all.
"""

from __future__ import annotations

import inspect
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import unicodedata
from collections.abc import Sequence
from pathlib import Path
from typing import Any, Callable, NamedTuple

import pytest

import fml

REAL_TOOLS = ("ffmpeg", "ffprobe", "mkvmerge", "mkvpropedit", "mediainfo")
HAVE_REAL_TOOLS = all(shutil.which(tool) for tool in REAL_TOOLS)
requires_tools = pytest.mark.skipif(
    not HAVE_REAL_TOOLS, reason="needs ffmpeg, mkvtoolnix and mediainfo")

MKV_HEADER = bytes.fromhex("1a45dfa39f428288") + b"matroska"
MP4_HEADER = b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom"
AVI_HEADER = b"RIFF\x00\x00\x00\x00AVI LIST"

# mediainfo reports two letter codes for MP4 files
TWO_LETTER = {"it": "ita", "en": "eng", "fr": "fre", "de": "ger", "ja": "jpn", "es": "spa"}


class Ran(NamedTuple):
    """The outcome of running fml in the test process."""

    code: int
    out: str
    err: str


@pytest.fixture(autouse=True)
def isolate_model(tmp_path_factory: pytest.TempPathFactory,
                  monkeypatch: pytest.MonkeyPatch) -> None:
    """No test touches the user's data folder or downloads the whisper.cpp model: the data
    folder is a temporary one, and the model address is a closed local port (a download there
    fails at once). macOS keeps the data folder under HOME (Library/Application Support) and
    Linux under XDG_DATA_HOME, so both are pointed at the temporary folder."""
    home = tmp_path_factory.mktemp("home")
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("XDG_DATA_HOME", str(home / "data"))
    monkeypatch.setenv("MAU_WHISPER_MODEL_URL", "http://127.0.0.1:9/no-model")


@pytest.fixture(autouse=True)
def isolate(monkeypatch: pytest.MonkeyPatch) -> Any:
    """Make runs repeatable: a known OS, and put back the SIGTERM handler fml installs."""
    monkeypatch.setattr(fml, "detect_distro", lambda: "Ubuntu")
    monkeypatch.setattr(fml.platform, "system", lambda: "Linux")
    previous = signal.getsignal(signal.SIGTERM)
    yield
    signal.signal(signal.SIGTERM, previous)


# Set MAU_BIN to the path of the Rust build (rust/target/release/mau-cli) to run the tests that
# go through ``run`` against it instead of fml.py. Tests that reach into fml.py's own functions
# are skipped then (see pytest_collection_modifyitems).
MAU_BIN = os.environ.get("MAU_BIN", "")


def _built_without_mkvtoolnix() -> bool:
    """Whether the binary under test was built without mkvtoolnix (``--features no-mkvtoolnix``).

    That build does not mention mkvtoolnix in ``--check`` at all, while every other build lists
    mkvmerge there, installed or not. MAU_SPIKE_NO_MKVTOOLNIX also says so, for a binary that
    hides the tools at run time instead.
    """
    if os.environ.get("MAU_SPIKE_NO_MKVTOOLNIX"):
        return True
    if not MAU_BIN:
        return False
    check = subprocess.run([MAU_BIN, "--check"], capture_output=True, text=True).stdout
    return "mkvmerge" not in check


# The build without mkvtoolnix (the App Store and LGPL build) does the MKV work with ffmpeg. Tests
# that need mkvtoolnix are skipped there; the others expect ffmpeg where mkvtoolnix would work.
NO_MKVTOOLNIX = _built_without_mkvtoolnix()


def _built_without_ffmpeg() -> bool:
    """Whether the binary under test has no ffmpeg at all (``--features no-ffmpeg``, ``--nolgpl``).

    That build does not mention ffmpeg in ``--check``, while every other build lists it, installed
    or not. MAU_SPIKE_NO_FFMPEG also says so, for a binary that hides ffmpeg at run time instead.
    """
    if os.environ.get("MAU_SPIKE_NO_FFMPEG"):
        return True
    if not MAU_BIN:
        return False
    check = subprocess.run([MAU_BIN, "--check"], capture_output=True, text=True).stdout
    return "ffmpeg" not in check


# The build without ffmpeg (--nolgpl) refuses a change that needs ffmpeg, before touching the file.
# Tests that need ffmpeg to succeed are skipped there.
NO_FFMPEG = _built_without_ffmpeg()
needs_ffmpeg = pytest.mark.skipif(NO_FFMPEG, reason="needs ffmpeg (not in the --nolgpl build)")
# the build that writes MKV flags, names and languages itself (native-mkv): MAU_NATIVE_MKV=1
NATIVE_MKV = bool(os.environ.get("MAU_NATIVE_MKV"))
# the Rust build does MP4 languages, flags and order in place
NATIVE_MP4 = bool(os.environ.get("MAU_NATIVE_MP4"))
# the Rust build puts an AVI's audio streams in order itself
NATIVE_AVI = bool(os.environ.get("MAU_NATIVE_AVI"))

# Stub tests that fake a tool the native MKV build does not run (it reads and writes MKV itself,
# so the stub mkvmerge or ffmpeg output is never seen). They are skipped under MAU_NATIVE_MKV, and
# each should be rewritten as a real-file test or removed (the list and its status are in
# APPSTORE.md, "Gaps to address later").
NATIVE_MKV_SKIPPED_STUBS = frozenset({
    "test_a_signal_during_an_edit_removes_the_copy",
    "test_a_failing_tool_leaves_the_original_and_exits_1",
    "test_a_move_that_keeps_the_old_order_is_caught",
    "test_a_removal_that_loses_names_is_caught",
    "test_a_tool_that_does_nothing_is_caught",
    "test_an_edit_that_leaves_the_language_alone_is_caught",
    "test_jobs_with_failures_report_each_in_order",
    "test_mkvmerge_errors_fail_the_file",
    "test_one_bad_file_does_not_stop_the_rest",
    "test_removal_with_the_wrong_tracks_is_rejected",
    "test_a_good_edit_replaces_the_file_with_the_checked_copy",
    "test_removing_every_subtitle_track_needs_no_force",
    "test_preserve_refuses_when_nothing_would_be_left",
    "test_leftover_temporary_files_are_not_treated_as_media",
    "test_directory_mode_is_in_sorted_order",
    "test_each_action_only_needs_its_own_tools",
    "test_backups_are_not_treated_as_media",
    "test_no_filters_finds_everything",
    "test_filters",
    "test_mkvmerge_warnings_count_as_success",
})

needs_mkvtoolnix = pytest.mark.skipif(
    NO_MKVTOOLNIX, reason="needs mkvtoolnix (not in the App Store build)")


def mkv_tool(name: str) -> str:
    """The tool that changes an MKV in this build: mkvtoolnix's, or ffmpeg without mkvtoolnix."""
    return "ffmpeg" if NO_MKVTOOLNIX and name.startswith("mkv") else name


PYTHON_ONLY = re.compile(
    r"monkeypatch\.setattr\(\s*fml|fml\.(Fml|main|Task|Result|Config|Media|shutil|subprocess|os|signal)\b"
    r"|fml\.terminate|signal\.getsignal|stop_during|import fml|SystemExit")


def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    if NATIVE_MKV:
        for item in items:
            if item.name.split("[")[0] in NATIVE_MKV_SKIPPED_STUBS:
                item.add_marker(pytest.mark.skip(
                    reason="tests how an external tool (mkvmerge or mkvpropedit) fails, which "
                           "the native MKV build does not run; the native build has its own "
                           "failure tests (see APPSTORE.md)"))
    if not MAU_BIN:
        return
    skip = pytest.mark.skip(reason="tests fml.py's own functions, not the command line")
    for item in items:
        function = getattr(item, "function", None)
        if item.fspath.basename == "test_unit.py" or (
                function is not None and PYTHON_ONLY.search(inspect.getsource(function))):
            item.add_marker(skip)


@pytest.fixture
def run(capsys: pytest.CaptureFixture[str]) -> Callable[..., Ran]:
    """Run fml.main with arguments and return its exit status, stdout and stderr."""
    if MAU_BIN:
        def external(*args: object) -> Ran:
            done = subprocess.run([MAU_BIN, *[str(arg) for arg in args]],
                                  capture_output=True, text=True, check=False)
            return Ran(done.returncode, done.stdout, done.stderr)
        return external

    def runner(*args: object) -> Ran:
        try:
            code = fml.main([str(arg) for arg in args])
        except SystemExit as stop:  # argparse exits for --help, --version and usage errors
            code = stop.code if isinstance(stop.code, int) else 1
        out, err = capsys.readouterr()
        return Ran(code, out, err)
    return runner


# Real media ------------------------------------------------------------------

SRT = "1\n00:00:00,000 --> 00:00:00,900\nHello\n\n"


def make_media(out: Path, audio: list[tuple[str, str | None, bool]],
               subs: list[tuple[str, bool, bool]], chapters: bool = False,
               video_title: str | None = None) -> None:
    """Generate a one second file with ffmpeg.

    audio is a list of (language, title, default) and subs of (language, forced, default).
    Subtitles are only possible for MKV and MP4. chapters adds two chapters, an attachment
    and a title (MKV only).
    """
    kind = out.suffix.lstrip(".")
    work = out.parent / f".{out.name}.build"
    work.mkdir(exist_ok=True)
    cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
           "-f", "lavfi", "-i", "testsrc=d=1:s=160x90:r=10"]
    for index in range(len(audio)):
        cmd += ["-f", "lavfi", "-i", f"sine=f={300 + 100 * (index + 1)}:d=1"]
    for index in range(len(subs)):
        srt = work / f"sub{index}.srt"
        srt.write_text(SRT)
        cmd += ["-i", str(srt)]
    inputs = 1 + len(audio) + len(subs)
    if chapters:
        (work / "meta.txt").write_text(
            ";FFMETADATA1\ntitle=Test Film\n"
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=One\n"
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Two\n")
        (work / "attachment.bin").write_text("attachment")
        cmd += ["-i", str(work / "meta.txt")]
    for number in range(inputs):
        cmd += ["-map", str(number)]
    if video_title:
        cmd += ["-metadata:s:v:0", f"title={video_title}"]
    if chapters:
        cmd += ["-map_metadata", str(inputs), "-map_chapters", str(inputs),
                "-attach", str(work / "attachment.bin"),
                "-metadata:s:t", "mimetype=application/octet-stream"]
    if kind == "avi":
        cmd += ["-c:v", "mpeg4", "-c:a", "mp2"]
    else:
        # ffmpeg's own encoders, not libx264 or libmp3lame, so an LGPL ffmpeg can run the suite
        cmd += ["-c:v", "mpeg4", "-c:a", "aac", "-c:s", "mov_text" if kind == "mp4" else "srt"]
    for index, (language, title, default) in enumerate(audio):
        cmd += [f"-metadata:s:a:{index}", f"language={language}"]
        if title:
            cmd += [f"-metadata:s:a:{index}", f"title={title}"]
        cmd += [f"-disposition:a:{index}", "default" if default else "0"]
    for index, (language, forced, default) in enumerate(subs):
        flags = "+".join(name for name, on in (("default", default), ("forced", forced)) if on)
        cmd += [f"-metadata:s:s:{index}", f"language={language}",
                f"-disposition:s:{index}", flags or "0"]
    subprocess.run(cmd + [str(out)], check=True, capture_output=True)
    shutil.rmtree(work)


class MediaCache:
    """Builds each distinct test file once for the whole run."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.built: dict[str, Path] = {}

    def get(self, name: str, audio: list[tuple[str, str | None, bool]],
            subs: list[tuple[str, bool, bool]], chapters: bool,
            video_title: str | None = None) -> Path:
        key = json.dumps([name, audio, subs, chapters, video_title])
        if key not in self.built:
            folder = Path(tempfile.mkdtemp(dir=self.root))
            make_media(folder / name, audio, subs, chapters, video_title)
            self.built[key] = folder / name
        return self.built[key]


@pytest.fixture(scope="session")
def media_cache(tmp_path_factory: pytest.TempPathFactory) -> MediaCache:
    return MediaCache(tmp_path_factory.mktemp("media"))


@pytest.fixture
def make(tmp_path: Path, media_cache: MediaCache) -> Callable[..., Path]:
    """Make a file in the test's own directory: ``make("a.mkv", audio, subs)``."""
    def maker(name: str, audio: list[tuple[str, str | None, bool]],
              subs: list[tuple[str, bool, bool]] | None = None, chapters: bool = False,
              where: Path | None = None, video_title: str | None = None) -> Path:
        source = media_cache.get(name, audio, subs or [], chapters, video_title)
        target = (where or tmp_path) / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        return target
    return maker


# The files most tests use
FILM_AUDIO = [("ita", "Italian", True), ("eng", "English", False), ("fre", "French", False)]
FILM_SUBS = [("eng", False, False), ("eng", True, False), ("ita", False, False)]


@pytest.fixture
def film_mkv(make: Callable[..., Path]) -> Path:
    """MKV: audio ita (default), eng, fre; subtitles eng, eng forced, ita; chapters."""
    return make("film.mkv", FILM_AUDIO, FILM_SUBS, chapters=True)


@pytest.fixture
def und_mkv(make: Callable[..., Path]) -> Path:
    """MKV whose three audio tracks all have no language."""
    return make("und.mkv", [("und", None, True), ("und", None, False), ("und", None, False)])


@pytest.fixture
def film_mp4(make: Callable[..., Path]) -> Path:
    """MP4: audio ita (default), eng, fre; subtitles eng (default), ita."""
    return make("film.mp4", [("ita", None, True), ("eng", None, False), ("fre", None, False)],
                [("eng", False, True), ("ita", False, False)])


@pytest.fixture
def three_avi(make: Callable[..., Path]) -> Path:
    """AVI with three audio tracks."""
    return make("three.avi", [("und", None, False)] * 3)


# Inspecting files without fml -----------------------------------------------

def mkv_json(path: Path) -> dict[str, Any]:
    out = subprocess.run(["mkvmerge", "-J", str(path)], capture_output=True, text=True,
                         check=True).stdout
    data: dict[str, Any] = json.loads(out)
    return data


def mkv_layout(path: Path) -> list[str]:
    """Audio and subtitle tracks of an MKV as ``a:ita*`` (``*`` default, ``F`` forced)."""
    layout = []
    for track in mkv_json(path)["tracks"]:
        if track["type"] == "video":
            continue
        props = track["properties"]
        layout.append(f"{track['type'][0]}:{props.get('language')}"
                      f"{'*' if props.get('default_track') else ''}"
                      f"{'F' if props.get('forced_track') else ''}")
    return layout


def mp4_layout(path: Path) -> list[str]:
    """The same for an MP4, read with mediainfo."""
    out = subprocess.run(["mediainfo", "--Output=JSON", str(path)], capture_output=True,
                         text=True, check=True).stdout
    layout = []
    for track in json.loads(out)["media"]["track"]:
        if track["@type"] not in ("Audio", "Text"):
            continue
        language = TWO_LETTER.get(track.get("Language", ""), track.get("Language", "und"))
        layout.append(f"{'a' if track['@type'] == 'Audio' else 's'}:{language}"
                      f"{'*' if track.get('Default') == 'Yes' else ''}"
                      f"{'F' if track.get('Forced', '').startswith('Yes') else ''}")
    return layout


def mkv_names(path: Path) -> list[str]:
    """The names of the audio and subtitle tracks of an MKV ('' for none)."""
    # mkvtoolnix on macOS stores accents in decomposed form: compare the composed form
    return [unicodedata.normalize("NFC", track["properties"].get("track_name", ""))
            for track in mkv_json(path)["tracks"] if track["type"] != "video"]


def mp4_titles(path: Path) -> list[str]:
    """The names mediainfo reports for the video, audio and subtitle tracks of an MP4."""
    out = subprocess.run(["mediainfo", "--Output=JSON", str(path)], capture_output=True,
                         text=True, check=True).stdout
    return [track.get("Title", "") for track in json.loads(out)["media"]["track"]
            if track["@type"] in ("Video", "Audio", "Text")]


def audio_streams(path: Path) -> int:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a", "-show_entries", "stream=index",
         "-of", "csv=p=0", str(path)], capture_output=True, text=True, check=True).stdout
    return len(out.split())


def leftovers(folder: Path) -> list[str]:
    """Temporary files fml should never leave behind."""
    return [path.name for path in folder.rglob("*") if fml.TEMP_MARK in path.name]


def same_file(first: Path, second: Path) -> bool:
    return first.read_bytes() == second.read_bytes()


# Stub tools ------------------------------------------------------------------

STUB_MKVMERGE = """
import json, os, sys
folder = os.path.dirname(os.path.abspath(__file__))
config = json.load(open(os.path.join(folder, "config.json")))
args = sys.argv[1:]
if args[:1] == ["-o"]:
    open(args[1], "wb").write(bytes.fromhex("1a45dfa39f428288") + b"matroska" + b"CHANGED")
    sys.exit(config.get("mkvmerge_exit", 0))
changed = args[-1].endswith(".fml-tmp") or open(args[-1], "rb").read().endswith(b"CHANGED")
key = "mkv_after" if changed else "mkv"
print(json.dumps({"tracks": config.get(key, config["mkv"])}))
"""

STUB_MKVPROPEDIT = """
import json, os, sys
folder = os.path.dirname(os.path.abspath(__file__))
config = json.load(open(os.path.join(folder, "config.json")))
if config.get("mkvpropedit_exit", 0):
    sys.stderr.write("mkvpropedit: Error: cannot open the file\\n")
else:
    open(sys.argv[1], "ab").write(b"CHANGED")  # so later reads see the changed tracks
sys.exit(config.get("mkvpropedit_exit", 0))
"""

STUB_MEDIAINFO = """
import json, os, sys
folder = os.path.dirname(os.path.abspath(__file__))
config = json.load(open(os.path.join(folder, "config.json")))
key = "mi_after" if sys.argv[-1].endswith(".fml-tmp.mp4") else "mi"
print(json.dumps({"media": {"track": config.get(key, config.get("mi", []))}}))
"""

STUB_FFMPEG = """
import json, os, sys
folder = os.path.dirname(os.path.abspath(__file__))
config = json.load(open(os.path.join(folder, "config.json")))
out = sys.argv[-1]
if ".mkv" in os.path.basename(out):
    # written as a changed copy, so the stand-in ffprobe reads the tracks of `mkv_after` from it
    open(out, "wb").write(bytes.fromhex("1a45dfa39f428288") + b"matroska" + b"CHANGED")
else:
    open(out, "wb").write(bytes.fromhex("0000001866747970") + b"mp42" + b"\\0" * 8)
# a failure from any stand-in tool (the mkvtoolnix keys count too, for the build without mkvtoolnix)
code = (config.get("ffmpeg_exit", 0) or config.get("mkvmerge_exit", 0)
        or config.get("mkvpropedit_exit", 0))
if code:
    sys.stderr.write("ffmpeg: Invalid data\\n")
sys.exit(code)
"""

# The ffprobe stand-in: the tracks of `mkv` as ffprobe prints streams, or `mkv_after` for a file
# that was written (one with the change marker), as the mkvmerge stand-in does.
STUB_FFPROBE = """
import json, os, sys
folder = os.path.dirname(os.path.abspath(__file__))
config = json.load(open(os.path.join(folder, "config.json")))
target = sys.argv[-1]
if "-show_entries" in sys.argv:
    print("0")
    sys.exit(0)
changed = open(target, "rb").read().endswith(b"CHANGED")
tracks = config.get("mkv_after" if changed else "mkv", config["mkv"])
kinds = {"video": "video", "audio": "audio", "subtitles": "subtitle"}
streams = []
for track in tracks:
    if track["type"] not in kinds:
        continue
    props = track.get("properties", {})
    tags = {}
    if props.get("language"):
        tags["language"] = props["language"]
    if props.get("track_name"):
        tags["title"] = props["track_name"]
    streams.append({
        "index": len(streams), "codec_type": kinds[track["type"]],
        "codec_name": track.get("codec", "").lower(), "channels": props.get("audio_channels", 0),
        "tags": tags,
        "disposition": {"default": int(bool(props.get("default_track"))),
                        "forced": int(bool(props.get("forced_track")))},
    })
print(json.dumps({"streams": streams}))
"""


def mkv_stub_tracks(audio: list[tuple[str, bool]],
                    subs: Sequence[tuple[str, bool]] = ()) -> list[dict[str, Any]]:
    """Tracks in the form ``mkvmerge -F json -i`` prints them."""
    tracks: list[dict[str, Any]] = [
        {"id": 0, "type": "video", "codec": "AVC", "properties": {"language": "und"}}]
    for language, default in audio:
        tracks.append({"id": len(tracks), "type": "audio", "codec": "AAC",
                       "properties": {"language": language, "default_track": default}})
    for language, default in subs:
        tracks.append({"id": len(tracks), "type": "subtitles", "codec": "SubRip/SRT",
                       "properties": {"language": language, "default_track": default}})
    return tracks


class Stubs:
    """A directory of stub tools that is the only thing on PATH."""

    def __init__(self, folder: Path, monkeypatch: pytest.MonkeyPatch) -> None:
        self.folder = folder
        self.config: dict[str, Any] = {}
        folder.mkdir()
        tools = [("mkvmerge", STUB_MKVMERGE), ("mkvpropedit", STUB_MKVPROPEDIT),
                 ("mediainfo", STUB_MEDIAINFO), ("ffmpeg", STUB_FFMPEG), ("ffprobe", STUB_FFPROBE)]
        for name, body in tools:
            path = folder / name
            path.write_text(f"#!{sys.executable}\n{body}")
            path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
        monkeypatch.setenv("PATH", str(folder))
        self.save()

    def save(self) -> None:
        (self.folder / "config.json").write_text(json.dumps(self.config))

    def set(self, **config: Any) -> None:
        self.config.update(config)
        self.save()


@pytest.fixture
def stubs(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Stubs:
    return Stubs(tmp_path / "stubbin", monkeypatch)


@pytest.fixture
def fake_mkv(tmp_path: Path) -> Path:
    """A file that looks like a Matroska file (for use with stub tools)."""
    path = tmp_path / "film.mkv"
    path.write_bytes(MKV_HEADER)
    return path


@pytest.fixture
def fake_mp4(tmp_path: Path) -> Path:
    path = tmp_path / "film.mp4"
    path.write_bytes(MP4_HEADER)
    return path


def env_without(tool_dir: Path) -> dict[str, str]:
    return {**os.environ, "PATH": str(tool_dir)}
