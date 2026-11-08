# mau in Rust

A Rust version of `archived/fml.py`, as a command line tool and a desktop app, both built on one library.
`archived/fml.py` is the archived reference: the Rust version is meant to behave the same, and the same test
suite is used to check it.

```text
rust/
  mau-core/   the library: languages, tracks, edits, safe rewriting, backup, log and undo
  mau-cli/    the `mau-cli` command (clap); same switches and output as fml.py
  mau-gui/    the desktop app (Tauri 2), built as the program `mau`: src/main.rs is the backend, ui/ is the page
```

`mau-core` prints nothing. A front end gives it a `Config` and a sink for `Event`s (warnings,
verbose notes, and one `Outcome` per file) and shows them its own way. `validate` holds the
rules about which switches go together, so the command line and the app enforce the same ones.

## Build and run

Needs `cargo` (1.85 or newer; 1.90 or newer for the app) and the same tools as `fml.py`: `mkvmerge`, `mkvpropedit`,
`mediainfo` and `ffmpeg`.

```sh
cd rust
cargo build --release            # the command line tool: target/release/mau-cli
target/release/mau-cli --set lang --default English --move --dryrun --dir films
cargo test                       # unit tests of the library, the command line and the app backend
```

A plain `cargo build` leaves the app out, because it needs the system webview. For the app,
on Ubuntu install `libwebkit2gtk-4.1-dev` and `libgtk-3-dev` (and `cargo install tauri-cli`
for bundling), then:

```sh
cargo build -p mau-gui --release     # target/release/mau (the app)
cd mau-gui && cargo tauri build --bundles deb    # a .deb in target/release/bundle/deb
```

`mau-cli --gui` opens the desktop app: the command finds the app, the program called `mau`, beside itself (or on `PATH`, skipping scripts,
or at the path in `MAU_GUI`; on a Mac also as `mau.app` beside it, in `~/Applications` or in `/Applications`, or only in
the folders of `MAU_APP_DIRS` if that is set) and replaces itself with it. It takes no other options. From a checkout,
`./mau` (a small script in the repository root) runs the release build, or the debug build if
there is no release one, so `./mau --gui` and `./mau --set lang ...` both work there once both
are built (`cargo build --release && cargo build -p mau-gui --release`).

## Windows

`../build.sh --target windows` cross-compiles for Windows from Linux or a Mac, in the container described by
`../docker/windows/Dockerfile` (Ubuntu, MinGW, NSIS, `x86_64-pc-windows-gnu`): the installer
(`mau_<version>_x64-setup.exe`) and `mau-cli.exe` (as `mau-cli-<version>-windows.zip`) end up in `../dist/`. The target
directory and cargo registry are Docker volumes (`mau-windows-target`, `mau-windows-cargo`), so only the first build
compiles everything.

The code that differs is small: `util.rs` (`which` through `PATHEXT`, `disk_free` through `GetDiskFreeSpaceExW`, no
signal numbers, running `.bat`/`.cmd` stand-ins by resolving the program first) and `mau-cli` (`SetConsoleCtrlHandler`
for Ctrl-C, and `--gui` starts the app with `mau.exe` instead of replacing itself). `libc` is only a dependency on
Unix. `docker/windows/smoke.sh` runs the built `mau-cli.exe` under Wine with `.bat` stand-ins for the media tools and
checks a real edit, the backup and log, and that a refused change leaves no temporary file.

Not covered: the real tools (`mkvtoolnix` and the rest for Windows), the app itself (it needs WebView2), Ctrl-C, and
running the installer. For that a Windows 11 virtual machine is the next step: `~/code/pampas/windows/` has a working
one (`dockurr/windows`, QEMU/KVM, a watcher that runs `build.cmd` from a shared folder) that could be adapted.

## Checking it against the Python suite

The pytest suite in `../tests` runs the command line through a `run` helper. With `MAU_BIN` set
it runs the Rust binary instead of `fml.py`, so every real-file and stub-tool test applies:

