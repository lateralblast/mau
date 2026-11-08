#!/usr/bin/env bash
# Build mau installers (the desktop app) and the command-line binary.
#   ./build.sh --target mac [--arch arm64|x64|universal] [--notarize|--nonotarize]   (on a Mac: builds a .dmg)
#                                                                 --notarize (the default) signs the app with the Developer ID
#                                                                 Application certificate in the keychain, if there is one, and
#                                                                 notarises the app and the .dmg; --nonotarize signs but does not
#                                                                 notarise. The notary credentials come from the environment, or from
#                                                                 the file MAU_NOTARY_ENV names (default ~/.config/mau/notary.env).
#                                                                 --sandboxed builds a sandboxed app that carries its tools with their
#                                                                 libraries inside it (mediainfo, ffmpeg, ffprobe, and whisper-cli if
#                                                                 installed), and names its .dmg with -sandboxed-lgpl. It is the LGPL
#                                                                 build: no mkvtoolnix (which is GPL), and ffmpeg and ffprobe built from
#                                                                 source as LGPL-only tools (packaging/ffmpeg/build-lgpl.sh, into
#                                                                 rust/target/lgpl-ffmpeg). The MKV, MP4 and AVI edits are done in the
#                                                                 app (the native-* features), and ffmpeg does the rest (APPSTORE.md).
#                                                                 --sandboxed --with-mkvtoolnix is the GPL build: it also carries
#                                                                 mkvmerge, mkvpropedit and mkvextract (mkvtoolnix, GPL) and Homebrew's
#                                                                 ffmpeg (GPL), and names its .dmg with -sandboxed.
#                                                                 The app's Resources/licences has the licence of each package the
#                                                                 tools come from, and NOTICE.txt (packaging/mac/collect_notices.py).
#                                                                 --lgpl is the same as --sandboxed (kept for existing scripts).
#                                                                 --sandboxed --nolgpl is the build with no LGPL or GPL code: no
#                                                                 mkvtoolnix, and no ffmpeg or ffprobe either (the native-* features
#                                                                 and the no-ffmpeg feature, built in rust/target-nolgpl). A change that
#                                                                 needs ffmpeg is refused with a message, before the file is touched.
#                                                                 It names its .dmg with -sandboxed-nolgpl.
#                                                                 The sandboxed builds are local, for --target mac.
#   ./build.sh --target linux [--package deb|rpm|appimage|all]               (comma list allowed, e.g. deb,rpm; default: the
#                                                                 distribution's own package: deb on Debian/Ubuntu, rpm on
#                                                                 RHEL/Fedora/Alma/Rocky/SUSE, else appimage;
#                                                                 --target deb / --target rpm = linux with that package only)
#   ./build.sh --target windows      (on Linux or a Mac: cross-compiles the NSIS installer and mau-cli.exe in a Docker
#                                                                 container, docker/windows/Dockerfile (MinGW, x86_64-pc-windows-gnu);
#                                                                 on Windows, e.g. in Git Bash: builds the installer there; untested)
# The app's installer and the command line are built apart: the installer is a .deb/.rpm/AppImage, .exe or .dmg, the
# command line an archive, mau-cli-<version>-<system>.tar.gz or .zip, holding just the `mau-cli` binary. Add --no-cli to skip it.
# Results are copied to ./dist/ (gitignored).
# A target can only be built on its own platform (the app links the system web view).
# Needs the Rust toolchain and tauri-cli (cargo install tauri-cli --version '^2'). The interface in rust/mau-gui/ui is plain
# HTML, CSS and JavaScript, so there is no Node step. On Linux the app also needs libwebkit2gtk-4.1-dev and libgtk-3-dev.
#   ./build.sh --target <t> --ci [--package <p>] [--arch <a>] [--dry-run]
# --ci builds the same installers on GitHub's runners instead (.github/workflows/build.yml): the branch you are on
# is built as it is on GitHub, so it must be committed and pushed, with nothing left uncommitted. The run is started,
# followed until it ends, and its files are downloaded into dist/. It needs `gh`, logged in. --local builds here, as
# above. With --target and neither flag, a terminal is asked which to use (local is the default; without a terminal
# it is local). --dry-run checks the above and prints the commands, but starts nothing.
#   ./build.sh [--target <t>] --release [<version>] [--commit <ref>] [--draft] [--yes] [--dry-run]
#   ./build.sh --delete-release [<version>] [--keep-tag] [--yes] [--dry-run]
# --release tags the commit v<version> (version from rust/Cargo.toml), pushes the tag and creates the GitHub release with the
# files in dist/ for that version (needs `gh`, logged in). The release notes are that version's section of CHANGELOG.md.
# If the release already exists the files are added to it, so each platform can be built and added on its own machine.
# It never commits: the working tree must already be committed and pushed. Without --target it only releases what is
# already in dist/. --draft creates a draft release, --yes skips the question, --dry-run prints what it would do and
# changes nothing. --release 0.7.1 releases that version instead of the current one: it uses the files in dist/ for it
# and tags its existing tag, else the newest commit whose rust/Cargo.toml says that version (which must be pushed);
# --commit <ref> (a commit, branch or tag) says which commit to tag instead: for a version no commit's rust/Cargo.toml says
# (say it was only ever built from the working tree), with --release <version>. The commit must be pushed.
# a version other than the current one cannot be combined with --target (build it from a checkout of that commit).
# --delete-release deletes the GitHub release v<version> (default: the current version) with its uploaded files, and the
# tag v<version> on GitHub and locally unless --keep-tag is given. It asks first (--yes skips the question). It never
# touches commits or dist/.
# Apart from --release and --delete-release it never commits, tags, pushes or publishes anything.
set -euo pipefail

