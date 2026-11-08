"""--detect: guessing the language of audio tracks with whisper.cpp (Rust only).

Real files are cut with the real ffmpeg; whisper.cpp is a stand-in script that prints the report
it would print (`auto-detected language: it (p = 0.95)`), so the tests check what mau does with
the report, not whether the guess is right. Accuracy needs a real model and is checked by hand.
"""

import functools
import json
import shutil
import stat
import threading
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Callable

import pytest
from conftest import MAU_BIN, Ran, leftovers, mkv_layout, needs_ffmpeg, requires_tools

pytestmark = [pytest.mark.skipif(not MAU_BIN, reason="--detect is Rust only"), requires_tools]

# The stand-in whisper-cli: it logs its arguments, and prints the report or fails.
STUB_WHISPER = """#!/bin/sh
echo "$@" >> "$STUB_LOG"
if [ "$STUB_FAIL" = 1 ]; then
  echo "error: failed to load model" >&2
  exit 1
fi
echo "whisper_full_with_state: auto-detected language: ${STUB_LANG:-it} (p = ${STUB_P:-0.95})" >&2
"""


@pytest.fixture
def whisper(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Point mau at the stand-in whisper-cli and a model, and return the log of its calls."""
    program = tmp_path / "whisper-cli"
    program.write_text(STUB_WHISPER)
    program.chmod(program.stat().st_mode | stat.S_IXUSR)
    log = tmp_path / "whisper.log"
    model = tmp_path / "ggml-base.bin"
    model.write_bytes(b"model")  # --detect needs the model file to be there
    monkeypatch.setenv("MAU_WHISPER", str(program))
    monkeypatch.setenv("MAU_WHISPER_MODEL", str(model))
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path / "data"))  # not the real data folder
    monkeypatch.setenv("STUB_LOG", str(log))
    monkeypatch.delenv("STUB_FAIL", raising=False)
    monkeypatch.delenv("STUB_LANG", raising=False)
    return log


def calls(log: Path) -> list[list[str]]:
    return [line.split() for line in log.read_text().splitlines()] if log.exists() else []


def test_detect_names_each_audio_track_and_changes_nothing(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path, tmp_path: Path
) -> None:
    two = make("two.mkv", [("und", None, False), ("und", None, False)])
    before = two.read_bytes()
    ran = run("--detect", "-i", two)
    assert ran.code == 0, ran.err
    assert f"{two}: track 1 sounds like ita" in ran.out
    assert f"{two}: track 2 sounds like ita" in ran.out
    assert "labelled und" in ran.out and "95% sure" in ran.out
    assert two.read_bytes() == before
    assert leftovers(tmp_path) == []
    # one clip per track for a short file, and the clips are gone afterwards
    clips = [args[args.index("-f") + 1] for args in calls(whisper)]
    assert len(clips) == 2 and not any(Path(clip).exists() for clip in clips)


def test_detect_with_track_listens_to_that_track_only(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path
) -> None:
    two = make("two.mkv", [("und", None, False), ("und", None, False)])
    ran = run("--detect", "--track", "2", "-i", two)
    assert ran.code == 0, ran.err
    assert "track 2 sounds like ita" in ran.out and "track 1" not in ran.out
    assert len(calls(whisper)) == 1


def test_detect_reports_the_language_as_json(
    run: Callable[..., Ran],
    make: Callable[..., Path],
    whisper: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("STUB_LANG", "en")
    monkeypatch.setenv("STUB_P", "0.5")
    two = make("two.mkv", [("und", None, False)])
    ran = run("--detect", "--format", "json", "-i", two)
    assert ran.code == 0, ran.err
    assert '"detected"' in ran.out and '"language": "eng"' in ran.out
    assert json.loads(ran.out)  # one document


def test_detect_on_an_mp4_is_refused_when_ffmpeg_is_hidden(
        run: Callable[..., Ran], film_mp4: Path, whisper: Path,
        monkeypatch: pytest.MonkeyPatch) -> None:
    """Without ffmpeg (the --nolgpl build, here MAU_SPIKE_NO_FFMPEG) an MP4's sound can't be cut:
    the detection says why, and the file is left as it was."""
    monkeypatch.setenv("MAU_SPIKE_NO_FFMPEG", "1")
    before = film_mp4.read_bytes()
    ran = run("--detect", "--track", "1", "-i", film_mp4)
    assert ran.code == 1
    assert "ffmpeg is not included in this build (--nolgpl)" in ran.err
    assert film_mp4.read_bytes() == before


@needs_ffmpeg
def test_detect_on_an_mp4(run: Callable[..., Ran], film_mp4: Path, whisper: Path) -> None:
    ran = run("--detect", "--track", "1", "-i", film_mp4)
    assert ran.code == 0, ran.err
    assert "track 1 sounds like ita" in ran.out


def test_detect_needs_a_model(
    run: Callable[..., Ran],
    make: Callable[..., Path],
    whisper: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv(
        "MAU_WHISPER_MODEL"
    )  # and the default model is not in the (empty) data folder
    two = make("two.mkv", [("und", None, False)])
    before = two.read_bytes()
    ran = run("--detect", "-i", two)
    assert ran.code == 1
    assert "MAU_WHISPER_MODEL" in ran.err
    assert two.read_bytes() == before and calls(whisper) == []


def test_detect_needs_the_program_to_run(
    run: Callable[..., Ran],
    make: Callable[..., Path],
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    model = tmp_path / "ggml-base.bin"
    model.write_bytes(b"model")
    monkeypatch.setenv("MAU_WHISPER", str(tmp_path / "no-such-whisper"))
    monkeypatch.setenv("MAU_WHISPER_MODEL", str(model))
    two = make("two.mkv", [("und", None, False)])
    ran = run("--detect", "-i", two)
    assert ran.code == 1
    assert "could not be run" in ran.err


def test_a_report_that_is_not_there_is_an_error_and_changes_nothing(
    run: Callable[..., Ran],
    make: Callable[..., Path],
    whisper: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("STUB_FAIL", "1")
    two = make("two.mkv", [("und", None, False)])
    before = two.read_bytes()
    ran = run("--detect", "-i", two)
    assert ran.code == 1
    assert "gave no language for track 1" in ran.err and "failed to load model" in ran.err
    assert two.read_bytes() == before


def test_detect_is_refused_for_subtitles(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path
) -> None:
    two = make("two.mkv", [("und", None, False)])
    ran = run("--detect", "--kind", "sub", "-i", two)
    assert ran.code != 0
    assert "work on audio tracks" in ran.err
    assert calls(whisper) == []


def check_lines(run: Callable[..., Ran]) -> list[str]:
    lines = run("--check").out.splitlines()
    return [line for line in lines if "whisper" in line or "MAU_WHISPER" in line]


def test_check_reports_whisper_as_found_when_it_is_there(
    run: Callable[..., Ran], whisper: Path
) -> None:
    lines = check_lines(run)
    assert any(line.startswith("Found:\t\twhisper-cli (") for line in lines), lines
    assert any(line.startswith("Found:\t\tMAU_WHISPER_MODEL (") for line in lines), lines


def test_check_reports_whisper_as_optional_when_it_is_missing(
    run: Callable[..., Ran], monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("MAU_WHISPER", str(tmp_path / "no-such-whisper"))
    ran = run("--check")
    assert ran.code == 0, ran.err  # optional: not an error
    assert "Missing:\twhisper-cli (optional: needed by --detect)" in ran.out
    entry = next(
        e
        for e in json.loads(run("--check", "--format", "json").out)["results"][0]["data"]
        if e["tool"] == "whisper-cli"
    )
    assert entry["path"] is None and entry["optional"] is True and entry["package"] == "whisper-cpp"


def test_check_finds_the_model_file(
    run: Callable[..., Ran], whisper: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    model = tmp_path / "ggml-base.bin"
    model.write_bytes(b"model")
    monkeypatch.setenv("MAU_WHISPER_MODEL", str(model))
    assert f"Found:\t\tMAU_WHISPER_MODEL ({model})" in run("--check").out


def test_check_install_would_install_whisper(
    run: Callable[..., Ran], monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # a dry run only: nothing is installed. Where there is no package, it says so instead.
    monkeypatch.setenv("MAU_WHISPER", str(tmp_path / "no-such-whisper"))
    ran = run("--check", "--install", "--dryrun", "--verbose")
    assert ran.code == 0, ran.err
    assert "whisper" in ran.out + ran.err


def test_check_install_dry_run_says_it_would_download_the_model_and_does_not(
    run: Callable[..., Ran], monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    model = tmp_path / "new-folder" / "ggml-base.bin"
    monkeypatch.setenv("MAU_WHISPER", str(tmp_path / "no-such-whisper"))
    monkeypatch.setenv("MAU_WHISPER_MODEL", str(model))
    ran = run("--check", "--install", "--dryrun", "--verbose")
    assert ran.code == 0, ran.err
    assert "would download the whisper.cpp model (147 MB)" in ran.out + ran.err
    assert not model.exists() and not model.parent.exists()


@pytest.mark.skipif(not shutil.which("curl"), reason="the model download uses curl")
def test_a_download_that_is_not_the_published_model_is_thrown_away(
    run: Callable[..., Ran], whisper: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # a local server stands in for the model host and serves some other file
    served = tmp_path / "served"
    served.mkdir()
    (served / "ggml-base.bin").write_bytes(b"not the model")
    handler = functools.partial(SimpleHTTPRequestHandler, directory=str(served))
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        model = tmp_path / "model" / "ggml-base.bin"
        monkeypatch.setenv("MAU_WHISPER_MODEL", str(model))
        monkeypatch.setenv(
            "MAU_WHISPER_MODEL_URL", f"http://127.0.0.1:{server.server_port}/ggml-base.bin"
        )
        # the stand-in program is found, so nothing is installed: only the model is fetched
        ran = run("--check", "--install")
    finally:
        server.shutdown()
        server.server_close()
    assert ran.code == 1
    assert "SHA-256 differs" in ran.err
    assert not model.exists()
    assert not (model.parent / "ggml-base.bin.fml-tmp").exists()  # the download was removed


@pytest.fixture
def french(whisper: Path, monkeypatch: pytest.MonkeyPatch) -> None:  # after whisper's setup
    """The stand-in whisper-cli hears French (the --update tests need a language that differs)."""
    monkeypatch.setenv("STUB_LANG", "fr")


def audio_labels(path: Path) -> list[str]:
    """The language of each audio track of an MKV, without the default and forced marks."""
    return [layout.split("*")[0] for layout in mkv_layout(path) if layout.startswith("a:")]


@pytest.mark.usefixtures("french")
def test_update_sets_each_track_to_the_language_it_sounds_like(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path, tmp_path: Path
) -> None:
    two = make("two.mkv", [("und", None, False), ("und", None, False)])
    ran = run("--update", "-i", two)
    assert ran.code == 0, ran.err
    assert "audio track 1: language und -> fre" in ran.out
    assert "audio track 2: language und -> fre" in ran.out
    assert audio_labels(two) == ["a:fre", "a:fre"]
    assert leftovers(tmp_path) == []


@pytest.mark.usefixtures("french")
def test_update_dry_run_changes_nothing(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path
) -> None:
    two = make("two.mkv", [("und", None, False), ("und", None, False)])
    before = two.read_bytes()
    ran = run("--update", "--dryrun", "-i", two)
    assert ran.code == 0, ran.err
    assert "[dry-run] " in ran.out and "-> fre" in ran.out
    assert two.read_bytes() == before


@pytest.mark.usefixtures("french")
def test_update_leaves_a_track_already_labelled_alone(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path
) -> None:
    two = make("two.mkv", [("fre", None, False), ("und", None, False)])
    ran = run("--update", "-i", two)
    assert ran.code == 0, ran.err
    assert "audio track 2: language und -> fre" in ran.out
    assert "audio track 1" not in ran.out
    assert audio_labels(two) == ["a:fre", "a:fre"]


@pytest.mark.usefixtures("french")
def test_update_with_nothing_to_change_changes_nothing(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path
) -> None:
    two = make("two.mkv", [("fre", None, False), ("fre", None, False)])
    before = two.read_bytes()
    ran = run("--update", "-i", two)
    assert ran.code == 0, ran.err
    assert "every track already has the language it sounds like" in ran.out
    assert two.read_bytes() == before


@needs_ffmpeg
@pytest.mark.usefixtures("french")
def test_update_is_not_done_for_avi(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path
) -> None:
    two = make("two.avi", [("und", None, False), ("und", None, False)])
    before = two.read_bytes()
    ran = run("--update", "-i", two)
    assert ran.code == 0
    assert "AVI files have no language to set" in ran.err
    assert two.read_bytes() == before


@pytest.mark.usefixtures("french")
def test_update_is_refused_for_subtitles(
    run: Callable[..., Ran], make: Callable[..., Path], whisper: Path
) -> None:
    two = make("two.mkv", [("und", None, False)])
    ran = run("--update", "--kind", "sub", "-i", two)
    assert ran.code != 0
    assert "work on audio tracks" in ran.err
