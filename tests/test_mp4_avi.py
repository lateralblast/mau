"""Tests on real MP4 and AVI files made with ffmpeg."""

from __future__ import annotations

import json
import os
import re
import stat
import struct
import subprocess
from pathlib import Path
from typing import Callable

import pytest
from conftest import (
    MAU_BIN,
    NATIVE_AVI,
    NATIVE_MP4,
    Ran,
    audio_streams,
    leftovers,
    mkv_layout,
    mp4_layout,
    mp4_titles,
    requires_tools,
)
from mp4_text import empty_vtt, stpp_entry, subtitle_mp4, ttml, vttc, wvtt_entry

import fml

pytestmark = [pytest.mark.real, requires_tools]

FILM = ["a:ita*", "a:eng", "a:fre", "s:eng*", "s:ita"]


def top_level_boxes(path: Path) -> list[str]:
    data = path.read_bytes()
    boxes, position = [], 0
    while position + 8 <= len(data):
        size, kind = struct.unpack(">I4s", data[position:position + 8])
        boxes.append(kind.decode())
        position += size if size >= 8 else len(data)
    return boxes


def duration(path: Path) -> float:
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration",
                          "-of", "csv=p=0", str(path)], capture_output=True, text=True,
                         check=True).stdout
    return float(out)


def stream_kinds(path: Path) -> list[str]:
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "stream=codec_type",
                          "-of", "csv=p=0", str(path)], capture_output=True, text=True,
                         check=True).stdout
    return out.split()


# MP4 -------------------------------------------------------------------------

def test_the_fixture_is_what_the_tests_expect(film_mp4: Path) -> None:
    assert mp4_layout(film_mp4) == FILM