usage() {
  echo "usage: $0 --target mac|linux|deb|rpm|windows [--arch arm64|x64|universal] [--notarize|--nonotarize] [--package deb|rpm|appimage|all] [--no-cli] [--sandboxed [--with-mkvtoolnix|--nolgpl]] [--lgpl] [--local|--ci] [--dry-run]"
  echo "       $0 [--target <t>] --release [<version>] [--commit <ref>] [--draft] [--yes] [--dry-run]"
  echo "       $0 --delete-release [<version>] [--keep-tag] [--yes] [--dry-run]"
  exit "${1:-1}"
}

target="" arch="" package="" cli=1 delete=0 del_version="" rel_version="" rel_commit="" keep_tag=0 release=0 draft=0 yes=0 dry=0 where=""
notarize=1 notarize_set=0 notarizing=0 sandboxed=0 nomkv=0 lgpl=0 gpl=0 nolgpl=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:-}"; shift 2 ;;
    --arch) arch="${2:-}"; shift 2 ;;
    --package|--format) package="${2:-}"; shift 2 ;;
    --no-cli) cli=0; shift ;;
    --notarize) notarize=1; notarize_set=1; shift ;;
    --nonotarize) notarize=0; notarize_set=1; shift ;;
    --sandboxed) sandboxed=1; shift ;;
    --no-mkvtoolnix) nomkv=1; shift ;;
    --lgpl) lgpl=1; shift ;;
    --nolgpl) nolgpl=1; shift ;;
    --with-mkvtoolnix) gpl=1; shift ;;
    --local) where=local; shift ;;
    --ci) where=ci; shift ;;
    --release)
      release=1; shift
      case "${1:-}" in -*|"") ;; *) rel_version="${1#v}"; shift ;;
      esac ;;
    --delete-release)
      delete=1; shift
      case "${1:-}" in -*|"") ;; *) del_version="${1#v}"; shift ;;
      esac ;;
    --commit) rel_commit="${2:-}"; [ -n "$rel_commit" ] || { echo "--commit needs a commit or tag" >&2; exit 1; }; shift 2 ;;
    --keep-tag) keep_tag=1; shift ;;
    --draft) draft=1; shift ;;
    --yes) yes=1; shift ;;
    --dry-run) dry=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
[ -n "$target" ] || [ "$release" = 1 ] || [ "$delete" = 1 ] || usage
[ -z "$where" ] || [ -n "$target" ] || { echo "--local and --ci build a --target: give one" >&2; exit 1; }
if [ "$notarize_set" = 1 ] && [ "$target" != mac ] && [ "$target" != macos ]; then
  echo "--notarize and --nonotarize only apply to --target mac" >&2; exit 1
fi
if [ "$sandboxed" = 1 ] && [ "$target" != mac ] && [ "$target" != macos ]; then
  echo "--sandboxed only applies to --target mac" >&2; exit 1
fi
if [ "$lgpl" = 1 ] && [ "$target" != mac ] && [ "$target" != macos ]; then
  echo "--lgpl only applies to --target mac" >&2; exit 1
fi
if [ "$lgpl" = 1 ] && { [ "$where" = ci ] || [ "$release" = 1 ] || [ "$delete" = 1 ]; }; then
  echo "--lgpl is for a local build of --target mac: it cannot be combined with --ci, --release or --delete-release" >&2; exit 1
fi
if [ "$sandboxed" = 1 ] && [ "$where" = ci ]; then
  echo "--sandboxed is for a local build: the GitHub runners do not have the tools bundled" >&2; exit 1
fi
if [ "$gpl" = 1 ] && [ "$sandboxed" != 1 ]; then
  echo "--with-mkvtoolnix goes with --sandboxed" >&2; exit 1
fi
if [ "$gpl" = 1 ] && [ "$lgpl" = 1 ]; then
  echo "--with-mkvtoolnix and --lgpl are different sandboxed builds: give one" >&2; exit 1
fi
# the sandboxed app is the LGPL build (no mkvtoolnix, the LGPL ffmpeg) unless --with-mkvtoolnix asks for the GPL one
if [ "$nolgpl" = 1 ] && [ "$sandboxed" != 1 ]; then
  echo "--nolgpl goes with --sandboxed" >&2; exit 1
fi
if [ "$nolgpl" = 1 ] && { [ "$gpl" = 1 ] || [ "$lgpl" = 1 ]; }; then
  echo "--nolgpl is a sandboxed build with neither mkvtoolnix nor ffmpeg: it cannot be combined with --with-mkvtoolnix or --lgpl" >&2; exit 1
fi
if [ "$nolgpl" = 1 ] && { [ "$where" = ci ] || [ "$release" = 1 ] || [ "$delete" = 1 ]; }; then
  echo "--nolgpl is for a local build of --target mac: it cannot be combined with --ci, --release or --delete-release" >&2; exit 1
fi
# the sandboxed app is the LGPL build (no mkvtoolnix, the LGPL ffmpeg) unless --with-mkvtoolnix asks for the GPL one,
# or --nolgpl asks for the build with no ffmpeg at all
if [ "$sandboxed" = 1 ] && [ "$gpl" != 1 ] && [ "$nolgpl" != 1 ]; then lgpl=1; fi
if [ "$lgpl" = 1 ]; then sandboxed=1; nomkv=1; fi
if [ "$nolgpl" = 1 ]; then nomkv=1; fi
if [ "$nomkv" = 1 ] && [ "$where" = ci ]; then
  echo "--no-mkvtoolnix is not yet available on the GitHub runners: build it locally" >&2; exit 1
fi
if [ "$nomkv" = 1 ] && { [ "$release" = 1 ] || [ "$delete" = 1 ]; }; then
  echo "--no-mkvtoolnix builds an installer for --target: it cannot be combined with --release or --delete-release" >&2; exit 1