```sh
cargo build --release
MAU_BIN=$PWD/target/release/mau-cli pytest ../tests        # from the repository root
```

Tests that call `fml.py`'s own functions or patch Python internals are skipped then (the whole
of `test_unit.py` and about 16 in `test_stubs.py`); the Rust unit tests in `mau-core` cover the
pure parts instead (`engine_tests.rs`). `tests/test_signals.py` starts a real process, so it
checks Ctrl-C and `kill` handling of either version.

What the suite does **not** cover for the Rust build, because those tests patch Python: the
free-space check, `--install`, and `--check` with missing tools. They are the same logic, but
check them by hand before relying on them.

## Differences from `fml.py`

- `--help` and the wording of option errors come from `clap`, not `argparse`. Errors that the
  tests look for (`unrecognized arguments`, `invalid choice`, `not allowed with argument`) are
  worded the same way; the help text layout is different.
- A track of a kind other than video, audio or subtitles (such as DVD buttons) is ignored
  instead of being listed, so `--track-order` does not mention it.
- The log is written as compact JSON lines; `fml.py` writes `", "` and `": "` separators. Each
  version reads the other's logs.
- A symbolic link to a media file is followed: the file it points to is the one changed, and the output
  names that file. `fml.py` replaces the link with the changed copy.
- A symbolic link to a folder is followed by `--recursive`, and a folder reached twice is read once. The
  archived `fml.py` uses `pathlib`'s `rglob`, which may not follow links, depending on the Python version.
- With no media files, `--dir` says so on stderr. `fml.py` says nothing.
- The forced flag of an MP4 subtitle track is a `kind` box in the track's `udta` (the role
  `forced-subtitle`, which ffmpeg writes and `mediainfo` reports as `Forced: Yes / No`). `fml.py` refuses
  to set it for an MP4; the Rust build sets and clears it in place (`native-mp4`), or with ffmpeg when the
  file is rewritten anyway.
- A file cut short inside its track list is an error ("cannot be read as an MKV"). `fml.py` says
  the file "has no audio track with language ...", which is not the real cause, and reports it as
  skipped (exit 0) rather than as an error (exit 1).
- `--info` shows codec names as ffprobe gives them (`aac`, `mpeg4`, `subrip`); `fml.py` shows mkvmerge's
  (`AAC`, `MPEG-4p2`, `SubRip/SRT`) for an MKV. The Rust names are kept: they are the ones used elsewhere
  (for MP4 and AVI, and for JSON). `fml.py` is archived and keeps its output.
