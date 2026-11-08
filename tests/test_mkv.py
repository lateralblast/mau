"""Tests on real MKV files made with ffmpeg and inspected with mkvmerge."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any, Callable

import pytest
from conftest import (
    MAU_BIN,
    NATIVE_MKV,
    NO_MKVTOOLNIX,
    Ran,
    leftovers,
    mkv_json,
    mkv_layout,
    mkv_names,
    needs_mkvtoolnix,
    requires_tools,
    same_file,
)

import fml

pytestmark = [pytest.mark.real, requires_tools]

FILM = ["a:ita*", "a:eng", "a:fre", "s:eng", "s:engF", "s:ita"]


def test_the_fixture_is_what_the_tests_expect(film_mkv: Path) -> None:
    assert mkv_layout(film_mkv) == FILM


# Reading ---------------------------------------------------------------------

def test_info_lists_the_tracks(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--info", "-i", film_mkv)
    rows = [line.split() for line in ran.out.splitlines()]
    assert rows[0] == [str(film_mkv), "(Matroska)"]
    assert rows[1][:2] == ["Track", "ID"]
    assert [r[0] for r in rows[2:]] == ["video:1", "audio:1", "audio:2", "audio:3",
                                        "subtitles:1", "subtitles:2", "subtitles:3"]
    assert rows[3][3:5] == ["ita", "yes"]
    # Rust reads MKV tracks with ffprobe ("subrip"), fml.py with mkvmerge ("SubRip/SRT")
    assert rows[6][:5] in (["subtitles:1", "4", "subrip", "eng", "-"],
                           ["subtitles:1", "4", "SubRip/SRT", "eng", "-"])
    assert rows[7][5] == "yes"  # the forced subtitle track


def test_info_json_has_the_tracks(run: Callable[..., Ran], film_mkv: Path) -> None:
    (result,) = json.loads(run("--info", "-i", film_mkv, "-F", "json").out)["results"]
    assert result["data"]["container"] == "Matroska"
    audio = [t for t in result["data"]["tracks"] if t["type"] == "audio"]
    assert [(t["number"], t["language"], t["default"], t["name"]) for t in audio] == [
        (1, "ita", True, "Italian"), (2, "eng", False, "English"), (3, "fre", False, "French")]


def test_get_reports_the_default_language(run: Callable[..., Ran], film_mkv: Path) -> None:
    assert run("--get", "lang", "-i", film_mkv).out == "ita\n"


def test_get_reports_none_when_no_subtitle_is_default(run: Callable[..., Ran],
                                                      film_mkv: Path) -> None:
    assert run("--get", "lang", "--kind", "sub", "-i", film_mkv).out == "none\n"


def test_get_lists_the_tracks_in_a_language(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--get", "lang", "--lang", "English", "-i", film_mkv)
    rows = [line.split() for line in ran.out.splitlines()]
    assert rows[0] == [str(film_mkv)] and [r[0] for r in rows[2:]] == ["audio:2"]


def test_get_in_a_language_the_file_lacks(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--get", "lang", "--lang", "German", "-i", film_mkv)
    assert ran.code == 0 and 'has no audio track with language "ger"' in ran.err


# Setting the default ---------------------------------------------------------

def test_set_default_language(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert ran.out.splitlines() == [f"{film_mkv}: audio track 1 (ita): default yes -> no",
                                    f"{film_mkv}: audio track 2 (eng): default no -> yes"]
    assert mkv_layout(film_mkv) == ["a:ita", "a:eng*", "a:fre", "s:eng", "s:engF", "s:ita"]


def test_set_default_is_unchanged_the_second_time(run: Callable[..., Ran],
                                                  film_mkv: Path) -> None:
    run("--set", "lang", "--default", "English", "-i", film_mkv)
    before = film_mkv.read_bytes()
    ran = run("--set", "lang", "--default", "English", "-i", film_mkv, "-F", "json")
    (result,) = json.loads(ran.out)["results"]
    assert result["status"] == "unchanged" and result["commands"] == []
    assert film_mkv.read_bytes() == before


@pytest.mark.parametrize("language", ["English", "en", "eng", "ENG", "English "])
def test_set_default_accepts_every_spelling_of_the_language(
        run: Callable[..., Ran], film_mkv: Path, language: str) -> None:
    assert run("--set", "lang", "--default", language, "-i", film_mkv).code == 0
    assert mkv_layout(film_mkv)[:3] == ["a:ita", "a:eng*", "a:fre"]


def test_set_default_for_a_language_that_is_missing(run: Callable[..., Ran],
                                                    film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "lang", "--default", "Japanese", "-i", film_mkv)
    assert ran.code == 0 and 'has no audio track with language "jpn"' in ran.err
    assert film_mkv.read_bytes() == before


def test_dry_run_changes_nothing(run: Callable[..., Ran], film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "lang", "--default", "English", "-i", film_mkv, "-T")
    assert ran.out.startswith("[dry-run]")
    assert film_mkv.read_bytes() == before and leftovers(film_mkv.parent) == []


def test_the_original_is_replaced_by_a_checked_copy(run: Callable[..., Ran],
                                                    film_mkv: Path) -> None:
    inode = film_mkv.stat().st_ino
    run("--set", "lang", "--default", "English", "-i", film_mkv)
    assert film_mkv.stat().st_ino != inode  # a new file took its place


def test_permissions_and_times_are_kept(run: Callable[..., Ran], film_mkv: Path) -> None:
    film_mkv.chmod(0o640)
    os.utime(film_mkv, (1_600_000_000, 1_600_000_000))
    run("--set", "lang", "--default", "English", "-i", film_mkv)
    info = film_mkv.stat()
    assert stat.S_IMODE(info.st_mode) == 0o640 and int(info.st_mtime) == 1_600_000_000
    assert leftovers(film_mkv.parent) == []


def test_set_the_language_of_a_track(run: Callable[..., Ran], und_mkv: Path) -> None:
    ran = run("--set", "lang", "--lang", "Italian", "--track", "2", "-i", und_mkv)
    assert ran.out.strip() == f"{und_mkv}: audio track 2: language und -> ita"
    assert mkv_layout(und_mkv) == ["a:und*", "a:ita", "a:und"]
    again = run("--set", "lang", "--lang", "Italian", "--track", "2", "-i", und_mkv)
    assert "is already ita" in again.out


def test_set_language_of_the_last_track_and_out_of_range(run: Callable[..., Ran],
                                                         und_mkv: Path) -> None:
    run("--set", "lang", "--lang", "fr", "--track", "last", "-i", und_mkv)
    assert mkv_layout(und_mkv) == ["a:und*", "a:und", "a:fre"]
    ran = run("--set", "lang", "--lang", "fr", "--track", "9", "-i", und_mkv)
    assert "has no audio track 9 (3 audio tracks)" in ran.err


def test_set_default_by_track_number_and_tag_it(run: Callable[..., Ran], und_mkv: Path) -> None:
    ran = run("--set", "default", "--track", "3", "--lang", "English", "-i", und_mkv)
    assert ran.out.splitlines() == [
        f"{und_mkv}: audio track 1 (und): default yes -> no",
        f"{und_mkv}: audio track 3 (und): default no -> yes, language und -> eng"]
    assert mkv_layout(und_mkv) == ["a:und", "a:und", "a:eng*"]


def test_swap_makes_the_second_track_the_only_default(run: Callable[..., Ran],
                                                      film_mkv: Path) -> None:
    run("--set", "default", "--track", "3", "-i", film_mkv)  # a third track is now default
    assert mkv_layout(film_mkv)[:3] == ["a:ita", "a:eng", "a:fre*"]
    ran = run("--swap", "lang", "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(film_mkv)[:3] == ["a:ita", "a:eng*", "a:fre"]
    assert "unchanged" not in ran.out and "is already the default" in run(
        "--swap", "lang", "-i", film_mkv).out


def test_swap_needs_two_audio_tracks(run: Callable[..., Ran],
                                     make: Callable[..., Path]) -> None:
    path = make("one.mkv", [("eng", None, True)])
    assert "needs at least two audio tracks" in run("--swap", "lang", "-i", path).err


def test_prefer_picks_the_first_language_each_file_has(run: Callable[..., Ran], tmp_path: Path,
                                                       make: Callable[..., Path]) -> None:
    folder = tmp_path / "lib"
    make("A.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    make("B.mkv", [("jpn", None, True), ("fre", None, False)], where=folder)
    make("C.mkv", [("spa", None, True)], where=folder)
    make("D.mkv", [("fre", None, True), ("jpn", None, False)], where=folder)
    ran = run("--set", "lang", "--prefer", "eng,jpn,fre", "-d", folder)
    assert ran.code == 0
    assert mkv_layout(folder / "A.mkv") == ["a:ita", "a:eng*"]
    assert mkv_layout(folder / "B.mkv") == ["a:jpn*", "a:fre"]  # already the default
    assert mkv_layout(folder / "C.mkv") == ["a:spa*"]  # none of them
    assert mkv_layout(folder / "D.mkv") == ["a:fre", "a:jpn*"]
    assert "has no audio track in any of: eng, jpn, fre" in ran.err


# Moving ----------------------------------------------------------------------

def test_move_makes_the_track_first_and_keeps_everything_else(run: Callable[..., Ran],
                                                              film_mkv: Path) -> None:
    before = mkv_json(film_mkv)
    ran = run("--set", "lang", "--default", "English", "--move", "-i", film_mkv)
    assert ran.out.splitlines() == [
        f"{film_mkv}: audio track 1 (ita): default yes -> no",
        f"{film_mkv}: audio track 2 (eng): default no -> yes, moved to the first audio track"]
    assert mkv_layout(film_mkv) == ["a:eng*", "a:ita", "a:fre", "s:eng", "s:engF", "s:ita"]
    after = mkv_json(film_mkv)
    names = [t["properties"].get("track_name") for t in after["tracks"] if t["type"] == "audio"]
    assert names == ["English", "Italian", "French"]  # names travel with their tracks
    assert [c["num_entries"] for c in after["chapters"]] == [c["num_entries"]
                                                             for c in before["chapters"]] == [2]
    assert len(after["attachments"]) == len(before["attachments"]) == 1
    assert after["container"]["properties"]["title"] == "Test Film"
    assert after["tracks"][0]["type"] == "video"  # video stays first
    assert leftovers(film_mkv.parent) == []


def test_move_when_the_track_is_already_first_only_edits_the_flag(run: Callable[..., Ran],
                                                                  film_mkv: Path) -> None:
    run("--set", "lang", "--default", "English", "-i", film_mkv)
    ran = run("--set", "lang", "--default", "Italian", "--move", "-i", film_mkv, "-F", "json")
    (result,) = json.loads(ran.out)["results"]
    # mkvpropedit edits the flags in place; without mkvtoolnix the flags are a remux with ffmpeg
    if NATIVE_MKV:
        assert result["commands"][0][0] == "native-mkv"
    else:
        assert result["commands"][0][0] == ("ffmpeg" if NO_MKVTOOLNIX else "mkvpropedit")
    assert mkv_layout(film_mkv)[:3] == ["a:ita*", "a:eng", "a:fre"]


def test_move_when_it_is_already_first_and_default_is_unchanged(run: Callable[..., Ran],
                                                                film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "Italian", "--move", "-i", film_mkv)
    assert ran.out.strip() == (
        f"{film_mkv}: audio track 1 (ita) is already the default and the first track")


def test_move_a_subtitle_track(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "Italian", "--kind", "sub", "--move", "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(film_mkv) == ["a:ita*", "a:eng", "a:fre", "s:ita*", "s:eng", "s:engF"]


def test_move_with_prefer_and_set_default_by_track(run: Callable[..., Ran], und_mkv: Path,
                                                   film_mkv: Path) -> None:
    run("--set", "lang", "--prefer", "jpn,fre", "--move", "-i", film_mkv)
    assert mkv_layout(film_mkv)[:3] == ["a:fre*", "a:ita", "a:eng"]
    ran = run("--set", "default", "--track", "3", "--lang", "English", "--move", "-i", und_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(und_mkv) == ["a:eng*", "a:und", "a:und"]


def test_move_dry_run_shows_the_remux(run: Callable[..., Ran], film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "lang", "--default", "English", "--move", "-i", film_mkv, "-T", "-v")
    if NATIVE_MKV:
        assert "native-mkv reorder-tracks" in ran.err
    elif NO_MKVTOOLNIX:
        assert "ffmpeg" in ran.err and "-map" in ran.err
    else:
        assert "--track-order 0:0,0:2,0:1,0:3,0:4,0:5,0:6" in ran.err
    assert "moved to the first audio track" in ran.out
    assert film_mkv.read_bytes() == before


# Subtitles ------------------------------------------------------------------

def test_default_subtitles_prefer_the_one_that_is_not_forced(run: Callable[..., Ran],
                                                             film_mkv: Path) -> None:
    run("--set", "lang", "--default", "English", "--kind", "sub", "-i", film_mkv)
    assert mkv_layout(film_mkv) == ["a:ita*", "a:eng", "a:fre", "s:eng*", "s:engF", "s:ita"]


def test_forced_picks_the_forced_subtitle(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--set", "lang", "--default", "English", "--kind", "sub", "--forced", "-i", film_mkv)
    assert mkv_layout(film_mkv) == ["a:ita*", "a:eng", "a:fre", "s:eng", "s:eng*F", "s:ita"]


def test_default_none_turns_the_subtitles_off(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--set", "lang", "--default", "Italian", "--kind", "sub", "-i", film_mkv)
    assert "s:ita*" in mkv_layout(film_mkv)
    ran = run("--set", "lang", "--default", "none", "--kind", "sub", "-i", film_mkv)
    assert ran.out.strip() == f"{film_mkv}: subtitle track 3 (ita): default yes -> no"
    assert not any(item.endswith("*") for item in mkv_layout(film_mkv)[3:])
    assert mkv_layout(film_mkv)[0] == "a:ita*"  # audio untouched
    assert "there is already no default subtitle track" in run(
        "--set", "lang", "--default", "none", "--kind", "sub", "-i", film_mkv).out


def test_set_a_subtitle_language(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--set", "lang", "--lang", "French", "--track", "3", "--kind", "sub", "-i", film_mkv)
    assert mkv_layout(film_mkv)[3:] == ["s:eng", "s:engF", "s:fre"]


# Removing tracks ------------------------------------------------------------

def test_delete_audio_by_language(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--delete", "lang", "--lang", "eng", "-i", film_mkv)
    assert ran.out.strip() == f"{film_mkv}: removed audio track 2 (eng)"
    assert mkv_layout(film_mkv) == ["a:ita*", "a:fre", "s:eng", "s:engF", "s:ita"]
    assert leftovers(film_mkv.parent) == []


def test_delete_a_track_by_number(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--delete", "track", "--track", "last", "-i", film_mkv)
    assert mkv_layout(film_mkv)[:2] == ["a:ita*", "a:eng"] and len(mkv_layout(film_mkv)) == 5
    run("--delete", "track", "--track", "2", "--kind", "sub", "-i", film_mkv)
    assert mkv_layout(film_mkv) == ["a:ita*", "a:eng", "s:eng", "s:ita"]


@pytest.mark.skipif(not MAU_BIN, reason="--track kind:N, id:N and name:TEXT are Rust only")
@pytest.mark.parametrize("spec, expected", [
    ("subtitles:2", ["a:ita*", "a:eng", "a:fre", "s:eng", "s:ita"]),
    ("sub:last", ["a:ita*", "a:eng", "a:fre", "s:eng", "s:engF"]),
    ("id:5", ["a:ita*", "a:eng", "a:fre", "s:eng", "s:ita"]),
    ("audio:3", ["a:ita*", "a:eng", "s:eng", "s:engF", "s:ita"]),
    ("name:french", ["a:ita*", "a:eng", "s:eng", "s:engF", "s:ita"]),
])
def test_delete_a_track_of_any_kind_by_place_id_or_name(
    run: Callable[..., Ran], film_mkv: Path, spec: str, expected: list[str]
) -> None:
    ran = run("--delete", "track", "--track", spec, "-i", film_mkv)
    assert ran.code == 0
    assert mkv_layout(film_mkv) == expected
    assert leftovers(film_mkv.parent) == []


@pytest.mark.skipif(not MAU_BIN, reason="--track kind:N, id:N and name:TEXT are Rust only")
def test_delete_a_track_that_is_not_there_changes_nothing(run: Callable[..., Ran],
                                                          film_mkv: Path) -> None:
    before = mkv_layout(film_mkv)
    ran = run("--delete", "track", "--track", "id:99", "-i", film_mkv)
    assert "has no track with ID 99" in ran.err
    assert mkv_layout(film_mkv) == before


def test_preserve_keeps_only_one_language(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--preserve", "lang", "--lang", "fre", "-i", film_mkv)
    assert ran.out.splitlines() == [f"{film_mkv}: removed audio track 1 (ita)",
                                    f"{film_mkv}: removed audio track 2 (eng)",
                                    f"{film_mkv}: audio track 3 (fre): default no -> yes"]
    assert mkv_layout(film_mkv)[0] == "a:fre*" and len(mkv_layout(film_mkv)) == 4


def test_preserve_with_prefer_keeps_the_first_preferred_language(run: Callable[..., Ran],
                                                                 film_mkv: Path) -> None:
    run("--preserve", "lang", "--prefer", "jpn,eng,ita", "-i", film_mkv)
    assert mkv_layout(film_mkv)[0] == "a:eng*" and len(mkv_layout(film_mkv)) == 4


def test_removal_keeps_chapters_attachments_and_the_title(run: Callable[..., Ran],
                                                          film_mkv: Path) -> None:
    run("--delete", "lang", "--lang", "eng", "-i", film_mkv)
    data = mkv_json(film_mkv)
    assert data["chapters"][0]["num_entries"] == 2 and len(data["attachments"]) == 1
    assert data["container"]["properties"]["title"] == "Test Film"


def test_removal_keeps_permissions_and_times(run: Callable[..., Ran], film_mkv: Path) -> None:
    film_mkv.chmod(0o600)
    os.utime(film_mkv, (1_500_000_000, 1_500_000_000))
    run("--delete", "lang", "--lang", "eng", "-i", film_mkv)
    info = film_mkv.stat()
    assert stat.S_IMODE(info.st_mode) == 0o600 and int(info.st_mtime) == 1_500_000_000


def test_the_last_audio_track_is_protected(run: Callable[..., Ran],
                                           make: Callable[..., Path]) -> None:
    path = make("solo.mkv", [("eng", None, True)])
    before = path.read_bytes()
    ran = run("--delete", "lang", "--lang", "eng", "-i", path)
    assert "would remove every audio track" in ran.err and path.read_bytes() == before
    forced = run("--delete", "lang", "--lang", "eng", "--force", "-i", path)
    assert forced.code == 0 and mkv_layout(path) == []


def test_preserve_a_language_the_file_lacks_needs_force(run: Callable[..., Ran],
                                                        film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--preserve", "lang", "--lang", "spa", "--kind", "sub", "-i", film_mkv)
    assert "so nothing would be left" in ran.err and film_mkv.read_bytes() == before
    run("--preserve", "lang", "--lang", "spa", "--kind", "sub", "--force", "-i", film_mkv)
    assert mkv_layout(film_mkv) == ["a:ita*", "a:eng", "a:fre"]  # every subtitle removed


# Directories and actions together --------------------------------------------

def test_directory_mode_with_a_mix_of_files(run: Callable[..., Ran], tmp_path: Path,
                                            make: Callable[..., Path]) -> None:
    folder = tmp_path / "season"
    first = make("e1.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    second = make("e2.mkv", [("eng", None, True), ("ita", None, False)], where=folder)
    third = make("e3.mkv", [("ita", None, True), ("eng", None, False)], where=folder / "sub")
    (folder / "readme.txt").write_text("not media")
    flat = run("--set", "lang", "--default", "English", "--move", "-d", folder)
    assert flat.code == 0, flat.err
    assert mkv_layout(first) == ["a:eng*", "a:ita"] and mkv_layout(second) == ["a:eng*", "a:ita"]
    assert mkv_layout(third) == ["a:ita*", "a:eng"]  # not recursive
    run("--set", "lang", "--default", "English", "--move", "-d", folder, "-r")
    assert mkv_layout(third) == ["a:eng*", "a:ita"]
    assert (folder / "readme.txt").read_text() == "not media"
    assert leftovers(folder) == []


def test_several_actions_run_in_order(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "--delete", "track", "--track", "1",
              "--get", "lang", "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(film_mkv)[:2] == ["a:eng*", "a:fre"]  # set first, then ita (track 1) removed
    assert ran.out.splitlines()[-1] == "eng"


@pytest.mark.skipif(bool(MAU_BIN), reason="the Rust version gives each action its own settings")
def test_settings_are_shared_by_every_action(run: Callable[..., Ran], film_mkv: Path) -> None:
    """In the archived Python version --lang is one setting for the whole command."""
    ran = run("--set", "lang", "--default", "English", "--delete", "lang", "--lang", "ita",
              "-i", film_mkv)
    assert ran.code == 2 and "not --default and --lang" in ran.err


# Settings that belong to an action ---------------------------------------------

@pytest.mark.skipif(not MAU_BIN, reason="settings per action are Rust only")
def test_each_action_has_the_settings_that_follow_it(run: Callable[..., Ran],
                                                     film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "--delete", "lang", "--lang", "ita",
              "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(film_mkv)[:2] == ["a:eng*",
                                        "a:fre"]  # English made default, then Italian removed


@pytest.mark.skipif(not MAU_BIN, reason="settings per action are Rust only")
def test_settings_before_the_first_action_are_shared(run: Callable[..., Ran],
                                                     film_mkv: Path) -> None:
    ran = run("--kind", "sub", "--get", "lang", "--info", "-i", film_mkv)
    assert ran.code == 0, ran.err
    two = run("--lang", "eng", "--delete", "lang", "--dryrun", "-i", film_mkv)
    assert "removed audio track 2 (eng)" in two.out
    both = run("--kind", "sub", "--lang", "ita", "--delete", "lang", "--get", "lang", "--dryrun",
               "-i", film_mkv)
    assert "removed subtitle track 3 (ita)" in both.out


@pytest.mark.skipif(not MAU_BIN, reason="settings per action are Rust only")
def test_a_switch_belongs_to_the_action_before_it(run: Callable[..., Ran], film_mkv: Path) -> None:
    """--move after the first action does not apply to the second one."""
    ran = run("--set", "lang", "--default", "French", "--move", "--set", "lang", "--default",
              "Italian",
              "--dryrun", "--verbose", "-i", film_mkv)
    assert ran.code == 0, ran.err
    # the first plan moves French first (an mkvmerge rewrite); the second only edits flags
    commands = [line for line in ran.err.splitlines() if line.startswith("Executing:")]
    if NATIVE_MKV:
        assert any("native-mkv reorder-tracks" in c for c in commands)
    elif NO_MKVTOOLNIX:
        assert any(c.startswith("Executing:\tffmpeg") and "-map" in c for c in commands)
    else:
        assert any("mkvmerge" in c and "--track-order" in c for c in commands)


@pytest.mark.skipif(not MAU_BIN, reason="settings per action are Rust only")
def test_the_last_value_of_a_setting_for_an_action_wins(run: Callable[..., Ran],
                                                        film_mkv: Path) -> None:
    ran = run("--delete", "lang", "--lang", "fre", "--lang", "eng", "--dryrun", "-i", film_mkv)
    assert "removed audio track 2 (eng)" in ran.out and "fre" not in ran.out


@pytest.mark.skipif(not MAU_BIN, reason="settings per action are Rust only")
def test_a_conflict_is_reported_for_the_action_it_is_in(run: Callable[..., Ran],
                                                        film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    first = run("--set", "lang", "--default", "Italian", "--lang", "eng", "--get", "lang", "-i",
                film_mkv)
    assert first.code == 2 and "not --default and --lang" in first.err
    second = run("--set", "lang", "--default", "Italian", "--delete", "lang", "-i", film_mkv)
    assert second.code == 2 and "--delete lang needs --lang" in second.err
    assert film_mkv.read_bytes() == before


def test_json_output_covers_every_file(run: Callable[..., Ran], tmp_path: Path,
                                       make: Callable[..., Path]) -> None:
    folder = tmp_path / "j"
    make("one.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    make("two.mkv", [("jpn", None, True)], where=folder)
    document = json.loads(run("--set", "lang", "--default", "English", "-d", folder,
                              "-T", "-F", "json").out)
    assert document["version"] == fml.__version__
    assert [(Path(r["file"]).name, r["status"]) for r in document["results"]] == [
        ("one.mkv", "changed"), ("two.mkv", "skipped")]


def test_the_exit_status_is_0_when_files_are_only_skipped(run: Callable[..., Ran],
                                                          film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    assert run("--set", "lang", "--default", "Japanese", "-i", film_mkv).code == 0
    assert same_file(film_mkv, film_mkv) and film_mkv.read_bytes() == before


# Removing the default track ------------------------------------------------------

def test_removing_the_default_audio_track_makes_the_next_one_default(
        run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--delete", "lang", "--lang", "ita", "-i", film_mkv)
    assert ran.out.splitlines() == [f"{film_mkv}: removed audio track 1 (ita)",
                                    f"{film_mkv}: audio track 2 (eng): default no -> yes"]
    assert mkv_layout(film_mkv) == ["a:eng*", "a:fre", "s:eng", "s:engF", "s:ita"]


def test_removing_a_track_that_is_not_the_default_changes_no_flags(run: Callable[..., Ran],
                                                                   film_mkv: Path) -> None:
    ran = run("--delete", "track", "--track", "3", "-i", film_mkv)
    assert ran.out.splitlines() == [f"{film_mkv}: removed audio track 3 (fre)"]
    assert mkv_layout(film_mkv)[:2] == ["a:ita*", "a:eng"]


def test_an_mkv_keeps_having_no_default_subtitle(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--set", "lang", "--default", "Italian", "--kind", "sub", "-i", film_mkv)  # s:ita*
    run("--delete", "lang", "--lang", "ita", "--kind", "sub", "-i", film_mkv)
    assert mkv_layout(film_mkv)[3:] == ["s:eng", "s:engF"]  # none was made default


# Names and the forced flag ------------------------------------------------------

def test_set_a_track_name(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "name", "--track", "2", "--name", "English 5.1", "-i", film_mkv)
    assert ran.out.strip() == f'{film_mkv}: audio track 2 (eng): name "English" -> "English 5.1"'
    assert mkv_names(film_mkv)[:3] == ["Italian", "English 5.1", "French"]
    again = run("--set", "name", "--track", "2", "--name", "English 5.1", "-i", film_mkv)
    assert again.out.strip() == f'{film_mkv}: audio track 2 is already named "English 5.1"'


def test_remove_a_track_name(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--set", "name", "--track", "1", "--name", "", "-i", film_mkv)
    assert mkv_names(film_mkv)[:3] == ["", "English", "French"]
    assert "has no name" in run("--set", "name", "--track", "1", "--name", "", "-i", film_mkv).out


@pytest.mark.skipif(
    sys.platform == "darwin" and not MAU_BIN,
    reason="archived fml.py compares names byte for byte; "
           "mkvtoolnix on macOS stores accents decomposed",
)
def test_names_may_hold_any_text(run: Callable[..., Ran], film_mkv: Path) -> None:
    for name in ("Français", "日本語", "A \"quoted\" name", "-leading dash", "with  spaces"):
        # the = form lets a name start with a dash, which argparse would take for an option
        assert run("--set", "name", "--track", "3", f"--name={name}", "-i", film_mkv).code == 0
        assert mkv_names(film_mkv)[2] == name
    assert run("--set", "name", "--track", "3", "--name", "-x", "-i", film_mkv).code == 2


def test_set_a_subtitle_name(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--set", "name", "--track", "3", "--kind", "sub", "--name", "Italiano", "-i", film_mkv)
    assert mkv_names(film_mkv)[3:] == ["", "", "Italiano"]


def test_set_and_clear_the_forced_flag(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "forced", "--track", "3", "--kind", "sub", "-i", film_mkv)
    assert ran.out.strip() == f"{film_mkv}: subtitle track 3 (ita): forced no -> yes"
    assert mkv_layout(film_mkv)[3:] == ["s:eng", "s:engF", "s:itaF"]
    assert "is already forced" in run("--set", "forced", "--track", "3", "--kind", "sub",
                                      "-i", film_mkv).out
    run("--set", "unforced", "--track", "2", "--kind", "sub", "-i", film_mkv)
    assert mkv_layout(film_mkv)[3:] == ["s:eng", "s:eng", "s:itaF"]
    assert "is already not forced" in run("--set", "unforced", "--track", "2", "--kind", "sub",
                                          "-i", film_mkv).out


def test_the_forced_flag_of_an_audio_track(run: Callable[..., Ran], film_mkv: Path) -> None:
    run("--set", "forced", "--track", "2", "-i", film_mkv)
    assert mkv_layout(film_mkv)[:3] == ["a:ita*", "a:engF", "a:fre"]


def test_name_and_forced_edits_dry_run_and_json(run: Callable[..., Ran], film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "name", "--track", "1", "--name", "X", "-i", film_mkv, "-T", "-F", "json")
    (result,) = json.loads(ran.out)["results"]
    assert result["changes"] == ['audio track 1 (ita): name "Italian" -> "X"']
    if NATIVE_MKV:
        assert result["commands"][0][0] == "native-mkv"
    else:
        assert ("title=X" if NO_MKVTOOLNIX else "name=X") in result["commands"][0]
    assert film_mkv.read_bytes() == before


def test_a_name_edit_out_of_range(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "name", "--track", "9", "--name", "X", "-i", film_mkv)
    assert "has no audio track 9" in ran.err


# Keeping the original ----------------------------------------------------------

def test_backup_keeps_the_original(run: Callable[..., Ran], film_mkv: Path) -> None:
    original = film_mkv.read_bytes()
    inode = film_mkv.stat().st_ino
    ran = run("--set", "lang", "--default", "English", "-i", film_mkv, "--backup")
    backup = Path(str(film_mkv) + fml.BACKUP_MARK)
    assert ran.out.splitlines()[-1] == f"{film_mkv}: the original is kept as {backup}"
    assert backup.read_bytes() == original and backup.stat().st_ino == inode
    assert mkv_layout(backup)[0] == "a:ita*"  # the backup is the file as it was
    assert mkv_layout(film_mkv)[1] == "a:eng*"
    assert leftovers(film_mkv.parent) == []  # backups use their own mark


def test_backup_on_every_kind_of_change(run: Callable[..., Ran], film_mkv: Path) -> None:
    for args in (["--set", "name", "--track", "1", "--name", "X"],
                 ["--set", "lang", "--default", "English", "--move"],
                 ["--delete", "lang", "--lang", "fre"]):
        before = film_mkv.read_bytes()
        run(*args, "-i", film_mkv, "--backup")
        newest = sorted(film_mkv.parent.glob(f"film.mkv{fml.BACKUP_MARK}*"))[-1]
        assert newest.read_bytes() == before, args


def test_backups_are_numbered(run: Callable[..., Ran], film_mkv: Path) -> None:
    for default in ("English", "French", "Italian"):
        run("--set", "lang", "--default", default, "-i", film_mkv, "--backup")
    names = sorted(path.name for path in film_mkv.parent.glob("film.mkv*"))
    assert names == ["film.mkv", "film.mkv.fml-bak", "film.mkv.fml-bak.1", "film.mkv.fml-bak.2"]


def test_backup_dry_run_and_directory_mode(run: Callable[..., Ran], tmp_path: Path,
                                           make: Callable[..., Path]) -> None:
    folder = tmp_path / "d"
    make("a.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    make("b.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    run("--set", "lang", "--default", "English", "-d", folder, "--backup", "-T")
    assert sorted(path.name for path in folder.iterdir()) == ["a.mkv", "b.mkv"]
    run("--set", "lang", "--default", "English", "-d", folder, "--backup")
    assert sorted(path.name for path in folder.iterdir()) == [
        "a.mkv", "a.mkv.fml-bak", "b.mkv", "b.mkv.fml-bak"]
    # the backups are not processed as media files on the next run
    ran = run("--get", "lang", "-d", folder)
    assert [line.split(": ")[1] for line in ran.out.splitlines()] == ["eng", "eng"]


# Extracting a subtitle track -------------------------------------------------

def subtitle_codec(path: Path) -> str:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "stream=codec_name", "-of", "csv=p=0",
         str(path)], capture_output=True, text=True, check=True).stdout
    return out.split()[0]


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_extract_a_subtitle_track_to_a_file_named_after_the_media(run: Callable[..., Ran],
                                                                  film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--extract", "subtitle", "--track", "2", "-i", film_mkv)
    # the second subtitle track is the forced English one
    out = film_mkv.with_name("film.eng.forced.srt")
    assert ran.code == 0, ran.err
    assert ran.out.strip() == f"{film_mkv}: extracted subtitle track 2 (eng) to {out}"
    assert out.stat().st_size > 0 and subtitle_codec(out) == "subrip"
    assert film_mkv.read_bytes() == before  # the media file is not touched
    assert leftovers(film_mkv.parent) == []


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_extract_to_a_folder_or_a_named_file(run: Callable[..., Ran], film_mkv: Path,
                                             tmp_path: Path) -> None:
    folder = tmp_path / "subs"
    folder.mkdir()
    assert run("--extract", "subtitle", "--track", "subtitles:3", "--output", folder, "-i",
               film_mkv).code == 0
    assert (folder / "film.ita.srt").is_file()
    named = tmp_path / "mine.srt"
    assert run("--extract", "subtitle", "--track", "1", "--output", named, "-i", film_mkv).code == 0
    assert subtitle_codec(named) == "subrip"
    missing = run("--extract", "subtitle", "--track", "1", "--output", tmp_path / "nope" / "x.srt",
                  "-i", film_mkv)
    assert "does not exist" in missing.err and not (tmp_path / "nope").exists()


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_extract_never_replaces_a_file_without_force(run: Callable[..., Ran], film_mkv: Path,
                                                   tmp_path: Path) -> None:
    target = tmp_path / "keep.srt"
    target.write_text("mine")
    refused = run("--extract", "subtitle", "--track", "1", "--output", target, "-i", film_mkv)
    assert "already exists" in refused.err and target.read_text() == "mine"
    replaced = run("--extract", "subtitle", "--track", "1", "--output", target, "--force", "-i",
                   film_mkv)
    assert replaced.code == 0 and target.read_text() != "mine"
    again = run("--extract", "subtitle", "--track", "1", "-i", film_mkv)
    assert again.code == 0
    second = run("--extract", "subtitle", "--track", "1", "-i", film_mkv)
    assert "already exists" in second.err  # the generated name is taken too
    assert leftovers(film_mkv.parent) == [] and leftovers(tmp_path) == []


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_a_dry_run_extract_writes_nothing(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--extract", "subtitle", "--track", "1", "--dryrun", "-i", film_mkv)
    assert ran.code == 0 and "extracted subtitle track 1 (eng)" in ran.out
    assert not film_mkv.with_name("film.eng.srt").exists()


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
@pytest.mark.parametrize("extra, message", [
    ([], "needs --track"),
    (["--track", "1", "--lang", "it"], "not --lang"),
])
def test_a_bad_extract_is_refused(run: Callable[..., Ran], film_mkv: Path, extra: list[str],
                                  message: str) -> None:
    ran = run("--extract", "subtitle", *extra, "-i", film_mkv)
    assert ran.code == 2 and message in ran.err, ran.err


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_output_needs_extract(run: Callable[..., Ran], film_mkv: Path, tmp_path: Path) -> None:
    ran = run("--set", "name", "--track", "1", "--name", "x", "--output", tmp_path / "a.srt", "-i",
              film_mkv)
    assert ran.code == 2 and "--output only works with --extract subtitle" in ran.err


@pytest.mark.skipif(not MAU_BIN, reason="--extract is Rust only")
def test_extract_a_track_that_is_not_there_says_so(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--extract", "subtitle", "--track", "9", "-i", film_mkv)
    assert "has no subtitle track 9 (3 subtitle tracks)" in ran.err
    assert not list(film_mkv.parent.glob("*.srt"))


# Adding the subtitle files of a folder ---------------------------------------

def subs_folder(root: Path, *names: str) -> Path:
    folder = root / "subs"
    folder.mkdir(exist_ok=True)
    for name in names:
        target = folder / name
        target.parent.mkdir(parents=True, exist_ok=True)
        srt(target.parent, target.name)
    return folder


@pytest.mark.skipif(not MAU_BIN, reason="--sub-dir is Rust only")
def test_add_the_subtitle_files_of_a_folder_by_name(run: Callable[..., Ran], film_mkv: Path,
                                                    tmp_path: Path) -> None:
    folder = subs_folder(tmp_path, "film.ger.srt", "film.spa.forced.srt", "other.ita.srt",
                         "nested/film.fre.srt")
    (folder / "notes.txt").write_text("not a subtitle")
    ran = run("--add", "subtitle", "--sub-dir", folder, "-i", film_mkv)
    assert ran.code == 0, ran.err
    # added in path order: the folder's own files, then the folder below it
    assert mkv_layout(film_mkv)[3:] == ["s:eng", "s:engF", "s:ita", "s:ger", "s:spaF", "s:fre"]
    assert mkv_names(film_mkv)[-2] == "Forced"  # the name comes from the file name too
    assert "no media file matches the subtitle" in ran.err and "other.ita.srt" in ran.err
    assert leftovers(film_mkv.parent) == []


@pytest.mark.skipif(not MAU_BIN, reason="--sub-dir is Rust only")
def test_a_second_run_adds_nothing(run: Callable[..., Ran], film_mkv: Path, tmp_path: Path) -> None:
    folder = subs_folder(tmp_path, "film.ger.srt")
    assert run("--add", "subtitle", "--sub-dir", folder, "-i", film_mkv).code == 0
    done = film_mkv.read_bytes()
    again = run("--add", "subtitle", "--sub-dir", folder, "-i", film_mkv)
    assert again.code == 0 and "already has a subtitle track in ger" in again.err
    assert film_mkv.read_bytes() == done  # not rewritten


@pytest.mark.skipif(not MAU_BIN, reason="--sub-dir is Rust only")
def test_lang_is_used_where_the_name_has_no_language(run: Callable[..., Ran], film_mkv: Path,
                                                      tmp_path: Path) -> None:
    folder = subs_folder(tmp_path, "film.srt")
    before = film_mkv.read_bytes()
    without = run("--add", "subtitle", "--sub-dir", folder, "-i", film_mkv)
    assert "is not in its name" in without.err and film_mkv.read_bytes() == before
    given = run("--add", "subtitle", "--sub-dir", folder, "--lang", "Portuguese", "-i", film_mkv)
    assert given.code == 0, given.err
    assert mkv_layout(film_mkv)[-1] == "s:por"


@pytest.mark.skipif(not MAU_BIN, reason="--sub-dir is Rust only")
def test_a_season_is_matched_by_name_or_by_episode(run: Callable[..., Ran], make: Callable[...,
                                                                                           Path],
                                                    tmp_path: Path) -> None:
    season = tmp_path / "season"
    audio = [("ita", None, True), ("eng", None, False)]
    first = make("Show.S01E01.1080p.mkv", audio, where=season)
    second = make("Show.S01E02.1080p.mkv", audio, where=season)
    third = make("Show.S01E03.1080p.mkv", audio, where=season)
    folder = subs_folder(tmp_path, "Show.S01E01.1080p.ita.srt", "Show S01E02 English.srt")
    ran = run("--add", "subtitle", "--sub-dir", folder, "--dir", season)
    assert ran.code == 0, ran.err
    assert mkv_layout(first)[-1] == "s:ita"
    assert mkv_layout(second)[-1] == "s:eng"  # by season and episode
    assert not [t for t in mkv_layout(third) if t.startswith("s:")]  # nothing for the third
    assert "Show.S01E03.1080p.mkv: no subtitle file" in ran.err


@pytest.mark.skipif(not MAU_BIN, reason="--sub-dir is Rust only")
@pytest.mark.parametrize("extra, message", [
    (["--sub-dir", "NOFOLDER"], "is not a folder"),
    (["--sub-dir", "FOLDER", "--sub-file", "FILE"], "not --sub-file or --name"),
    (["--sub-dir", "FOLDER", "--name", "x"], "not --sub-file or --name"),
])
def test_a_bad_sub_dir_is_refused(run: Callable[..., Ran], film_mkv: Path, tmp_path: Path,
                                  extra: list[str], message: str) -> None:
    folder = subs_folder(tmp_path, "film.ger.srt")
    file = srt(tmp_path, "one.srt")
    args = [str(folder) if a == "FOLDER" else str(file) if a == "FILE"
            else str(tmp_path / "nowhere") if a == "NOFOLDER" else a for a in extra]
    before = film_mkv.read_bytes()
    ran = run("--add", "subtitle", *args, "-i", film_mkv)
    assert ran.code == 2 and message in ran.err, ran.err
    assert film_mkv.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--sub-dir is Rust only")
def test_sub_dir_needs_add_subtitle(run: Callable[..., Ran], film_mkv: Path,
                                    tmp_path: Path) -> None:
    ran = run("--get", "lang", "--sub-dir", subs_folder(tmp_path, "film.ger.srt"), "-i", film_mkv)
    assert ran.code == 2 and "only work with --add subtitle" in ran.err


# Changing several properties of a track at once -----------------------------

@pytest.mark.skipif(not MAU_BIN, reason="--set track is Rust only")
def test_set_track_changes_language_name_and_forced_together(run: Callable[..., Ran],
                                                             film_mkv: Path,
                                                             tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    before = (mkv_layout(film_mkv), mkv_names(film_mkv))
    ran = run("--set", "track", "--track", "3", "--kind", "sub", "--lang", "Spanish", "--name",
              "Latino",
              "--forced-flag", "yes", "--log", log, "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert ran.out.strip() == (f'{film_mkv}: subtitle track 3 (ita): language ita -> spa, '
                               'name "" -> "Latino", forced no -> yes')
    assert mkv_layout(film_mkv)[-3:] == ["s:eng", "s:engF", "s:spaF"]
    assert mkv_names(film_mkv)[-1] == "Latino"
    assert leftovers(film_mkv.parent) == []
    assert run("--undo", log).code == 0
    assert (mkv_layout(film_mkv), mkv_names(film_mkv)) == before


@pytest.mark.skipif(not MAU_BIN, reason="--set track is Rust only")
def test_set_track_only_touches_what_is_given(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "track", "--track", "2", "--name", "English 5.1", "-i", film_mkv)
    assert 'name "English" -> "English 5.1"' in ran.out and "language" not in ran.out
    assert mkv_layout(film_mkv)[:3] == ["a:ita*", "a:eng", "a:fre"]
    again = run("--set", "track", "--track", "2", "--lang", "eng", "--name", "English 5.1", "-i",
                film_mkv)
    assert "already as asked" in again.out
    gone = run("--set", "track", "--track", "2", "--name", "", "-i", film_mkv)
    assert mkv_names(film_mkv)[1] == "" and gone.code == 0


@pytest.mark.skipif(not MAU_BIN, reason="--set track is Rust only")
@pytest.mark.parametrize("extra, message", [
    ([], "needs --track"),
    (["--track", "1"], "needs at least one of --lang, --name and --forced-flag"),
    (["--track", "1", "--lang", "it", "--default", "eng"], "not --default"),
    (["--track", "1", "--forced-flag", "maybe"], "invalid choice"),
])
def test_a_bad_set_track_is_refused(run: Callable[..., Ran], film_mkv: Path, extra: list[str],
                                    message: str) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "track", *extra, "-i", film_mkv)
    assert ran.code == 2 and message in ran.err, ran.err
    assert film_mkv.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--set track is Rust only")
def test_forced_flag_needs_set_track(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "name", "--track", "1", "--name", "x", "--forced-flag", "yes", "-i",
              film_mkv)
    assert ran.code == 2 and "--forced-flag only works with --set track" in ran.err


# Adding a subtitle file ------------------------------------------------------

def srt(folder: Path, name: str = "it.srt") -> Path:
    path = folder / name
    path.write_text("1\n00:00:00,000 --> 00:00:00,900\nCiao\n\n", encoding="utf-8")
    return path


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_add_a_subtitle_file_as_the_last_subtitle_track(run: Callable[..., Ran], film_mkv: Path,
                                                         tmp_path: Path) -> None:
    before = mkv_json(film_mkv)
    ran = run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "Italian", "--name",
              "Italiano SDH",
              "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert ran.out.strip() == f"{film_mkv}: added subtitle track 4 (ita) from it.srt"
    assert mkv_layout(film_mkv) == ["a:ita*", "a:eng", "a:fre", "s:eng", "s:engF", "s:ita", "s:ita"]
    assert mkv_names(film_mkv)[3:] == ["", "", "", "Italiano SDH"]
    after = mkv_json(film_mkv)
    assert [t["codec"] for t in after["tracks"][-1:]] == ["SubRip/SRT"]
    assert len(after["chapters"]) == len(before["chapters"]) > 0  # nothing else was lost
    assert leftovers(film_mkv.parent) == []


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_a_new_subtitle_is_never_default_or_forced(run: Callable[..., Ran], und_mkv: Path,
                                                   tmp_path: Path) -> None:
    assert run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "eng", "-i",
               und_mkv).code == 0
    assert mkv_layout(und_mkv)[-1] == "s:eng"  # no star: not default, no F: not forced


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_undo_an_added_subtitle_from_the_backup(run: Callable[..., Ran], film_mkv: Path,
                                               tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    before = (mkv_layout(film_mkv), mkv_names(film_mkv))
    run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "ita", "--backup", "--log",
        log, "-i", film_mkv)
    assert len(mkv_layout(film_mkv)) == len(before[0]) + 1
    assert run("--undo", log).code == 0
    assert (mkv_layout(film_mkv), mkv_names(film_mkv)) == before


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_undo_an_added_subtitle_without_a_backup_says_why(run: Callable[..., Ran], film_mkv: Path,
                                                          tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--add", "subtitle", "--sub-file", srt(tmp_path), "--lang", "ita", "--log", log, "-i",
        film_mkv)
    ran = run("--undo", log)
    assert "use --backup" in ran.err
    assert len(mkv_layout(film_mkv)) == 7


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
@pytest.mark.parametrize("extra, message", [
    (["--lang", "ita", "--sub-file", "missing.srt"], "is not a file"),
    (["--lang", "ita", "--sub-file", "notes.txt"], "not a subtitle file I can add"),
    (["--sub-file", "it.srt"], "needs --lang"),
    ([], "needs --sub-file"),
    (["--lang", "ita", "--sub-file", "it.srt", "--track", "1"], "not --track"),
])
def test_a_bad_add_is_refused_and_changes_nothing(run: Callable[..., Ran], film_mkv: Path,
                                                  tmp_path: Path,
                                                  extra: list[str], message: str) -> None:
    srt(tmp_path)
    (tmp_path / "notes.txt").write_text("hello")
    args = [str(tmp_path / a) if a in ("missing.srt", "notes.txt", "it.srt") else a for a in extra]
    before = film_mkv.read_bytes()
    ran = run("--add", "subtitle", *args, "-i", film_mkv)
    assert ran.code == 2 and message in ran.err, ran.err
    assert film_mkv.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--add subtitle is Rust only")
def test_sub_file_needs_add(run: Callable[..., Ran], film_mkv: Path, tmp_path: Path) -> None:
    ran = run("--set", "name", "--track", "1", "--name", "x", "--sub-file", srt(tmp_path), "-i",
              film_mkv)
    assert ran.code == 2 and "--sub-file and --sub-dir only work with --add subtitle" in ran.err


# What a change will do, as data -----------------------------------------------

def layouts(ran: Ran) -> tuple[list[str], list[str]]:
    """The audio and subtitle tracks before and after, as ``a:ita*`` (``*`` default)."""
    layout = json.loads(ran.out)["results"][0]["data"]["layout"]

    def show(tracks: list[dict[str, Any]]) -> list[str]:
        return [f"{t['type'][0]}:{t['language']}{'*' if t['default'] else ''}"
                f"{'F' if t['forced'] else ''}" for t in tracks if t["type"] != "video"]
    return show(layout["before"]), show(layout["after"])


@pytest.mark.skipif(not MAU_BIN, reason="data.layout is Rust only")
def test_a_dry_run_says_what_the_tracks_will_be(run: Callable[..., Ran], film_mkv: Path,
                                                tmp_path: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "order", "--order", "3,1,2", "--default-first", "--dryrun", "--format",
              "json", "-i", film_mkv)
    shown_before, shown_after = layouts(ran)
    assert shown_before == mkv_layout(film_mkv) == ["a:ita*", "a:eng", "a:fre", "s:eng", "s:engF",
                                                    "s:ita"]
    assert shown_after == ["a:fre*", "a:ita", "a:eng", "s:eng", "s:engF", "s:ita"]
    assert film_mkv.read_bytes() == before
    # and it is what the real change then does
    run("--set", "order", "--order", "3,1,2", "--default-first", "-i", film_mkv)
    assert mkv_layout(film_mkv) == shown_after


@pytest.mark.skipif(not MAU_BIN, reason="data.layout is Rust only")
@pytest.mark.parametrize("args, after", [
    (["--delete", "lang", "--lang", "eng"], ["a:ita*", "a:fre", "s:eng", "s:engF", "s:ita"]),
    (["--delete", "track", "--track", "subtitles:2"], ["a:ita*", "a:eng", "a:fre", "s:eng",
                                                       "s:ita"]),
    (["--preserve", "lang", "--lang", "fre"], ["a:fre*", "s:eng", "s:engF", "s:ita"]),
    (["--set", "lang", "--default", "English"], ["a:ita", "a:eng*", "a:fre", "s:eng", "s:engF",
                                                 "s:ita"]),
])
def test_the_layout_matches_the_result_for_each_kind_of_change(
        run: Callable[..., Ran], film_mkv: Path, args: list[str], after: list[str]) -> None:
    preview = run(*args, "--dryrun", "--format", "json", "-i", film_mkv)
    assert layouts(preview)[1] == after
    run(*args, "-i", film_mkv)
    assert mkv_layout(film_mkv) == after


@pytest.mark.skipif(not MAU_BIN, reason="data.layout is Rust only")
def test_adding_a_subtitle_says_what_the_tracks_will_be(run: Callable[..., Ran], film_mkv: Path,
                                                         tmp_path: Path) -> None:
    sub = tmp_path / "x.srt"
    sub.write_text("1\n00:00:00,000 --> 00:00:00,900\nCiao\n\n")
    ran = run("--add", "subtitle", "--sub-file", sub, "--lang", "fre", "--dryrun", "--format",
              "json", "-i", film_mkv)
    assert layouts(ran)[1][-2:] == ["s:ita", "s:fre"]


@pytest.mark.skipif(not MAU_BIN, reason="data.layout is Rust only")
def test_a_result_with_nothing_to_show_has_no_layout(run: Callable[..., Ran],
                                                     film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "Italian", "--format", "json", "-i", film_mkv)
    assert json.loads(ran.out)["results"][0]["status"] == "unchanged"
    assert json.loads(ran.out)["results"][0]["data"] in (None, {})


# Reordering ------------------------------------------------------------------

@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
@pytest.mark.parametrize("kind, order, expected, names", [
    ("audio", "3,1,2", ["a:fre", "a:ita*", "a:eng", "s:eng", "s:engF", "s:ita"],
     ["French", "Italian", "English"]),
    ("audio", "2,1,3", ["a:eng", "a:ita*", "a:fre", "s:eng", "s:engF", "s:ita"],
     ["English", "Italian", "French"]),
    ("sub", "3,2,1", ["a:ita*", "a:eng", "a:fre", "s:ita", "s:engF", "s:eng"],
     ["Italian", "English", "French"]),
])
def test_put_the_tracks_in_any_order(run: Callable[..., Ran], film_mkv: Path, kind: str, order: str,
                                     expected: list[str], names: list[str]) -> None:
    ran = run("--set", "order", "--order", order, "--kind", kind, "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(film_mkv) == expected  # flags travel with their tracks
    assert mkv_names(film_mkv)[:3] == names
    assert leftovers(film_mkv.parent) == []
    assert run("--set", "order", "--order", ",".join(str(i) for i in range(1, 4)), "--kind", kind,
               "-i", film_mkv).code == 0  # a second order is applied to what is there now


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_an_order_in_the_order_it_is_changes_nothing(run: Callable[..., Ran],
                                                     film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "order", "--order", "1,2,3", "-i", film_mkv)
    assert "already in that order" in ran.out
    assert film_mkv.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
@pytest.mark.parametrize("order, message", [
    ("1,2", "must list each of the 3 audio tracks once"),
    ("1,2,3,4", "must list each of the 3 audio tracks once"),
    ("1,2,5", "must list each of the 3 audio tracks once"),
    ("1,1,2", "names a track twice"),
    ("1,x", "is not an order of tracks"),
])
def test_a_bad_order_is_refused_and_changes_nothing(run: Callable[..., Ran], film_mkv: Path,
                                                    order: str, message: str) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "order", "--order", order, "-i", film_mkv)
    assert message in ran.err
    assert film_mkv.read_bytes() == before


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_order_needs_its_own_settings(run: Callable[..., Ran], film_mkv: Path) -> None:
    assert "needs one of --order" in run("--set", "order", "-i", film_mkv).err
    assert "needs one of --order" in run("--set", "order", "--order", "1,2,3", "--order-lang",
                                         "ita", "-i", film_mkv).err
    other = run("--set", "lang", "--default", "eng", "--order-lang", "ita", "-i", film_mkv)
    assert "--order-lang only works with --set order" in other.err
    assert "only works with --set order" in run("--set", "lang", "--default", "eng", "--order",
                                                "1,2,3", "-i", film_mkv).err
    assert "not --track" in run("--set", "order", "--order", "2,1,3", "--track", "1", "-i",
                                film_mkv).err


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_undo_a_reorder(run: Callable[..., Ran], film_mkv: Path, tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    before = (mkv_layout(film_mkv), mkv_names(film_mkv))
    run("--set", "order", "--order", "3,1,2", "-i", film_mkv, "--log", log)
    assert mkv_layout(film_mkv)[:3] == ["a:fre", "a:ita*", "a:eng"]
    ran = run("--undo", log)
    assert ran.code == 0, ran.err
    assert (mkv_layout(film_mkv), mkv_names(film_mkv)) == before


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_order_can_make_the_new_first_track_the_default(run: Callable[..., Ran], film_mkv: Path,
                                                        tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    before = (mkv_layout(film_mkv), mkv_names(film_mkv))
    ran = run("--set", "order", "--order", "3,1,2", "--default-first", "--log", log, "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(film_mkv)[:3] == ["a:fre*", "a:ita",
                                        "a:eng"]  # one default, on the first track
    assert mkv_names(film_mkv)[:3] == ["French", "Italian", "English"]
    assert run("--undo", log).code == 0
    assert (mkv_layout(film_mkv), mkv_names(film_mkv)) == before


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_default_first_fixes_the_flag_when_the_order_is_already_right(run: Callable[..., Ran],
                                                                    film_mkv: Path) -> None:
    run("--set", "order", "--order", "2,1,3", "-i", film_mkv)
    assert mkv_layout(film_mkv)[:3] == ["a:eng", "a:ita*", "a:fre"]  # the flag stayed on ita
    ran = run("--set", "order", "--order", "1,2,3", "--default-first", "-i", film_mkv)
    assert ran.code == 0, ran.err
    assert mkv_layout(film_mkv)[:3] == ["a:eng*", "a:ita", "a:fre"]
    again = run("--set", "order", "--order", "1,2,3", "--default-first", "-i", film_mkv)
    assert "already in that order and the first is the default" in again.out


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_default_first_needs_set_order(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "eng", "--default-first", "-i", film_mkv)
    assert ran.code == 2 and "--default-first only works with --set order" in ran.err


@pytest.mark.skipif(not MAU_BIN, reason="--set order is Rust only")
def test_order_by_language_suits_files_with_different_tracks(run: Callable[..., Ran],
                                                             make: Callable[..., Path],
                                                             tmp_path: Path) -> None:
    full = make("a.mkv", [("ita", "Italian", True), ("eng", "English", False), ("fre", "French",
                                                                                False)], [])
    short = make("b.mkv", [("eng", "English", True), ("ita", "Italian", False)], [])
    other = make("c.mkv", [("fre", "French", True), ("ger", "German", False), ("eng", "English",
                                                                               False)], [])
    folder = tmp_path / "season"
    folder.mkdir()
    for path in (full, short, other):
        path.rename(folder / path.name)
    ran = run("--set", "order", "--order-lang", "eng,ita", "--default-first", "--dir", folder)
    assert ran.code == 0, ran.err
    assert mkv_layout(folder / "a.mkv") == ["a:eng*", "a:ita", "a:fre"]
    assert mkv_layout(folder / "b.mkv") == ["a:eng*", "a:ita"]
    assert mkv_layout(folder / "c.mkv") == ["a:eng*", "a:fre",
                                            "a:ger"]  # others follow in their old order
    again = run("--set", "order", "--order-lang", "eng,ita", "--dir", folder)
    assert again.out.count("already in that order") == 3


# The log and undoing ------------------------------------------------------------

def test_undo_reverses_stacked_changes_exactly(run: Callable[..., Ran], film_mkv: Path,
                                               tmp_path: Path) -> None:
    log = tmp_path / "changes.jsonl"
    before = (mkv_layout(film_mkv), mkv_names(film_mkv))
    for args in (["--set", "lang", "--default", "English", "--move"],
                 ["--set", "name", "--track", "1", "--name", "Original"],
                 ["--set", "forced", "--track", "1", "--kind", "sub"],
                 ["--set", "lang", "--lang", "Spanish", "--track", "3"]):
        assert run(*args, "-i", film_mkv, "--log", log).code == 0
    assert (mkv_layout(film_mkv), mkv_names(film_mkv)) != before
    assert len(log.read_text().splitlines()) == 4
    ran = run("--undo", log)
    assert ran.code == 0, ran.err
    assert (mkv_layout(film_mkv), mkv_names(film_mkv)) == before
    assert leftovers(film_mkv.parent) == []


def test_undo_a_move_to_a_later_place(run: Callable[..., Ran], film_mkv: Path,
                                      tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--set", "lang", "--default", "French", "--move", "-i", film_mkv, "--log", log)
    assert mkv_layout(film_mkv)[:3] == ["a:fre*", "a:ita", "a:eng"]
    run("--undo", log)
    assert mkv_layout(film_mkv)[:3] == ["a:ita*", "a:eng", "a:fre"]


def test_undo_keeps_everything_else_in_the_file(run: Callable[..., Ran], film_mkv: Path,
                                                tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--set", "lang", "--default", "English", "--move", "-i", film_mkv, "--log", log)
    run("--undo", log)
    data = mkv_json(film_mkv)
    assert data["chapters"][0]["num_entries"] == 2 and len(data["attachments"]) == 1
    assert data["container"]["properties"]["title"] == "Test Film"


def test_undo_dry_run_changes_nothing(run: Callable[..., Ran], film_mkv: Path,
                                      tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--set", "lang", "--default", "English", "-i", film_mkv, "--log", log)
    before = film_mkv.read_bytes()
    ran = run("--undo", log, "-T")
    assert ran.out.startswith("[dry-run]") and film_mkv.read_bytes() == before


def test_undo_dry_run_of_stacked_changes_is_not_alarming(run: Callable[..., Ran],
                                                         film_mkv: Path, tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--set", "lang", "--default", "English", "-i", film_mkv, "--log", log)
    run("--set", "name", "--track", "1", "--name", "X", "-i", film_mkv, "--log", log)
    ran = run("--undo", log, "-T")
    assert ran.code == 0 and ran.err == ""  # the older change is not reported as a problem
    assert 'name "X" -> "Italian"' in ran.out and "an older change" in ran.out


def test_undo_leaves_a_file_that_has_changed_since(run: Callable[..., Ran], film_mkv: Path,
                                                   tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--set", "lang", "--default", "English", "-i", film_mkv, "--log", log)
    run("--set", "lang", "--default", "French", "-i", film_mkv)  # not logged
    before = film_mkv.read_bytes()
    ran = run("--undo", log)
    assert "has changed since it was logged, so it was left alone" in ran.err
    assert film_mkv.read_bytes() == before


def test_undo_twice_is_harmless(run: Callable[..., Ran], film_mkv: Path, tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--set", "lang", "--default", "English", "-i", film_mkv, "--log", log)
    run("--undo", log)
    restored = film_mkv.read_bytes()
    ran = run("--undo", log)
    assert ran.code == 0 and "is already as it was before this change" in ran.out
    assert film_mkv.read_bytes() == restored


def test_undo_cannot_reverse_a_removal_without_a_backup(run: Callable[..., Ran],
                                                        film_mkv: Path, tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--delete", "lang", "--lang", "eng", "-i", film_mkv, "--log", log)
    before = film_mkv.read_bytes()
    ran = run("--undo", log)
    assert ran.code == 0 and "can't be undone without the backup it would need" in ran.err
    assert film_mkv.read_bytes() == before


def test_undo_restores_a_removal_from_its_backup_exactly(run: Callable[..., Ran],
                                                         film_mkv: Path, tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    original = film_mkv.read_bytes()
    run("--delete", "lang", "--lang", "eng", "-i", film_mkv, "--log", log, "--backup")
    assert film_mkv.read_bytes() != original
    ran = run("--undo", log)
    assert ran.out.strip().startswith(f"{film_mkv}: restored from ")
    assert film_mkv.read_bytes() == original
    assert not Path(str(film_mkv) + fml.BACKUP_MARK).exists()  # the backup was used up


@needs_mkvtoolnix
def test_undo_refuses_a_backup_that_is_not_what_was_logged(run: Callable[..., Ran],
                                                           film_mkv: Path,
                                                           tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--delete", "lang", "--lang", "eng", "-i", film_mkv, "--log", log, "--backup")
    backup = Path(str(film_mkv) + fml.BACKUP_MARK)
    run("--set", "lang", "--default", "French", "-i", backup)  # the backup changed
    before = film_mkv.read_bytes()
    ran = run("--undo", log)
    assert "is not what was logged, so it was not used" in ran.err
    assert film_mkv.read_bytes() == before and backup.exists()


def test_undo_scope_is_limited_by_file_and_directory(run: Callable[..., Ran], tmp_path: Path,
                                                     make: Callable[..., Path]) -> None:
    folder = tmp_path / "d"
    first = make("a.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    second = make("b.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    other = make("c.mkv", [("ita", None, True), ("eng", None, False)], where=tmp_path / "e")
    log = tmp_path / "c.jsonl"
    for path in (first, second, other):
        run("--set", "lang", "--default", "English", "-i", path, "--log", log)
    run("--undo", log, "--file", first)
    assert mkv_layout(first)[0] == "a:ita*" and mkv_layout(second)[1] == "a:eng*"
    run("--undo", log, "--dir", folder)
    assert mkv_layout(second)[0] == "a:ita*" and mkv_layout(other)[1] == "a:eng*"
    run("--undo", log)
    assert mkv_layout(other)[0] == "a:ita*"


def test_undo_json_and_a_missing_file(run: Callable[..., Ran], film_mkv: Path,
                                      tmp_path: Path) -> None:
    log = tmp_path / "c.jsonl"
    run("--set", "lang", "--default", "English", "-i", film_mkv, "--log", log)
    document = json.loads(run("--undo", log, "-F", "json").out)
    (result,) = document["results"]
    assert result["action"] == "undo" and result["status"] == "changed"
    film_mkv.unlink()
    assert "no longer exists" in run("--undo", log).err


# Several files at once ------------------------------------------------------------

def test_jobs_on_real_files_match_a_sequential_run(run: Callable[..., Ran], tmp_path: Path,
                                                   make: Callable[..., Path]) -> None:
    sequential, parallel = tmp_path / "seq", tmp_path / "par"
    for folder in (sequential, parallel):
        for number in range(6):
            make(f"e{number}.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    one = run("--set", "lang", "--default", "English", "--move", "-d", sequential)
    four = run("--set", "lang", "--default", "English", "--move", "-d", parallel, "-j", "4")
    assert one.code == four.code == 0
    assert one.out.replace(str(sequential), "X") == four.out.replace(str(parallel), "X")
    for number in range(6):
        assert mkv_layout(sequential / f"e{number}.mkv") == mkv_layout(
            parallel / f"e{number}.mkv") == ["a:eng*", "a:ita"]
    assert leftovers(parallel) == []


def test_jobs_with_backups_and_a_log(run: Callable[..., Ran], tmp_path: Path,
                                     make: Callable[..., Path]) -> None:
    folder, log = tmp_path / "d", tmp_path / "c.jsonl"
    for number in range(5):
        make(f"e{number}.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    run("--set", "lang", "--default", "English", "--move", "-d", folder, "-j", "3",
        "--backup", "--log", log)
    assert len(list(folder.glob("*.fml-bak"))) == 5 and len(log.read_text().splitlines()) == 5
    run("--undo", log)
    assert all(mkv_layout(folder / f"e{n}.mkv") == ["a:ita*", "a:eng"] for n in range(5))


# Symbolic links -------------------------------------------------------------

@pytest.mark.skipif(not MAU_BIN, reason="the Rust version follows a symbolic link, fml.py does not")
@pytest.mark.skipif(sys.platform == "win32", reason="Windows needs extra rights to make a link")
def test_a_symbolic_link_changes_the_file_it_points_to(run: Callable[..., Ran], film_mkv: Path,
                                                       tmp_path: Path) -> None:
    link = tmp_path / "link.mkv"
    link.symlink_to(film_mkv)
    ran = run("--set", "lang", "--default", "English", "--move", "--file", link)
    assert ran.code == 0, ran.err
    assert link.is_symlink(), "the link is still a link"
    assert mkv_layout(film_mkv)[0] == "a:eng*", "the real file is the one that changed"
    assert not leftovers(film_mkv)


@pytest.mark.skipif(not MAU_BIN, reason="the Rust version follows a linked folder, fml.py may not")
@pytest.mark.skipif(sys.platform == "win32", reason="Windows needs extra rights to make a link")
def test_a_linked_folder_is_searched_and_its_files_changed(run: Callable[..., Ran], film_mkv: Path,
                                                           tmp_path: Path) -> None:
    store = tmp_path / "store" / "Season 1"
    store.mkdir(parents=True)
    film = store / "film.mkv"
    film.write_bytes(film_mkv.read_bytes())
    library = tmp_path / "library"
    library.mkdir()
    (library / "Season 1").symlink_to(store, target_is_directory=True)
    ran = run("--set", "lang", "--default", "English", "--move", "--dir", library, "--recursive")
    assert ran.code == 0, ran.err
    assert "no media files" not in ran.err
    assert mkv_layout(film)[0] == "a:eng*", "the file in the linked folder is the one changed"


@pytest.mark.skipif(not MAU_BIN, reason="the Rust version warns about an empty folder")
def test_an_empty_folder_says_so(run: Callable[..., Ran], tmp_path: Path) -> None:
    (tmp_path / "empty").mkdir()
    ran = run("--set", "lang", "--default", "English", "--dir", tmp_path / "empty", "--recursive")
    assert ran.code == 0
    assert "no media files found" in ran.err


# Dry runs, JSON, backups and the log, on real files (these were stub tests) ----------

def test_a_dry_run_shows_each_change_and_changes_nothing(run: Callable[..., Ran],
                                                         film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    ran = run("--set", "lang", "--default", "English", "-i", film_mkv, "-T")
    assert ran.code == 0 and ran.out.splitlines() == [
        f"[dry-run] {film_mkv}: audio track 1 (ita): default yes -> no",
        f"[dry-run] {film_mkv}: audio track 2 (eng): default no -> yes"]
    assert film_mkv.read_bytes() == before and leftovers(film_mkv.parent) == []


def test_a_dry_run_json_records_the_plan(run: Callable[..., Ran], film_mkv: Path) -> None:
    before = film_mkv.read_bytes()
    document = json.loads(run("--set", "lang", "--default", "English", "-i", film_mkv,
                              "-T", "-F", "json").out)
    assert document["dry_run"] is True and document["version"] == fml.__version__
    (result,) = document["results"]
    assert result["status"] == "changed" and result["dry_run"] is True
    assert result["changes"] == ["audio track 1 (ita): default yes -> no",
                                 "audio track 2 (eng): default no -> yes"]
    assert film_mkv.read_bytes() == before


def test_json_keeps_stdout_to_the_document(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "Spanish", "-i", film_mkv, "-F", "json", "-v")
    assert ran.code == 0
    (result,) = json.loads(ran.out)["results"]  # the only thing on stdout
    assert result["status"] == "skipped"
    assert result["message"] == 'has no audio track with language "spa"'


def test_the_json_names_the_backup_only_when_one_was_made(run: Callable[..., Ran],
                                                          film_mkv: Path) -> None:
    result = json.loads(run("--set", "lang", "--default", "English", "-i", film_mkv,
                            "--backup", "-F", "json").out)["results"][0]
    assert result["backup"] == str(film_mkv) + fml.BACKUP_MARK
    plain = json.loads(run("--set", "lang", "--default", "Spanish", "-i", film_mkv,
                           "-F", "json").out)["results"][0]
    assert plain["backup"] is None


def test_a_backup_dry_run_says_what_it_would_keep(run: Callable[..., Ran],
                                                  film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "-i", film_mkv, "--backup", "-T")
    assert ran.out.splitlines()[-1] == (
        f"[dry-run] {film_mkv}: the original would be kept as {film_mkv}{fml.BACKUP_MARK}")
    assert not Path(str(film_mkv) + fml.BACKUP_MARK).exists()


def test_no_backup_is_made_when_nothing_changes(run: Callable[..., Ran], film_mkv: Path) -> None:
    ran = run("--set", "lang", "--default", "Italian", "-i", film_mkv, "--backup")
    assert "is already the default" in ran.out
    assert not Path(str(film_mkv) + fml.BACKUP_MARK).exists()


def test_the_log_describes_the_change(run: Callable[..., Ran], film_mkv: Path,
                                      tmp_path: Path) -> None:
    log = tmp_path / "changes.jsonl"
    run("--set", "lang", "--default", "English", "-i", film_mkv, "--log", log)
    (line,) = log.read_text().splitlines()
    entry = json.loads(line)
    assert entry["file"] == str(film_mkv.resolve()) and entry["action"] == "set"
    assert entry["kind"] == "audio" and entry["version"] == fml.__version__
    assert entry["changes"] == ["audio track 1 (ita): default yes -> no",
                                "audio track 2 (eng): default no -> yes"]
    assert [t["default"] for t in entry["before"] if t["type"] == "audio"] == [True, False, False]
    assert [t["default"] for t in entry["after"] if t["type"] == "audio"] == [False, True, False]
    assert entry["backup"] is None and entry["moved_from"] is None and "time" in entry


def test_the_log_is_added_to_not_replaced(run: Callable[..., Ran], film_mkv: Path,
                                          tmp_path: Path) -> None:
    log = tmp_path / "changes.jsonl"
    log.write_text('{"earlier": "entry"}\n')
    run("--set", "lang", "--default", "English", "-i", film_mkv, "--log", log)
    lines = log.read_text().splitlines()
    assert lines[0] == '{"earlier": "entry"}' and len(lines) == 2


def test_a_log_that_cannot_be_written_is_a_warning(run: Callable[..., Ran], film_mkv: Path,
                                                   tmp_path: Path) -> None:
    ran = run("--set", "lang", "--default", "English", "-i", film_mkv,
              "--log", tmp_path / "no-such-folder" / "l.jsonl")
    assert ran.code == 0 and "could not write the log" in ran.err
    assert mkv_layout(film_mkv)[1] == "a:eng*"  # the change was still made


def test_actions_run_in_the_order_given_on_a_real_file(run: Callable[..., Ran],
                                                       film_mkv: Path) -> None:
    ran = run("--get", "lang", "--info", "--get", "lang", "-i", film_mkv)
    lines = ran.out.splitlines()
    assert ran.code == 0
    assert lines[0] == "ita" and lines[1].endswith("(Matroska)") and lines[-1] == "ita"


def make_folder(make: Callable[..., Path], tmp_path: Path, count: int = 8) -> Path:
    folder = tmp_path / "jobs"
    for number in range(count):
        make(f"e{number}.mkv", [("ita", None, True), ("eng", None, False)], where=folder)
    return folder


def test_jobs_report_every_file_in_the_order_given(run: Callable[..., Ran], tmp_path: Path,
                                                   make: Callable[..., Path]) -> None:
    folder = make_folder(make, tmp_path)
    ran = run("--set", "lang", "--default", "English", "-d", folder, "-j", "4", "-F", "json")
    results = json.loads(ran.out)["results"]
    assert [Path(r["file"]).name for r in results] == [f"e{n}.mkv" for n in range(8)]
    assert all(r["status"] == "changed" for r in results)
    assert all(mkv_layout(folder / f"e{n}.mkv") == ["a:ita", "a:eng*"] for n in range(8))
    assert leftovers(folder) == []


def test_jobs_log_every_change_as_valid_json(run: Callable[..., Ran], tmp_path: Path,
                                             make: Callable[..., Path]) -> None:
    folder = make_folder(make, tmp_path)
    log = tmp_path / "jobs.jsonl"
    run("--set", "lang", "--default", "English", "-d", folder, "-j", "4", "--log", log)
    entries = [json.loads(line) for line in log.read_text().splitlines()]
    assert sorted(Path(e["file"]).name for e in entries) == [f"e{n}.mkv" for n in range(8)]


def test_one_job_for_one_file_is_run_directly(run: Callable[..., Ran], film_mkv: Path) -> None:
    assert run("--get", "lang", "-i", film_mkv, "-j", "8").out == "ita\n"


# Files that are broken or too big to finish (the native build has its own failure paths) -----

def cut_short_inside_the_tracks(film_mkv: Path, where: Path) -> Path:
    """A copy of the film cut off part way through its track list (the file is shorter than
    its Tracks element)."""
    data = film_mkv.read_bytes()
    cut = data.find(bytes([0x16, 0x54, 0xAE, 0x6B])) + 40
    path = where / "cut.mkv"
    path.write_bytes(data[:cut])
    return path


def test_a_file_cut_short_inside_its_tracks_is_left_alone(run: Callable[..., Ran],
                                                          film_mkv: Path, tmp_path: Path) -> None:
    path = cut_short_inside_the_tracks(film_mkv, tmp_path)
    before = path.read_bytes()
    ran = run("--set", "lang", "--default", "English", "-i", path)
    assert path.read_bytes() == before, "the cut-short file is not changed"
    assert leftovers(tmp_path) == [], "no temporary file is left"
    if MAU_BIN:
        assert ran.code == 1 and "cannot be read as an MKV" in ran.err, ran.err
    else:  # fml.py's warning, see rust/README.md
        assert ran.code == 0 and "has no audio track" in ran.err, ran.err


def unknown_size_cluster(data: bytes) -> bytes:
    """The same file with the size of its first cluster set to 'unknown' (all ones), as live
    muxers write it."""
    at = data.find(bytes([0x1F, 0x43, 0xB6, 0x75])) + 4
    length = 1
    while not data[at] & (0x80 >> (length - 1)):
        length += 1
    unknown = bytes([(0x80 >> (length - 1)) | ((1 << (8 - length)) - 1)]) + b"\xff" * (length - 1)
    return data[:at] + unknown + data[at + length:]


@pytest.mark.skipif(not NATIVE_MKV,
                    reason="the native build reads a cluster of unknown size; the default build "
                           "refuses it")
def test_the_native_build_changes_a_file_with_a_cluster_of_unknown_size(run: Callable[..., Ran],
                                                                      film_mkv: Path,
                                                                      tmp_path: Path) -> None:
    path = tmp_path / "live.mkv"
    path.write_bytes(unknown_size_cluster(film_mkv.read_bytes()))
    ran = run("--set", "lang", "--default", "English", "-i", path)
    assert ran.code == 0, ran.err
    assert mkv_layout(path)[:2] == ["a:ita", "a:eng*"]
    assert leftovers(tmp_path) == []


# Failures of the files themselves, on real files (both builds) -------------------------------

@pytest.mark.parametrize("jobs", ["1", "3"])
def test_a_bad_file_in_a_batch_does_not_stop_the_others(run: Callable[..., Ran], film_mkv: Path,
                                                        tmp_path: Path, jobs: str) -> None:
    folder = tmp_path / "batch"
    folder.mkdir()
    (folder / "a-good.mkv").write_bytes(film_mkv.read_bytes())
    cut_short_inside_the_tracks(film_mkv, folder).rename(folder / "b-cut.mkv")
    ran = run("--set", "lang", "--default", "English", "-d", folder, "-j", jobs, "-F", "json")
    results = json.loads(ran.out)["results"]
    names = [Path(r["file"]).name for r in results]
    assert names == ["a-good.mkv", "b-cut.mkv"], "every file is reported, in order"
    by_name = {Path(r["file"]).name: r for r in results}
    if MAU_BIN:
        assert by_name["b-cut.mkv"]["status"] == "error"
        assert "cannot be read as an MKV" in by_name["b-cut.mkv"]["message"]
        assert ran.code == 1
    else:
        # fml.py still reports the cut-short file as having no track (see rust/README.md)
        assert by_name["b-cut.mkv"]["status"] == "skipped"
        assert ran.code == 0
    assert by_name["a-good.mkv"]["status"] == "changed"
    assert mkv_layout(folder / "a-good.mkv")[:2] == ["a:ita", "a:eng*"]
    assert leftovers(folder) == []


@pytest.mark.skipif(sys.platform == "win32" or (hasattr(os, "geteuid") and os.geteuid() == 0),
                    reason="a folder's permissions do not stop a change on Windows, or for root")
def test_a_folder_that_cannot_be_written_leaves_the_file_alone(run: Callable[..., Ran],
                                                              film_mkv: Path,
                                                               tmp_path: Path) -> None:
    folder = tmp_path / "locked"
    folder.mkdir()
    path = folder / "film.mkv"
    path.write_bytes(film_mkv.read_bytes())
    before = path.read_bytes()
    os.chmod(folder, 0o555)  # the copy cannot be made beside the file
    try:
        ran = run("--set", "lang", "--default", "English", "-i", path)
    finally:
        os.chmod(folder, 0o755)
    assert ran.code == 1, ran.err
    assert path.read_bytes() == before, "the file is not changed"
    assert leftovers(folder) == []
