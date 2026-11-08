"""Write PGS (``.sup``) subtitle files for the tests: display sets of one rectangle, and cleared
sets. Each display set is PCS, WDS, PDS, ODS and END segments with the ``PG`` header, which
mkvmerge reads."""

from __future__ import annotations

import struct
from pathlib import Path


def rle_row(colours: list[int]) -> bytes:
    """One row of a PGS object, run length encoded (a run of colour 0 is 0x00 then the count)."""
    out = bytearray()
    i = 0
    while i < len(colours):
        colour = colours[i]
        run = 1
        while i + run < len(colours) and colours[i + run] == colour and run < 16383:
            run += 1
        if colour == 0:
            if run <= 63:
                out += bytes([0, run])
            else:
                out += bytes([0, 0x40 | (run >> 8), run & 0xFF])
        elif run == 1:
            out.append(colour)
        elif run <= 63:
            out += bytes([0, 0x80 | run, colour])
        else:
            out += bytes([0, 0xC0 | (run >> 8), run & 0xFF, colour])
        i += run
    out += bytes([0, 0])  # the end of the row
    return bytes(out)


def segment(kind: int, pts: int, payload: bytes) -> bytes:
    header = b"PG" + struct.pack(">II", pts, pts) + bytes([kind]) + struct.pack(">H", len(payload))
    return header + payload


def display_set(pts: int, rect: tuple[int, int, int, int], colour: int,
                pattern: bool = False) -> list[bytes]:
    """A rectangle of ``colour`` at ``rect`` (x, y, width, height), shown at ``pts`` (90 kHz). With
    ``pattern`` the colour changes every three pixels, which makes the picture large."""
    x, y, width, height = rect
    pcs = (struct.pack(">HHBHBBBB", 1920, 1080, 0x10, 1, 0x80, 0, 0, 1)
           + struct.pack(">HBBHH", 0, 0, 0, x, y))
    wds = struct.pack(">B", 1) + struct.pack(">BHHHH", 0, x, y, width, height)
    pds = (struct.pack(">BB", 0, 0) + struct.pack(">BBBBB", 1, 16, 128, 128, 255)
           + struct.pack(">BBBBB", 2, 235, 128, 128, 255))
    if pattern:
        rows = [[colour + (i // 3 + j) % 2 for i in range(width)] for j in range(height)]
    else:
        rows = [[colour] * width for _ in range(height)]
    rle = b"".join(rle_row(row) for row in rows)
    ods = (struct.pack(">HBB", 0, 0, 0xC0) + struct.pack(">I", len(rle) + 4)[1:]
           + struct.pack(">HH", width, height) + rle)
    return [segment(0x16, pts, pcs), segment(0x17, pts, wds), segment(0x14, pts, pds),
            segment(0x15, pts, ods), segment(0x80, pts, b"")]


def cleared_set(pts: int) -> list[bytes]:
    """A display set with no objects: the picture is removed at ``pts``."""
    pcs = struct.pack(">HHBHBBBB", 1920, 1080, 0x10, 2, 0x00, 0, 0, 0)
    return [segment(0x16, pts, pcs), segment(0x80, pts, b"")]


def write_sup(path: Path, sets: list[list[bytes]]) -> None:
    path.write_bytes(b"".join(b"".join(display) for display in sets))