def test_info(run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--info", "-i", film_mp4)
    rows = [line.split() for line in ran.out.splitlines()]
    assert rows[0] == [str(film_mp4), "(MP4)"]
    assert [r[0] for r in rows[2:]] == ["video:1", "audio:1", "audio:2", "audio:3",
                                        "subtitles:1", "subtitles:2"]
    assert rows[3][3:5] == ["ita", "yes"]


def test_get(run: Callable[..., Ran], film_mp4: Path) -> None:
    assert run("--get", "lang", "-i", film_mp4).out == "ita\n"
    assert run("--get", "lang", "--kind", "sub", "-i", film_mp4).out == "eng\n"
    ran = run("--get", "lang", "--lang", "fr", "-i", film_mp4)
    assert ran.out.splitlines()[2].split()[0] == "audio:3"


def test_set_default_language(run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert ran.out.splitlines() == [f"{film_mp4}: audio track 1 (ita): default yes -> no",
                                    f"{film_mp4}: audio track 2 (eng): default no -> yes"]
    assert mp4_layout(film_mp4) == ["a:ita", "a:eng*", "a:fre", "s:eng*", "s:ita"]
    assert leftovers(film_mp4.parent) == []


def test_set_default_is_unchanged_the_second_time(run: Callable[..., Ran],
                                                  film_mp4: Path) -> None:
    run("--set", "lang", "--default", "English", "-i", film_mp4)
    before = film_mp4.read_bytes()
    ran = run("--set", "lang", "--default", "English", "-i", film_mp4)
    assert "is already the default" in ran.out and film_mp4.read_bytes() == before


def test_the_streams_and_duration_are_kept(run: Callable[..., Ran], film_mp4: Path) -> None:
    kinds, length = stream_kinds(film_mp4), duration(film_mp4)
    run("--set", "lang", "--default", "English", "--move", "-i", film_mp4)
    assert stream_kinds(film_mp4) == kinds and duration(film_mp4) == pytest.approx(length, abs=0.05)


def test_permissions_and_times_are_kept(run: Callable[..., Ran], film_mp4: Path) -> None:
    film_mp4.chmod(0o640)
    os.utime(film_mp4, (1_600_000_000, 1_600_000_000))
    run("--set", "lang", "--default", "English", "-i", film_mp4)
    info = film_mp4.stat()
    assert stat.S_IMODE(info.st_mode) == 0o640 and int(info.st_mtime) == 1_600_000_000


@pytest.mark.skipif(NATIVE_MP4,
                    reason="the native MP4 edit changes the tracks in place and does not move "
                           "the index to the front (see APPSTORE.md)")
def test_the_index_is_moved_to_the_front(run: Callable[..., Ran], film_mp4: Path) -> None:
    assert top_level_boxes(film_mp4).index("moov") > top_level_boxes(film_mp4).index("mdat")
    run("--set", "lang", "--default", "English", "-i", film_mp4)
    boxes = top_level_boxes(film_mp4)
    assert boxes.index("moov") < boxes.index("mdat")


def test_default_subtitles(run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--set", "lang", "--default", "Italian", "--kind", "sub", "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:eng", "s:ita*"]


@pytest.mark.skipif(bool(MAU_BIN),
                    reason="the Rust version can turn the default subtitles off in an MP4")
def test_default_none_is_refused_for_subtitles(run: Callable[..., Ran], film_mp4: Path) -> None:
    before = film_mp4.read_bytes()
    ran = run("--set", "lang", "--default", "none", "--kind", "sub", "-i", film_mp4)
    assert "MP4 files always have a default subtitle track" in ran.err
    assert film_mp4.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--default none for an MP4 is Rust only")
def test_default_none_turns_the_default_subtitles_off_in_an_mp4(run: Callable[..., Ran],
                                                                film_mp4: Path,
                                                                tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    names = mp4_titles(film_mp4)
    size = film_mp4.stat().st_size
    ran = run("--set", "lang", "--default", "none", "--kind", "sub", "--log", log, "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert ran.out.strip() == f"{film_mp4}: subtitle track 1 (eng): default yes -> no"
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:eng",
                                    "s:ita"]  # the audio is untouched
    assert film_mp4.stat().st_size == size  # nothing was rewritten, one bit was changed
    assert mp4_titles(film_mp4) == names
    flags = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "s", "-show_entries",
                            "stream_disposition=default", "-of", "csv=p=0", str(film_mp4)],
                           capture_output=True, text=True, check=True).stdout.split()
    assert flags == ["0", "0"]  # ffprobe agrees with mediainfo
    assert subprocess.run(["ffmpeg", "-v", "error", "-i", str(film_mp4), "-c", "copy", "-f",
                           "null", "-"],
                          capture_output=True).returncode == 0  # the file still reads through
    assert leftovers(film_mp4.parent) == []
    again = run("--set", "lang", "--default", "none", "--kind", "sub", "-i", film_mp4)
    assert "there is already no default subtitle track" in again.out
    assert run("--undo", log).code == 0
    assert mp4_layout(film_mp4)[3:] == ["s:eng*", "s:ita"]  # and the flag comes back


@pytest.mark.skipif(not MAU_BIN, reason="--default none for an MP4 is Rust only")
def test_default_none_in_an_mp4_dry_run_changes_nothing(run: Callable[..., Ran],
                                                        film_mp4: Path) -> None:
    before = film_mp4.read_bytes()
    ran = run("--set", "lang", "--default", "none", "--kind", "sub", "--dryrun", "-i", film_mp4)
    assert "subtitle track 1 (eng): default yes -> no" in ran.out
    assert film_mp4.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--default none for an MP4 is Rust only")
def test_default_none_works_for_an_mp4_with_a_default_subtitle_added_by_mau(
        run: Callable[..., Ran], make: Callable[..., Path], tmp_path: Path) -> None:
    plain = make("plain.mp4", [("ita", None, True), ("eng", None, False)])
    sub = tmp_path / "x.srt"
    sub.write_text("1\n00:00:00,000 --> 00:00:00,900\nCiao\n\n")
    assert run("--add", "subtitle", "--sub-file", sub, "--lang", "ita", "-i", plain).code == 0
    assert mp4_layout(plain)[-1] == "s:ita*"  # the muxer made it the default
    assert run("--set", "lang", "--default", "none", "--kind", "sub", "-i", plain).code == 0
    assert mp4_layout(plain) == ["a:ita*", "a:eng", "s:ita"]


def test_set_the_language_of_a_track(run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--set", "lang", "--lang", "German", "--track", "3", "-i", film_mp4)
    assert ran.out.strip() == f"{film_mp4}: audio track 3: language fre -> ger"
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:ger", "s:eng*", "s:ita"]


def test_set_default_by_track_and_tag_it(run: Callable[..., Ran], film_mp4: Path) -> None:
    run("--set", "default", "--track", "3", "--lang", "Japanese", "-i", film_mp4)
    assert mp4_layout(film_mp4) == ["a:ita", "a:eng", "a:jpn*", "s:eng*", "s:ita"]


def test_swap_makes_the_second_track_the_only_default(run: Callable[..., Ran],
                                                      film_mp4: Path) -> None:
    run("--set", "default", "--track", "3", "-i", film_mp4)
    run("--swap", "lang", "-i", film_mp4)
    assert mp4_layout(film_mp4)[:3] == ["a:ita", "a:eng*", "a:fre"]


def test_move_the_audio_track_first(run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "--move", "-i", film_mp4)
    assert ran.out.splitlines() == [
        f"{film_mp4}: audio track 1 (ita): default yes -> no",
        f"{film_mp4}: audio track 2 (eng): default no -> yes, moved to the first audio track"]
    assert mp4_layout(film_mp4) == ["a:eng*", "a:ita", "a:fre", "s:eng*", "s:ita"]


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_put_the_tracks_in_any_order(run: Callable[..., Ran], film_mp4: Path,
                                     tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    before = mp4_layout(film_mp4)
    ran = run("--set", "order", "--order", "3,1,2", "-i", film_mp4, "--log", log)
    assert ran.code == 0, ran.err
    assert mp4_layout(film_mp4) == ["a:fre", "a:ita*", "a:eng", "s:eng*", "s:ita"]
    assert run("--undo", log).code == 0
    assert mp4_layout(film_mp4) == before


def srt(folder: Path, name: str = "it.srt") -> Path:
    path = folder / name
    path.write_text("1\n00:00:00,000 --> 00:00:00,900\nCiao\n\n", encoding="utf-8")
    return path


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_add_a_subtitle_to_an_mp4(run: Callable[..., Ran], film_mp4: Path, tmp_path: Path) -> None:
    before_names = mp4_titles(film_mp4)
    ran = run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "Spanish", "--name",
              "Latino",
              "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert ran.out.strip() == f"{film_mp4}: added subtitle track 3 (spa) from it.srt"
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:eng*", "s:ita", "s:spa"]
    assert mp4_titles(film_mp4) == before_names + ["Latino"]  # the other names survive the rewrite
    codecs = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "s", "-show_entries",
                             "stream=codec_name", "-of", "csv=p=0", str(film_mp4)],
                            capture_output=True, text=True, check=True).stdout.split()
    assert codecs == ["mov_text"] * 3
    assert leftovers(film_mp4.parent) == []


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_the_first_subtitle_of_an_mp4_becomes_the_default(run: Callable[..., Ran],
                                                          make: Callable[..., Path],
                                                          tmp_path: Path) -> None:
    plain = make("plain.mp4", [("ita", None, True), ("eng", None, False)])
    assert run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "ita", "-i",
               plain).code == 0
    assert mp4_layout(plain) == ["a:ita*", "a:eng", "s:ita*"]  # the mov muxer insists on it


@pytest.mark.skipif(not MAU_BIN, reason="--sub-dir is Rust only")
def test_an_mp4_gets_the_folder_subtitles_it_can_have(run: Callable[..., Ran], film_mp4: Path,
                                                      tmp_path: Path) -> None:
    folder = tmp_path / "subs"
    folder.mkdir()
    for name in ("film.spa.srt", "film.por.forced.srt"):
        srt(folder, name)
    (folder / "film.ger.sup").write_bytes(b"PG")
    ran = run("--add", "subtitle", "--sub-dir", folder, "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert mp4_layout(film_mp4)[-1] == "s:spa"
    assert "film.por.forced.srt" not in ran.err  # a forced MP4 subtitle is added, forced
    assert mp4_layout(film_mp4)[-2:] == ["s:ptF", "s:spa"]  # por is written as pt
    assert "film.ger.sup was not added: a .sup subtitle can't be added to an MP4" in ran.err


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_undo_an_added_mp4_subtitle_from_the_backup(run: Callable[..., Ran], film_mp4: Path,
                                                    tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    before = mp4_layout(film_mp4)
    run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "ita", "--backup", "--log",
        log, "-i", film_mp4)
    assert len(mp4_layout(film_mp4)) == len(before) + 1
    assert run("--undo", log).code == 0
    assert mp4_layout(film_mp4) == before


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_a_picture_subtitle_is_not_added_to_an_mp4(run: Callable[..., Ran], film_mp4: Path,
                                                   tmp_path: Path) -> None:
    sup = tmp_path / "x.sup"
    sup.write_bytes(b"PG")
    before = film_mp4.read_bytes()
    ran = run("--add", "subtitle", "--sub-file", sup, "--lang", "ita", "-i", film_mp4)
    assert "a .sup subtitle can't be added to an MP4" in ran.err
    assert film_mp4.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_adding_a_subtitle_is_not_for_avi(run: Callable[..., Ran], three_avi: Path,
                                          tmp_path: Path) -> None:
    before = three_avi.read_bytes()
    ran = run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "ita", "-i", three_avi)
    assert "only supported for MKV and MP4" in ran.err
    assert three_avi.read_bytes() == before


MULTI_SRT = ("1\n00:00:00,500 --> 00:00:02,000\nCiao\n\n"
             "2\n00:00:03,000 --> 00:00:04,250\nSeconda riga\nche continua\n\n"
             "3\n00:00:05,000 --> 00:00:07,500\nFine\n")


def cues_of(path: Path, track: int = 0) -> str:
    """Subtitle track ``track`` (counted among the subtitles) of an MP4, as ffmpeg reads it back,
    as SubRip."""
    command = ["ffmpeg", "-v", "error", "-i", str(path), "-map", f"0:s:{track}", "-f", "srt", "-"]
    return subprocess.run(command, capture_output=True, text=True, check=True).stdout


@pytest.mark.skipif(not NATIVE_MP4,
                    reason="the native build adds SubRip files to an MP4 in this program")
def test_a_srt_is_added_to_an_mp4_in_this_program_and_reads_back_as_ffmpeg_adds_it(
        run: Callable[..., Ran], film_mp4: Path, tmp_path: Path) -> None:
    """The same file, once with the native add and once with ffmpeg's: the layout, and the cues
    ffmpeg reads back (their text and timing, the gaps included), are the same."""
    subs = tmp_path / "multi.srt"
    subs.write_text(MULTI_SRT, encoding="utf-8")
    reference = tmp_path / "reference.mp4"
    reference.write_bytes(film_mp4.read_bytes())
    made = str(reference) + ".out.mp4"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(reference), "-i", str(subs),
                    "-map", "0",
                    "-map", "1:0", "-c", "copy", "-c:s", "mov_text", "-metadata:s:s:2",
                    "language=spa", "-movflags", "+faststart", made], check=True)
    ran = run("--add", "subtitle", "--sub-file", subs, "--lang", "spa", "-i", film_mp4, "-v")
    assert ran.code == 0, ran.err
    assert "native-mp4 add-text-tracks" in ran.err
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:eng*", "s:ita", "s:spa"]
    # the new track is the third subtitle (the fixture has two)
    assert cues_of(film_mp4, 2) == cues_of(Path(made), 2)
    assert "00:00:03,000 --> 00:00:04,250" in cues_of(film_mp4, 2)
    decode = ["ffmpeg", "-v", "error", "-xerror", "-i", str(film_mp4), "-f", "null", "-"]
    assert subprocess.run(decode, capture_output=True).returncode == 0
    assert leftovers(film_mp4.parent) == []