fi
if [ "$notarize_set" = 1 ] && [ "$where" = ci ]; then
  echo "--notarize and --nonotarize are for a local build: a --ci build notarises when the GitHub secrets are set" >&2; exit 1
fi
if [ "$where" = ci ] && { [ "$release" = 1 ] || [ "$delete" = 1 ]; }; then
  echo "--ci only builds a --target: it cannot be combined with --release or --delete-release" >&2; exit 1
fi
[ -z "$rel_version" ] || [ -z "$target" ] || { echo "--release <version> cannot be combined with --target: build that version first" >&2; exit 1; }
[ "$delete" = 0 ] || { [ "$release" = 0 ] && [ -z "$target" ]; } || { echo "--delete-release cannot be combined with --release or --target" >&2; exit 1; }

root=$(cd "$(dirname "$0")" && pwd)
dist="$root/dist"
# the version of the workspace (the first `version =` line of rust/Cargo.toml, under [workspace.package])
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/rust/Cargo.toml" | head -1)
current=$version

# The package format of the Linux distribution this runs on (/etc/os-release ID and ID_LIKE).
native_package() {
  local ids=""
  [ -r /etc/os-release ] && ids=$(. /etc/os-release; echo "${ID:-} ${ID_LIKE:-}")
  case " $ids " in
    *" debian "*|*" ubuntu "*) echo deb ;;
    *" rhel "*|*" fedora "*|*" centos "*|*" almalinux "*|*" rocky "*|*" suse "*|*" opensuse "*|*" opensuse-leap "*|*" sles "*|*" amzn "*|*" ol "*) echo rpm ;;
    *) echo appimage ;;
  esac
}

# The command line is shipped as an archive, apart from the app's installer: mau-cli-<version>-<label>.tar.gz
# (Linux, macOS) or .zip (Windows), holding a folder of the same name with just the `mau-cli` binary in it.
# usage: package_cli <path to the built binary> <label, e.g. linux or windows> [.exe]
package_cli() {
  local bin="$1" label="$2" exe="${3:-}" name work
  name="mau-cli-$version-$label"
  work=$(mktemp -d)
  mkdir "$work/$name"
  cp "$bin" "$work/$name/mau-cli$exe"
  chmod +x "$work/$name/mau-cli$exe"
  mkdir -p "$dist"
  if [ -n "$exe" ]; then
    rm -f "$dist/$name.zip"
    if command -v zip >/dev/null; then
      (cd "$work" && zip -q -r "$dist/$name.zip" "$name")
    elif command -v python3 >/dev/null; then
      python3 -I -c 'import shutil, sys; shutil.make_archive(sys.argv[1], "zip", sys.argv[2], sys.argv[3])' "$dist/$name" "$work" "$name"
    else
      echo "neither zip nor python3 found: cannot make $name.zip" >&2; exit 1
    fi
    echo "  $dist/$name.zip"
  else
    (cd "$work" && tar -czf "$dist/$name.tar.gz" "$name")
    echo "  $dist/$name.tar.gz"
  fi
  rm -rf "$work"
}

