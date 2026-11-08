"""Build one-track MP4 files of subtitle samples: WebVTT (``wvtt``) and TTML (``stpp``), for the
tests that read them. ffmpeg in the test environment can't read these tracks, so the expected cues
are written out by hand in the tests."""

from __future__ import annotations

import struct
from pathlib import Path


def box(kind: bytes, *parts: bytes) -> bytes:
    body = b"".join(parts)
    return struct.pack(">I", 8 + len(body)) + kind + body


def full(kind: bytes, version_flags: int, *parts: bytes) -> bytes:
    return box(kind, struct.pack(">I", version_flags), *parts)


def _parts(samples: list[tuple[int, bytes]], entry: bytes,
           chunk_offset: int) -> tuple[bytes, bytes, bytes]:
    """The ftyp, moov and mdat boxes, with the one chunk of samples at ``chunk_offset``."""
    durations = [duration for duration, _ in samples]
    ftyp = box(b"ftyp", b"isom", struct.pack(">I", 0), b"isom")
    mdat = box(b"mdat", b"".join(sample for _, sample in samples))
    total = sum(durations)
    mvhd = full(b"mvhd", 0, struct.pack(">IIII", 0, 0, 1000, total), b"\0" * 80)
    tkhd = full(b"tkhd", 3, struct.pack(">IIII", 0, 0, 1, 0), struct.pack(">I", total), b"\0" * 60)
    mdhd = full(b"mdhd", 0, struct.pack(">IIII", 0, 0, 1000, total), struct.pack(">HH", 0x55C4, 0))
    hdlr = full(b"hdlr", 0, struct.pack(">I", 0), b"sbtl", b"\0" * 12, b"SubtitleHandler\0")
    dinf = box(b"dinf", full(b"dref", 0, struct.pack(">I", 1), full(b"url ", 1)))
    stsd = full(b"stsd", 0, struct.pack(">I", 1), entry)
    stts = full(b"stts", 0, struct.pack(">I", len(durations)),
                b"".join(struct.pack(">II", 1, duration) for duration in durations))
    stsc = full(b"stsc", 0, struct.pack(">I", 1), struct.pack(">III", 1, len(durations), 1))
    stsz = full(b"stsz", 0, struct.pack(">II", 0, len(samples)),
                b"".join(struct.pack(">I", len(sample)) for _, sample in samples))
    stco = full(b"stco", 0, struct.pack(">II", 1, chunk_offset))
    stbl = box(b"stbl", stsd, stts, stsc, stsz, stco)
    minf = box(b"minf", full(b"nmhd", 0), dinf, stbl)
    trak = box(b"trak", tkhd, box(b"mdia", mdhd, hdlr, minf))
    return ftyp, box(b"moov", mvhd, trak), mdat


def subtitle_mp4(path: Path, entry: bytes, samples: list[tuple[int, bytes]]) -> None:
    """Write the samples (a duration in milliseconds and the bytes, for each) as one chunk."""
    # the moov box comes before the mdat box, and the chunk offset is after both headers: the
    # moov's size does not depend on the offset, so it is built once to find its size, then again
    ftyp, moov, _ = _parts(samples, entry, 0)
    ftyp, moov, mdat = _parts(samples, entry, len(ftyp) + len(moov) + 8)
    path.write_bytes(ftyp + moov + mdat)


def wvtt_entry() -> bytes:
    return box(b"wvtt", b"\0" * 6, struct.pack(">H", 1), box(b"vttC", b"WEBVTT\n"))


def stpp_entry() -> bytes:
    namespace = b"http://www.w3.org/ns/ttml\0"
    return box(b"stpp", b"\0" * 6, struct.pack(">H", 1), namespace, b"\0", b"\0")


def vttc(text: bytes) -> bytes:
    """A WebVTT cue sample: a ``vttc`` box with its text in a ``payl`` box."""
    return box(b"vttc", box(b"payl", text))


def empty_vtt() -> bytes:
    return box(b"vtte")


def ttml(body: str) -> bytes:
    return ('<?xml version="1.0" encoding="utf-8"?>\n<tt xmlns="http://www.w3.org/ns/ttml"><body>'
            f'<div>{body}</div></body></tt>').encode()