@pytest.mark.skipif(not NATIVE_MP4,
                    reason="the native build adds SubRip files to an MP4 in this program")
def test_the_first_srt_added_to_an_mp4_with_no_subtitles_is_the_default(
        run: Callable[..., Ran], make: Callable[..., Path], tmp_path: Path) -> None:
    plain = make("plain.mp4", [("ita", None, True), ("eng", None, False)], [])
    subs = srt(tmp_path)
    ran = run("--add", "subtitle", "--sub-file", subs, "--lang", "ita", "-i", plain)
    assert ran.code == 0, ran.err
    assert mp4_layout(plain) == ["a:ita*", "a:eng", "s:ita*"]
    assert "00:00:00,000 --> 00:00:00,900" in cues_of(plain, 0)


@pytest.mark.skipif(not NATIVE_MP4,
                    reason="the native build adds SubRip files to an MP4 in this program")
def test_cues_that_overlap_are_added_by_ffmpeg_not_this_program(
        run: Callable[..., Ran], film_mp4: Path, tmp_path: Path) -> None:
    """mov_text can't hold overlapping cues, so the file is converted by ffmpeg, as before."""
    subs = tmp_path / "overlap.srt"
    overlapping = "1\n00:00:00,000 --> 00:00:02,000\nA\n\n2\n00:00:01,000 --> 00:00:03,000\nB\n"
    subs.write_text(overlapping, encoding="utf-8")
    ran = run("--add", "subtitle", "--sub-file", subs, "--lang", "spa", "-i", film_mp4, "-T", "-v")
    assert ran.code == 0, ran.err
    assert "native-mp4 add-text-tracks" not in ran.err and "ffmpeg" in ran.err


@pytest.mark.skipif(not MAU_BIN, reason="the refusal is in the Rust build")
def test_overlapping_cues_are_refused_without_ffmpeg_and_ffmpeg_is_never_started(
        run: Callable[..., Ran], film_mp4: Path, tmp_path: Path,
        monkeypatch: pytest.MonkeyPatch) -> None:
    """Without ffmpeg (--nolgpl) an MP4 that needs ffmpeg for its overlapping cues is refused
    before the file is replaced. A stand-in ffmpeg on PATH records if it is started, and it must not
    be."""
    stand_in = tmp_path / "stand-in"
    stand_in.mkdir()
    marker = tmp_path / "ffmpeg-was-started"
    program = stand_in / "ffmpeg"
    program.write_text(f'#!/bin/sh\ntouch "{marker}"\nexit 0\n')
    program.chmod(0o755)
    monkeypatch.setenv("PATH", f"{stand_in}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setenv("MAU_SPIKE_NO_FFMPEG", "1")
    subs = tmp_path / "overlap.srt"
    subs.write_text("1\n00:00:00,000 --> 00:00:02,000\nA\n\n2\n00:00:01,000 --> 00:00:03,000\nB\n",
                    encoding="utf-8")
    before = film_mp4.read_bytes()
    ran = run("--add", "subtitle", "--sub-file", subs, "--lang", "spa", "-i", film_mp4)
    assert ran.code == 1
    assert "ffmpeg is not included in this build (--nolgpl), and this change needs it" in ran.err
    assert not marker.exists()
    assert film_mp4.read_bytes() == before
    assert not [p for p in tmp_path.iterdir() if ".fml-" in p.name], "no temporary file is left"


@pytest.mark.skipif(not MAU_BIN, reason="--set track is Rust only")
def test_set_track_in_an_mp4_changes_language_and_name_but_not_the_forced_flag(
        run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--set", "track", "--track", "2", "--lang", "German", "--name", "Deutsch", "-i",
              film_mp4)
    assert ran.code == 0, ran.err
    assert mp4_layout(film_mp4)[:3] == ["a:ita*", "a:ger", "a:fre"]
    before = film_mp4.read_bytes()
    refused = run("--set", "track", "--track", "1", "--forced-flag", "yes", "-i", film_mp4)
    assert "the forced flag of an MP4 can only be set on a subtitle track" in refused.err
    assert film_mp4.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--set name is Rust only")
def test_an_mp4_track_name_is_set_read_back_and_cleared(run: Callable[..., Ran],
                                                        film_mp4: Path) -> None:
    before = mp4_layout(film_mp4)
    ran = run("--set", "name", "--name", "Italiano", "--track", "1", "--kind", "audio",
              "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert mp4_titles(film_mp4)[1] == "Italiano"  # film_mp4 has no video name, so audio 1 is second
    assert mp4_layout(film_mp4) == before  # nothing else moved
    # the audio still decodes, and the file is still an MP4 with all its streams
    assert subprocess.run(["ffmpeg", "-v", "error", "-i", str(film_mp4), "-f", "null", "-"],
                          capture_output=True).returncode == 0
    assert stream_kinds(film_mp4).count("audio") == 3
    cleared = run("--set", "name", "--name", "", "--track", "1", "--kind", "audio", "-i",
                  film_mp4)
    assert cleared.code == 0, cleared.err
    assert mp4_titles(film_mp4)[1] == ""
    assert mp4_layout(film_mp4) == before


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_extract_a_text_subtitle_from_an_mp4_as_srt(run: Callable[..., Ran],
                                                    film_mp4: Path) -> None:
    before = film_mp4.read_bytes()
    ran = run("--extract", "subtitle", "--track", "2", "-i", film_mp4)
    out = film_mp4.with_name("film.ita.srt")
    assert ran.code == 0, ran.err
    assert out.stat().st_size > 0
    codec = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "stream=codec_name", "-of",
                            "csv=p=0", str(out)], capture_output=True, text=True, check=True).stdout
    assert codec.split()[0] == "subrip"
    assert film_mp4.read_bytes() == before
    assert leftovers(film_mp4.parent) == []


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_an_mp4_subtitle_is_extracted_as_ffmpeg_writes_it(run: Callable[..., Ran],
                                                          film_mp4: Path) -> None:
    """The bytes match ffmpeg's own SubRip output for the same track (a native build reads the
    track itself, the default build runs ffmpeg; both must give the same file)."""
    command = ["ffmpeg", "-v", "error", "-i", str(film_mp4), "-map", "0:s:1", "-f", "srt", "-"]
    expected = subprocess.run(command, capture_output=True, check=True).stdout
    out = film_mp4.parent / "ita.srt"
    ran = run("--extract", "subtitle", "--track", "2", "-i", film_mp4, "--output", str(out))
    assert ran.code == 0, ran.err
    assert out.read_bytes() == expected