# Windows from Linux or a Mac: cross-compile in a container (docker/windows), no Windows machine needed. The repository
# is mounted read-only; the target directory and the cargo registry are Docker volumes, so only the first build compiles
# everything. The installer and mau-cli.exe are copied to dist/, and mau-cli.exe is packaged like the other platforms.
windows_docker() {
  command -v docker >/dev/null || { echo "docker not found: install it, or build on Windows" >&2; exit 1; }
  docker info >/dev/null 2>&1 || { echo "docker is not running, or you may not use it (is your user in the docker group?)" >&2; exit 1; }
  tauri_version=$(sed -n 's/^ *"version": *"\(.*\)".*/\1/p' "$root/rust/mau-gui/tauri.conf.json" | head -1)
  [ "$tauri_version" = "$version" ] || { echo "rust/mau-gui/tauri.conf.json says $tauri_version but rust/Cargo.toml says $version: make them the same" >&2; exit 1; }
  echo "== Windows cross build in Docker (x86_64-pc-windows-gnu) =="
  docker build -t mau-windows-cross "$root/docker/windows"
  out=$(mktemp -d)
  inner=(); [ "$cli" = 1 ] || inner=(--no-cli)
  docker run --rm \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/target \
    -v "$root":/src:ro -v "$out":/out \
    -v mau-windows-target:/target -v mau-windows-cargo:/cargo-home \
    mau-windows-cross \
    sh -c '/src/docker/windows/build.sh "$@"; rc=$?; chown -R "$HOST_UID:$HOST_GID" /out 2>/dev/null; exit $rc' sh ${inner[@]+"${inner[@]}"} \
    || { rm -rf "$out"; echo "the Windows build failed" >&2; exit 1; }
  mkdir -p "$dist"
  found=0
  for f in "$out"/*-setup.exe; do
    [ -f "$f" ] || continue
    cp "$f" "$dist/"; echo "  $dist/$(basename "$f")"; found=1
  done
  [ "$found" = 1 ] || { rm -rf "$out"; echo "no installer was built" >&2; exit 1; }
  if [ "$cli" = 1 ]; then
    [ -f "$out/mau-cli.exe" ] || { rm -rf "$out"; echo "mau-cli.exe was not built" >&2; exit 1; }
    package_cli "$out/mau-cli.exe" windows .exe
  fi
  rm -rf "$out"
  echo "done: $dist"
}

# Developer ID signing and notarisation for --target mac. The certificate is the Developer ID Application identity in the
# keychain (or APPLE_SIGNING_IDENTITY): with one, the app is signed; with --notarize (the default) the app and the disk
# image are notarised too. Without one, the app is built unsigned and the build says so. The notary credentials are
# either an App Store Connect key (APPLE_API_KEY, APPLE_API_ISSUER and APPLE_API_KEY_PATH) or an Apple ID (APPLE_ID,
# APPLE_PASSWORD and APPLE_TEAM_ID). They come from the environment, or from the file MAU_NOTARY_ENV names when none is set.
notary_settings_file() {
  local file="${MAU_NOTARY_ENV:-${FML_NOTARY_ENV:-$HOME/.config/mau/notary.env}}"
  # before the rename the default was ~/.config/fml/notary.env: keep using it until there is a new one
  if [ -z "${MAU_NOTARY_ENV:-}${FML_NOTARY_ENV:-}" ] && [ ! -r "$file" ] && [ -r "$HOME/.config/fml/notary.env" ]; then
    file="$HOME/.config/fml/notary.env"
  fi
  echo "$file"
}

load_notary_settings() {
  local file
  file=$(notary_settings_file)
  if [ -z "${APPLE_API_KEY:-}" ] && [ -z "${APPLE_ID:-}" ] && [ -r "$file" ]; then
    set -a; . "$file"; set +a
  fi
}

# The Developer ID Application identity to sign with: APPLE_SIGNING_IDENTITY, else the one in the keychain. Nothing (and
# success) if there is none; an error if there are several and none is named.
developer_id_identity() {
  local ids count
  if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then echo "$APPLE_SIGNING_IDENTITY"; return 0; fi
  ids=$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p')
  count=$(printf '%s\n' "$ids" | grep -c . || true)
  if [ "$count" -gt 1 ]; then
    echo "more than one Developer ID Application certificate: set APPLE_SIGNING_IDENTITY to the one to use" >&2; exit 1
  fi
  if [ "$count" -eq 1 ]; then echo "$ids"; fi
  return 0
}

# Whether the notary credentials are complete (an App Store Connect key, or an Apple ID with its password and team).
notary_ready() {
  { [ -n "${APPLE_API_KEY:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ] && [ -r "${APPLE_API_KEY_PATH:-/nonexistent}" ]; } ||
  { [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; }
}

# Decide how the Mac app is signed and whether it is notarised, before cargo tauri build (which signs when
# APPLE_SIGNING_IDENTITY is set, and notarises when the notary variables are). Sets notarizing=1 when the disk image
# must be notarised after the build too.
mac_signing_setup() {
  local identity
  load_notary_settings
  notarizing=0
  identity=$(developer_id_identity)
  if [ -z "$identity" ]; then
    echo "no Developer ID Application certificate in the keychain: the app is built unsigned and not notarised" >&2
    unset APPLE_SIGNING_IDENTITY
    return
  fi
  export APPLE_SIGNING_IDENTITY="$identity"
  if [ "$notarize" = 0 ]; then
    # --nonotarize: sign, but unset the notary variables so that Tauri does not notarise the app
    unset APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
    echo "== signing with $identity; not notarising (--nonotarize) =="
    return
  fi
  notary_ready || {
    echo "a Developer ID certificate was found, but not the notarisation credentials: set APPLE_API_KEY, APPLE_API_ISSUER and APPLE_API_KEY_PATH (or APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID), in the environment or in $(notary_settings_file); or build with --nonotarize" >&2
    exit 1
  }
  notarizing=1
  echo "== signing with $identity and notarising (--notarize) =="
}

# Notarise the disk image itself (Tauri notarises only the app inside it), then staple the ticket and check it.
notarise_disk_image() {
  local dmg="$1"
  echo "== notarising $dmg (this takes a few minutes) =="
  if [ -n "${APPLE_API_KEY:-}" ]; then
    xcrun notarytool submit "$dmg" --key-id "$APPLE_API_KEY" --key "$APPLE_API_KEY_PATH" --issuer "$APPLE_API_ISSUER" --wait ||
      { echo "notarisation of $dmg failed: see xcrun notarytool history" >&2; exit 1; }
  else
    xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait ||
      { echo "notarisation of $dmg failed: see xcrun notarytool history" >&2; exit 1; }
  fi
  xcrun stapler staple "$dmg"
  xcrun stapler validate "$dmg"
  spctl -a -vvv -t install "$dmg"
}

# --sandboxed: stage the tools the app uses into rust/target/sandbox-tools (bin/ and lib/, made by
# packaging/mac/bundle_tools.py), and sign them inside out with the inherit entitlement, so that each one runs under the
# sandbox of the app that starts it. Each is signed with the Developer ID identity when there is one (for notarising),
# else ad-hoc (a local test only).
# --lgpl: the LGPL ffmpeg is built once, into rust/target/lgpl-ffmpeg. Its licences/SOURCE.txt is written last, so a build
# that failed part way is built again. Delete that folder to build it again.
lgpl_ffmpeg() {
  local prefix="$root/rust/target/lgpl-ffmpeg"
  if [ ! -f "$prefix/licences/SOURCE.txt" ]; then
    "$root/packaging/ffmpeg/build-lgpl.sh" "$prefix"
  fi
  echo "== using the LGPL ffmpeg in $prefix =="
}

sandbox_stage() {
  local tools="$root/rust/target/sandbox-tools" tool path identity file
  local found=()
  command -v python3 >/dev/null || { echo "python3 not found: needed to bundle the tools" >&2; exit 1; }
  if [ "$lgpl" = 1 ]; then lgpl_ffmpeg; fi
  for tool in mkvmerge mkvpropedit mkvextract mediainfo ffmpeg ffprobe; do
    # the build without mkvtoolnix does not use (or carry) its commands
    if [ "$nomkv" = 1 ] && [ "${tool#mkv}" != "$tool" ]; then continue; fi
    # the --nolgpl build has no ffmpeg or ffprobe at all
    if [ "$nolgpl" = 1 ] && { [ "$tool" = ffmpeg ] || [ "$tool" = ffprobe ]; }; then continue; fi
    if [ "$lgpl" = 1 ] && { [ "$tool" = ffmpeg ] || [ "$tool" = ffprobe ]; }; then
      path="$root/rust/target/lgpl-ffmpeg/bin/$tool"
    else
      path=$(command -v "$tool") || { echo "$tool not found: install mkvtoolnix, mediainfo and ffmpeg (mau-cli --check --install)" >&2; exit 1; }
    fi
    found+=("$path")
  done
  plugins=""
  if path=$(command -v whisper-cli); then
    found+=("$path")
    # whisper.cpp's backends are plugins in Homebrew's ggml (libexec); they are bundled beside whisper-cli (bundle_tools.py)
    plugins=$(brew --prefix ggml 2>/dev/null)/libexec
    [ -d "$plugins" ] || { echo "no whisper.cpp backends in $plugins: the sandboxed app will find no devices" >&2; plugins=""; }
  else
    echo "whisper-cli not found: the sandboxed app will not detect languages (brew install whisper-cpp bundles it)" >&2
  fi
  rm -rf "$tools"
  echo "== bundling the tools =="
  if [ -n "$plugins" ]; then
    python3 "$root/packaging/mac/bundle_tools.py" --plugins "$plugins" "$tools" "${found[@]}"
  else
    python3 "$root/packaging/mac/bundle_tools.py" "$tools" "${found[@]}"
  fi
  # the licences of the packages the tools come from, with NOTICE.txt (collect_notices.py), for the app's Resources
  python3 "$root/packaging/mac/collect_notices.py" "$tools/licences" "$tools/sources.txt"
  rm -f "$tools/sources.txt"
  if [ "$lgpl" = 1 ]; then
    cp "$root/rust/target/lgpl-ffmpeg/licences/"* "$tools/licences/"
    cat >> "$tools/licences/NOTICE.txt" <<NOTE
ffmpeg and ffprobe (and the libav* and libsw* libraries) are not from Homebrew: they are built from source as LGPL-only
tools for this app (build.sh --sandboxed). Their licence is COPYING.LGPLv2.1 and their source is described in SOURCE.txt,
both beside this file.
NOTE
  fi
  identity="${APPLE_SIGNING_IDENTITY:--}"
  echo "== signing the bundled tools ($identity) =="
  for file in "$tools"/lib/* "$tools"/bin/*; do
    if [ "$identity" = "-" ]; then
      codesign --force -s - --options runtime --entitlements "$root/packaging/mac/tool.entitlements" "$file"
    else
      codesign --force --timestamp --options runtime --entitlements "$root/packaging/mac/tool.entitlements" -s "$identity" "$file"
    fi
  done
  for file in "$tools"/bin/*; do codesign --verify --strict "$file"; done
}

# The Tauri configuration of the sandboxed build: the staged tools are resources of the app and the app gets the
# sandbox entitlements. Written with absolute paths beside the staged tools, and given to cargo tauri build by --config.
sandbox_config() {
  local config="$root/rust/target/sandbox-tools.tauri.conf.json"
  cat > "$config" <<JSON
{
  "bundle": {
    "resources": {
      "$root/rust/target/sandbox-tools/bin/": "bin/",
      "$root/rust/target/sandbox-tools/lib/": "lib/",
      "$root/rust/target/sandbox-tools/licences/": "licences/"
    },
    "macOS": {
      "entitlements": "$root/packaging/mac/app.entitlements"
    }
  }
}
JSON
  echo "$config"
}

build_target() {
  host=$(uname -s)
  case "$target" in deb|rpm) package="$target"; target=linux ;; esac
  case "$target" in
    mac|macos)   want=Darwin; bundles=dmg
                 # the sandboxed app is kept (Tauri deletes it after the disk image): mau --gui --sandboxed opens it
                 if [ "$sandboxed" = 1 ]; then bundles=app,dmg; fi ;;
    linux)       want=Linux;  bundles=$(native_package) ;;
    windows|win) want=MINGW;  bundles=nsis ;;
    *) echo "unknown target: $target (mac, linux, deb, rpm, windows)" >&2; exit 1 ;;
  esac
  if [ -n "$package" ] && [ "$target" != linux ]; then
    echo "--package only applies to --target linux" >&2; exit 1
  fi
  if [ "$target" = linux ] && [ -n "$package" ]; then
    bundles=""
    for p in ${package//,/ }; do
      case "$p" in
        all) bundles="deb,rpm,appimage"; break ;;
        deb|rpm|appimage) bundles="${bundles:+$bundles,}$p" ;;
        *) echo "unknown --package: $p (deb, rpm, appimage, all)" >&2; exit 1 ;;
      esac
    done
  fi
  if [ "$want" = MINGW ]; then
    case "$host" in
      MINGW*|MSYS*|CYGWIN*) ;;
      *) windows_docker; return ;;
    esac
  fi
  case "$host" in
    $want*) ;;
    *) echo "the $target installer must be built on $target (this is $host)" >&2; exit 1 ;;
  esac
  if [ "$target" = linux ]; then
    case ",$bundles," in
      *,rpm,*) command -v rpmbuild >/dev/null || { echo "rpmbuild not found: install it (Debian/Ubuntu: apt install rpm; Fedora: dnf install rpm-build)" >&2; exit 1; } ;;
    esac
    if command -v pkg-config >/dev/null && ! pkg-config --exists webkit2gtk-4.1 gtk+-3.0; then
      echo "the system web view is missing: install libwebkit2gtk-4.1-dev and libgtk-3-dev (Debian/Ubuntu)" >&2; exit 1
    fi
  fi

  command -v cargo >/dev/null || { echo "cargo not found: install Rust (https://rustup.rs)" >&2; exit 1; }
  cargo tauri --version >/dev/null 2>&1 || { echo "tauri-cli not found: cargo install tauri-cli --version '^2'" >&2; exit 1; }

  # the installer's file name comes from tauri.conf.json, not Cargo.toml: they must agree
  tauri_version=$(sed -n 's/^ *"version": *"\(.*\)".*/\1/p' "$root/rust/mau-gui/tauri.conf.json" | head -1)
  [ "$tauri_version" = "$version" ] || { echo "rust/mau-gui/tauri.conf.json says $tauri_version but rust/Cargo.toml says $version: make them the same" >&2; exit 1; }

  triple="" triple_args=()
  if [ "$target" = mac ] || [ "$target" = macos ]; then
    case "${arch:-native}" in
      native) ;;
      arm64) triple=aarch64-apple-darwin ;;
      x64) triple=x86_64-apple-darwin ;;
      universal) triple=universal-apple-darwin ;;
      *) echo "unknown --arch: $arch (arm64, x64, universal)" >&2; exit 1 ;;
    esac
    if [ -n "$triple" ]; then
      triple_args=(--target "$triple")
      # make sure the Rust standard library for the requested architecture is installed
      case "$triple" in
        universal-apple-darwin) rustup target add aarch64-apple-darwin x86_64-apple-darwin ;;
        *) rustup target add "$triple" ;;
      esac
    fi
  elif [ -n "$arch" ]; then
    echo "--arch only applies to --target mac" >&2; exit 1
  fi

  if [ "$target" = mac ] || [ "$target" = macos ]; then mac_signing_setup; fi
  sandbox_args=()
  if [ "$sandboxed" = 1 ]; then
    sandbox_stage
    sandbox_args=(--config "$(sandbox_config)")
  fi
  # the build without mkvtoolnix has a target folder of its own, so the normal build's files are not replaced
  tdir="$root/rust/target" feature_args=() suffix=""
  if [ "$nomkv" = 1 ]; then
    tdir="$root/rust/target-nomkv"
    # the native features do the MKV, MP4 and AVI edits, the MKV subtitles and the clips for --detect in the app
    # (see APPSTORE.md); ffmpeg is still bundled for what they do not yet do
    features="no-mkvtoolnix native-mkv native-mp4 native-avi native-detect"
    suffix="-no-mkvtoolnix"
    if [ "$nolgpl" = 1 ]; then
      # nothing is left to bundle from ffmpeg: the no-ffmpeg feature refuses what needs it
      tdir="$root/rust/target-nolgpl"
      features="$features no-ffmpeg"
      suffix="-nolgpl"
    fi
    export CARGO_TARGET_DIR="$tdir"
    feature_args=(--features "$features")
  fi
  if [ "$lgpl" = 1 ]; then suffix="-lgpl"; fi
  mkdir -p "$dist"
  stamp=$(mktemp)
  echo "== building the installer ($target, $bundles) =="
  # cargo tauri build runs in rust/mau-gui; the workspace puts the bundles under rust/target/
  (cd "$root/rust/mau-gui" && cargo tauri build --bundles "$bundles" ${triple_args[@]+"${triple_args[@]}"} ${sandbox_args[@]+"${sandbox_args[@]}"} ${feature_args[@]+"${feature_args[@]}"})

  # Tauri puts bundles under target/[<triple>/]release/bundle/<kind>/
  found=0
  dmg_out=""
  while IFS= read -r f; do
    # the sandboxed disk image is named apart, so both builds can sit in dist/
    name=$(basename "$f")
    if [ "$sandboxed" = 1 ] && [ "${f##*.}" = dmg ]; then name="${name%.dmg}-sandboxed.dmg"; fi
    if [ -n "$suffix" ]; then name="${name%.*}$suffix.${name##*.}"; fi
    cp -R "$f" "$dist/$name"
    echo "  $dist/$name"
    found=1
    case "$f" in *.dmg) dmg_out="$dist/$name" ;; esac
  done < <(find "$tdir" -path '*/release/bundle/*' -type f \
    \( -name '*.dmg' -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' -o -name '*-setup.exe' \) -newer "$stamp" 2>/dev/null)
  rm -f "$stamp"
  [ "$found" = 1 ] || { echo "no installer found under $tdir (did the bundle step run?)" >&2; exit 1; }
  if [ "$notarizing" = 1 ] && [ -n "$dmg_out" ]; then notarise_disk_image "$dmg_out"; fi

  if [ "$cli" = 1 ]; then
    echo "== building the mau-cli command-line binary =="
    exe=""; [ "$target" != windows ] || exe=".exe"
    if [ "$triple" = universal-apple-darwin ]; then
      # plain cargo has no universal target: build both architectures and join them with lipo
      (cd "$root/rust" && cargo build --release -p mau-cli --target aarch64-apple-darwin &&
        cargo build --release -p mau-cli --target x86_64-apple-darwin)
      mkdir -p "$tdir/universal-apple-darwin/release"
      lipo -create -output "$tdir/universal-apple-darwin/release/mau-cli" \
        "$tdir/aarch64-apple-darwin/release/mau-cli" "$tdir/x86_64-apple-darwin/release/mau-cli"
      bin="$tdir/universal-apple-darwin/release/mau-cli"
    elif [ -n "$triple" ]; then
      (cd "$root/rust" && cargo build --release -p mau-cli --target "$triple" ${feature_args[@]+"${feature_args[@]}"})
      bin="$tdir/$triple/release/mau-cli$exe"
    else
      (cd "$root/rust" && cargo build --release -p mau-cli ${feature_args[@]+"${feature_args[@]}"})
      bin="$tdir/release/mau-cli$exe"
    fi
    [ -f "$bin" ] || { echo "the command-line binary was not found at $bin" >&2; exit 1; }
    # the archive's label: the system and, on a Mac, the architecture
    label="$target"
    case "$target" in
      mac|macos)
        case "${triple:-native}" in
          aarch64-apple-darwin) label=mac-arm64 ;;
          x86_64-apple-darwin) label=mac-x64 ;;
          universal-apple-darwin) label=mac-universal ;;
          *) case "$(uname -m)" in arm64|aarch64) label=mac-arm64 ;; *) label=mac-x64 ;; esac ;;
        esac ;;
    esac
    package_cli "$bin" "$label$suffix" "$exe"
  fi
  echo "done: $dist"
}

