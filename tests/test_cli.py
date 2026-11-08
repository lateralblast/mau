"""Command line tests: parsing, argument rules and messages. No media tools are needed."""

from __future__ import annotations

from pathlib import Path
from typing import Callable

import pytest
from conftest import MAU_BIN, NO_MKVTOOLNIX, Ran, Stubs

import fml


def test_no_arguments_prints_the_help(run: Callable[..., Ran]) -> None:
    ran = run()
    assert ran.code == 0
    assert ran.out.startswith(("usage: fml", "usage: mau"))
    assert "--set" in ran.out and "--move" in ran.out


@pytest.mark.parametrize("flag", ["--help", "-h"])
def test_help(run: Callable[..., Ran], flag: str) -> None:
    ran = run(flag)
    assert ran.code == 0 and "examples:" in ran.out


@pytest.mark.parametrize("flag", ["--version", "-V"])
def test_version(run: Callable[..., Ran], flag: str) -> None:
    ran = run(flag)
    # the Rust command is called mau-cli, the archived Python script fml
    program = "mau-cli" if MAU_BIN else "fml"
    assert ran.code == 0 and ran.out.strip() == f"{program} {fml.__version__}"


def test_usage_errors_exit_2_and_print_nothing_on_stdout(run: Callable[..., Ran]) -> None:
    ran = run("--bogus")
    assert ran.code == 2 and ran.out == ""
    assert "unrecognized arguments: --bogus" in ran.err


# Each rule check_arguments enforces: arguments and the message that must appear
BAD_ARGUMENTS = [
    (["--info"], "no file given"),
    (["--set", "lang", "--default", "English"], "no file given"),
    (["-i", "x.mkv"], "no action given"),
    (["--set", "lang", "-i", "x.mkv"], "--set lang needs one of --default"),
    (["--set", "lang", "--default", "English", "--lang", "it", "-i", "x.mkv"],
     "not --default and --lang"),
    (["--set", "lang", "--default", "English", "--prefer", "en", "-i", "x.mkv"],
     "not --default and --prefer"),
    (["--set", "default", "-i", "x.mkv"], "--set default needs --track"),
    (["--set", "default", "--track", "2", "--default", "English", "-i", "x.mkv"],
     "not --default or --prefer"),
    (["--set", "default", "--track", "2", "--prefer", "en", "-i", "x.mkv"],
     "not --default or --prefer"),
    (["--delete", "lang", "-i", "x.mkv"], "--delete lang needs --lang"),
    (["--delete", "track", "-i", "x.mkv"], "--delete track needs --track"),
    (["--preserve", "lang", "-i", "x.mkv"], "--preserve lang needs --lang or --prefer"),
    (["--delete", "lang", "--lang", "eng", "--prefer", "en", "-i", "x.mkv"],
     "--prefer only works with --set lang and --preserve"),
    (["--get", "lang", "--prefer", "en", "-i", "x.mkv"],
     "--prefer only works with --set lang and --preserve"),
    (["--set", "lang", "--default", "English", "--forced", "-i", "x.mkv"],
     "--forced only applies to subtitles"),
    (["--set", "lang", "--default", "none", "-i", "x.mkv"], "--default none only works"),
    (["--set", "lang", "--default", "none", "--kind", "sub", "--prefer", "en", "-i", "x.mkv"],
     "not --default and --prefer"),
    (["--get", "lang", "--default", "none", "--kind", "sub", "-i", "x.mkv"],
     "--default none only works"),
    (["--swap", "lang", "--kind", "sub", "-i", "x.mkv"], "--swap only works on audio"),
    (["--set", "lang", "--default", "none", "--kind", "sub", "--move", "-i", "x.mkv"],
     "--move can't be used with --default none"),
    (["--swap", "lang", "--move", "-i", "x.mkv"], "--move only works with --set"),
    (["--get", "lang", "--move", "-i", "x.mkv"], "--move only works with --set"),
    (["--delete", "lang", "--lang", "eng", "--move", "-i", "x.mkv"], "--move only works with"),
    (["--set", "lang", "--lang", "English", "--move", "-i", "x.mkv"], "--move only works with"),
    (["--info", "-i", "x.mkv", "-d", "."], "not allowed with argument"),
    (["--set", "name", "-i", "x.mkv"], "--set name needs --track"),
    (["--set", "name", "--track", "1", "-i", "x.mkv"], "--set name needs --name"),
    (["--set", "name", "--track", "1", "--name", "x", "--default", "en", "-i", "x.mkv"],
     "not --default, --prefer, --move or --forced"),
    (["--set", "name", "--track", "1", "--name", "x", "--move", "-i", "x.mkv"],
     "--move only works with --set lang --default"),
    (["--set", "forced", "-i", "x.mkv"], "--set forced needs --track"),
    (["--set", "unforced", "-i", "x.mkv"], "--set unforced needs --track"),
    (["--set", "forced", "--track", "1", "--forced", "--kind", "sub", "-i", "x.mkv"],
     "not --default, --prefer, --move or --forced"),
    (["--info", "--name", "x", "-i", "x.mkv"], "--name only works with --set name"),
    (["--set", "lang", "--default", "en", "--name", "x", "-i", "x.mkv"],
     "--name only works with --set name"),
    (["--info", "-i", "x.mkv", "--include", "*.mkv"],
     "--include and --exclude only work with --dir"),
    (["--info", "-i", "x.mkv", "--exclude", "Extras"],
     "--include and --exclude only work with --dir"),
    (["--undo", "no-such-log.jsonl"], "is not a file"),
]


@pytest.mark.parametrize("arguments, message", BAD_ARGUMENTS,
                         ids=[" ".join(arguments) for arguments, _ in BAD_ARGUMENTS])
