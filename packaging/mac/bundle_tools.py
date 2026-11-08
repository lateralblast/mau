#!/usr/bin/env python3
"""Copy command-line tools, and every library they load that is not part of macOS, into one folder
that can be moved about: the tools go in OUT/bin and the libraries in OUT/lib.

  usage: bundle_tools.py [--plugins DIR] OUT TOOL [TOOL ...]

--plugins DIR copies the shared objects in DIR (whisper.cpp's ggml backends) into OUT/bin, beside
the tools: ggml looks for its backends in the folder of the program that runs (it does not read
GGML_BACKEND_DIR), and the libraries they load are rewritten in the same way as the tools'.

Each reference to a library that is not in /usr/lib or /System is rewritten (install_name_tool)
to a path relative to the copy: a tool refers to @loader_path/../lib/<name>, a library to
@loader_path/<name>. The copies are not signed: build.sh --sandboxed signs them. Needs otool and
install_name_tool (Xcode's command line tools) and the tools' own libraries (Homebrew, or wherever
they are installed).
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

SYSTEM = ("/usr/lib/", "/System/")


def run(*args: str) -> str:
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout


def install_names(path: Path) -> list[str]:
    """The library names a file loads, as written in it (otool -L's first line is the file)."""
    names = []
    for line in run("otool", "-L", str(path)).splitlines()[1:]:
        names.append(line.strip().split(" (")[0])
    return names


def rpaths(path: Path) -> list[str]:
    """The LC_RPATH entries of a file, with @loader_path and @executable_path resolved."""
    found = []
    lines = run("otool", "-l", str(path)).splitlines()
    for index, line in enumerate(lines):
        if line.strip() == "cmd LC_RPATH" and index + 2 < len(lines):
            entry = lines[index + 2].strip().split(" (offset")[0].removeprefix("path ")
            here = str(path.parent)
            found.append(entry.replace("@loader_path", here).replace("@executable_path", here))
    return found


def locate(name: str, owner: Path) -> Path:
    """Where a library a file loads really is (@rpath names are looked for along its rpaths)."""
    if name.startswith("/"):
        candidates = [Path(name)]
    elif name.startswith("@rpath/"):
        base = name.removeprefix("@rpath/")
        folders = rpaths(owner) + ["/opt/homebrew/lib", "/usr/local/lib"]
        candidates = [Path(folder) / base for folder in folders]
    elif name.startswith(("@loader_path/", "@executable_path/")):
        candidates = [owner.parent / name.split("/", 1)[1]]
    else:
        raise SystemExit(f"cannot find {name} (needed by {owner})")
    for candidate in candidates:
        if candidate.exists():
            return candidate.resolve()
    raise SystemExit(f"cannot find {name} (needed by {owner})")


def bundle(
    out: Path, source: Path, copy_to: Path, link_prefix: str, bundled: dict[str, str]
) -> None:
    """Make `copy_to` (a copy of `source`) use its libraries in the bundle, copying new ones."""
    for name in install_names(source):
        if name.startswith(SYSTEM):
            continue
        real = locate(name, source)
        base = Path(name).name
        if real not in bundled and base not in bundled.values():
            target = out / "lib" / base
            shutil.copy2(real, target)
            target.chmod(0o755)
            set_id = ["install_name_tool", "-id", f"@loader_path/{base}", str(target)]
            subprocess.run(set_id, check=True)
            bundled[real] = base
            bundle(out, real, target, "@loader_path/", bundled)
        elif real not in bundled:
            raise SystemExit(f"two different libraries are named {base}: {real} and another")
        new_name = f"{link_prefix}{bundled[real]}"
        subprocess.run(["install_name_tool", "-change", name, new_name, str(copy_to)], check=True)


def main(argv: list[str]) -> None:
    plugins = None
    if len(argv) > 2 and argv[1] == "--plugins":
        plugins = Path(argv[2])
        argv = argv[:1] + argv[3:]
    if len(argv) < 3:
        raise SystemExit(__doc__)
    out = Path(argv[1])
    tools = [Path(t) for t in argv[2:]]
    (out / "bin").mkdir(parents=True, exist_ok=True)
    (out / "lib").mkdir(parents=True, exist_ok=True)
    bundled: dict[str, str] = {}
    sources: list[str] = []
    if plugins is not None:
        for plugin in sorted(plugins.glob("*.so")):
            real = plugin.resolve()
            target = out / "bin" / plugin.name
            shutil.copy2(real, target)
            target.chmod(0o755)
            bundle(out, real, target, "@loader_path/../lib/", bundled)
            sources.append(str(real))
            print(f"  bin/{plugin.name} (from {real})")
    for tool in tools:
        real = tool.resolve()
        target = out / "bin" / tool.name
        shutil.copy2(real, target)
        target.chmod(0o755)
        bundle(out, real, target, "@loader_path/../lib/", bundled)
        sources.append(str(real))
        print(f"  bin/{tool.name} (from {real})")
    for real, base in sorted(bundled.items(), key=lambda item: item[1]):
        print(f"  lib/{base} (from {real})")
    # the real path of every file copied, for collect_notices.py (it finds each package's licence)
    sources += [str(real) for real in bundled]
    (out / "sources.txt").write_text("\n".join(sorted(set(sources))) + "\n")
    total = sum(p.stat().st_size for p in out.rglob("*") if p.is_file())
    print(f"{len(tools)} tools, {len(bundled)} libraries, {total / 1048576:.1f} MB in {out}")


if __name__ == "__main__":
    main(sys.argv)