# Build on GitHub's runners (.github/workflows/build.yml) and bring the files back into dist/. The runners build the
# commit the branch points at on GitHub, so the working tree must be clean and that commit pushed; nothing is committed
# or pushed here. The run is started, its URL printed, followed until it ends, and its artifacts downloaded.
ci_build() {
  die() { echo "ci: $*" >&2; exit 1; }
  # a real start is refused here, but a dry run runs the same checks and only reports them
  run() { if [ "$dry" = 1 ]; then echo "  [dry-run] $*"; else "$@"; fi; }
  command -v gh >/dev/null || die "gh (the GitHub CLI) not found: install it from https://cli.github.com, then gh auth login"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in to GitHub: run gh auth login"
  cd "$root"
  # GitHub only starts a manual workflow it has on the default branch: push .github/workflows/build.yml there first
  gh workflow view build.yml >/dev/null 2>&1 || die "GitHub has no build.yml workflow yet: merge .github/workflows/build.yml to the default branch and push it"
  branch=$(git symbolic-ref --quiet --short HEAD) || die "HEAD is not on a branch: check out the branch to build"
  [ -z "$(git status --porcelain)" ] || die "there are uncommitted changes: commit them and push, since the runners build what is on GitHub"
  upstream=$(git rev-parse --quiet --verify "@{u}") || die "branch $branch has no upstream: push it first (git push -u origin $branch)"
  [ "$(git rev-parse HEAD)" = "$upstream" ] || die "branch $branch is not the same as its upstream: push it first"
  case "$target" in
    deb|rpm) ci_target=linux; package="$target" ;;
    linux) ci_target=linux ;;
    mac|macos) ci_target=mac ;;
    windows|win) ci_target=windows ;;
    *) die "unknown target: $target (mac, linux, deb, rpm, windows)" ;;
  esac
  [ -z "$package" ] || [ "$ci_target" = linux ] || die "--package only applies to --target linux"
  [ -z "$arch" ] || [ "$ci_target" = mac ] || die "--arch only applies to --target mac"
  # the runs of this workflow on this branch: the one we start is the first newer than the last one seen
  last=$(gh run list --workflow build.yml --branch "$branch" --limit 1 --json databaseId --jq '.[0].databaseId // 0')
  echo "== building $ci_target on GitHub from $branch ($(git rev-parse --short HEAD)) =="
  run gh workflow run build.yml --ref "$branch" -f "target=$ci_target" -f "package=${package:-}" -f "arch=${arch:-}"
  if [ "$dry" = 1 ]; then echo "dry run: nothing was started"; return; fi
  id=0
  for _ in $(seq 1 30); do
    id=$(gh run list --workflow build.yml --branch "$branch" --limit 1 --json databaseId --jq '.[0].databaseId // 0')
    [ "$id" -gt "$last" ] && break
    sleep 2
  done
  [ "$id" -gt "$last" ] || die "the run did not appear: see gh run list --workflow build.yml"
  echo "  run $id: $(gh run view "$id" --json url --jq .url)"
  if ! gh run watch "$id" --exit-status; then
    die "the run failed: gh run view $id --log-failed shows why"
  fi
  tmp=$(mktemp -d)
  gh run download "$id" -D "$tmp"
  mkdir -p "$dist"
  echo "done: the files of run $id, in $dist:"
  find "$tmp" -type f | sort | while IFS= read -r f; do
    cp "$f" "$dist/"
    echo "  $dist/$(basename "$f")"
  done
  rm -rf "$tmp"
}

