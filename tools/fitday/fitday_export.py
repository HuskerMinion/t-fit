#!/usr/bin/env python3
"""
Export the weight log from a FitDay PC .fdy / .fbk file to CSV.

FitDay PC was abandoned around 2004 and has no export. This reads its
undocumented binary directly and recovers dates, weights and the full note
text — including notes far longer than FitDay's own printout will show you.

This is a standalone copy of the decoder in t-fit's src/fitday.rs, for people
who just want a CSV without building anything. `t-fit --import-fitday` does
the same thing in one step.

    python3 fitday_export.py MyAccount.fdy -o weight_history.csv

See FORMAT.md for how the format works.
"""

import argparse
import csv
import datetime as dt
import struct
import sys

MAGIC = bytes([0xDE, 0xFE, 0xC8, 0x42])
PAGE, HEADER = 4096, 128
SLOTS = (PAGE - HEADER) // 8      # 8-byte slots of usable space per page
UNIT_LB = 100
DATE_MARKER = 0x000F4240
MIN_CHAIN, MIN_LEAF, LEAF_GAP = 5, 100, 200


def read_record(b, off):
    """f32 weight, u32 unit, u32 flags, u32 note_len, note; padded to 8."""
    if off < 0 or off + 16 > len(b):
        return None
    weight, unit, _flags, note_len = struct.unpack_from("<fIII", b, off)
    if unit != UNIT_LB or not (50.0 < weight < 600.0) or note_len > 4000:
        return None
    if off + 16 + note_len > len(b):
        return None
    note = b[off + 16:off + 16 + note_len].decode("latin-1", "replace")
    return {
        "weight_lb": round(weight, 1),
        "note": note.replace("\x00", "").strip(),
        "size": 16 + ((note_len + 7) // 8) * 8,
    }


def record_pages(b):
    """4096-byte pages holding weight records, in file order. A page counts
    when a run of records walks cleanly through it — which rules out floats
    that merely look like a plausible body weight."""
    pages = set()
    for off in range(0, len(b) - 16, 8):
        if read_record(b, off) is None:
            continue
        cur, n = off, 0
        while True:
            r = read_record(b, cur)
            if r is None or n > MIN_CHAIN:
                break
            n += 1
            cur += r["size"]
        if n >= MIN_CHAIN:
            pages.add(off // PAGE)
    return sorted(pages)


def index_entries(b):
    """24-byte date records: unix midnight, sub-index, marker, pointer."""
    lo = int(dt.datetime(2000, 1, 1).timestamp())
    hi = int(dt.datetime(2100, 1, 1).timestamp())
    out = []
    for off in range(0, len(b) - 24, 4):
        stamp, = struct.unpack_from("<I", b, off)
        if not (lo < stamp < hi):
            continue
        when = dt.datetime.utcfromtimestamp(stamp)
        if (when.hour, when.minute, when.second) != (0, 0, 0):
            continue
        _sub, marker, ptr = struct.unpack_from("<III", b, off + 4)
        if marker != DATE_MARKER:
            continue
        out.append({"offset": off, "date": when.date().isoformat(), "ptr": ptr})
    return out


def weight_log_leaves(entries):
    """Keep the long contiguous runs — the weight log's B-tree leaves. Short
    runs index other sections (food, exercise); following them would attach
    real weights to the wrong days."""
    if not entries:
        return entries
    runs, current = [], [entries[0]]
    for e in entries[1:]:
        if e["offset"] - current[-1]["offset"] > LEAF_GAP:
            runs.append(current)
            current = [e]
        else:
            current.append(e)
    runs.append(current)
    long_runs = [r for r in runs if len(r) >= MIN_LEAF]
    return [e for r in (long_runs or runs) for e in r]


def parse(b):
    if b[:4] != MAGIC:
        raise SystemExit(f"not a FitDay file — expected it to start with "
                         f"{MAGIC.hex(' ')}, found {b[:4].hex(' ')}")
    pages = record_pages(b)
    if not pages:
        raise SystemExit("no weight records found in this file")

    rows, unresolved = {}, 0
    for e in weight_log_leaves(index_entries(b)):
        k, rem = divmod(e["ptr"], SLOTS)
        if k >= len(pages):
            unresolved += 1
            continue
        rec = read_record(b, pages[k] * PAGE + HEADER + rem * 8)
        if rec is None:
            unresolved += 1
            continue
        rows.setdefault(e["date"], rec)
    return dict(sorted(rows.items())), unresolved


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("file")
    ap.add_argument("-o", "--out", default="weight_history.csv")
    args = ap.parse_args()

    rows, unresolved = parse(open(args.file, "rb").read())
    with open(args.out, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["date", "weight_lb", "memo"])
        for day, rec in rows.items():
            w.writerow([day, f"{rec['weight_lb']:.1f}", rec["note"]])

    noted = sum(1 for r in rows.values() if r["note"])
    days = list(rows)
    print(f"{len(rows)} days written to {args.out} ({noted} with notes)")
    if days:
        print(f"  {days[0]} → {days[-1]}")
    if unresolved:
        print(f"  warning: {unresolved} index entries didn't resolve to a "
              f"record — the decode may be incomplete", file=sys.stderr)


if __name__ == "__main__":
    main()