- In a build with `native-mp4`, an MP4's languages, default flags and track order are changed in place (the
  `trak` boxes are moved within `moov`, and nothing else moves). The file is not rewritten, so the index is not
  moved to the front of the file as ffmpeg's rewrite moves it: playback is the same, but a file that streams
  from a web server may start more slowly. Names and the forced flag of a subtitle are changed the same way,
  by rebuilding `moov`. Removing tracks rewrites the file in this program too: the removed tracks' chunks are
  cut out of `mdat` and the chunk offsets of the others are moved (`mp4::remove_traks`), so the result has no
  index at the front either, and it needs no ffmpeg. A track is matched to its `trak` box by its track ID,
  not by its place in mediainfo's list, which leaves out tracks such as a timecode track. A kept track that
  refers to a removed one through `tref` is refused. Adding a SubRip file (`--add subtitle`) is native too: a
  `tx3g` text track (`mov_text`, as ffmpeg writes it) is appended to `moov`, and its samples go in an `mdat` at the
  end of the file, so the media data does not move. The track is in alternate group 3, as ffmpeg's subtitle tracks
  are (mediainfo reports the default flag only for a track in that group), and has the one-segment edit list
  ffmpeg writes (the readers checked do not need it; it is kept to match ffmpeg's layout). A SubRip, ASS, SSA or
  WebVTT file is read in this program. Its styling is not kept (an MP4 subtitle here is plain text): override
  blocks, tags and positions are dropped, and ffmpeg keeps them as `<font>` and `<b>` tags. Cues that overlap, a cue
  too long for a sample, and a file too big for 32-bit offsets are converted by ffmpeg as before.
- In a build with `native-mp4`, a text subtitle of an MP4 (`--extract subtitle`) is read in this program, and written
  as SubRip with no byte order mark, the way ffmpeg writes it. The track's sample entry says which format it is:
  `mov_text` (`tx3g`), WebVTT (`wvtt`) or TTML (`stpp`). The `mov_text` output matches ffmpeg's bytes on the files
  checked, including accented text and millisecond times. The WebVTT and TTML readers follow the MP4 specification
  and are checked against files made for the tests, since ffmpeg in the test environment can't read those two
  tracks. CEA-608 captions (`c608`) need a caption decoder and are refused with a message.
- In a build with `native-avi`, the audio streams of an AVI are put in order in place (`--swap`): the stream
  lists are reordered and the stream numbers in the chunks and the index are renamed, and nothing changes
  size. An AVI with an OpenDML index (`indx`) is refused, where the ffmpeg path would swap it. Removing a
  stream (`--delete track`) is native too: the file is written again, with the stream's chunks and index entries
  left out, the stream numbers after it moved down, each index offset moved to where its chunk now is, and the
  header's stream count changed. It is read back before it replaces the file. The same OpenDML refusal applies.
  The streams come from the header's `strl` lists, not from `mediainfo`: with `native-avi` the track list that
  `--info`, the Edit dialog and the removal use is read from the same lists (`avi::stream_tracks`), so an AVI whose
  chunks are in `rec ` lists lists its real streams. `mediainfo` is not needed for an AVI's info, get, delete or
  preserve in that build. Codec names follow mediainfo's for the common codecs.
- In a build with `native-detect` on macOS, the clips of an MKV for `--detect` are decoded with Apple's
  AudioToolbox, not ffmpeg: the sound is close to ffmpeg's (correlation 0.95 to 0.99 on the files checked), and
  the languages found were the same. The clip length of a file with no `Duration` in its header is one clip.
- In the build without mkvtoolnix (`--features no-mkvtoolnix`), a global `TITLE` tag is written
  by ffmpeg as the file's Title element instead, so its value survives but is not a tag. ffmpeg's
  Matroska muxer cannot write a global tag. Attachments and chapters are kept. Accepted for now
  (see `TODO.md`).
- The project was renamed from `fml` to `mau` in 1.5.7, so the engine type that mirrors `fml.py`'s
  `Fml` class is `Mau`, and the command is `mau-cli`, not `fml.py`. The `.fml-tmp` and `.fml-bak`
  suffixes are the same in both.

## The app

Add a folder (or files), pick a change, **Preview** it, then **Apply**. A preview is a dry run;
applying asks first, and the log (on by default, `~/mau-log.jsonl`) lets **Undo from a log** put
changes back, from the kept original when `Keep the original` was ticked. The page is plain
HTML, CSS and JavaScript in `mau-gui/ui` (no build step); the Rust side exposes `scan`, `run`,
`cancel`, `app_info`, `languages` and `file_details` (the size, date and `mediainfo` report for the Edit dialog), `rename_file` and `classify_dropped` (what was dropped on the window: a folder, or a list of media files).

In a debug build, `MAU_GUI_SCRIPT=script.js MAU_GUI_REPORT=out.txt target/debug/mau` runs
the script in the window after it loads, and the script can send text back with
`invoke('dev_report', {text})`. That is how the app was driven and checked without a person
clicking (`xvfb-run` gives it a screen on a machine with no display). `MAU_GUI_THEME=dark` (or `light`)
shows the window in that theme whatever the system uses, which is how the page was checked in both, and
`MAU_GUI_SIZE=1120x1000` gives it a size, which is how the screenshots in `docs/images/` were taken. Both are
compiled out of release builds.