@pytest.mark.skipif(not NATIVE_MP4,
                    reason="the native build reads the cues of an MP4 itself")
def test_srt_cues_survive_an_add_and_an_extract_in_this_program(run: Callable[..., Ran],
                                                                film_mp4: Path,
                                                                tmp_path: Path) -> None:
    """Add a file with gaps and two-line cues, extract it again: the cues come back as they went
    in. The file written ends each cue with a blank line (as ffmpeg's does), which the source may
    not have."""
    subs = tmp_path / "multi.srt"
    subs.write_text(MULTI_SRT, encoding="utf-8")
    assert run("--add", "subtitle", "--sub-file", subs, "--lang", "spa", "-i", film_mp4).code == 0
    out = tmp_path / "back.srt"
    ran = run("--extract", "subtitle", "--track", "3", "--kind", "sub", "-i", film_mp4,
              "--output", str(out))
    assert ran.code == 0, ran.err
    assert out.read_bytes() == (MULTI_SRT + "\n").encode("utf-8")


FOUR_CUES = ("1\n00:00:00,500 --> 00:00:02,000\nCiao\n\n"
             "2\n00:00:03,000 --> 00:00:04,250\nSeconda riga\nche continua\n\n")


@pytest.mark.skipif(not NATIVE_MP4, reason="the native build reads a WebVTT track of an MP4 itself")
def test_a_webvtt_track_of_an_mp4_is_extracted_as_srt_in_this_program(
        run: Callable[..., Ran], tmp_path: Path) -> None:
    """ffmpeg in the test environment can't read a WebVTT track, so the cues are checked against the
    ones the file was made from (and mediainfo must find the track)."""
    path = tmp_path / "webvtt.mp4"
    samples = [(500, empty_vtt()), (1500, vttc(b"Ciao")), (1000, empty_vtt()),
               (1250, vttc(b"Seconda riga\nche continua")), (2500, empty_vtt())]
    subtitle_mp4(path, wvtt_entry(), samples)
    assert [t for t in mp4_titles_and_formats(path) if t == "wvtt"] == ["wvtt"]
    out = tmp_path / "webvtt.srt"
    ran = run("--extract", "subtitle", "--track", "1", "--kind", "sub", "-i", path,
              "--output", str(out))
    assert ran.code == 0, ran.err
    assert out.read_text(encoding="utf-8") == FOUR_CUES


@pytest.mark.skipif(not NATIVE_MP4, reason="the native build reads a TTML track of an MP4 itself")
def test_a_ttml_track_of_an_mp4_is_extracted_as_srt_in_this_program(
        run: Callable[..., Ran], tmp_path: Path) -> None:
    path = tmp_path / "ttml.mp4"
    samples = [(500, ttml("")),
               (1500, ttml('<p begin="00:00:00.000" end="00:00:01.500">Ciao</p>')),
               (1000, ttml("")),
               (1250, ttml('<p begin="00:00:03.000" end="00:00:04.250">'
                           'Seconda riga<br/>che continua</p>'))]
    subtitle_mp4(path, stpp_entry(), samples)
    assert [t for t in mp4_titles_and_formats(path) if t == "stpp"] == ["stpp"]
    out = tmp_path / "ttml.srt"
    ran = run("--extract", "subtitle", "--track", "1", "--kind", "sub", "-i", path,
              "--output", str(out))
    assert ran.code == 0, ran.err
    assert out.read_text(encoding="utf-8") == FOUR_CUES


def mp4_titles_and_formats(path: Path) -> list[str]:
    """The format of each text track, as mediainfo names it."""
    report = subprocess.run(["mediainfo", "--Output=JSON", str(path)], capture_output=True,
                            text=True, check=True).stdout
    return [track["Format"] for track in json.loads(report)["media"]["track"]
            if track["@type"] == "Text"]


@pytest.mark.skipif(not NATIVE_MP4, reason="the native build reads ASS and WebVTT files itself")
@pytest.mark.parametrize("name, text", [
    ("s.ass", "[Script Info]\nScriptType: v4.00+\n\n[Events]\n"
              "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
              "Dialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,{\\an8}Ciao\\Nseconda riga\n"
              "Dialogue: 0,0:00:03.00,0:00:04.25,Default,,0,0,0,,"
              "{\\b1}Seconda{\\b0} riga, che continua\n"),
    ("s.vtt", "WEBVTT\n\nNOTE a comment\n\n00:00:00.500 --> 00:00:02.000 align:start\n<i>Ciao</i>\n"
              "seconda riga\n\n00:00:03.000 --> 00:00:04.250\nSeconda riga, che continua\n"),
])
def test_an_ass_or_webvtt_file_is_added_to_an_mp4_in_this_program(
        run: Callable[..., Ran], film_mp4: Path, tmp_path: Path, name: str, text: str) -> None:
    """The text and timing are those ffmpeg writes (its styling tags are left out of the comparison,
    since an MP4 subtitle here is plain text)."""
    subs = tmp_path / name
    subs.write_text(text, encoding="utf-8")
    reference = tmp_path / "reference.mp4"
    reference.write_bytes(film_mp4.read_bytes())
    made = tmp_path / "made.mp4"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(reference), "-i", str(subs),
                    "-map", "0",
                    "-map", "1:0", "-c", "copy", "-c:s", "mov_text", "-metadata:s:s:2",
                    "language=eng", "-movflags", "+faststart", str(made)], check=True)
    ran = run("--add", "subtitle", "--sub-file", subs, "--lang", "eng", "-i", film_mp4, "-v")
    assert ran.code == 0, ran.err
    assert "native-mp4 add-text-tracks" in ran.err
    assert cues_of(film_mp4, 2) == strip_tags(cues_of(made, 2))


