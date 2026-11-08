"""MKV bitmap and WebVTT subtitles extracted to their files: PGS (.sup), VobSub (.idx and .sub)
and WebVTT (.vtt). The references are mkvextract's output (PGS, VobSub, and WebVTT with the
S_TEXT codec), and ffmpeg's SRT of the same track for the WebVTT with the D_WEBVTT codec."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Callable

import pytest
from conftest import MAU_BIN, NATIVE_MKV, Ran, needs_mkvtoolnix, requires_tools
from sup_gen import cleared_set, display_set, write_sup

pytestmark = [needs_mkvtoolnix, requires_tools]


def mux(source: Path, dest: Path) -> Path:
    """Mux a subtitle file into an MKV with mkvmerge."""
    subprocess.run(["mkvmerge", "-o", str(dest), str(source)], capture_output=True, check=False)
    assert dest.exists()
    return dest


def mkvextract_track(mkv: Path, out: Path) -> Path:
    subprocess.run(["mkvextract", "tracks", str(mkv), f"0:{out}"], capture_output=True, check=True)
    return out


def extract(run: Callable[..., Ran], mkv: Path, out: Path) -> Ran:
    """Extract the first subtitle track of ``mkv`` to ``out``."""
    return run("--extract", "subtitle", "--track", "1", "--kind", "sub", "-i", str(mkv),
               "--output", str(out))


def pgs_mkv(tmp_path: Path, sets: list[list[bytes]]) -> Path:
    sup = tmp_path / "subs.sup"
    write_sup(sup, sets)
    return mux(sup, tmp_path / "pgs.mkv")


@pytest.mark.skipif(not NATIVE_MKV, reason="the native build reads the blocks of PGS itself")
def test_a_pgs_track_is_extracted_as_mkvextract_writes_it(run: Callable[..., Ran],
                                                          tmp_path: Path) -> None:
    mkv = pgs_mkv(tmp_path, [display_set(90000, (100, 800, 300, 80), 1), cleared_set(180000),
                             display_set(270000, (500, 700, 200, 60), 2), cleared_set(360000)])
    reference = mkvextract_track(mkv, tmp_path / "reference.sup")
    out = tmp_path / "ours.sup"
    ran = extract(run, mkv, out)
    assert ran.code == 0, ran.err
    assert out.read_bytes() == reference.read_bytes()


@pytest.mark.skipif(not NATIVE_MKV, reason="the native build reads the blocks of VobSub itself")
@pytest.mark.parametrize("width, height, pattern", [(60, 20, False), (400, 120, True)],
                         ids=["one sector", "several sectors"])
def test_a_vobsub_track_is_extracted_as_mkvextract_writes_it(
        run: Callable[..., Ran], tmp_path: Path, width: int, height: int, pattern: bool) -> None:
    """The PGS is converted to VobSub by ffmpeg (which can write it), then both are compared: one
    subpicture fits a sector, and a big one is split over several."""
    pgs = pgs_mkv(tmp_path, [display_set(90000, (10, 10, width, height), 1, pattern),
                             cleared_set(180000),
                             display_set(270000, (20, 20, width, height), 2, pattern),
                             cleared_set(360000)])
    vob = tmp_path / "vob.mkv"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(pgs), "-map", "0:s:0", "-c:s",
                    "dvdsub",
                    str(vob)], check=True)
    reference_idx = tmp_path / "reference.idx"
    subprocess.run(["mkvextract", "tracks", str(vob), f"0:{reference_idx}"], capture_output=True,
                   check=True)
    out = tmp_path / "ours.idx"
    ran = extract(run, vob, out)
    assert ran.code == 0, ran.err
    assert out.read_bytes() == reference_idx.read_bytes()
    assert out.with_suffix(".sub").read_bytes() == reference_idx.with_suffix(".sub").read_bytes()


@pytest.mark.skipif(not NATIVE_MKV, reason="the native build reads WebVTT cues itself")
def test_an_s_text_webvtt_track_is_extracted_as_mkvextract_writes_it(
        run: Callable[..., Ran], tmp_path: Path) -> None:
    """Cues with and without an identifier and with and without settings (the settings and the
    identifier are kept in the block's additions, and mkvextract puts them back together)."""
    source = tmp_path / "cues.vtt"
    source.write_text("WEBVTT\n\nNOTE a comment\n\na\n00:00:01.000 --> 00:00:02.000\nid only\n\n"
                      "00:00:03.000 --> 00:00:04.000 line:90%\nsettings only\n\n"
                      "b\n00:00:05.000 --> 00:00:06.000 align:left\nboth\n\n"
                      "00:00:07.000 --> 00:00:08.000\nnone\n", encoding="utf-8")
    mkv = mux(source, tmp_path / "cues.mkv")
    reference = mkvextract_track(mkv, tmp_path / "reference.vtt")
    out = tmp_path / "ours.vtt"
    ran = extract(run, mkv, out)
    assert ran.code == 0, ran.err
    assert out.read_bytes() == reference.read_bytes()


def newest_subtitle_id(mkv: Path) -> int:
    """The ID of the subtitle track with the highest ID (the one a test added last)."""
    report = subprocess.run(["mkvmerge", "-J", str(mkv)], capture_output=True, text=True,
                            check=True).stdout
    return max(t["id"] for t in json.loads(report)["tracks"] if t["type"] == "subtitles")


def mkv_track_bytes(mkv: Path, track: int, out: Path) -> bytes:
    """A track of an MKV, extracted by mkvextract (the reference) for comparison."""
    subprocess.run(["mkvextract", "tracks", str(mkv), f"{track}:{out}"], capture_output=True,
                   check=True)
    return out.read_bytes()


HARD_ASS = (
    "\ufeff[Script Info]\r\nTitle: test\r\nScriptType: v4.00+\r\nPlayResX: 384\r\n\r\n"
    "[V4+ Styles]\r\nFormat: Name, Fontname, Fontsize, PrimaryColour\r\n"
    "Style: Default,Arial,20,&H00FFFFFF\r\nStyle: Top,Arial,24,&H0000FFFF\r\n\r\n"
    "[Events]\r\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, "
    "Text\r\n"
    "Dialogue: 0,0:00:00.00,0:00:01.50,Default,Anna,10,10,5,,"
    "Hello, world {\\i1}italic{\\i0}\\N2nd line\r\n"
    "Dialogue: 1,0:00:02.00,0:00:03.00,Top,,0,0,0,karaoke,{\\k20}Ka{\\k30}ra\r\n"
    "Dialogue: 0,0:00:04.00,0:00:04.00,Default,,0,0,0,,\r\n"
    "Dialogue: 0,0:00:05.00,0:00:06.25,Default,,0,0,0,,Ends, here.\r\n"
)


HARD_VTT = (
    "﻿WEBVTT\nKind: captions\nLanguage: en\n\nNOTE a comment\nspanning two lines\n\n"
    "intro\n00:00:00.500 --> 00:00:02.000 align:start position:10%\n"
    "<i>Ciao</i> &amp; hi\n<c.yellow>tagged</c>\n\n"
    "00:00:03.000 --> 00:00:04.250\nSeconda riga\nche continua\n\n"
    "end\n00:00:05.000 --> 00:00:07.500 line:90%\nFine.\n"
)

# a NOTE after the first cue: the native add gives up on it and mkvmerge adds the file
LATE_NOTE_VTT = (
    "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nfirst\n\nNOTE late note\n\n"
    "00:00:03.000 --> 00:00:04.000\nsecond\n"
)

ADDED_SUBTITLES = {
    "hard.ass": HARD_ASS,
    "plain.srt": "1\n00:00:07,000 --> 00:00:08,000\nSRT after the ASS\n",
    "hard.vtt": HARD_VTT,
    "late_note.vtt": LATE_NOTE_VTT,
}


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
@pytest.mark.parametrize("subs", list(ADDED_SUBTITLES))
def test_an_ass_srt_or_webvtt_file_is_added_to_an_mkv_as_mkvmerge_adds_it(
        run: Callable[..., Ran], film_mkv: Path, tmp_path: Path, subs: str) -> None:
    """The track as mkvextract reads it back is the same as mkvmerge's: the CRLF, the byte order
    mark, the commas and tags in the text, the WebVTT header and identifiers, and the empty event
    all survive."""
    source = tmp_path / subs
    source.write_bytes(ADDED_SUBTITLES[subs].encode("utf-8"))
    reference = tmp_path / "reference.mkv"
    subprocess.run(["mkvmerge", "-o", str(reference), str(film_mkv), str(source)],
                   capture_output=True, check=True)
    ours = tmp_path / "ours.mkv"
    ours.write_bytes(film_mkv.read_bytes())
    ran = run("--add", "subtitle", "--sub-file", source, "--lang", "eng", "-i", ours, "-v")
    assert ran.code == 0, ran.err
    if NATIVE_MKV and subs != "late_note.vtt":
        assert "native-mkv add-subtitles" in ran.err
    elif NATIVE_MKV:
        assert "Executing:\tmkvmerge" in ran.err, "a NOTE after a cue is left to mkvmerge"
    # the new track is the subtitle track with the highest ID in each file (the fixture has its own)
    expected = mkv_track_bytes(reference, newest_subtitle_id(reference), tmp_path / "reference.out")
    got = mkv_track_bytes(ours, newest_subtitle_id(ours), tmp_path / "ours.out")
    assert got == expected
    assert not [p for p in tmp_path.iterdir() if ".fml-" in p.name], "no temporary file is left"


def ssa_of(ass: str) -> str:
    """The ASS text of HARD_ASS as an SSA file: `[V4 Styles]`, the `Marked` column and the
    `Marked=0` values."""
    text = ass.replace("[V4+ Styles]", "[V4 Styles]")
    text = text.replace("ScriptType: v4.00+", "ScriptType: v4.00")
    text = text.replace("Layer, Start", "Marked, Start")
    return re.sub(r"Dialogue: (\d+),", r"Dialogue: Marked=\1,", text)


def track_codec(mkv: Path, track: int) -> str:
    report = subprocess.run(["mkvmerge", "-J", str(mkv)], capture_output=True, text=True,
                            check=True).stdout
    tracks = json.loads(report)["tracks"]
    return next(t["properties"]["codec_id"] for t in tracks if t["id"] == track)


def decoded_srt(mkv: Path, out: Path) -> bytes:
    """The last subtitle track of an MKV (the one a test added), decoded by ffmpeg as SubRip."""
    report = subprocess.run(["mkvmerge", "-J", str(mkv)], capture_output=True, text=True,
                            check=True).stdout
    count = sum(1 for t in json.loads(report)["tracks"] if t["type"] == "subtitles")
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(mkv), "-map", f"0:s:{count - 1}",
                    "-f", "srt", str(out)], check=True)
    return out.read_bytes()


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_an_ssa_file_is_added_as_an_ssa_track_whose_text_reads_as_mkvmerges_ass_track(
        run: Callable[..., Ran], film_mkv: Path, tmp_path: Path) -> None:
    """mkvmerge makes an SSA file an ASS track, so the reference is its track, compared by
    ffmpeg's SRT of each. The native add keeps the SSA codec, and mkvextract reads its text back as
    the file's."""
    source = tmp_path / "hard.ssa"
    source.write_bytes(ssa_of(HARD_ASS).encode("utf-8"))
    reference = tmp_path / "reference.mkv"
    subprocess.run(["mkvmerge", "-o", str(reference), str(film_mkv), str(source)],
                   capture_output=True, check=True)
    ours = tmp_path / "ours.mkv"
    ours.write_bytes(film_mkv.read_bytes())
    ran = run("--add", "subtitle", "--sub-file", source, "--lang", "eng", "-i", ours, "-v")
    assert ran.code == 0, ran.err
    assert decoded_srt(ours, tmp_path / "ours.srt") == decoded_srt(reference, tmp_path / "ref.srt")
    if NATIVE_MKV:
        assert "native-mkv add-subtitles" in ran.err
        assert track_codec(ours, newest_subtitle_id(ours)) == "S_TEXT/SSA"
        text = mkv_track_bytes(ours, newest_subtitle_id(ours), tmp_path / "ours.out")
        assert text.decode("utf-8").replace("\r\n", "\n") == ssa_of(HARD_ASS).replace("\r\n", "\n")
    assert not [p for p in tmp_path.iterdir() if ".fml-" in p.name], "no temporary file is left"


