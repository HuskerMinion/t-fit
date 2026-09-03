# The FitDay PC `.fdy` file format

Reverse engineered from FitDay PC 1.0.0.6 (Cyser Software, 2003–2004). This
covers the weight log only — the food and exercise sections live in the same
file and are not decoded here, though the same structures appear to apply.

Everything is **little-endian**.

## Header

The file starts with the magic bytes `DE FE C8 42`, followed by four more
bytes that vary between files.

## Pages

The file is a sequence of **4096-byte pages**. A page holding records begins
with a **128-byte header**, leaving 3968 bytes of usable space — exactly
**496 slots** of 8 bytes.

Record pages are scattered through the file, interleaved with index pages and
other sections. They are *not* contiguous, and that turns out to matter a
great deal (see Pointers).

## Weight records

```
offset  size  field
     0     4  f32  weight, in the unit given below
     4     4  u32  unit — 100 means pounds
     8     4  u32  flags (0 in every file examined)
    12     4  u32  note length in bytes
    16     n  u8[] note text, Windows-1252
```

The whole record is padded to a multiple of 8 bytes, so:

```
size = 16 + ceil(note_len / 8) * 8
```

Records sit end to end. Reading one tells you where the next begins, which is
how you walk a page without needing a count.

**Finding record pages:** scan every 8-byte-aligned offset for something that
parses as a record, then walk forward. A run of five or more is real data; a
shorter run is a float that happens to look like a plausible body weight. Any
page containing such a run is a record page.

## Date index

The dates live in a B-tree. Its leaf entries are 24 bytes:

```
offset  size  field
     0     4  u32  unix timestamp, always exactly midnight UTC
     4     4  u32  sub-index — 0 for the primary entry of a day
     8     4  u32  0x000F4240 (1,000,000) — a constant marker
    12     4  u32  pointer to the record (see below)
    16     8  u64  zero
```

The marker at offset 8 is the reliable way to find these; scanning for
plausible timestamps alone produces false positives.

**Leaves vs. everything else.** Index entries appear in contiguous runs. The
weight log's leaves are long — 165 entries in the files examined, with a
shorter final leaf. Short runs (1–73 entries) belong to other sections of the
file. Filtering to runs of 100+ is what separates the weight log from the
food and exercise logs; skipping this step attaches real weights to the wrong
days, which is worse than failing outright.

Group entries into runs by file offset, breaking wherever the gap exceeds
~200 bytes.

## Pointers — the part that isn't obvious

`ptr` is **not a file offset**, and it is not a file offset divided by
anything either. It counts 8-byte slots through a *virtual* address space
formed by concatenating the usable areas of the record pages — page headers
don't exist in that space, and neither do the pages in between that hold
other things.

```
page_index_in_sequence = ptr / 496
slot_within_page       = ptr % 496
file_offset = record_pages[page_index_in_sequence] * 4096 + 128 + slot_within_page * 8
```

where `record_pages` is the ascending list of page numbers found above.

### Why this is easy to get wrong

Fit a naive `file_offset = base + ptr * 8` to real data and it *works* — for a
few hundred records at a time. You get a series of bases that look arbitrary:

```
28800, 147712, 147840, 156160, 168576, 172800, 181120
```

They're not arbitrary. Each is `page_start - (page_index_in_sequence * 496 * 8)`,
and every difference between them is a multiple of 128. Chasing per-region
bases will fit one file and silently fail on the next; the page walk above is
the actual rule.

### Worked example

A file whose record pages are 7, 37, 38, 41, 45, 47, 50:

| ptr | page slot | within | file offset |
|---|---|---|---|
| 0 | 0 → page 7 | 0 | 7·4096 + 128 = **28800** |
| 1 | 0 → page 7 | 1 | 28808 |
| 496 | 1 → page 37 | 0 | 37·4096 + 128 = **151680** |
| 992 | 2 → page 38 | 0 | **155776** |

Note the jump from page 7 to page 37 at `ptr = 496`: thirty pages of the file
are skipped because they hold no records, and the pointer space doesn't know
they exist.

## Decoding, end to end

1. Check the magic bytes.
2. Find record pages by walking record chains; keep pages with a run of ≥5.
3. Find index entries by the `0x000F4240` marker and a midnight timestamp.
4. Group them into runs; keep runs of ≥100 (or all of them, on a small file).
5. Resolve each pointer with the page walk above and read the record there.
6. Deduplicate by date — a day may be indexed more than once; the first entry
   in file order is the primary one.

## Confidence

Validated against a 16-year, 803-entry file spanning 7 record pages, with an
independently established ground truth: **803/803 weights and 143/143 notes
decoded exactly.** Two independent implementations (Rust in `src/fitday.rs`,
Python in `fitday_export.py`) agree on every value.

Both implementations report unresolved pointers rather than guessing. If you
see that warning, the assumptions above don't hold for your file and the
result should not be trusted — please open an issue.

## Not yet decoded

- The 8 bytes after the magic
- Record page headers (128 bytes each)
- The B-tree's internal nodes — the leaves are found by scanning, not by
  descending the tree from a root
- The `flags` field on weight records, always 0 in the files examined
- The meaning of the `sub-index` on duplicate dates
- Food and exercise logs, which share the file
