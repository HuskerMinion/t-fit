"""Shared decoding for FitDay PC `.fdy` / `.fbk` files.

This mirrors what `src/fitday.rs` already knows, and stops where that module
stops. Read the doc comment at the top of `fitday.rs` first — it describes
the page layout, the weight record, the 24-byte date index record, and the
pointer scheme, which is the part that took real work to establish.

What's here that isn't in the Rust: the parts needed to look at a file
rather than decode one. `fitday.rs` deliberately throws away every index run
shorter than 100 entries because those belong to other sections (food,
exercise) and would decode into nonsense as weights. This module keeps them,
so we can find out what they actually are.

Nothing here is authoritative about food or exercise records. Their layout
is unknown; these helpers only locate and display candidate bytes.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass
from datetime import date, datetime, timezone

MAGIC = bytes([0xDE, 0xFE, 0xC8, 0x42])
PAGE = 4096
HEADER = 128
#: 8-byte slots of usable space in a record page.
SLOTS = (PAGE - HEADER) // 8

UNIT_LB = 100
DATE_MARKER = 0x000F_4240

_EPOCH_LO = 946_684_800    # 2000-01-01
_EPOCH_HI = 4_102_444_800  # 2100-01-01

#: `fitday.rs` uses these to isolate the weight log; kept identical so this
#: module and the parser agree on what "a weight run" means.
MIN_CHAIN = 5
MIN_LEAF = 100
LEAF_GAP = 200


def u32(b: bytes, off: int):
    if off + 4 > len(b) or off < 0:
        return None
    return struct.unpack_from("<I", b, off)[0]


def f32(b: bytes, off: int):
    if off + 4 > len(b) or off < 0:
        return None
    return struct.unpack_from("<f", b, off)[0]


# ── weight records ──────────────────────────────────────────────────

@dataclass
class WeightRecord:
    weight_lb: float
    note: str
    size: int  # total bytes including padding


def read_weight_record(b: bytes, off: int):
    """The same test `fitday.rs` applies: unit must be pounds and the value
    must be a plausible body weight. Anything else isn't a weight record —
    which is exactly why food and exercise rows are invisible to it."""
    weight, unit = f32(b, off), u32(b, off + 4)
    note_len = u32(b, off + 12)
    if weight is None or unit is None or note_len is None:
        return None
    if unit != UNIT_LB or not (50.0 <= weight < 600.0) or note_len > 4000:
        return None
    raw = b[off + 16: off + 16 + note_len]
    if len(raw) != note_len:
        return None
    note = raw.decode("latin-1").replace("\x00", "").strip()
    return WeightRecord(round(weight, 1), note, 16 + -(-note_len // 8) * 8)


def weight_record_pages(b: bytes) -> list[int]:
    """Pages holding a clean run of weight records, in file order."""
    pages, off = set(), 0
    while off + 16 <= len(b):
        if read_weight_record(b, off) is not None:
            cursor, n = off, 0
            while n <= MIN_CHAIN:
                r = read_weight_record(b, cursor)
                if r is None:
                    break
                n += 1
                cursor += r.size
            if n >= MIN_CHAIN:
                pages.add(off // PAGE)
        off += 8
    return sorted(pages)


# ── the date index ──────────────────────────────────────────────────

@dataclass
class IndexEntry:
    offset: int
    day: date
    sub: int
    ptr: int

    @property
    def page(self) -> int:
        return self.offset // PAGE


def index_entries(b: bytes) -> list[IndexEntry]:
    """Every 24-byte date index record in the file — all sections, not just
    the weight log. Matching is by the 0x000F4240 marker plus a timestamp
    landing exactly on midnight UTC, same as the Rust."""
    out, off = [], 0
    while off + 24 <= len(b):
        stamp, marker, ptr = u32(b, off), u32(b, off + 8), u32(b, off + 12)
        if marker == DATE_MARKER and stamp is not None and _EPOCH_LO < stamp < _EPOCH_HI:
            dt = datetime.fromtimestamp(stamp, timezone.utc)
            if (dt.hour, dt.minute, dt.second) == (0, 0, 0):
                out.append(IndexEntry(off, dt.date(), u32(b, off + 4), ptr))
        off += 4
    return out


def runs(entries: list[IndexEntry], gap: int = LEAF_GAP) -> list[list[IndexEntry]]:
    """Split index entries into contiguous runs. A run is one B-tree leaf;
    long runs are the weight log, short ones are everything else."""
    if not entries:
        return []
    out, cur = [], [entries[0]]
    for e in entries[1:]:
        if e.offset - cur[-1].offset > gap:
            out.append(cur)
            cur = []
        cur.append(e)
    out.append(cur)
    return out


def resolve(ptr: int, record_pages: list[int]):
    """Pointer → file offset, through the virtual space of `record_pages`.

    The pointer counts 8-byte slots across those pages' usable areas
    concatenated; page headers aren't in that space. Which pages belong to
    the space depends on the section, so the caller supplies them — for the
    weight log that's `weight_record_pages()`, for anything else it's the
    open question.
    """
    k, rem = divmod(ptr, SLOTS)
    if k >= len(record_pages):
        return None
    return record_pages[k] * PAGE + HEADER + rem * 8


# ── looking at unknown bytes ────────────────────────────────────────

def ascii_strings(b: bytes, minlen: int = 4) -> list[tuple[int, str]]:
    """Printable runs and where they start. Food names, if they're stored
    inline at all, will show up here."""
    out, start, buf = [], None, bytearray()
    for i, c in enumerate(b):
        if 0x20 <= c < 0x7F:
            if start is None:
                start = i
            buf.append(c)
        else:
            if start is not None and len(buf) >= minlen:
                out.append((start, buf.decode("ascii")))
            start, buf = None, bytearray()
    if start is not None and len(buf) >= minlen:
        out.append((start, buf.decode("ascii")))
    return out


def hexdump(b: bytes, off: int, length: int = 64, width: int = 16) -> str:
    lines = []
    for i in range(0, length, width):
        chunk = b[off + i: off + i + width]
        if not chunk:
            break
        hexpart = " ".join(f"{c:02x}" for c in chunk).ljust(width * 3 - 1)
        text = "".join(chr(c) if 0x20 <= c < 0x7F else "." for c in chunk)
        lines.append(f"  {off + i:08x}  {hexpart}  |{text}|")
    return "\n".join(lines)


def page_profile(b: bytes, p: int) -> dict:
    """Cheap shape summary of one page: enough to tell a mostly-empty page
    from a dense one, and text from binary.

    `nonzero` is the count, not a ratio, and that matters: a page holding a
    dozen small records is over 98% zero bytes and would read as "empty" on
    a ratio, which is precisely the kind of page worth looking at.
    """
    chunk = b[p * PAGE:(p + 1) * PAGE]
    if not chunk:
        return {"zero": 1.0, "printable": 0.0, "nonzero": 0, "bytes": 0}
    zeros = chunk.count(0)
    printable = sum(1 for c in chunk if 0x20 <= c < 0x7F)
    return {
        "zero": zeros / len(chunk),
        "printable": printable / len(chunk),
        "nonzero": len(chunk) - zeros,
        "bytes": len(chunk),
    }


def load(path: str) -> bytes:
    with open(path, "rb") as fh:
        b = fh.read()
    if len(b) < PAGE:
        raise SystemExit(f"{path}: too small to be a FitDay file ({len(b)} bytes)")
    if b[:4] != MAGIC:
        raise SystemExit(f"{path}: not a FitDay file — starts with {b[:4].hex()}, expected {MAGIC.hex()}")
    return b
