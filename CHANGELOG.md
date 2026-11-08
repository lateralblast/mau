# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.6.8] - 2026-11-08

### Added
- `./build.sh --target mac --sandboxed --nolgpl` builds the app with no ffmpeg or ffprobe, and no mkvtoolnix, so it carries no LGPL or GPL code. Its installers are named `-nolgpl` and it is built in `rust/target-nolgpl`. It is the `no-ffmpeg` Cargo feature, which `--check` reflects (it does not ask for ffmpeg). `--nolgpl` goes with `--sandboxed` only, and not with `--with-mkvtoolnix` or `--lgpl`.
- In that build a change that needs ffmpeg is refused before the file is touched, and ffmpeg is never started: adding a subtitle to an MP4 whose cues overlap, adding a WebVTT file with a NOTE after its first cue to an MKV, and `--detect` on an MP4 or AVI. The message says `ffmpeg is not included in this build (--nolgpl)`. `README.md` lists what the build does without ffmpeg.

## [1.6.7] - 2026-11-07

### Added
- A "Speech model" card in the desktop app says whether the whisper.cpp model that Detect languages reads is there, and downloads it on request after a confirmation (about 148 MB from Hugging Face, checked against its published SHA-256 before it is kept). The download installs no packages, so the sandboxed app can do it. `--check --install` still downloads it as before.

## [1.6.6] - 2026-11-07

### Changed
- `./build.sh --target mac --sandboxed` builds the LGPL app: it carries no mkvtoolnix (which is GPL), and its ffmpeg and ffprobe are built from source as LGPL-only tools, so no GPL code is bundled. The MKV work is done with ffmpeg in that app. Its disk image is named `-sandboxed-lgpl`. `--lgpl` means the same. The first build needs Xcode's command line tools to compile ffmpeg, once.
- The README's licence section describes that build.

### Added
- `--with-mkvtoolnix` with `--sandboxed` builds the GPL app that carries mkvmerge, mkvpropedit and mkvextract (mkvtoolnix, GPL) and Homebrew's ffmpeg (GPL). Its disk image is named `-sandboxed`. It can't be combined with `--lgpl`.

## [1.6.5] - 2026-11-07

### Changed
- The Rust `--info` keeps ffprobe's codec names (`subrip`, `aac`, `mpeg4`), which is how it has always printed them. `rust/README.md` records that `fml.py` prints mkvmerge's names (`SubRip/SRT`, `AAC`, `MPEG-4p2`) instead, and that the difference is kept, since `fml.py` is archived.

## [1.6.4] - 2026-11-07