def test_bad_arguments(run: Callable[..., Ran], arguments: list[str], message: str) -> None:
    ran = run(*arguments)
    assert ran.code == 2, ran.err
    assert message in ran.err
    assert ran.out == ""


@pytest.mark.parametrize("arguments, message", [
    (["--set", "lang", "--default", "Klingonese", "-i", "x"], "unknown language"),
    (["--set", "lang", "--lang", "xx", "-i", "x"], "unknown language"),
    (["--set", "lang", "--prefer", "eng,Klingonese", "-i", "x"], "unknown language"),
    (["--set", "lang", "--prefer", ",", "-i", "x"], "give at least one language"),
    (["--set", "lang", "--lang", "it", "--track", "x", "-i", "x"], "not a track number"),
    (["--set", "lang", "--lang", "it", "--track", "0", "-i", "x"], "not a track number"),
    (["--set", "lang", "--lang", "it", "--track", "-1", "-i", "x"], "not a track number"),
    (["--info", "--kind", "video", "-i", "x"], "is not audio or sub"),
    (["--info", "--format", "xml", "-i", "x"], "invalid choice"),
    (["--info", "-d", "x", "--jobs", "0"], "is not a number of jobs"),
    (["--info", "-d", "x", "-j", "many"], "is not a number of jobs"),
    (["--get", "frob", "-i", "x"], "invalid choice"),
    (["--delete", "frob", "-i", "x"], "invalid choice"),
    (["--set", "frob", "-i", "x"], "invalid choice"),
])
def test_bad_values(run: Callable[..., Ran], arguments: list[str], message: str) -> None:
    ran = run(*arguments)
    assert ran.code == 2 and message in ran.err


def test_values_are_accepted_in_any_form(run: Callable[..., Ran], tmp_path: Path) -> None:
    """Names, codes and spellings all parse; the file is missing so the run stops there."""
    missing = tmp_path / "missing.mkv"
    for args in (["--set", "lang", "--default", "English"], ["-c", "lang", "-D", "en"],
                 ["--set", "lang", "--default", "fra"], ["--set", "lang", "--prefer", "en,ja,FR"],
                 ["--set", "default", "--track", "last", "--lang", "German"],
                 ["--set", "lang", "--default", "none", "--kind", "subs"],
                 ["--set", "lang", "--default", "NONE", "--kind", "subtitle"],
                 ["--set", "lang", "--default", "en", "--kind", "subtitles", "--forced"],
                 ["--set", "lang", "--default", "en", "--move"]):
        ran = run(*args, "-i", missing)
        assert ran.code == 1, (args, ran.err)  # got as far as looking for the file
        assert "does not exist" in ran.err


def test_long_options_can_be_abbreviated(run: Callable[..., Ran], tmp_path: Path) -> None:
    ran = run("--info", "--fil", tmp_path / "missing.mkv")
    assert ran.code == 1 and "does not exist" in ran.err


def test_check_needs_no_file(run: Callable[..., Ran], stubs: Stubs) -> None:
    ran = run("--check")
    assert ran.code == 0
    # the required commands (whisper-cli and the model are optional, and are listed too): four,
    # or two without mkvtoolnix
    required = [line for line in ran.out.splitlines()
                if line.startswith("Found:")
                and "whisper" not in line and "MAU_WHISPER" not in line]
    assert len(required) == (2 if NO_MKVTOOLNIX else 4)


def test_the_new_options_parse(run: Callable[..., Ran], tmp_path: Path) -> None:
    """Each gets as far as looking for the file, which is missing."""
    missing = tmp_path / "missing.mkv"
    for args in (["--set", "name", "--track", "2", "--name", "English"],
                 ["--set", "name", "--track", "2", "--name", ""],
                 ["--set", "forced", "--track", "1", "--kind", "sub"],
                 ["--set", "unforced", "--track", "last", "--kind", "sub"],
                 ["--set", "lang", "--default", "en", "--backup"],
                 ["--set", "lang", "--default", "en", "--log", str(tmp_path / "l.jsonl")],
                 ["--set", "lang", "--default", "en", "--backup", "--force"]):
        ran = run(*args, "-i", missing)
        assert ran.code == 1 and "does not exist" in ran.err, (args, ran.err)


def test_directory_options_parse(run: Callable[..., Ran], tmp_path: Path) -> None:
    ran = run("--info", "-d", tmp_path, "--include", "*.mkv", "--include", "*.mp4",
              "--exclude", "Extras", "-j", "4")
    assert ran.code == 0  # an empty directory: nothing to do


def test_undo_needs_no_file(run: Callable[..., Ran], tmp_path: Path) -> None:
    log = tmp_path / "l.jsonl"
    log.write_text("")
    assert run("--undo", log).code == 0
    assert run("-u", log, "-F", "json").out.count("results") == 1


def test_undo_ignores_bad_lines(run: Callable[..., Ran], tmp_path: Path) -> None:
    log = tmp_path / "l.jsonl"
    log.write_text('not json\n{"file": "/nowhere/x.mkv"}\n\n{"also": "wrong"}\n')
    ran = run("--undo", log)
    assert ran.code == 0
    assert "line 1 is not valid JSON" in ran.err and ran.err.count("not understood") == 2


def test_undo_cannot_be_combined_with_log(run: Callable[..., Ran], tmp_path: Path) -> None:
    log = tmp_path / "l.jsonl"
    log.write_text("")
    ran = run("--undo", log, "--log", tmp_path / "other.jsonl")
    assert ran.code == 2 and "--undo reads a log and can't be used with --log" in ran.err
