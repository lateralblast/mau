"""Add a second audio stream to an AVI, keeping the file's own layout: the copy's chunks go into
the same movi (and rec) lists right after each first audio chunk, the header gets a copy of the
audio stream list after the last stream list, and the index is rebuilt in the file's own
convention (offsets from the movi fourcc, or from the start of the file). The second audio is the
first with every byte inverted: the same size, a different sound."""

from __future__ import annotations

import struct
from pathlib import Path


def _chunks(data: bytes, start: int, end: int) -> list[tuple[bytes, int, int]]:
    out, at = [], start
    while at + 8 <= end:
        size = struct.unpack("<I", data[at + 4:at + 8])[0]
        out.append((data[at:at + 4], at, size))
        at += 8 + size + (size & 1)
    return out


def _pad(body: bytes) -> bytes:
    return body + (b"\0" if len(body) & 1 else b"")


def _chunk(cid: bytes, body: bytes) -> bytes:
    return cid + struct.pack("<I", len(body)) + _pad(body)


def _raw(data: bytes, at: int, size: int) -> bytes:
    return data[at:at + 8 + size + (size & 1)]


def add_second_audio(source: Path, dest: Path) -> None:
    data = source.read_bytes()
    top = _chunks(data, 12, len(data))
    hdrl = next(c for c in top if c[0] == b"LIST" and data[c[1] + 8:c[1] + 12] == b"hdrl")
    movi = next(c for c in top if c[0] == b"LIST" and data[c[1] + 8:c[1] + 12] == b"movi")
    idx = next(c for c in top if c[0] == b"idx1")
    kids = _chunks(data, hdrl[1] + 12, hdrl[1] + 8 + hdrl[2])
    strls = [c for c in kids if c[0] == b"LIST" and data[c[1] + 8:c[1] + 12] == b"strl"]
    audio_strl = strls[1]  # the first audio stream
    movi_fourcc = movi[1] + 8
    old_index = [data[idx[1] + 8 + 16 * i: idx[1] + 24 + 16 * i] for i in range(idx[2] // 16)]
    relative = all(
        data[movi_fourcc + struct.unpack("<I", e[8:12])[0]:][:4] == e[:4] or e[:4] == b"rec "
        for e in old_index
    )

    # the header: the same boxes, avih with one more stream, and the copy after the last stream list
    hdrl_body = bytearray(b"hdrl")
    for c in kids:
        raw = bytearray(_raw(data, c[1], c[2]))
        if c[0] == b"avih":
            raw[8 + 24:8 + 28] = struct.pack("<I", len(strls) + 1)
        hdrl_body += raw
        if c[1] == strls[-1][1]:
            hdrl_body += _raw(data, audio_strl[1], audio_strl[2])
    hdrl_out = b"LIST" + struct.pack("<I", len(hdrl_body)) + bytes(hdrl_body)

    # the movi list, rebuilt: each 01wb gets a 02wb twin after it, and rec lists are rebuilt
    place: dict[object, int] = {}
    sizes: dict[int, int] = {}

    def rebuild(start: int, end: int, base: int) -> bytes:
        out = bytearray()
        for cid, at, size in _chunks(data, start, end):
            here = base + len(out)
            if cid == b"LIST" and data[at + 8:at + 12] == b"rec ":
                place[at] = here
                inner = rebuild(at + 12, at + 8 + size, here + 12)
                out += b"LIST" + struct.pack("<I", 4 + len(inner)) + b"rec " + inner
                sizes[at] = 4 + len(inner)
                continue
            place[at] = here
            body = data[at + 8:at + 8 + size]
            out += _chunk(cid, body)
            if cid == b"01wb":
                place[("twin", at)] = base + len(out)
                out += _chunk(b"02wb", bytes(b ^ 0xFF for b in body))
        return bytes(out)

    movi_body = rebuild(movi[1] + 12, movi[1] + 8 + movi[2], 4)
    movi_out = b"LIST" + struct.pack("<I", 4 + len(movi_body)) + b"movi" + movi_body

    # where the movi fourcc is in the new file, for absolute offsets
    before_movi = b"".join(
        _raw(data, c[1], c[2]) for c in top if c[1] < movi[1] and c[1] != hdrl[1]
    )
    movi_fourcc_out = 12 + len(hdrl_out) + len(before_movi) + 8

    def offset(place_in_movi: int) -> int:
        return place_in_movi if relative else movi_fourcc_out + place_in_movi

    new_index = bytearray()
    for e in old_index:
        cid = e[:4]
        stored = struct.unpack("<I", e[8:12])[0]
        old = movi_fourcc + stored if relative else stored
        flags = struct.unpack("<I", e[4:8])[0]
        if cid == b"rec ":
            new_index += cid + struct.pack("<II", flags, offset(place[old]))
            new_index += struct.pack("<I", sizes[old])
            continue
        length = struct.unpack("<I", e[12:16])[0]
        new_index += cid + struct.pack("<II", flags, offset(place[old])) + struct.pack("<I", length)
        if cid == b"01wb":
            twin = offset(place[("twin", old)])
            new_index += b"02wb" + struct.pack("<II", flags, twin) + struct.pack("<I", length)
    idx_out = b"idx1" + struct.pack("<I", len(new_index)) + bytes(new_index)

    rest = b"".join(_raw(data, c[1], c[2]) for c in top if c[1] > movi[1] and c[1] != idx[1])
    body = b"AVI " + hdrl_out + before_movi + movi_out + idx_out + rest
    dest.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)