def strip_tags(srt: str) -> str:
    return re.sub(r"<[^>]+>", "", srt)


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_extract_is_not_for_avi(run: Callable[..., Ran], three_avi: Path) -> None:
    ran = run("--extract", "subtitle", "--track", "1", "-i", three_avi)
    assert "only supported for MKV and MP4" in ran.err
    assert not list(three_avi.parent.glob("*.srt"))


def test_move_a_subtitle_track_first(run: Callable[..., Ran], film_mp4: Path) -> None:
    run("--set", "lang", "--default", "Italian", "--kind", "sub", "--move", "-i", film_mp4)
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:ita*", "s:eng"]


def test_move_with_a_tag_in_one_rewrite(run: Callable[..., Ran], film_mp4: Path) -> None:
    run("--set", "default", "--track", "3", "--lang", "German", "--move", "-i", film_mp4)
    assert mp4_layout(film_mp4) == ["a:ger*", "a:ita", "a:eng", "s:eng*", "s:ita"]


def test_move_when_already_first_is_unchanged(run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--set", "lang", "--default", "Italian", "--move", "-i", film_mp4)
    assert "is already the default and the first track" in ran.out


def test_delete_audio_by_language(run: Callable[..., Ran], film_mp4: Path) -> None:
    run("--delete", "lang", "--lang", "eng", "-i", film_mp4)
    assert mp4_layout(film_mp4) == ["a:ita*", "a:fre", "s:eng*", "s:ita"]


def decoded_audio(path: Path, index: int) -> bytes:
    """The PCM of the audio track at audio index ``index``, as ffmpeg decodes it."""
    return subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-map", f"0:a:{index}", "-f",
                           "s16le", "-"], capture_output=True, check=True).stdout


def timecode_first(path: Path) -> None:
    """Move the timecode track (handler ``tmcd``, which mediainfo does not list as audio, video or
    subtitles) to the front of the ``moov`` box. Only the ``trak`` boxes are reordered, which keeps
    the size and the chunk offsets."""
    data = bytearray(path.read_bytes())
    at = data.find(b"moov")
    end = at - 4 + int.from_bytes(data[at - 4:at], "big")
    traks = []
    pos = at + 4
    while pos < end:
        size = int.from_bytes(data[pos:pos + 4], "big")
        if data[pos + 4:pos + 8] == b"trak":
            traks.append((pos, pos + size))
        pos += size
    # hdlr: the tag, then version and flags (4), pre_defined (4), and the handler type
    is_timecode = [data[start:stop].find(b"hdlr") > 0 and
                   data[start:stop][data[start:stop].find(b"hdlr") + 12:][:4] == b"tmcd"
                   for start, stop in traks]
    order = [t for t, flag in zip(traks, is_timecode) if flag] + \
        [t for t, flag in zip(traks, is_timecode) if not flag]
    body = b"".join(bytes(data[start:stop]) for start, stop in order)
    data[traks[0][0]:traks[-1][1]] = body
    path.write_bytes(bytes(data))


@pytest.mark.skipif(not NATIVE_MP4,
                    reason="ffmpeg cannot write an MP4 whose first track is a timecode track (the "
                           "Rust build with native-mp4 removes the track in place)")
def test_a_track_mediainfo_does_not_list_can_come_before_the_audio(run: Callable[..., Ran],
                                                                    tmp_path: Path) -> None:
    """The trak numbers must match the tracks by their IDs, not by their place in the list that
    mediainfo gives."""
    path = tmp_path / "timecode.mp4"
    make = ["ffmpeg", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=3:size=160x120:rate=25",
            "-f", "lavfi", "-i", "sine=frequency=300:duration=3",
            "-f", "lavfi", "-i", "sine=frequency=500:duration=3",
            "-map", "0:v", "-map", "1:a", "-map", "2:a", "-c:v", "mpeg4", "-c:a", "aac",
            "-timecode", "00:00:01:00", "-metadata:s:a:0", "language=ita",
            "-metadata:s:a:1", "language=eng", str(path)]
    subprocess.run(make, check=True)
    timecode_first(path)
    before = decoded_audio(path, 0)
    ran = run("--delete", "lang", "--lang", "eng", "-i", path)
    assert ran.code == 0, ran.err
    assert mp4_layout(path) == ["a:ita*"]
    assert decoded_audio(path, 0) == before
    report = subprocess.run(["mediainfo", "--Output=JSON", str(path)], capture_output=True,
                            text=True, check=True).stdout
    others = [track for track in json.loads(report)["media"]["track"] if track["@type"] == "Other"]
    assert len(others) == 1, "the timecode track is kept"


