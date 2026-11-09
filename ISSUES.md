# Issues

## Todo

- `--install` on Fedora, Arch and Windows has not been run for real: only the commands it would run are tested
- `--preserve` can leave a file with no default track, when the track that is kept was not flagged default before
- `mediainfo` does not report the forced flag that `ffmpeg` writes into an MP4, so `--set forced` is MKV only and `--forced` can't pick a forced subtitle in an MP4 reliably
- A track name that starts with `-` must be written `--name=-text`, because `argparse` takes `--name -text` for an option
- `--undo` can only reverse a removal, or an AVI swap, from a `--backup` copy; without one the log can say what changed but not put it back
- An interrupt (or `kill`) during `--jobs` waits for the files already being processed to finish before it exits
- `--swap` only changes the default flag and never the order of the tracks (`--set ... --move` reorders), which the name does not suggest
- A dry run with several actions plans each one from the file as it is now, so `--set lang --default X --swap lang --dryrun` does not show the second action's effect on the first
- AVI files carry no language or default flag, so `--get lang` on AVI reports the first audio track (usually `und`), and the AVI `--swap` reorder could not be observed in testing (the test tracks were identical)
- Only about 60 languages are built in; other names and two letter codes are rejected, and only three letter codes pass through, so a valid language outside the table needs its ISO 639-2 code
- Actions that find nothing to do (no track in that language, a format that doesn't support the action) print a warning but exit 0, so a script cannot tell nothing was changed

- `./build.sh --target mac --sandboxed --dry-run` (a local build) is not a dry run: it builds, signs and writes `dist/`, and stages the bundled tools. Only the release and CI paths honour `--dry-run`
- `./mau --gui --sandboxed` rebuilds a stale sandboxed app before opening it, which takes minutes and writes new installers into `dist/`; no cleanup of old installers

## In Progress

## Done

- Desktop app shows "not responding" during a run on a busy disk, because some commands did disk work on the window's thread (fixed in 1.0.7: those commands now run off it)
- `--install` only knew apt on Ubuntu and brew on macOS, and warned on other systems (fixed in 1.0.2: Debian, Fedora and Arch families, winget and Chocolatey; installs on those systems not yet run)
- `--default none` was not supported for MP4 subtitles, because ffmpeg always enables the first track of the group (fixed in 1.0.1: the track header flag is changed in a copy of the file)
- `--default`, `--lang`, `--track` and the other settings were shared by every action in one command, so two actions that needed different values had to be separate commands (fixed in 1.0.0: settings belong to the action they follow)
- Renaming a file from the Edit dialog breaks `--undo` for changes logged before the rename, because the log has the old path (fixed in 0.9.6)
- A subtitle track can be added but not taken out to a file (fixed in 0.9.5)
- The app's preview is a list of messages, with no before and after track table (fixed in 0.9.4)
- No preview of what `--preserve` would leave in a file's Edit dialog (fixed in 0.9.3)
- A subtitle file can only be added to an MKV, not an MP4 (fixed in 0.9.2)
- A track's language, name and forced flag can't be changed from the Edit dialog (fixed in 0.9.1)
- Subtitle files can only be added one file at a time, with the language chosen by hand (fixed in 0.9.0)
- A reorder or removal made in a file's Edit dialog applies to that file only; a whole season needs the same change on every file (fixed in 0.8.9)
- Reordering tracks leaves the default flag on the old first track, so a player that plays the first track and one that follows the flag disagree (fixed in 0.8.8, `--default-first`)
- Rewriting an MP4 with `ffmpeg` lost track names that its demuxer cannot read, and the check before replacing the original did not look at names (fixed in 0.6.6 and 0.6.8)
- Removing the default track left a file with no default audio track (fixed in 0.6.7)

- `fml.py` had only been run on Python 3.14, the 3.9 target was checked by `ruff` only, and there was no test suite (fixed in 0.6.3 to 0.6.5: 278 tests pass on 3.9 to 3.14)

- There was no way to reorder tracks, so a player that always plays the first track kept playing the old first track (added in 0.6.2 as `--move`)

- An interrupted rewrite (Ctrl-C or `kill`) left the `.fml-tmp` temporary file behind, next to the original (fixed in 0.6.1; `kill -9` and a power cut can still leave one, and the next run ignores it)
- Rewrites of MKV (`--delete`, `--preserve`) and AVI files were not checked before the original was replaced, and free disk space was not checked first (fixed in 0.6.0)