@pytest.mark.skipif(not NATIVE_MKV, reason="the native build reads WebVTT cues itself")
def test_a_d_webvtt_track_is_extracted_as_webvtt_that_reads_back_as_ffmpegs_srt(
        run: Callable[..., Ran], tmp_path: Path) -> None:
    """ffmpeg writes WebVTT into an MKV as D_WEBVTT/SUBTITLES, which mkvextract can't read. The
    reference is ffmpeg's SRT of the track, and the extracted WebVTT must read back as the same
    SRT."""
    source = tmp_path / "cues.vtt"
    source.write_text("WEBVTT\n\nintro\n00:00:00.500 --> 00:00:02.000 align:start\n"
                      "<i>Ciao</i> &amp; hi\n\n"
                      "00:00:03.000 --> 00:00:04.250\nSeconda riga\nche continua\n",
                      encoding="utf-8")
    mkv = tmp_path / "dweb.mkv"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(source), "-c:s", "copy", str(mkv)],
                   check=True)
    reference = subprocess.run(["ffmpeg", "-v", "error", "-i", str(mkv), "-map", "0:s:0", "-f",
                                "srt", "-"],
                               capture_output=True, check=True).stdout
    out = tmp_path / "ours.vtt"
    ran = extract(run, mkv, out)
    assert ran.code == 0, ran.err
    assert out.read_text(encoding="utf-8").startswith("WEBVTT\n\n")
    back = subprocess.run(["ffmpeg", "-v", "error", "-i", str(out), "-f", "srt", "-"],
                          capture_output=True, check=True).stdout
    assert back == reference
