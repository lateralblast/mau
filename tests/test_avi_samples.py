"""AVI files from the NASA HEASARC historical video collection, for the AVI code on real files:
the track list, the removal of the only audio stream, and the index. They are not in the
repository: download them (https://heasarc.gsfc.nasa.gov/docs/heasarc/videos/historical.html,
nine files: choose, commit, saturn5, eagleland, smallstep, flagmoon, moonflag, mooncall and
lemviews, all .avi, from the /Videos/historical/ folder of that site), put them in one folder,
and set MAU_AVI_SAMPLES to the folder. The tests skip when it is not set."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import struct
import subprocess
from pathlib import Path
from typing import Callable

import pytest
from avi_twin import add_second_audio
from conftest import NATIVE_AVI, Ran, requires_tools

NAMES = ["choose", "commit", "saturn5", "eagleland", "smallstep", "flagmoon", "moonflag",
         "mooncall", "lemviews"]
SAMPLES = Path(os.environ["MAU_AVI_SAMPLES"]) if os.environ.get("MAU_AVI_SAMPLES") else None
pytestmark = [requires_tools, pytest.mark.skipif(SAMPLES is None or not SAMPLES.is_dir(),
                                                 reason="set MAU_AVI_SAMPLES to the NASA files")]
SAMPLE_NAMES = [f"{name}.avi" for name in NAMES]


def video_md5(path: Path) -> str:
    out = subprocess.run(["ffmpeg", "-v", "error", "-xerror", "-i", str(path), "-map", "0:v",
                          "-f", "rawvideo", "-"], capture_output=True, check=True).stdout
    return hashlib.md5(out).hexdigest()


def audio_streams(path: Path) -> int:
    out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "a", "-show_entries",
                          "stream=index", "-of", "csv=p=0", str(path)], capture_output=True,
                         text=True, check=True).stdout
    return len(out.split())


def index_is_right(path: Path) -> bool:
    """Every index entry names its chunk, from the movi fourcc or from the start of the file, and
    a rec entry names the LIST header of a rec list."""
    data = path.read_bytes()
    movi = data.find(b"movi") - 8
    index = data.rfind(b"idx1")
    count = struct.unpack("<I", data[index + 4:index + 8])[0] // 16
    entries = [data[index + 8 + 16 * i:index + 24 + 16 * i] for i in range(count)]

    def names(base: int, entry: bytes) -> bool:
        at = base + struct.unpack("<I", entry[8:12])[0]
        got = data[at:at + 12]
        is_rec = entry[:4] == b"rec " and got[:4] == b"LIST" and got[8:12] == b"rec "
        return got[:4] == entry[:4] or is_rec

    return all(names(movi + 8, e) for e in entries) or all(names(0, e) for e in entries)


@pytest.mark.skipif(not NATIVE_AVI,
                    reason="the native build lists an AVI's streams from its header")
@pytest.mark.parametrize("name", SAMPLE_NAMES)
def test_the_track_list_matches_mediainfo(run: Callable[..., Ran], name: str) -> None:
    path = Path(SAMPLES or "") / name
    listed = json.loads(run("--info", "--format", "json", "-i", path).out)["results"][0]["data"]
    report = subprocess.run(["mediainfo", "--Output=JSON", str(path)], capture_output=True,
                            text=True, check=True).stdout
    mediainfo = [(t["@type"].lower(), t.get("Format")) for t in json.loads(report)["media"]["track"]
                 if t["@type"] in ("Video", "Audio")]
    ours = [(t["type"], t["codec"]) for t in listed["tracks"]]
    assert [kind for kind, _ in ours] == [kind for kind, _ in mediainfo]
    assert [codec for _, codec in ours] == [codec for _, codec in mediainfo]


@pytest.mark.parametrize("name", SAMPLE_NAMES)
def test_removing_the_only_audio_keeps_the_video_and_a_right_index(
        run: Callable[..., Ran], tmp_path: Path, name: str) -> None:
    source = Path(SAMPLES or "") / name
    path = tmp_path / name
    shutil.copy(source, path)
    video = video_md5(source)
    ran = run("--delete", "track", "--track", "1", "--kind", "audio", "--force", "-i", path)
    assert ran.code == 0, ran.err
    assert audio_streams(path) == 0
    assert video_md5(path) == video, "the video is the same as before"
    assert index_is_right(path)
    assert path.stat().st_size < source.stat().st_size


@pytest.mark.parametrize("name", SAMPLE_NAMES)
def test_a_swap_needs_two_audio_streams(run: Callable[..., Ran], tmp_path: Path, name: str) -> None:
    path = tmp_path / name
    shutil.copy(Path(SAMPLES or "") / name, path)
    before = path.read_bytes()
    ran = run("--swap", "lang", "-i", path)
    assert "needs at least two audio tracks" in ran.out + ran.err
    assert path.read_bytes() == before


def decoded_audio(path: Path, index: int) -> str:
    command = ["ffmpeg", "-v", "error", "-xerror", "-i", str(path), "-map", f"0:a:{index}",
               "-f", "s16le", "-"]
    out = subprocess.run(command, capture_output=True, check=True).stdout
    return hashlib.md5(out).hexdigest()


TWIN_NAMES = ["choose.avi", "mooncall.avi"]  # an absolute index, and a relative one with rec lists


@pytest.fixture
def twin(tmp_path: Path, request: pytest.FixtureRequest) -> Path:
    """The sample, with a second audio stream added in its own layout (see avi_twin)."""
    path = tmp_path / ("twin-" + request.param)
    add_second_audio(Path(SAMPLES or "") / request.param, path)  # type: ignore[arg-type]
    return path


@pytest.mark.parametrize("twin", TWIN_NAMES, indirect=True)
def test_a_second_audio_stream_swaps_on_a_real_layout(run: Callable[..., Ran], twin: Path) -> None:
    first, second = decoded_audio(twin, 0), decoded_audio(twin, 1)
    assert first != second
    video = video_md5(twin)
    ran = run("--swap", "lang", "-i", twin)
    assert ran.code == 0, ran.err
    assert decoded_audio(twin, 0) == second and decoded_audio(twin, 1) == first
    assert video_md5(twin) == video
    assert index_is_right(twin)


@pytest.mark.parametrize("twin", TWIN_NAMES, indirect=True)
@pytest.mark.parametrize("track, keep", [("1", 1), ("2", 0)])
def test_removing_one_of_two_audio_streams_keeps_the_other(
        run: Callable[..., Ran], twin: Path, track: str, keep: int) -> None:
    audio = [decoded_audio(twin, 0), decoded_audio(twin, 1)]
    video = video_md5(twin)
    ran = run("--delete", "track", "--track", track, "--kind", "audio", "-i", twin)
    assert ran.code == 0, ran.err
    assert audio_streams(twin) == 1
    assert decoded_audio(twin, 0) == audio[keep]
    assert video_md5(twin) == video
    assert index_is_right(twin)