release_it() {
  die() { echo "release: $*" >&2; exit 1; }
  # run a command, or only show it with --dry-run
  run() { if [ "$dry" = 1 ]; then echo "  [dry-run] $*"; else "$@"; fi; }
  # a problem stops a real release; a dry run only reports it
  problem() { if [ "$dry" = 1 ]; then echo "  [dry-run] would stop: $*"; else die "$*"; fi; }

  command -v gh >/dev/null || die "gh (the GitHub CLI) is not installed"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
  # the version to release: the one given (--release 0.5.3), else the current one
  local version="${rel_version:-$version}"
  local tag="v$version"
  shopt -s nullglob
  local assets=("$dist"/*_"$version"_* "$dist"/*-"$version"-*)
  shopt -u nullglob
  [ "${#assets[@]}" -gt 0 ] || die "no files for $version in $dist (build it first: $0 --target <t>)"

  cd "$root"
  git fetch -q origin 2>/dev/null || true
  local commit
  if [ -n "$rel_commit" ]; then
    commit=$(git rev-parse -q --verify "$rel_commit^{commit}") || die "$rel_commit is not a commit"
    [ -n "$(git branch -r --contains "$commit" 2>/dev/null)" ] || problem "commit $(git rev-parse --short "$commit") is not on GitHub: push it first"
  elif [ "$version" = "$current" ]; then
    commit=$(git rev-parse HEAD)
    [ -z "$(git status --porcelain)" ] || problem "uncommitted changes: commit them first (this script never commits)"
    local upstream
    upstream=$(git rev-parse --abbrev-ref '@{u}' 2>/dev/null) || upstream=""
    if [ -z "$upstream" ]; then
      problem "this branch has no upstream: push it first"
    elif [ "$commit" != "$(git rev-parse '@{u}')" ]; then
      problem "HEAD is not the same as $upstream: push (or pull) first"
    fi
  else
    # an older version: its tag if there is one, else the newest commit whose rust/Cargo.toml says that version
    if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
      commit=$(git rev-list -n1 "$tag")
    else
      commit=""
      local c
      for c in $(git log --format=%H -- rust/Cargo.toml); do
        if [ "$(git show "$c:rust/Cargo.toml" 2>/dev/null | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)" = "$version" ]; then commit=$c; break; fi
      done
      [ -n "$commit" ] || die "no tag $tag and no commit with version $version in rust/Cargo.toml"
    fi
    [ -n "$(git branch -r --contains "$commit" 2>/dev/null)" ] || problem "commit $(git rev-parse --short "$commit") is not on GitHub: push it first"
  fi
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null && [ "$(git rev-list -n1 "$tag")" != "$commit" ]; then
    problem "tag $tag already exists on another commit"
  fi

  local notes
  notes=$(mktemp)
  awk -v ver="$version" 'index($0, "## [" ver "]") == 1 { p = 1; next } /^## \[/ { p = 0 } p' "$root/CHANGELOG.md" > "$notes"
  local notes_args=(--notes-file "$notes")
  if [ ! -s "$notes" ]; then
    echo "  CHANGELOG.md has no section for $version: GitHub will generate the notes"
    notes_args=(--generate-notes)
  fi

  local exists=0
  gh release view "$tag" >/dev/null 2>&1 && exists=1
  echo "== release $tag =="
  echo "  commit:  $(git log -1 --format='%h %s' "$commit")"
  echo "  files:"
  printf '    %s\n' "${assets[@]##*/}"
  if [ "$exists" = 1 ]; then echo "  the release exists: the files above are added to it"; else echo "  a new release will be created$([ "$draft" = 1 ] && echo ' (draft)')"; fi
  if [ "$yes" != 1 ] && [ "$dry" != 1 ]; then
    read -r -p "Tag, push and publish? [y/N] " ans
    case "$ans" in y|Y|yes) ;; *) echo "stopped"; exit 1 ;; esac
  fi

  git rev-parse -q --verify "refs/tags/$tag" >/dev/null || run git tag -a "$tag" -m "mau $version" "$commit"
  run git push origin "$tag"
  if [ "$exists" = 1 ]; then
    run gh release upload "$tag" "${assets[@]}" --clobber
  else
    local draft_args=()
    [ "$draft" = 1 ] && draft_args=(--draft)
    run gh release create "$tag" "${assets[@]}" --title "mau $version" "${notes_args[@]}" ${draft_args[@]+"${draft_args[@]}"}
  fi
  rm -f "$notes"
  if [ "$dry" = 1 ]; then echo "dry run: nothing was tagged, pushed or published"; return; fi
  echo "release done: $(gh release view "$tag" --json url -q .url 2>/dev/null || echo "$tag")"
}