def test_removed_mp4_audio_is_cut_out_and_the_rest_decodes_the_same(run: Callable[..., Ran],
                                                                   film_mp4: Path) -> None:
    languages = ["ita", "eng", "fre"]
    before = {language: decoded_audio(film_mp4, index) for index, language in enumerate(languages)}
    size = film_mp4.stat().st_size
    ran = run("--delete", "lang", "--lang", "eng", "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert mp4_layout(film_mp4) == ["a:ita*", "a:fre", "s:eng*", "s:ita"]
    assert film_mp4.stat().st_size < size, "the removed track's media data is gone from the file"
    assert decoded_audio(film_mp4, 0) == before["ita"]
    assert decoded_audio(film_mp4, 1) == before["fre"]
    assert stream_kinds(film_mp4).count("audio") == 2
    assert leftovers(film_mp4.parent) == []


def test_delete_the_last_subtitle_track(run: Callable[..., Ran], film_mp4: Path) -> None:
    run("--delete", "track", "--track", "last", "--kind", "sub", "-i", film_mp4)
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:eng*"]


def test_preserve_one_language(run: Callable[..., Ran], film_mp4: Path) -> None:
    run("--preserve", "lang", "--lang", "ita", "-i", film_mp4)
    assert mp4_layout(film_mp4) == ["a:ita*", "s:eng*", "s:ita"]


def test_the_last_audio_track_is_protected(run: Callable[..., Ran],
                                           make: Callable[..., Path]) -> None:
    path = make("solo.mp4", [("eng", None, True)])
    before = path.read_bytes()
    ran = run("--delete", "track", "--track", "1", "-i", path)
    assert "would remove every audio track" in ran.err and path.read_bytes() == before


def test_dry_run_changes_nothing_and_shows_the_command(run: Callable[..., Ran],
                                                       film_mp4: Path) -> None:
    before = film_mp4.read_bytes()
    ran = run("--set", "lang", "--default", "English", "--move", "-i", film_mp4, "-T", "-v")
    assert ran.out.startswith("[dry-run]")
    if NATIVE_MP4:
        assert "native-mp4 edit-in-place" in ran.err
    else:
        assert "-map 0:a:1 -map 0:a:0 -map 0:a:2" in ran.err
    assert film_mp4.read_bytes() == before and leftovers(film_mp4.parent) == []


def test_json_lists_the_ffmpeg_command(run: Callable[..., Ran], film_mp4: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "-i", film_mp4, "-T", "-F", "json")
    (result,) = json.loads(ran.out)["results"]
    engine = "native-mp4" if NATIVE_MP4 else "ffmpeg"
    assert result["commands"][0][0] == engine and result["status"] == "changed"


# AVI -------------------------------------------------------------------------

def test_avi_info_and_get(run: Callable[..., Ran], three_avi: Path) -> None:
    ran = run("--info", "-i", three_avi)
    assert ran.out.splitlines()[0] == f"{three_avi} (AVI)"
    assert sum(1 for line in ran.out.splitlines() if line.startswith("audio:")) == 3
    assert run("--get", "lang", "-i", three_avi).out == "und\n"  # no flags: the first track


@pytest.mark.parametrize("args", [
    ["--set", "lang", "--default", "English"],
    ["--set", "lang", "--default", "English", "--move"],
    ["--set", "default", "--track", "2"],
])
def test_avi_set_is_not_supported(run: Callable[..., Ran], three_avi: Path,
                                  args: list[str]) -> None:
    before = three_avi.read_bytes()
    ran = run(*args, "-i", three_avi)
    assert ran.code == 0 and "set is only supported for MKV and MP4 files" in ran.err
    assert three_avi.read_bytes() == before


@pytest.mark.parametrize("args, message", [
    (["--delete", "lang", "--lang", "eng"], "deleting by language is only supported"),
    (["--preserve", "lang", "--lang", "eng"], "preserving by language is only supported"),
])
def test_avi_by_language_is_not_supported(run: Callable[..., Ran], three_avi: Path,
                                          args: list[str], message: str) -> None:
    assert message in run(*args, "-i", three_avi).err


def test_avi_swap_rewrites_the_file(run: Callable[..., Ran], three_avi: Path) -> None:
    three_avi.chmod(0o640)
    os.utime(three_avi, (1_600_000_000, 1_600_000_000))
    before = three_avi.read_bytes()
    ran = run("--swap", "lang", "-i", three_avi)
    assert ran.code == 0, ran.err
    assert ran.out.strip() == f"{three_avi}: audio tracks 1 and 2 swapped"
    assert three_avi.read_bytes() != before and audio_streams(three_avi) == 3
    info = three_avi.stat()
    assert stat.S_IMODE(info.st_mode) == 0o640 and int(info.st_mtime) == 1_600_000_000
    assert leftovers(three_avi.parent) == []


def test_avi_delete_a_track(run: Callable[..., Ran], three_avi: Path) -> None:
    run("--delete", "track", "--track", "last", "-i", three_avi)
    assert audio_streams(three_avi) == 2
    run("--delete", "track", "--track", "1", "-i", three_avi)
    assert audio_streams(three_avi) == 1
    assert leftovers(three_avi.parent) == []


def decoded_video(path: Path) -> bytes:
    """The video of an AVI as raw pixels, as ffmpeg decodes it."""
    command = ["ffmpeg", "-v", "error", "-i", str(path), "-map", "0:v", "-f", "rawvideo",
               "-pix_fmt", "rgb24", "-"]
    return subprocess.run(command, capture_output=True, check=True).stdout


def test_a_removed_avi_stream_leaves_the_rest_decoding_the_same(run: Callable[..., Ran],
                                                                three_avi: Path) -> None:
    before_video = decoded_video(three_avi)
    before = [decoded_audio(three_avi, index) for index in range(3)]
    size = three_avi.stat().st_size
    ran = run("--delete", "track", "--track", "2", "-i", three_avi)
    assert ran.code == 0, ran.err
    assert audio_streams(three_avi) == 2
    assert three_avi.stat().st_size < size
    assert decoded_video(three_avi) == before_video
    assert decoded_audio(three_avi, 0) == before[0]
    assert decoded_audio(three_avi, 1) == before[2]
    assert leftovers(three_avi.parent) == []


def wrap_in_rec_lists(path: Path, per: int) -> None:
    """Rewrite an AVI so that each run of ``per`` chunks in its ``movi`` list is in a LIST ``rec ``
    (as some muxers write), and rebuild the index to match. The streams and their data are
    unchanged."""
    data = path.read_bytes()

    def chunks(start: int, end: int) -> list[tuple[bytes, int, int]]:
        out, at = [], start
        while at + 8 <= end:
            size = struct.unpack("<I", data[at + 4:at + 8])[0]
            out.append((data[at:at + 4], at, size))
            at += 8 + size + (size & 1)
        return out

    def padded(body: bytes) -> bytes:
        return body + (b"\0" if len(body) % 2 else b"")

    top = chunks(12, len(data))
    movi = next(c for c in top if c[0] == b"LIST" and data[c[1] + 8:c[1] + 12] == b"movi")
    index = next(c for c in top if c[0] == b"idx1")
    kids = chunks(movi[1] + 12, movi[1] + 8 + movi[2])
    old_index = data[index[1] + 8:index[1] + 8 + index[2]]
    new_at: dict[int, int] = {}
    body = bytearray(b"movi")
    for start in range(0, len(kids), per):
        rec = bytearray(b"rec ")
        for cid, at, size in kids[start:start + per]:
            # the chunk's header, counted from the movi fourcc as the index counts
            new_at[at - (movi[1] + 8)] = len(body) + 8 + len(rec)
            rec += cid + struct.pack("<I", size) + padded(data[at + 8:at + 8 + size])
        body += b"LIST" + struct.pack("<I", len(rec)) + padded(bytes(rec))
    new_index = bytearray()
    for i in range(0, len(old_index), 16):
        entry = bytearray(old_index[i:i + 16])
        entry[8:12] = struct.pack("<I", new_at[struct.unpack("<I", entry[8:12])[0]])
        new_index += entry
    out = bytearray(data[:12]) + data[12:movi[1]]
    out += b"LIST" + struct.pack("<I", len(body)) + bytes(body)
    out += b"idx1" + struct.pack("<I", len(new_index)) + bytes(new_index)
    for cid, at, size in top:
        if cid not in (b"idx1",) and not (cid == b"LIST" and at == movi[1]):
            out += data[at:at + 8 + size + (size & 1)]
    out[4:8] = struct.pack("<I", len(out) - 8)
    path.write_bytes(bytes(out))


@pytest.mark.skipif(not NATIVE_AVI,
                    reason="the native AVI build reads the streams from their lists (mediainfo "
                           "lists ghost tracks for these files)")
def test_an_avi_whose_chunks_are_in_rec_lists_is_listed_and_changed_correctly(
        run: Callable[..., Ran], three_avi: Path) -> None:
    before_video = decoded_video(three_avi)
    before = [decoded_audio(three_avi, index) for index in range(3)]
    wrap_in_rec_lists(three_avi, per=3)
    info = run("--info", "--format", "json", "-i", three_avi).out
    listed = json.loads(info)["results"][0]["data"]
    assert [t["type"] for t in listed["tracks"]] == ["video", "audio", "audio", "audio"]
    ran = run("--delete", "track", "--track", "2", "-i", three_avi)
    assert ran.code == 0, ran.err
    assert audio_streams(three_avi) == 2
    assert decoded_video(three_avi) == before_video
    assert decoded_audio(three_avi, 0) == before[0]
    assert decoded_audio(three_avi, 1) == before[2]


def test_avi_last_audio_track_is_protected(run: Callable[..., Ran], three_avi: Path) -> None:
    run("--delete", "track", "--track", "1", "-i", three_avi)
    run("--delete", "track", "--track", "1", "-i", three_avi)
    before = three_avi.read_bytes()
    ran = run("--delete", "track", "--track", "1", "-i", three_avi)
    assert "would remove every audio track" in ran.err and three_avi.read_bytes() == before
    assert run("--delete", "track", "--track", "1", "--force", "-i", three_avi).code == 0
    assert audio_streams(three_avi) == 0


def test_avi_dry_run(run: Callable[..., Ran], three_avi: Path) -> None:
    before = three_avi.read_bytes()
    ran = run("--swap", "lang", "-i", three_avi, "-T", "-v")
    assert ran.out.startswith("[dry-run]")
    assert ("native-avi reorder-streams" if NATIVE_AVI else "-map 0:a:1 -map 0:a:0") in ran.err
    assert three_avi.read_bytes() == before and leftovers(three_avi.parent) == []


def test_avi_swap_needs_two_audio_tracks(run: Callable[..., Ran],
                                         make: Callable[..., Path]) -> None:
    path = make("one.avi", [("und", None, False)])
    assert "needs at least two audio tracks" in run("--swap", "lang", "-i", path).err


# All three containers together ----------------------------------------------

def test_a_directory_with_every_kind_of_file(run: Callable[..., Ran], tmp_path: Path,
                                             make: Callable[..., Path]) -> None:
    folder = tmp_path / "mixed"
    mkv = make("a.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    mp4 = make("b.mp4", [("ita", None, True), ("eng", None, False)], where=folder)
    avi = make("c.avi", [("und", None, False), ("und", None, False)], where=folder)
    (folder / "d.txt").write_text("not media")
    ran = run("--set", "lang", "--default", "English", "--move", "-d", folder)
    assert ran.code == 0, ran.err
    assert mkv_layout(mkv) == ["a:eng*", "a:ita"]
    assert mp4_layout(mp4) == ["a:eng*", "a:ita"]
    assert audio_streams(avi) == 2  # untouched: AVI can't be set
    assert f"{avi}: set is only supported" in ran.err
    assert leftovers(folder) == []


# Track names ---------------------------------------------------------------------

NAMED_AUDIO = [("ita", "Italiano", True), ("eng", "English 5.1", False), ("fre", "Français", False)]


@pytest.fixture
def named_mp4(make: Callable[..., Path]) -> Path:
    """An MP4 whose video and audio tracks have names (ffmpeg loses these when it rewrites)."""
    return make("named.mp4", NAMED_AUDIO, video_title="Video Title")


def test_the_named_fixture_has_its_names(named_mp4: Path) -> None:
    assert mp4_titles(named_mp4) == ["Video Title", "Italiano", "English 5.1", "Français"]


def handler_names(path: Path) -> list[str]:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "stream_tags=handler_name", "-of", "csv=p=0",
         str(path)], capture_output=True, text=True, check=True).stdout
    return out.split("\n")[:-1]