### Fixed
- The test that checks every switch is documented in the README finds the help output when its code block names a language (```` ```text ````), as the README's markdown cleanup made it. The CI `python` jobs had failed on it.

## [1.6.3] - 2026-11-06

### Changed

- The test suite recognises the build made without mkvtoolnix (`--features no-mkvtoolnix`, the App Store and LGPL build) from its `--check` output, which does not list `mkvmerge` in that build, and expects ffmpeg where that build does the MKV work. The suite passes against it (439 passed, 0 failed), where 21 tests had failed. The environment variable `MAU_SPIKE_NO_MKVTOOLNIX` still works. No change to the command.

## [1.6.2] - 2026-11-06

### Fixed

- The progress bar of a long rewrite (the app, and `--verbose` progress) no longer goes back to 0 during the rewrite. The file is emptied before it is filled again, and the bar read that empty file; it now keeps the largest size seen.

## [1.6.1] - 2026-11-06

### Added

- The sandboxed Mac app's `Contents/Resources/licences` has the texts of Qt's own licence terms: the Qt Company GPL Exception 1.0 (`Qt-GPL-exception-1.0`), the GNU Free Documentation License 1.3 without invariant sections (`GFDL-1.3-no-invariants-only`), and the BSD 3-clause licence Qt uses (`BSD-3-Clause`). They come from Qt's source (`LICENSES/` in qtbase 6.12.0, checked against the SHA-256 Homebrew records), so the app's `NOTICE.txt` no longer warns that they are missing. The BSD text is Qt's template, so its copyright line is generic; the holders are named in the Qt source headers.

## [1.6.0] - 2026-11-06

### Changed

- The test suite accepts either codec name for a subtitle track in `--info` on an MKV: the Rust version prints ffprobe's `subrip`, and `fml.py` prints mkvmerge's `SubRip/SRT`. The two differ on purpose or not; see the open item in `TODO.md`. No change to what the command does.

## [1.5.9] - 2026-11-06

### Fixed

- `--dir` with `--recursive` follows a symbolic link to a folder, so a season folder that is a link is searched. Before, it was skipped without a word and the command reported nothing. The app's dropped folders and the subtitle search (`--sub-dir`) follow links the same way.
- A folder reached twice, such as a link back up the tree, is read once, so nothing loops and no file is listed twice. A file reached through two links is listed and changed once.
- A folder with no media files now says `no media files found in "<folder>"` on stderr, so an empty result is no longer silent.

## [1.5.8] - 2026-11-06

### Fixed

- A media file that is a symbolic link is changed where it points to. Before, the edited copy replaced the link, so the real file was left as it was and the link became an ordinary file. The file is named by its real path in the output and the log, and `--undo` finds it there. A link that points nowhere is still reported as not existing.

## [1.5.7] - 2026-11-06

### Changed

- The project is renamed from `fml` (Fix Media Language) to `mau` (Media Audio Utility). The crates are `mau-core`, `mau-cli` and `mau-gui`; the command is `mau-cli`, the desktop app is `mau` (`mau.app`, and installers named `mau_<version>_...`), and the wrapper in a checkout is `./mau` (it was `./fml`). The README, `--help`, the app's title and About text, `build.sh`, the CI workflows and the tests use the new name.
- The environment variables are `MAU_WHISPER`, `MAU_WHISPER_MODEL`, `MAU_WHISPER_MODEL_URL`, `MAU_OPENSUBTITLES_KEY`, `MAU_OPENSUBTITLES_USER`, `MAU_OPENSUBTITLES_PASSWORD`, `MAU_OPENSUBTITLES_URL`, `MAU_GUI`, `MAU_APP_DIRS`, `MAU_NO_BUILD` and `MAU_NOTARY_ENV`. The `FML_` name of each is still read when the `MAU_` one is not set. The switches for debugging the app (`MAU_GUI_REPORT`, `MAU_GUI_SCRIPT`, `MAU_GUI_THEME`, `MAU_GUI_SIZE`), `MAU_SPIKE_NO_MKVTOOLNIX` and the tests' `MAU_BIN` have no old-name fallback.
- mau keeps its files (the whisper model and the saved OpenSubtitles account) in a `mau` folder in the user's data folder. An existing `fml` folder is still used until a `mau` folder exists, so nothing has to be moved or downloaded again. `build.sh` does the same for `~/.config/fml/notary.env`.
- The app's identifier is `au.com.lateralblast.mau` (it was `au.com.lateralblast.fml`), so macOS sees the installed app as a new one. The log file the app suggests is `mau-log.jsonl`, and the Rust engine type `Fml` is `Mau`.
- Not renamed, on purpose: the `.fml-tmp` and `.fml-bak` suffixes of temporary files and `--backup` copies (so `--dir` still skips backups made by earlier versions, and the archived Python version agrees), and `archived/fml.py` and `archived/fml.sh`, which keep their names (`fml.py` only has its version number changed). The Python package built from `pyproject.toml` is still `fml`, since it is the archived script.

## [1.5.6] - 2026-11-06

### Added

- The sandboxed Mac app (both `--sandboxed` and `--lgpl`) carries the licence of each package its tools come from, in `Contents/Resources/licences/<package>/`, with a `NOTICE.txt` that lists every package with its version, the source it was built from (the URL and SHA-256 from Homebrew) and the files it supplies. `packaging/mac/collect_notices.py` builds it from the list of files `packaging/mac/bundle_tools.py` copies.
- A package that ships no licence file (boost, glib) gets the standard text of its licence, from `packaging/notices/standard`. Licences with no text there (Qt's own exception and GFDL, and BSD-3-Clause for Qt) are a warning in the build, and `NOTICE.txt` says they are not in the app.

## [1.5.5] - 2026-11-06

### Removed

- `--in-place` is gone from `archived/fml.py`, as it has been from the Rust command line since 1.5.1, so the archived reference and the README agree. Changes are still made on a copy that replaces the file once it is checked.

## [1.5.4] - 2026-11-06

### Added

- The `--lgpl` app carries ffmpeg's licence text (`COPYING.LGPLv2.1`) and a `SOURCE.txt` (where its source is, its SHA-256 and the configure options) in `Contents/Resources/licences`, as the LGPL asks for a library that ships with an app.

## [1.5.3] - 2026-11-06

### Added

- `./build.sh --target mac --lgpl` builds the sandboxed Mac app with an LGPL-only ffmpeg and ffprobe in place of Homebrew's GPL build, and without mkvtoolnix, which is GPL. `packaging/ffmpeg/build-lgpl.sh` builds ffmpeg 9.0.2 from the source at ffmpeg.org, checked against its SHA-256, with no `--enable-gpl`, no nonfree code and no external codec libraries such as x264 or x265. Its shared libraries go into the app. The disk image is named `-lgpl.dmg`. It is for a local build on a Mac.

### Changed

- The test suite makes its sample media with ffmpeg's own encoders (`mpeg4`, `aac`, `mp2`) rather than `libx264` and `libmp3lame`, so it runs against an LGPL ffmpeg as well as the Homebrew one.

## [1.5.2] - 2026-11-06

### Changed

- fml is now licensed under the Apache License, version 2.0 (`LICENSE`, `Cargo.toml`, `pyproject.toml`, the README, the About text and the `archived/fml.py` header), in place of GPL-3.0-or-later. The change applies to this version and later; earlier versions keep the licence they were released under. The tools the sandboxed Mac app bundles (mkvtoolnix, ffmpeg) keep their own licences.

## [1.5.1] - 2026-11-06

### Removed

- `--in-place` is gone, from the command line, the desktop app (its checkbox) and the documentation. Every change is made on a copy, which replaces the file once it has been checked, as before. The copy is always used now, so the free-space check no longer suggests `--in-place`.

## [1.5.0] - 2026-11-06

### Added

- `--no-mkvtoolnix` (for `build.sh`) builds a version that does not use mkvtoolnix at all: MKV changes are made with `ffmpeg`, which covers flags, languages, names, reordering, removing and adding tracks, and extracting subtitles. It is the build for the App Store, and is written to `rust/target-nomkv`, with `-no-mkvtoolnix` in its file names. It is a Cargo feature, `no-mkvtoolnix`, and `--check` does not ask for mkvtoolnix in it.

### Changed

- MKV files are read with `ffprobe`, in every build, so track codec names are ffprobe's (`subrip`, `aac`, `ac3`, `eac3`) rather than mkvmerge's (`SubRip/SRT`, `AAC`, `AC-3`). `--codec` ignores hyphens, so `ac-3` matches `ac3` and `eac3`.
- `--in-place` needs `mkvpropedit` (from mkvtoolnix). Without it the command is refused with a message, and the copy-and-replace mode (the default) is used.
- Flags that a change does not ask for (such as hearing-impaired) are now kept when a flag is changed.

## [1.4.0] - 2026-11-05

### Added

- Desktop app: the About window (opened by the **About** button, or **About fml** in the app menu on a Mac) shows the app icon, the version and `ABOUT.md`, the same as the About dialog did. It replaces the plain About panel that macOS shows by default. On a Mac the app menu now has Edit and Window menus as well.

### Changed

- The About text is in a window of its own (`about.html`) rather than a dialog on the main page.

## [1.3.9] - 2026-11-05

### Fixed

- The About dialog (and `ABOUT.md`) said the licence was GPL-3.0. It now says GPL-3.0-or-later, as `LICENSE` and `Cargo.toml` do, with a link to the licence text.

## [1.3.8] - 2026-11-05

### Added

- Desktop app: an **About** button in the top bar opens a dialog with the text of `ABOUT.md` (compiled into the app, so it is the same file as in the repository) and the version. Its web links open in the browser.

## [1.3.7] - 2026-11-05

### Added

- `./build.sh --target mac --sandboxed` builds a sandboxed app that carries the tools it uses inside it: `mkvmerge`, `mkvpropedit`, `mkvextract`, `mediainfo`, `ffmpeg`, `ffprobe`, and `whisper-cli` when it is installed, with the libraries they load (`packaging/mac/bundle_tools.py` copies them and rewrites their references). Each tool is signed with the inherit entitlement, so it runs under the app's sandbox (`packaging/mac/app.entitlements`: files the user chooses, and network access). The app uses its own tools before any installed ones. The disk image is named `fml_<version>_<arch>-sandboxed.dmg`, so it sits in `dist/` beside the usual one. It is a local build only (`--ci` is refused).

### Changed

- fml is licensed under the GNU General Public License, version 3 or later (`GPL-3.0-or-later`), in place of CC BY-NC-SA 4.0. The bundled tools (mkvtoolnix under GPL-2.0-or-later, ffmpeg under GPL-3.0-or-later) need a licence that allows them to be distributed with fml, and the non-commercial term of the old licence would not allow it. `LICENSE` now holds the GPL-3.0 text, and `Cargo.toml`, `pyproject.toml`, the README and the `archived/fml.py` header say so. `archived/fml.sh` keeps its old header, as it is archived.

## [1.3.6] - 2026-11-05

### Fixed

- A test of the subtitle files found below a folder split paths on `/` only, so it failed on Windows, where paths use `\`. The Windows tests job (informational) reported it. The file names are now taken with either separator. No change to the app.

## [1.3.5] - 2026-11-05

### Added

- `./build.sh --target mac` signs and notarises by default. It looks for a Developer ID Application certificate in the keychain (or `APPLE_SIGNING_IDENTITY`): with one, the app is signed; with `--notarize` (the default) the app and the `.dmg` are notarised, the `.dmg` is stapled and both are checked. `--nonotarize` signs without notarising. Without a certificate the app is built unsigned and the build says so. A certificate with no notarisation credentials stops the build before it starts. The credentials come from the environment or from the file `FML_NOTARY_ENV` names (default `~/.config/fml/notary.env`).
- The **Build** workflow's mac job leaves signing and notarising to `build.sh`, so the disk image is no longer notarised by a separate step.

## [1.3.4] - 2026-11-05

### Added

- The **Build** workflow's mac job also notarises the `.dmg` itself when the notary key is set, then staples its ticket and checks it with `stapler` and `spctl`. The build only notarises the app inside, so without this the disk image has no ticket of its own.

## [1.3.3] - 2026-11-05

### Added

- The **Build** workflow's mac job signs and notarises the `.dmg` when the repository has the signing secrets and variables (listed in the README). It imports the Developer ID certificate into a temporary keychain, writes the App Store Connect key to the runner, and passes both to `cargo tauri build`. Without them the build is unsigned, as before.

## [1.3.2] - 2026-11-05

### Added

- `./build.sh --target <t> --ci` builds the installers on GitHub's runners instead of this machine, through the new **Build** workflow (`.github/workflows/build.yml`, started by hand: linux, mac or windows, with `--package` and `--arch` as the local build takes them). It checks that the working tree is clean and the branch pushed, follows the run, and downloads its files into `dist/`. `--local` builds here, as before; with neither flag a terminal is asked which to use. `--dry-run` runs the checks and prints the commands without starting anything.
- The **Build** workflow also has a `windows tests` job that runs `cargo test` on Windows. It is informational: it does not fail the run.

## [1.3.1] - 2026-11-05

### Added

- Desktop app: the bulk change menu offers three more changes, the same as on the command line, with the same checks. **Keep several languages** keeps the tracks in every language listed in **Languages to keep** (as `--keep`). **Remove duplicate tracks** removes every track after the first in each language (as `--delete dupes`). **Remove tracks that match** removes the tracks that fit a language (or any language), a part of the codec name and a channel count, of which at least one must be given (as `--delete match`); the channel count applies to audio only.

## [1.3.0] - 2026-11-05

### Added

- `--enable search` turns on the online subtitle search. It is needed for `--search subtitle` and `--add subtitle --online` (the switch is shared by the whole command, like `--dryrun`).
- `fml-cli --gui --enable search` (or `./fml --gui --enable search`) opens the desktop app with the search on. The app takes no other option than `--enable search`.

### Changed

- The online subtitle search is **off by default**, because it has not been tested fully yet. Without `--enable search`, `--search subtitle` and `--add subtitle --online` are refused before anything is sent, with a message that says what to add. In the desktop app, **Search online (OpenSubtitles)…** is hidden unless the app is started with `--enable search`. Adding subtitle files from disk is not affected.
- The `--help` text of `--search`, `--add subtitle --online` and `--gui` says what they need.

## [1.2.9] - 2026-11-05

### Changed

- Desktop app: removed a status check that nothing could trigger (`ok`, which the engine never reports), so the status shown after a change is the engine's status as it is. No change to what is shown.

## [1.2.8] - 2026-11-05

### Fixed

- The test suite could read the real data folder on macOS, where the data folder is under `HOME` and the test isolation only set `XDG_DATA_HOME`. A test that expects no whisper model found the user's model and failed. Tests now use a temporary `HOME` too. No change to the app.

## [1.2.7] - 2026-11-05

### Fixed

- Desktop app: the track tables in the Edit dialog that can be picked (audio and subtitle tracks, not video) are now marked as a grid with column headers, the role that rows which can be focused and selected need. Not tried with a screen reader.

## [1.2.6] - 2026-11-05

### Fixed

- Desktop app: after a change was applied, the list was read again while the run was still on screen: the run's progress bar and Stop button stayed up during the re-read, and the re-read's own busy state was switched on and off inside the run's. The list is now read once the run has been cleaned up.

## [1.2.5] - 2026-11-05

### Fixed

- Desktop app: a track kind that is not `audio` or `sub` was quietly taken as audio. The app now reports it as an error, with the same wording as the command line (`"video" is not audio or sub (subtitles)`); a missing kind is still audio.

## [1.2.4] - 2026-11-05

### Fixed

- Desktop app: files dropped on the window while a run or a dialog was open were ignored without a word. The window now says why they were not added and what to do.

## [1.2.3] - 2026-11-05

### Added

- Desktop app: a **Stop** button in the Edit dialog while a change, a subtitle extraction or a language detection is running there. The dialog covers the window, so the main Stop button could not be reached; a stopped change says so, and the file is left as it was.

### Changed

- Desktop app: the confirmation for a bulk change says what it does, such as "Make English the default audio track in 12 files?" or "Remove the Italian subtitle tracks in 3 files?", instead of only "Change 12 files?". After a preview, "Apply these changes…" asks the same question about the previewed change, and the order, keep and remove previews from the Edit dialog and adding subtitles say what they do too.

### Fixed

- Desktop app: a confirmation dialog that was closed with Escape could apply the change anyway, because it kept the answer of the previous confirmation (Apply). Each confirmation now starts with no answer, so Escape always cancels.

## [1.2.0] - 2026-11-02

### Added

- `--search subtitle` lists the subtitles OpenSubtitles has for a file (best first: made for exactly that file, then the most downloaded), with their IDs, languages, downloads and notes; `--format json` gives `data.hits`. `--lang` limits the language and `--query TEXT` searches for another title. The file is not changed.
- `--add subtitle --online --lang L` finds the best subtitle in a language on OpenSubtitles (not a machine translated or forced one, and a hearing impaired one only when there is no other), downloads it and adds it as `--add subtitle --sub-file` does; `--sub-id N` adds the subtitle that `--search subtitle` listed with that ID. `--dir` finds one for each file by its own name, with a single login for the run. A hearing impaired subtitle is named `SDH`. `--dryrun` searches and says what it would add but downloads nothing; `--backup`, `--log` and `--undo` (from the backup) work, and the downloaded file is removed afterwards. `--online`, `--sub-id` and `--query` are new settings.
- The key and login are the ones saved by the app, or `FML_OPENSUBTITLES_KEY`, `FML_OPENSUBTITLES_USER` and `FML_OPENSUBTITLES_PASSWORD`.

### Changed

- This is 1.2.0 because a patch number may not go past 9.

## [1.1.9] - 2026-11-02

### Added

- Desktop app: the Edit dialog can search OpenSubtitles for subtitles. **Search online (OpenSubtitles)…**, under **Add a subtitle file**, searches by the title guessed from the file name (with the season and episode or the year) and the file's OpenSubtitles hash, in one language or any; the results show the language, release, downloads and notes, with the ones made for this very file first. **Use this…** downloads the subtitle to a temporary file and fills in the add-a-subtitle form (file, language, and the name `SDH` or `Forced`), so it is added, checked, the usual way; the temporary file is removed after it is added or the dialog is closed.
- The search needs an OpenSubtitles API key and, to download, the account's user name and password, entered under **OpenSubtitles account** and kept in `opensubtitles.json` in fml's data folder in a file only the user can read (`FML_OPENSUBTITLES_KEY`, `FML_OPENSUBTITLES_USER` and `FML_OPENSUBTITLES_PASSWORD` take the place of what is saved). The requests are made with `curl` through a private config file, so no secret is on a command line. Nothing is sent until **Search** is pressed.
- `fml-core` has `online` (the search, the download, the hash, the guess from the file name, and the saved settings); `FML_OPENSUBTITLES_URL` gives another address for the service.

## [1.1.8] - 2026-11-02

### Added

- `--delete match` removes the tracks of `--kind` that fit every one of the `--lang`, `--codec` and `--channels` that are given (at least one is needed). `--codec` is part of the codec name as `--info` shows it, in any case (`ac-3` matches `AC-3` and `E-AC-3`), and `--channels` is a number of audio channels (audio only). It works like the other removals: a checked copy, the last audio track is never removed without `--force`, `--dryrun`, `--backup`, `--log` and `--undo` (from the backup) apply.
- Tracks have a number of channels where the tool reports it (`mkvmerge`'s `audio_channels`, `mediainfo`'s `Channels`): the `--info` table has a **Ch** column (`-` when unknown), and the track in `--format json` has `channels` (left out when unknown, so the older output is unchanged).

## [1.1.7] - 2026-11-02

### Added

- `--delete dupes` removes every track after the first one in its language, so each language keeps one track of the kind (`--kind sub` for subtitles, where a forced track and a full one in the same language are kept apart). A file with no duplicates is left unchanged. `--delete dupes` takes no `--lang`.

## [1.1.6] - 2026-11-02

### Added

- `--keep LANGS` for `--preserve`: keep the tracks in every language of a list (`--preserve lang --keep eng,jpn,und`) and remove the rest, where `--lang` and `--prefer` keep only one language. Languages the file lacks are ignored; if it has none of them the file is left alone (`--force` removes them all), as with `--lang`. When the default track goes, the first track left becomes the default. `--keep` can't be given with `--lang` or `--prefer`, and only works with `--preserve`.

## [1.1.5] - 2026-11-02

### Changed

- Desktop app: the tools message at the top reads `mkvtoolnix, ffmpeg, mediainfo, whisper-cli found` when the optional whisper-cli is found (the same lookup as `--check`), and the usual `mkvtoolnix, ffmpeg, mediainfo found` when it is not. A missing whisper-cli is not reported as missing. Its path is in the tooltip either way.

## [1.1.4] - 2026-11-02

### Added

- `--update` listens to the audio tracks as `--detect` does, then sets the language of each track whose label differs to the language it sounds like. It is a change like any other (`--dryrun` previews it, `--backup` and `--log` apply). A track already labelled with the language heard is left alone, AVI files are skipped (they have no language to set), and `--kind sub` is refused.
- Desktop app: after a successful **Detect languages…**, the Edit dialog has **Update the languages…** when a track's label differs from the language heard. It asks first, then makes the change as the command line does. It is not offered for AVI files.

## [1.1.3] - 2026-11-02

### Added

- `--check --install` downloads the whisper.cpp model that `--detect` reads, `ggml-base.bin` (148 MB), when there is none. The model goes to `FML_WHISPER_MODEL` if it is set, else to `ggml-base.bin` in fml's data folder. Before the download the disk is checked for that much room with a tenth to spare: if there is not enough, fml says so and downloads nothing. The download is checked against its published SHA-256 before it is kept. `--check --install --dryrun` says what it would download, and downloads nothing.
- `--detect` finds the model in fml's data folder when `FML_WHISPER_MODEL` is not set.
- `FML_WHISPER_MODEL_URL` gives another address for the model download (a mirror, or a test server).

## [1.1.2] - 2026-11-02

### Added

- `--check` lists `whisper-cli` (whisper.cpp, which `--detect` runs) as an optional command, and `FML_WHISPER_MODEL` with the model file it finds. A missing optional command is reported as optional and is not an error, so `--check` still exits 0.
- `--check --install` installs whisper.cpp where a package is known: `sudo apt install -y whisper.cpp` on Debian and Ubuntu, `brew install whisper-cpp` on macOS. On other systems it says to install it by hand. The model file is not downloaded (it is large), so `--check` still reports it missing.

## [1.1.1] - 2026-11-02

### Added

- `--detect` guesses the language of each audio track (or of `--track N`) from its speech, with whisper.cpp: three 30-second clips of each track are cut with `ffmpeg` and whisper.cpp's language detection is run on them. It prints one line per track (`track 1 sounds like ita (93% sure, 3 of 3 clips; labelled und)`), and `--format json` gives `data.detected`. The file is not changed. It needs the program `whisper-cli` (or `whisper-cpp`) on `PATH`, or `FML_WHISPER` set to it, and the model file in `FML_WHISPER_MODEL`. Without them it says what is missing. `--kind sub` is refused.
- Desktop app: the Edit dialog has **Detect languages…** for the audio tracks of a file, which shows what each one sounds like in a **Sounds like** column. The file is not changed.

## [1.1.0] - 2026-11-02

### Added

- Desktop app: the Edit dialog has **Swap the two audio tracks…** when a file has exactly two audio tracks. It swaps them in one step (one rewrite of the file, for MKV, MP4 and AVI alike), so there is no need to move a track first and then apply the order. It is not offered while moves are pending.

## [1.0.9] - 2026-11-02

### Added

- Desktop app: an AVI file with exactly two audio tracks can be reordered in the Edit dialog (pick a track, **Move up** or **Move down**, **Apply this order…**), which swaps the two tracks. Other AVI settings stay unavailable, as AVI has no flags, names or languages to set.
- Desktop app: **Swap the two audio tracks** in the bulk change menu, offered when every ticked file has exactly two audio tracks. It reorders them, for MKV, MP4 and AVI files alike.
- `--set order --order 2,1` now works on an AVI file with exactly two audio tracks (the same swap `--swap` makes in the file). AVI files with other numbers of tracks say so, and are left unchanged.

## [1.0.8] - 2026-11-02

### Added

- The `./fml` script brings the Rust build up to date before it runs, with `cargo`: after a change to the code, the next `./fml` rebuilds what changed (and prints `fml: rebuilt fml-cli` when it did). `./fml --gui` does the same for the desktop app. A build that is already current costs only a quick check. A failed build stops the run with the cargo error, instead of running an old build. `FML_NO_BUILD=1` skips the check and runs what is built. The first build (nothing built yet) shows cargo's progress.

## [1.0.7] - 2026-11-02

### Fixed

- Desktop app: GNOME (and other desktops) could show "the application is not responding" while a run was writing a large file, because some commands did disk work on the window's own thread. Dropping files or folders on the window, the subtitle matching in the Add subtitles dialog, renaming a file and the version and tool check at start-up now run off that thread, like scanning and the Info dialog already did. The results are the same.

## [1.0.6] - 2026-10-24

### Added

- Screenshots of the desktop app in the README (`docs/images/`): the main window, a preview with the before-and-after tables, the Edit dialog, the Add subtitles dialog, and the dark theme. They were captured from the app itself (its window only) on demo files, with generic names.
- Debug builds of the app take `FML_GUI_SIZE=1120x1000` to give the window a size, as `FML_GUI_THEME` gives it a theme (see `rust/README.md`).

### Changed

- The README is reorganised. It now opens with a short description, a screenshot and a contents list with the subsections; "What it does" and a quick start come first; the desktop app has a section of its own (it was spread over Requirements, the ordering section and the subtitle section); the command line is grouped (basics, choosing a track, removing, subtitles, languages, moving and ordering, names and forced flags, adding and extracting subtitles, MP4 and AVI, many files, several actions, a whole season); safety and reference sections are grouped; and `--track`'s forms are with choosing a track, not under removing tracks. Nothing was dropped: every paragraph and example was moved, checked line by line against the old README.
- The README no longer says the Rust version is "at the same version (0.7.2)" and shows the current version in the JSON example; the notes about the archived scripts are together at the end; "Files that are not MKV or AVI are skipped" now says media files.

### Fixed

- `fml-cli --gui` on a Mac that has `fml.app` installed started the real app in the tests, whose "app is missing" and "a script called fml is not the app" cases expected it not to be found (so the test run hung until the app was closed). The folders searched for `fml.app` can now be set with `FML_APP_DIRS` (folders separated as in `PATH`, replacing the default ones), the tests use it, and a test covers finding a bundle.
- Desktop app: long file names in the tables wrap at hyphens and dots where they can, rather than anywhere in a word.

## [1.0.5] - 2026-10-24

### Added

- A GitHub Actions workflow (`.github/workflows/ci.yml`) that runs on every push to `main` and every pull request: `ruff`, `mypy` (not on 3.9, which current `mypy` no longer supports) and `pytest` of the archived `fml.py` on Python 3.9 to 3.14 with the real `mkvtoolnix`, `mediainfo` and `ffmpeg`; on Linux and macOS a build with warnings as errors, `cargo test --workspace` and the whole `pytest` suite run against the built `fml-cli`; a build of the core and command line tool with the oldest Rust they support; and a `cargo check` of the code for Windows. All of it uses `--locked`, so a stale `Cargo.lock` fails. It checks, signs and publishes nothing.
- `rust-version` in the crate manifests: 1.85 for `fml-core` and `fml-cli`, 1.90 for `fml-gui`.

### Fixed

- The README, `rust/README.md` and the notes said Rust 1.80 was enough; the dependencies in `Cargo.lock` need 1.85 for the core and command line tool (a dependency uses edition 2024) and 1.90 for the app (Tauri). Found by building with each version, and the CI now checks the 1.85 claim.

## [1.0.4] - 2026-10-24

### Added

- Rust tests for the paths the pytest suite cannot reach through the binary, which need a full disk or a `PATH` without the tools: the free-space check (room set aside per file until it is done and per disk, a refused change reported as not made, a disk or file that can't be read, a copy that would not fit never started, and one that exactly fits is), `--check` naming the commands that are missing (and with `--install`, in a dry run and on a system with no known package manager), and an action stopping before it touches anything when a command it needs is missing. Breaking the reservation, the comparison and the hint each made one of them fail.

### Changed

- Inside `fml-core` the disk lookup and the command lookup are fields of the engine's shared state (the real `disk_free` and `which` by default), so a test can put a full disk or a missing command in their place. Nothing changes for a user.

## [1.0.3] - 2026-10-24

### Added

- `--add subtitle --sub-dir DIR` adds the subtitle files in a folder (and the folders below it) to all the files being processed, matched by name as the app's **Add subtitles…** does: by the media file's name, else by season and episode. It uses all the files of the run at once, so the longest matching name wins. The language, track name (`SDH`, `CC`, `Forced`) and forced flag come from the file name; `--lang` is used for files whose name has no language (without it they are left out, and named in a warning), and files that match nothing are named too. A file's subtitles are added in one rewrite; ones it already has (same language and name) are skipped, so running it again adds nothing, and for an MP4 the ones that can't be converted (`.sup`, forced) are skipped one by one with the reason instead of refusing the file. It works with `--dryrun`, `--log` and `--undo` (with `--backup`).
- `fml-core` has `subtitles::plan_from_folder` and `subtitle_files_in`, which do the planning.

## [1.0.2] - 2026-10-24

### Added

- `--install` knows more systems: the Debian family (`apt`, now by `ID`/`ID_LIKE` instead of only Ubuntu, so Debian, Mint, Pop!_OS and the like work), the Fedora and RHEL family (`dnf`, with `ffmpeg-free`), the Arch family (`pacman`, with `mkvtoolnix-cli`), macOS (`brew`) and Windows (`winget` with `MoritzBunkus.MKVToolNix`, `MediaArea.MediaInfo` and `Gyan.FFmpeg`, else `choco` with `mediainfo-cli`). The package names were checked against the package repositories. Only the commands built are tested: the installs on Fedora, Arch and Windows have not been run there. `fml-core` has `Installer` for it.
- On Windows, after an install the folders the installers use (`MKVToolNix` in Program Files, winget's `Links`, Chocolatey's `bin`) are added to this run's `PATH`, so the commands that were just installed are found.

### Changed

- The `--install` help says which package managers it uses, and the "don't know how to install" warning on Windows says to install winget or Chocolatey.

## [1.0.1] - 2026-10-24

### Added

- `--set lang --default none --kind sub` now works on MP4 files, which refused it because `ffmpeg`'s MP4 muxer always enables the first track of each kind. The default flag of an MP4 track is the "enabled" bit of its track header (`tkhd`), which `mediainfo` and `ffprobe` both read, so fml changes that one bit in a copy of the file (nothing is rewritten or re-encoded, and the size is the same) and reads the copy back before it replaces the original. Dry runs, `--log`, `--undo` and the before-and-after table work as for the other changes.
- `fml-core` has `mp4` (`read_track_flags`, `set_enabled`), a small reader of the track headers of an MP4.

## [1.0.0] - 2026-10-24

### Added

- Each action on the command line has its own settings. The settings that follow an action (`--lang`, `--default`, `--prefer`, `--track`, `--kind`, `--forced`, `--move`, `--name`, `--order`, `--order-lang`, `--default-first`, `--forced-flag`, `--sub-file`, `--output`) belong to it, and the ones given before the first action are shared by every action, with an action's own added on top. So `--set lang --default English --delete lang --lang ita` now works, where it was refused because `--default` and `--lang` were shared. Each action is checked with its own settings.

### Changed

- A setting that used to be shared by actions written before it (for example `--set lang --default X --kind sub` meant subtitles for the `--set`, and for every other action too) now belongs only to the action it follows; put it before the first action to share it. Commands with one action, or with the settings first, are unchanged. A setting can be given more than once for an action (the last wins); it was an error before.
- The archived Python version still shares its settings, as before.
- This is 1.0.0 because a patch or minor number may not go past 9 (0.9.9 + one change).

## [0.9.9] - 2026-10-24

### Fixed

- Desktop app, dark theme: the placeholder text in the text boxes ("*.mkv, Season 1/*", "eng, jpn, fre", "empty removes the name") was as bright as typed text, so it looked like a value; it is now muted in both themes.

### Changed

- The dark theme was checked on a Mac (Apple silicon, macOS 27) by capturing the app window in both themes: the main page, the Edit dialog with its track editor, keep-only preview and tables, and the Add subtitles dialog. Apart from the placeholders, contrast, buttons, status colours and tables were fine.
- Debug builds of the app take `FML_GUI_THEME=dark|light` to show the window in a theme whatever the system uses (see `rust/README.md`).

## [0.9.8] - 2026-10-24

### Added

- Desktop app: a folder, or files, can be dropped on the window instead of using **Add a folder…** and **Add files…**. One folder on its own is listed as a folder (with the subfolder and filter settings, and Rescan); anything else becomes a list of the media files among the files dropped and inside the folders dropped, each once, skipping `.fml-tmp` and `.fml-bak` files. Dropped files that are not media files are counted in the message. The window shows a dashed outline while something is dragged over it, and a drop is ignored while a run is going or a dialog is open.
- The app has a `classify_dropped` command behind it (see `rust/README.md`).

## [0.9.7] - 2026-10-24

### Added

- Desktop app: while a file is being copied or rewritten, the results show a progress bar for it with a percentage (one per file when several are processed at once), so a long rewrite of a big file no longer looks stuck. It follows the size of the new file against the size of the original, so it is approximate (a removal ends below 100%), and it goes when the file's result arrives.
- `fml-core` sends `Event::Progress` (file, stage, bytes done, bytes total) when `Config::progress` is set. The command line does not set it, so it pays nothing for it.

## [0.9.6] - 2026-10-24

### Fixed

- Renaming a file from the Edit dialog broke `--undo` for changes made before, because the log names the file by its path. The app now rewrites the entries about that file in the log the form is using (when "Record a log" is ticked) to the new name, through a temporary file so the log is never half written, and says how many it changed. Entries in other logs are not found, and the dialog says so. A kept original (`--backup`) is still found, as the log records its path.

### Added

- `fml-core` has `rename_in_log`, which does the rewrite, and the app's `rename_file` command takes the log as an optional argument and returns the new path with `log_entries` and `log_error`.

## [0.9.5] - 2026-10-24

### Added

- `--extract subtitle --track T [--output FILE|FOLDER]` writes a subtitle track to a file and leaves the media file alone. MKV uses `mkvextract` and writes `.srt`, `.ass`, `.vtt` or `.sup` to match the track; a text subtitle in an MP4 is written as `.srt` with `ffmpeg`. It is written under a temporary name and only given its real one if the tool made something, never replaces a file that exists unless `--force` is given with a file name, and works with `--dryrun`. VobSub and other multi-file formats, and AVI files, are refused. The result's `data.output` is the file written.
- Desktop app: a subtitle track picked in the Edit dialog has an **Extract…** button that asks where to save it (the name is suggested from the media file and the track).

## [0.9.4] - 2026-10-24

### Added

- Desktop app: a changed file in the results (the preview, and after a run) has a **Before and after** table with its audio and subtitle tracks side by side by place, language, `*` for default, forced and name, with what changes highlighted. A preview shows it open.
- `--format json`: a result that changes the tracks of a file (`--set`, `--delete`, `--preserve`, `--add`, `--swap`) has `data.layout` with `before` and `after`, the tracks as `--info` gives them. In a dry run `after` is what the file is expected to be. Results with nothing to show still have `data` null.

## [0.9.3] - 2026-10-24

### Added

- Desktop app: the audio and subtitle sections of the Edit dialog have a **Keep only** language chooser. Choosing a language marks in the track table which tracks would be removed (struck through, with an "After" column) and says how many would go and which track becomes the default, before anything is changed. **Remove the others…** makes the change after asking (the same as `--preserve --lang`), and **Preview for all ticked files…** shows it for the other ticked files in the results.

## [0.9.2] - 2026-10-24

### Added

- `--add subtitle` (and the app's single and bulk **Add subtitles**) now works on MP4 files as well as MKV: the subtitle is converted to `mov_text` with `ffmpeg` in one rewrite, every existing track name is written back (as for every MP4 rewrite), and the result is read back before it replaces the original. `.srt`, `.ass`, `.ssa` and `.vtt` can be added; a `.sup` picture subtitle and the forced flag are refused for an MP4. The MP4 muxer makes the first subtitle track default when there is none, so the first subtitle added to an MP4 without any is default.

### Changed

- AVI files are refused with "only supported for MKV and MP4 files" (it said MKV only).

## [0.9.1] - 2026-10-24

### Added

- `--set track --track T [--lang L] [--name TEXT] [--forced-flag yes|no]` changes the language, name and forced flag of one track in a single edit of the file (one copy and one `mkvpropedit` for an MKV, instead of one per property). Only what is given is changed, and it says so when the track is already as asked. The forced flag is MKV only, as with `--set forced`. `--undo` reverses it.
- Desktop app: picking an audio or subtitle track in the Edit dialog shows an editor above the table with its language, name and forced flag (MKV only), and **Save changes…** makes the change after asking. It is off while a new order is waiting to be applied or reset.

## [0.9.0] - 2026-10-24

### Added

- Desktop app: **Add subtitles…** adds subtitle files to many MKV files at once. Each chosen file is matched to a ticked MKV file by name (`Show.S01E03.ita.srt` goes with `Show.S01E03.mkv`; failing that, the same season and episode such as `S01E03` or `1x03`, if only one file has it), and the language, a track name (`SDH`, `CC`, `Forced`) and the forced flag are guessed from the words after the media name, using known languages only. The table can be edited, files that match nothing are listed, and the change can be previewed first. A file with several subtitles is rewritten once.
- `fml-core` has `subtitles::match_subtitles`, which does the matching, and `add subtitle` takes a list of subtitles each with its own language, name and forced flag (the command line still adds one at a time).

### Changed

- The desktop app can run a list of requests as one run (preview, apply and the stop button work on the whole list).

## [0.8.9] - 2026-10-24

### Added

- `--set order --order-lang ita,eng` puts the tracks in the order of a list of languages: the tracks in the first language first, then the second, then the rest in their old order. Unlike `--order` (track numbers) it suits files whose tracks differ, so one command fixes a season (`--dir`). It works with `--default-first`, `--log` and `--undo`, and `--order` and `--order-lang` can't be given together.
- Desktop app: in the Edit dialog, **Preview this order…** and **Preview removing this language…** apply the arrangement (as languages) or the removal of the picked track's language to all the ticked files. They show the usual preview in the results, from where **Apply these changes…** makes them.

## [0.8.8] - 2026-10-24

### Added

- `--set order ... --default-first` also makes the track that ends up first the only default track of its kind, so players that follow the flag and players that play the first track agree. It is done in the same rewrite, undone by `--undo`, and also fixes the flag when the order is already right. Before, the default flag stayed on the old first track.
- Desktop app: the audio and subtitle sections of the Edit dialog have a **Make the first track the default** checkbox, used by **Apply this order…** (which can now be pressed with the order unchanged, to fix only the flag).

## [0.8.7] - 2026-10-24

### Added

- `--add subtitle --sub-file FILE --lang L [--name TEXT]` adds a subtitle file (`.srt`, `.ass`, `.ssa`, `.vtt` or `.sup`) to an MKV as a new subtitle track, last among the subtitles, not default and not forced. It is written with `mkvmerge` on a copy and read back before it replaces the original, so chapters, attachments and the other tracks are kept. It is refused for MP4 and AVI files, with `--in-place`, and when the subtitle file or the language is missing or is not a subtitle file. `--undo` needs `--backup` to undo it, since the number of tracks changed.
- Desktop app: the Edit dialog of an MKV has an **Add a subtitle file** section with a file picker, a language (guessed from the file name where it holds one, such as `Film.ita.srt`) and an optional name.

## [0.8.6] - 2026-10-24

### Added

- Desktop app: the Edit dialog has a **Delete…** button beside **Move up** and **Move down**. It removes the picked audio or subtitle track after asking, in one rewrite of the file (with the `Keep the original`, log and `Allow removing every audio track` settings of the main form), then shows the new track list. The next audio track becomes the default if the default one was removed, and the last audio track is refused unless removing every audio track is allowed. **Delete…** is off while a new order is waiting to be applied or reset. A track with one track of a kind now shows the buttons too.

## [0.8.5] - 2026-10-24

### Added

- Desktop app: the Edit dialog has a **File name** field and a **Rename** button (Enter works too). The file is renamed in its own folder; a name with `/` or `\`, an empty name, or one that is already taken is refused, so nothing is ever overwritten, and it is refused while a run is going. The list, the ticked files and the dialog follow the new name. The dialog warns that a torrent client seeding the file will lose it, and that an older `--log` can no longer undo changes to it.
- The app has a `rename_file` command behind it (see `rust/README.md`).

## [0.8.4] - 2026-10-24

### Added

- Desktop app: a **Clear selected** button in "1. Choose files" takes the ticked files off the list, so later changes leave them alone. The files on disk are not touched. **Rescan** of the same folder or files keeps them off the list; choosing a folder or files again brings everything back.

## [0.8.3] - 2026-10-24

### Changed

- Desktop app: the per-file **Info** button is now called **Edit** and has an accent-coloured border (filled on hover), so it is easy to see. The dialog it opens is unchanged.

## [0.8.2] - 2026-10-24

### Changed

- Desktop app: the second step is now titled "2. Choose a bulk change", since a single file's tracks can be reordered from its Info dialog.

## [0.8.1] - 2026-10-24

### Added

- `--set order --order N,N,...` puts the audio or subtitle tracks (`--kind`) in any order in one rewrite of an MKV or MP4: each number is the current number of a track, listed in the new order (`--order 3,1,2`). Flags, languages and names travel with their tracks. It is refused if a track is missing or named twice, and with `--in-place`. `--log` records the reorder and `--undo` puts the old order back. Before this only one track could be moved (`--move`, to be the first).
- Desktop app: in a file's Info dialog the audio and subtitle tracks can be picked and moved up and down, and **Apply this order…** rewrites the file (with the `Keep the original`, log and `Allow removing…` settings of the main form) and shows the new track list.

### Changed

- `fml-core` has `reordered_by`, which puts several tracks at their places at once (`reordered` still moves one); the log has an `order` field for a reorder of several tracks.

## [0.8.0] - 2026-10-24

### Added

- Desktop app: each file in the list has an **Info** button that opens a dialog with the file's path, container, track counts, size and modified date, the full track table (now with the track ID, which `--delete track --track id:N` uses) and, under "Full technical report", everything `mediainfo` knows about it (duration, bitrate, resolution, each track's properties). The report says why it is missing if `mediainfo` is not installed. The expanded track list under a row also shows the ID now.
- The app has a `file_details` command behind the dialog (see `rust/README.md`).

## [0.7.9] - 2026-10-23

### Fixed

- Setting a track name with an accent (`--set name --name Français`) failed on macOS with "mkvpropedit did not make the change to the file", because MKVToolNix there stores text in decomposed form (`c` plus a combining cedilla) and the check compared the names byte for byte. Names are now compared after Unicode normalisation, here and wherever fml compares names to see if a file is as it left it (`--undo`, "already has this name"). On macOS the stored name still shows as the decomposed form in `mkvmerge -J`, which looks the same.

## [0.7.8] - 2026-10-23

### Added

- `--track` accepts a track of any kind by its place in the `--info` table (`subtitles:18`, `sub:last`, `audio:2`), its ID (`id:20`) or its name (`name:Forced`, ignoring case). `--delete track` removes that track whatever `--kind` is, so a subtitle track no longer needs `--kind sub` with its position among subtitles. A name that matches more than one track is refused and the IDs are listed.

### Fixed

- `--delete track --track 20` on a file with two audio tracks looked only at audio tracks, and `--track subtitles:18` was refused as "not a track number". Both ways to name a subtitle track by what `--info` shows now work.

## [0.7.7] - 2026-10-23

### Added

- macOS support for the desktop app and `fml-cli --gui`. The app adds `/opt/homebrew/bin`, `/usr/local/bin`, `/opt/local/bin` and the `MKVToolNix*.app` folders in `/Applications` to its `PATH` when it starts, because an app opened from Finder gets a bare `PATH` and would report `mkvmerge`, `mediainfo` and `ffmpeg` as missing even when Homebrew has installed them.
- `fml-cli --gui` finds the app as `fml.app` beside `fml-cli`, in `~/Applications` or in `/Applications` when no `fml` program is found, so it works after installing from the `.dmg`.
- The app's bundle sets a minimum macOS version (10.15).

### Changed

- The README says how to build and install on a Mac (`./build.sh --target mac`, Homebrew for the commands).

## [0.7.6] - 2026-10-23

### Added

- `./build.sh --target windows` now works on Linux and macOS: it cross-compiles the Windows installer (`fml_<version>_x64-setup.exe`) and `fml-cli.exe` (packaged as `fml-cli-<version>-windows.zip`) in a Docker container (`docker/windows`, MinGW, `x86_64-pc-windows-gnu`) and copies them to `dist/`. Building on Windows itself is unchanged.
- `docker/windows/smoke.sh` runs the built `fml-cli.exe` under Wine against stand-in tools and checks a real edit, the backup and the log, and that a refused change leaves no temporary file.

### Changed

- `fml-core` and `fml-cli` now build for Windows: the Unix-only parts (signal numbers, `statvfs`, the execute bit, `exec`, `SIGINT`/`SIGTERM`) have Windows versions (`PATHEXT` lookup, `GetDiskFreeSpaceExW`, `SetConsoleCtrlHandler`, starting `fml.exe` for `--gui`), and `libc` is only used on Unix. A command that is a `.bat` or `.cmd` file is found and started on Windows. The app shows Windows paths relative to the chosen folder, and finds the home folder from `USERPROFILE`.
- `--undo --dir` compares folders as paths, so it also works with Windows paths.

## [0.7.5] - 2026-10-23

### Changed

- The desktop app is now the program called `fml` (it was `fml-gui`): `rust/target/release/fml`, `/usr/bin/fml` in the `.deb`. `fml-cli --gui` looks for `fml` beside itself, then on `PATH`, and at the path in `FML_GUI`. It skips any `fml` that is a script (one starting with `#!`), so the archived Python `fml` and the `./fml` wrapper are never started by mistake. The crate and directory are still `rust/fml-gui`. If you installed the app as `fml-gui`, rename it or copy the new binary.

## [0.7.4] - 2026-10-23

### Changed

- The desktop app uses `fml.png` as its icon: the window and the installer icon sizes (`.png`, `.ico`, `.icns`) are generated from it, and the app's header shows it beside the title. Regenerate the set with `cargo tauri icon icons/source.png -o icons` in `rust/fml-gui`.

## [0.7.3] - 2026-10-23

### Added

- `build.sh` builds the desktop app's installer (`.deb`, `.rpm`, AppImage, `.dmg` or NSIS `.exe`, one platform at a time) and the command line archive into `dist/`, and `--release` tags the commit, pushes the tag and publishes both to a GitHub release (`--delete-release` undoes it; `--dry-run` shows what would happen)

### Changed

- The Rust command line tool is now called `fml-cli` (it was `fml`), so it no longer shares a name with the checkout wrapper and the `fml` project: install it as `fml-cli`, and `fml-cli --gui` opens the desktop app. `./fml` in the checkout still works: it runs `fml-cli`, and `./fml --gui` still opens the app. The switches and output are unchanged apart from the program name in `--version`, the usage line and error messages

## [0.7.2] - 2026-10-16

### Added

- `--jobs N` (`-j`) processes N files at once, for `--dir`. The output is in the same order, and the same, as a sequential run; the free space each job needs is set aside while it runs, so jobs can't use it up between them; and an interrupt waits for the files already running, removes their temporary copies and starts no more

## [0.7.1] - 2026-10-16

### Added

- `--include PATTERN` and `--exclude PATTERN` choose which files `--dir` processes, and can be given more than once. A pattern with no `/` matches a file name, or the name of any folder above it (`Extras` skips every folder of that name); a pattern with a `/` matches the whole path below the directory (`Season 1/*`). Matching ignores case

## [0.7.0] - 2026-10-16

### Added

- `--log FILE` records each change in a file, one JSON line per changed file, with the tracks before and after
- `--undo FILE` (`-u`) reverses the changes in a log, newest first, for the files that are still exactly as the change left them (`--file` or `--dir` limits it to some files). Flags, languages, names and the order of tracks are put back by editing the file; a removal can only be undone from a `--backup` copy, which is checked against the log first. `--dryrun` shows what would be undone

## [0.6.9] - 2026-10-16

### Added

- `--backup` keeps the original beside the changed file as `NAME.fml-bak` (`NAME.fml-bak.1` and so on if that is taken). It is a hard link to the original made just before it is replaced, so it costs no space, and a real copy where links are not possible or with `--in-place`. If the backup can't be made, nothing is replaced. `--dir` ignores backups, and the JSON result has a `backup` field

## [0.6.8] - 2026-10-16

### Added

- `--set name --track N --name TEXT` sets the name of a track (an empty name removes it), for MKV and MP4. A name that starts with `-` is written `--name=-TEXT`
- `--set forced` and `--set unforced` set and clear the forced flag of a track, for MKV (an MP4's forced flag can't be read back to check the change)

### Changed

- The check made on a changed file now also covers track names and, for MKV, forced flags, and it is made for removals and the AVI swap as well, with the message `<tool> did not make the change to the file`

## [0.6.7] - 2026-10-16

### Changed

- After `--delete` or `--preserve` removes the default track, the first track left becomes the default (always for audio; for subtitles in an MP4, which insists on one), so a file isn't left without a default audio track. An MKV may still have no default subtitle track

## [0.6.6] - 2026-10-16

### Fixed

- Track names are kept when an MP4 is rewritten. `ffmpeg` dropped the names it can't read back (the ones `mediainfo` shows as the title), including the video track's; each is now written again at the track's place in the new file, and checked

## [0.6.5] - 2026-10-16

### Changed

- `fml.py` is now checked on Python 3.9, 3.10, 3.11, 3.12, 3.13 and 3.14: the whole test suite passes on each, and `mypy --strict` passes on 3.10 to 3.14 (current `mypy` no longer supports 3.9, which `ruff` and the tests cover). No code change was needed

## [0.6.4] - 2026-10-16

### Added

- `pyproject.toml`, so `pip install .` (or `pipx install .`) installs an `fml` command. The version is read from `fml.__version__`, there are no Python dependencies, and the `dev` extra (`pip install -e ".[dev]"`) adds `pytest`, `ruff` and `mypy`. The settings for `ruff`, `mypy` and `pytest` are in the same file

## [0.6.3] - 2026-10-16

### Added

- A `pytest` suite in `tests/` (278 tests) covering the language handling, track parsing and edit logic, every command line rule and message, the failure and safety paths with stub tools (a tool that fails or does nothing, no free space, Ctrl-C and `kill`, missing commands), and real MKV, MP4 and AVI files made with `ffmpeg` and checked with `mkvmerge`, `mediainfo` and `ffprobe`. The tests that need those tools are skipped if they are missing
- Tests that the version in `fml.py`, the README and the changelog agree, and that every switch is in the README help

## [0.6.2] - 2026-10-16

### Added

- `--move` with `--set lang --default` (or `--prefer`) or `--set default --track` also moves the chosen track to be the first track of its kind, for players that play the first track whatever its default flag. An MKV is rewritten once with `mkvmerge`, which reorders the tracks and sets the flags and languages together, keeping the chapters, attachments and track names; an MP4 is rewritten with `ffmpeg`. If the track is already first, only the flags are edited. The result is checked, including the order, before it replaces the original

### Changed

- The check made on a changed copy now covers the order of the tracks as well as their flags and languages, and its message is `<tool> did not make the change to the file`

## [0.6.1] - 2026-10-16

### Fixed

- Stopping `fml.py` part way (Ctrl-C or `kill`) removes the temporary copy it was working on and exits 130; before, a large `.fml-tmp` file was left beside the original
- `--dir` ignores leftover `.fml-tmp` files from an earlier crashed run, instead of processing them as media files

## [0.6.0] - 2026-10-16

### Added

- Every rewrite is checked before it replaces the original: `--delete`, `--preserve` and the AVI `--swap` re-read the new file and confirm it has exactly the expected tracks, and if not the original is left alone and the file is reported as an error
- Before copying or rewriting a file, `fml.py` checks there is enough free space beside it for a second copy, and reports an error instead of failing part way

## [0.5.9] - 2026-10-16

### Changed

- `--set` and `--swap` on an MKV file now copy the file, change the copy with `mkvpropedit`, check the copy has the changes, and then replace the original with it, so a failure or interruption can't damage the original. The file's permissions and timestamps are kept

### Added

- `--in-place` edits an MKV file directly, as before: quicker and needs no free space, but an interruption can leave the file damaged

## [0.5.8] - 2026-10-15

### Fixed

- `--swap` makes the second audio track the only default, so it also clears the flag on a third track that was default; before, two tracks could end up default

## [0.5.7] - 2026-10-15

### Added

- MP4 (and M4V, MOV) files are supported. They are rewritten with `ffmpeg`, copying the streams (nothing is re-encoded), and the result is read back and checked before it replaces the original. The index is moved to the front of the file (`faststart`), and permissions and timestamps are kept
- `--set`, `--swap`, `--delete`, `--preserve`, `--get` and `--info` work on MP4. `--default none` does not, as MP4 always has a default subtitle track

## [0.5.6] - 2026-10-15

### Added

- `--prefer eng,jpn,fre` with `--set lang` makes the first of those languages that a file has the default, so one command suits a mixed library; with `--preserve` it keeps only that language. Files with none of them are skipped

## [0.5.5] - 2026-10-15

### Added

- `--set default --track N` makes track N the only default, and with `--lang` also sets its language in the same edit, for files whose tracks are untagged (`und`)

## [0.5.4] - 2026-10-15

### Added

- `--kind sub` (`-k`) makes the actions work on subtitle tracks: `--get`, `--set lang` (`--default`, `--lang` and `--track`), `--delete` and `--preserve`
- `--forced` chooses the forced subtitle track, and without it a track that is not forced is preferred
- `--set lang --default none --kind sub` turns the default subtitles off
- Removing subtitle tracks does not need `--force`; `--preserve` with a language the file lacks still does

### Changed

- `--preserve` with a language the file lacks now says to use `--force` to remove all the tracks, and `--force` allows it

## [0.5.3] - 2026-10-15

### Added

- `--info` lists the tracks of an MKV or AVI file as a table (track, ID, codec, language, default, forced, name); `--format json` gives the same as data
- `--get lang` reports the default audio language of each file (the first audio track for AVI, which has no default flag)

## [0.5.2] - 2026-10-15

### Added

- `--format json` prints one JSON document of the results of every action, for scripts, including the changes and the commands that were run
- `--dryrun` lists each change that would be made (for example `audio track 1 (ita): default yes -> no`) rather than only the commands
- `--set lang --default` only edits the tracks whose default flag has to change, and reports `unchanged` when nothing does
- `--delete track` works on MKV files as well as AVI
- `--force` allows `--delete` to remove every audio track; without it `--delete` and `--preserve` refuse, so a file is never left without audio
- Files are rewritten through a temporary file that is only moved into place when the command succeeded, keeping the original permissions and timestamps, and `--delete` and `--preserve` remove all their tracks in one `mkvmerge` run
- A failure on one file is reported and the run continues with the rest; the exit status is 1 if any file failed
- Warnings and `--verbose` output go to stderr, so stdout only has the results

### Changed

- Each action only checks for the commands it uses (`mkvmerge`/`mkvpropedit` for MKV, `mediainfo`/`ffmpeg` for AVI), and `jq`, `file`, `ffprobe` and `mkvinfo` are no longer needed

## [0.5.1] - 2026-10-15

### Added

- `--lang` and `--default` take language names, two letter (ISO 639-1) and three letter (ISO 639-2) codes, so `French`, `fr`, `fre` and `fra` are the same language, and unknown names are rejected with an error instead of being cut to three letters
- Languages in files are converted the same way, so a file tagged `fra` or `deu` matches `--lang French` or `--lang German`

## [0.5.0] - 2026-10-15

### Changed

- `fml.py` now uses `argparse`, with standard `--help`, usage errors (exit status 2), abbreviated long options and a typed value for each option. It no longer mirrors `fml.sh`, which is archived and no longer maintained
- `--options verbose,dryrun` is now `--verbose --dryrun`, and `--options install` is `--install`
- `--get lang` reports the default audio language; `--get lang --lang <language>` lists the audio tracks in that language
- `--set lang --lang <language>` sets the language of any audio track, chosen with `--track` (the first by default); it takes `--default` or `--lang`, not both
- `--format` is `text` or `json`
- `--track` is an audio track number from 1, or `last`
- `fml.py` reads the version from `__version__`

### Removed

- `--action`, `--option`, `--usage`, `--debug`, `--strict`, `--shellcheck` and `--yes` from `fml.py`, along with the `printenv`, `printdefaults` and `shellcheck` actions
- `--format xml` and `--format html`, and `--lang default`

## [0.4.9] - 2026-10-15

### Fixed

- `--option` in `fml.sh` and `fml.py` rejects names that aren't switchable options (`recursive`, `verbose`, `strict`, `dryrun`, `debug`, `force`, `yes`, `install`) with `Unknown option` and exit 1, instead of silently accepting a typo or setting a value such as `file` to `true`
- A `no` prefix is only stripped when the rest is an option name, so it no longer mangles a name that merely starts with `no`

## [0.4.8] - 2026-10-15

### Fixed

- `fml.sh` now checks that `file` is installed (`--check`, and the startup check). It uses `file` to detect the format, and without it every MKV and AVI operation silently did nothing

## [0.4.7] - 2026-10-15

### Fixed

- An unknown switch now prints the help and exits 1 in `fml.sh` and `fml.py`; it exited 0, so scripts couldn't tell a mistyped switch from success

## [0.4.6] - 2026-10-15

### Changed

- `fml.sh` rewritten to current shell practice: no `eval` (commands are run from argument lists), `local` variables, `[[ ]]` tests, `jq --arg` instead of splicing values into filters, shared helpers in place of duplicated code, and a `main` function; the switches, actions and options are unchanged
- `fml.py` restructured as a typed `Fml` class driven by an `Options` dataclass instead of module-level globals; it passes `ruff` and `mypy --strict`
- `--action printenv` and `--action printdefaults` in `fml.sh` list the options in sorted order, matching `fml.py`
- `--dir` processes files in sorted order
- Rewritten files are replaced with a single `mv -f` instead of `rm` then `mv`, so a failed move no longer loses the original
- `install` is now a documented option (default `false`), shown in `--usage options`

### Fixed

- `--set lang --default` in `fml.sh` now clears other default audio tracks when the requested language is already default; the branch that did this could never run, so it warned that no track existed
- `--get lang --lang <language>` in `fml.sh` no longer warns that the language is missing when the file has more than one track in that language
- `--dir` in `fml.sh` no longer processes files again for each extra action (the file list grew with every action)
- `--delete lang` and `--preserve lang` in `fml.sh` no longer exit 1 after succeeding
- `--dryrun` no longer reports "Failed to process file" after `--delete`, `--preserve` and AVI `--swap`; it shows the `mv` that would run
- `--delete track` with a missing, non-numeric or zero `--track` reports an invalid track instead of passing a bad stream index to `ffmpeg` (or a traceback in `fml.py`)
- `--strict` no longer aborts with an unbound variable when a command is run
- `fml.py` flushes its output before running a command, so messages stay in order when output is redirected
- `fml.py` reads the Linux distribution from `/etc/os-release` when `lsb_release` is missing

## [0.4.5] - 2026-10-15

### Fixed

- `--check --option install` in `fml.sh` and `fml.py` now installs the missing packages (one `mkvtoolnix` install covers `mkvinfo`, `mkvmerge` and `mkvpropedit`) and rechecks, instead of only printing the install hint
- `--option install` in `fml.sh` now runs `apt install` / `brew install` rather than only printing the command; `--dryrun` still just shows it
- Both scripts warn when they don't know how to install a package on the current OS

## [0.4.4] - 2026-10-15

### Added

- `--check` (`-C`, or `--action check`) in `fml.sh` and `fml.py` reports whether each required command is installed and exits 1 if any are missing

### Changed

- The required command list now includes `mkvmerge` and `mkvpropedit`, so the startup check in both scripts fails if either is missing

## [0.4.3] - 2026-10-15

### Added

- `fml.py`, a Python port of `fml.sh` with the same switches, actions and options; it runs the media tools with argument lists and parses `mkvmerge` JSON itself instead of using `jq`

### Changed

- In `fml.py`, `--set lang --default` now clears other default audio tracks when the requested language is already default, which the bash version's unreachable branch never did

## [0.4.2] - 2026-09-29

### Fixed

- `--strict` no longer aborts on unset options; all option keys are initialised

## [0.4.1] - 2026-09-29

### Fixed

- Literal `\n` in `IFS`, `find` option order, stray file list output for `--get` with `--dir`, missing `ffmpeg` check, template leftover script name, and deprecated `let`/`expr`

## [0.4.0] - 2026-09-29

### Fixed

- Language names now map to the correct ISO 639-2 codes for Chinese, Dutch, French, German, Greek and Japanese instead of the first three letters

## [0.3.9] - 2026-09-29

### Fixed

- MKV default track changes used mkvmerge track IDs where mkvpropedit expects audio track numbers; AVI `--track last` and AVI swap no longer assume a fixed stream layout

## [0.3.8] - 2026-09-29

### Fixed

- Missing files, directories, tools and bad parameter values now exit with an error in `--dryrun` mode instead of carrying on

## [0.3.7] - 2026-09-29

### Fixed

- File names containing quotes, `$`, backticks or spaces broke, or could inject into, the commands run through `eval`; file names are now shell escaped

## [0.3.6] - 2026-09-29

### Fixed

- `get` on non-MKV files never used plain `mediainfo` output, and the file name was not quoted; `--format` values are now case insensitive and `text` gives plain output

## [0.3.5] - 2026-09-29

### Fixed

- `get` on MKV files without a language used a mistyped array name and a stray character in the file name, and its `json` check never matched the `JSON` default

## [0.3.4] - 2026-09-29

### Fixed

- The `no` prefix for options (e.g. `nodryrun`) now turns the option off instead of on

## [0.3.3] - 2026-09-29

### Fixed

- `-o` was claimed by both `--format` and `--opt`, so options could not be set with the short switch; `--format` now uses `-F`

## [0.3.2] - 2026-09-29

### Fixed

- `--delete lang` now removes the tracks matching the language instead of the non-matching ones

## [0.3.1] - 2026-09-29

### Fixed

- `--preserve` and `--delete` now read the language from `--lang` (previously only `--default`, so an empty value matched every track and could strip all audio tracks); they now refuse to run without a language

## [0.3.0] - 2025-09-26

### Fixed

- Parameter check

## [0.2.9] - 2025-09-05

### Added

- Support for short command line options

## [0.2.8] - 2025-08-17

### Changed

- Improved handling of deletion of multiple audio tracks

## [0.2.7] - 2025-08-17

### Changed

- Improvements and bug fixes

## [0.2.6] - 2025-08-17

### Changed

- Improved processing of multiple tracks

## [0.2.5] - 2025-08-17

### Added

- Delete a specific audio track from AVI files

## [0.2.4] - 2025-08-17

### Added

- Delete the last audio track from AVI files

## [0.2.3] - 2025-08-17

### Added

- Check for the ffmpeg package

## [0.2.2] - 2025-08-17

### Added

- Swapping audio tracks on AVI files

## [0.2.1] - 2025-08-17

### Added

- Additional check for other tracks set to default

## [0.2.0] - 2025-08-17

### Fixed

- Typo

## [0.1.9] - 2025-08-17

### Fixed

- Escaping of file names

## [0.1.8] - 2025-08-17

### Added

- File check for preserve/delete functions

## [0.1.7] - 2025-08-17

### Changed

- Updated documentation

## [0.1.6] - 2025-08-17

### Added

- Preserve and delete functions

## [0.1.5] - 2025-08-16

### Fixed

- Documentation for switches

## [0.1.4] - 2025-08-16

### Changed

- Improved handling of files without a requested language

## [0.1.3] - 2025-08-16

### Changed

- Improved get function

## [0.1.2] - 2025-08-16

### Fixed

- Bug fixes

## [0.1.1] - 2025-08-16

### Added

- Capability to process directories

## [0.1.0] - 2025-08-16

### Fixed

- Bug fixes

## [0.0.9] - 2025-08-16

### Changed

- Documentation update

## [0.0.8] - 2025-08-16

### Added

- Code to get default language

### Changed

- Updated documentation

## [0.0.7] - 2025-08-16

### Changed

- Updated documentation

## [0.0.6] - 2025-08-16

### Added

- First working version of default audio track change based on language

## [0.0.5] - 2025-08-16

### Added

- Basic default functionality

## [0.0.4] - 2025-08-16

### Added

- Basic swap functionality

## [0.0.3] - 2025-08-16

### Added

- Basic set functionality

## [0.0.2] - 2025-08-16

### Added

- Basic get functionality

## [0.0.1] - 2025-08-16

### Added

- Initial version

[0.4.2]: https://github.com/lateralblast/fml/compare/v0.3.0...v0.4.2
[0.3.0]: https://github.com/lateralblast/fml/compare/v0.2.9...v0.3.0