delete_release() {
  die() { echo "delete-release: $*" >&2; exit 1; }
  run() { if [ "$dry" = 1 ]; then echo "  [dry-run] $*"; else "$@"; fi; }
  command -v gh >/dev/null || die "gh (the GitHub CLI) is not installed"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
  local tag="v${del_version:-$version}"
  cd "$root"
  local exists=0 remote_tag=0 local_tag=0
  gh release view "$tag" >/dev/null 2>&1 && exists=1
  git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1 && remote_tag=1
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null && local_tag=1
  [ "$exists" = 1 ] || [ "$remote_tag" = 1 ] || [ "$local_tag" = 1 ] || die "no release or tag $tag found"
  echo "== delete release $tag =="
  [ "$exists" = 1 ] && echo "  GitHub release: $(gh release view "$tag" --json url -q .url) (with its files)" || echo "  no GitHub release"
  if [ "$keep_tag" = 1 ]; then
    echo "  the tag is kept"
  else
    echo "  tag on GitHub: $([ "$remote_tag" = 1 ] && echo yes || echo no), local tag: $([ "$local_tag" = 1 ] && echo yes || echo no)"
  fi
  if [ "$yes" != 1 ] && [ "$dry" != 1 ]; then
    read -r -p "Delete? This cannot be undone. [y/N] " ans
    case "$ans" in y|Y|yes) ;; *) echo "stopped"; exit 1 ;; esac
  fi
  if [ "$exists" = 1 ]; then
    if [ "$keep_tag" = 1 ]; then run gh release delete "$tag" --yes; else run gh release delete "$tag" --yes --cleanup-tag; fi
  fi
  if [ "$keep_tag" != 1 ]; then
    [ "$exists" = 1 ] || [ "$remote_tag" = 0 ] || run git push origin ":refs/tags/$tag"
    [ "$local_tag" = 0 ] || run git tag -d "$tag"
  fi
  echo "delete done: $tag"
}

if [ "$delete" = 1 ]; then delete_release; exit 0; fi
if [ -n "$target" ] && [ -z "$where" ]; then
  if [ -t 0 ]; then
    printf 'Build locally or on GitHub CI? [local/ci] (local): '
    answer=""; read -r answer || true
    case "$answer" in
      ci|CI|c|C) where=ci ;;
      ""|local|LOCAL|l|L) where=local ;;
      *) echo "answer local or ci" >&2; exit 1 ;;
    esac
  else
    where=local
  fi
fi
if [ -n "$target" ] && [ "$where" = ci ] && [ "$lgpl" = 1 ]; then
  echo "--lgpl is for a local build: the GitHub runners do not build ffmpeg" >&2; exit 1
fi
if [ -n "$target" ] && [ "$where" = ci ]; then ci_build; exit 0; fi
if [ -n "$target" ]; then build_target; fi
if [ "$release" = 1 ]; then release_it; fi