@pytest.mark.parametrize("args, titles", [
    (["--set", "lang", "--default", "English"],
     ["Video Title", "Italiano", "English 5.1", "Français"]),
    (["--set", "lang", "--default", "French", "--move"],
     ["Video Title", "Français", "Italiano", "English 5.1"]),
    (["--set", "default", "--track", "3", "--lang", "German", "--move"],
     ["Video Title", "Français", "Italiano", "English 5.1"]),
    (["--swap", "lang"], ["Video Title", "Italiano", "English 5.1", "Français"]),
    (["--delete", "lang", "--lang", "ita"], ["Video Title", "English 5.1", "Français"]),
    (["--delete", "track", "--track", "last"], ["Video Title", "Italiano", "English 5.1"]),
    (["--preserve", "lang", "--lang", "eng"], ["Video Title", "English 5.1"]),
])
def test_names_survive_every_kind_of_rewrite(run: Callable[..., Ran], named_mp4: Path,
                                             args: list[str], titles: list[str]) -> None:
    ran = run(*args, "-i", named_mp4)
    assert ran.code == 0, ran.err
    assert mp4_titles(named_mp4) == titles


def test_handler_names_are_left_alone(run: Callable[..., Ran], make: Callable[..., Path]) -> None:
    path = make("handlers.mp4", NAMED_AUDIO)
    before = handler_names(path)
    run("--set", "lang", "--default", "French", "--move", "-i", path)
    assert sorted(handler_names(path)) == sorted(before)
    assert len(before) == 4


def test_set_and_clear_a_name(run: Callable[..., Ran], named_mp4: Path) -> None:
    ran = run("--set", "name", "--track", "2", "--name", "Commentary", "-i", named_mp4)
    expected = f'{named_mp4}: audio track 2 (eng): name "English 5.1" -> "Commentary"'
    assert ran.out.strip() == expected
    assert mp4_titles(named_mp4) == ["Video Title", "Italiano", "Commentary", "Français"]
    run("--set", "name", "--track", "2", "--name", "", "-i", named_mp4)
    assert mp4_titles(named_mp4) == ["Video Title", "Italiano", "", "Français"]


@pytest.mark.skipif(not MAU_BIN,
                    reason="the forced flag of an MP4 is set in place by the Rust build only")
def test_the_forced_flag_is_set_and_cleared_on_an_mp4_subtitle(run: Callable[..., Ran],
                                                               film_mp4: Path) -> None:
    ran = run("--set", "forced", "--track", "1", "--kind", "sub", "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert ran.out.strip() == f"{film_mp4}: subtitle track 1 (eng): forced no -> yes"
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:eng*F", "s:ita"]
    flags = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "s", "-show_entries",
                            "stream_disposition=forced", "-of", "csv=p=0", str(film_mp4)],
                           capture_output=True, text=True, check=True).stdout.split()
    assert flags == ["1", "0"]  # ffprobe agrees with mediainfo
    before = film_mp4.read_bytes()
    audio = run("--set", "forced", "--track", "1", "-i", film_mp4)  # track 1 of the audio
    assert "the forced flag of an MP4 can only be set on a subtitle track" in audio.err
    assert film_mp4.read_bytes() == before
    cleared = run("--set", "unforced", "--track", "1", "--kind", "sub", "-i", film_mp4)
    assert cleared.code == 0, cleared.err
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:eng*", "s:ita"]


# Removing the default track ------------------------------------------------------

def test_removing_the_default_audio_makes_the_next_one_default(run: Callable[..., Ran],
                                                               film_mp4: Path) -> None:
    ran = run("--delete", "lang", "--lang", "ita", "-i", film_mp4)
    assert ran.out.splitlines() == [f"{film_mp4}: removed audio track 1 (ita)",
                                    f"{film_mp4}: audio track 2 (eng): default no -> yes"]
    assert mp4_layout(film_mp4) == ["a:eng*", "a:fre", "s:eng*", "s:ita"]


def test_removing_the_default_subtitle_makes_the_next_one_default(run: Callable[..., Ran],
                                                                  film_mp4: Path) -> None:
    ran = run("--delete", "lang", "--lang", "eng", "--kind", "sub", "-i", film_mp4)
    assert ran.out.splitlines()[-1] == f"{film_mp4}: subtitle track 2 (ita): default no -> yes"
    assert mp4_layout(film_mp4) == ["a:ita*", "a:eng", "a:fre", "s:ita*"]


def test_preserve_leaves_the_kept_track_default(run: Callable[..., Ran], film_mp4: Path) -> None:
    run("--preserve", "lang", "--lang", "fre", "-i", film_mp4)
    assert mp4_layout(film_mp4)[0] == "a:fre*"


# Keeping the original and undoing ------------------------------------------------

def test_backup_of_an_mp4_rewrite(run: Callable[..., Ran], film_mp4: Path) -> None:
    original = film_mp4.read_bytes()
    run("--set", "lang", "--default", "English", "-i", film_mp4, "--backup")
    backup = Path(str(film_mp4) + fml.BACKUP_MARK)
    assert backup.read_bytes() == original and mp4_layout(backup) == FILM
    assert mp4_layout(film_mp4)[1] == "a:eng*"
    assert leftovers(film_mp4.parent) == []


def test_undo_stacked_mp4_changes(run: Callable[..., Ran], named_mp4: Path,
                                  tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    layout, titles = mp4_layout(named_mp4), mp4_titles(named_mp4)
    for args in (["--set", "lang", "--default", "French", "--move"],
                 ["--set", "name", "--track", "2", "--name", "Renamed"],
                 ["--set", "lang", "--lang", "German", "--track", "3"]):
        assert run(*args, "-i", named_mp4, "--log", log).code == 0
    assert mp4_layout(named_mp4) != layout
    ran = run("--undo", log)
    assert ran.code == 0, ran.err
    assert mp4_layout(named_mp4) == layout and mp4_titles(named_mp4) == titles


def test_undo_restores_an_mp4_removal_from_its_backup(run: Callable[..., Ran], film_mp4: Path,
                                                      tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    original = film_mp4.read_bytes()
    run("--delete", "lang", "--lang", "eng", "-i", film_mp4, "--backup", "--log", log)
    run("--undo", log)
    assert film_mp4.read_bytes() == original


def test_undo_a_removal_without_a_backup_is_refused(run: Callable[..., Ran], film_mp4: Path,
                                                    tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--delete", "lang", "--lang", "eng", "-i", film_mp4, "--log", log)
    assert "can't be undone without the backup" in run("--undo", log).err


# Several files at once and filters, across containers ---------------------------------

def test_jobs_across_every_kind_of_file(run: Callable[..., Ran], tmp_path: Path,
                                        make: Callable[..., Path]) -> None:
    folder = tmp_path / "mixed"
    mkv = make("a.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    mp4 = make("b.mp4", [("ita", None, True), ("eng", None, False)], where=folder)
    avi = make("c.avi", [("und", None, False), ("und", None, False)], where=folder)
    ran = run("--set", "lang", "--default", "English", "--move", "-d", folder, "-j", "3")
    assert ran.code == 0, ran.err
    assert mkv_layout(mkv) == ["a:eng*", "a:ita"] and mp4_layout(mp4) == ["a:eng*", "a:ita"]
    assert audio_streams(avi) == 2 and f"{avi}: set is only supported" in ran.err


def test_include_picks_a_container(run: Callable[..., Ran], tmp_path: Path,
                                   make: Callable[..., Path]) -> None:
    folder = tmp_path / "mixed"
    mkv = make("a.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    mp4 = make("b.mp4", [("ita", None, True), ("eng", None, False)], where=folder)
    run("--set", "lang", "--default", "English", "-d", folder, "--include", "*.mp4")
    assert mkv_layout(mkv)[0] == "a:ita*" and mp4_layout(mp4)[1] == "a:eng*"


# AVI -------------------------------------------------------------------------------

def test_avi_backup_and_undo_of_a_swap(run: Callable[..., Ran], three_avi: Path,
                                       tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    original = three_avi.read_bytes()
    run("--swap", "lang", "-i", three_avi, "--backup", "--log", log)
    assert three_avi.read_bytes() != original
    assert Path(str(three_avi) + fml.BACKUP_MARK).read_bytes() == original
    run("--undo", log)
    assert three_avi.read_bytes() == original


def test_avi_swap_undo_needs_a_backup(run: Callable[..., Ran], three_avi: Path,
                                      tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--swap", "lang", "-i", three_avi, "--log", log)
    swapped = three_avi.read_bytes()
    ran = run("--undo", log)
    assert "can't be undone without the backup" in ran.err and three_avi.read_bytes() == swapped


def avi_track_names(run: Callable[..., Ran], path: Path) -> list[str]:
    """The names of the audio tracks of an AVI, in file order, as --info shows them."""
    lines = run("--info", "-i", path).out.splitlines()
    return [line.split()[-1] for line in lines if line.startswith("audio:")]


@pytest.mark.skipif(not MAU_BIN, reason="reordering an AVI with --set order is Rust only")
def test_avi_two_audio_tracks_swap_with_set_order(
    run: Callable[..., Ran], make: Callable[..., Path], tmp_path: Path
) -> None:
    two = make("two.avi", [("und", "first", False), ("und", "second", False)])
    assert avi_track_names(run, two) == ["first", "second"]
    ran = run("--set", "order", "--order", "2,1", "-i", two)
    assert ran.code == 0, ran.err
    assert ran.out.strip() == f"{two}: audio tracks 1 and 2 swapped"
    assert avi_track_names(run, two) == ["second", "first"]
    assert audio_streams(two) == 2
    assert leftovers(tmp_path) == []


@pytest.mark.skipif(not MAU_BIN, reason="reordering an AVI with --set order is Rust only")
def test_avi_set_order_is_only_two_tracks(run: Callable[..., Ran], three_avi: Path) -> None:
    before = three_avi.read_bytes()
    ran = run("--set", "order", "--order", "2,1,3", "-i", three_avi)
    assert ran.code == 0
    assert "AVI files can only swap their two audio tracks (--order 2,1)" in ran.out + ran.err
    assert three_avi.read_bytes() == before
    assert leftovers(three_avi.parent) == []


@pytest.mark.skipif(not MAU_BIN, reason="reordering an AVI with --set order is Rust only")
def test_avi_set_order_dry_run_changes_nothing(
    run: Callable[..., Ran], make: Callable[..., Path]
) -> None:
    two = make("dry.avi", [("und", "first", False), ("und", "second", False)])
    before = two.read_bytes()
    ran = run("--set", "order", "--order", "2,1", "--dryrun", "-i", two)
    assert ran.code == 0, ran.err
    assert two.read_bytes() == before
