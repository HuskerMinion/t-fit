# Decoding the rest of a FitDay PC file

t-fit reads the **weight log** out of a `.fdy` / `.fbk` and nothing else.
The format is undocumented — FitDay PC was abandoned around 2004 and has no
export — so everything known about it was established by inspection, and is
written down in the doc comment at the top of `src/fitday.rs`. Read that
first; this file picks up where it stops.

## What is already known

- The file is a sequence of 4096-byte pages. A record page has a 128-byte
  header, leaving exactly 496 eight-byte slots.
- A **weight record** is `f32 weight | u32 unit (100 = lb) | u32 flags |
  u32 note_len | note bytes`, padded to a multiple of 8.
- A **date index record** is 24 bytes: `u32 unix midnight | u32 sub-index |
  u32 0x000F4240 | u32 ptr | u64 zero`.
- `ptr` is not a file offset. It counts 8-byte slots through a virtual space
  made of the *record pages' usable areas* concatenated — headers aren't in
  that space, and neither are the pages in between holding other things.

That pointer scheme was the hard part, and it is not weight-specific.
Neither are the dates. Both should carry over to any other section.

## What is not known

Everything about the food and exercise sections except that they exist.

`fitday.rs` finds every date index run in the file and then discards any run
shorter than 100 entries, because those index other sections and would
decode into nonsense if read as weights. So their *index* is already being
located on every parse — what's missing is:

1. Which pages hold each section's records, and therefore what virtual space
   its pointers count through. The weight log's pages are found by looking
   for runs of weight-shaped records; there's no equivalent detector yet for
   anything else.
2. The record payload layout — what a food row's bytes mean, what an
   exercise row's mean.
3. Whether food names are stored inline at all. FitDay shipped a food
   database, so entries may reference foods by ID. If those IDs point into a
   table the installer laid down rather than into the user's file, names may
   not be recoverable from a `.fdy` alone. **Settle this question first** —
   it decides whether the rest is worth doing.

## The tools

Neither touches the working parser; both are read-only.

```bash
python3 tools/fdy_census.py FILE [--strings] [--pages] [--sample N]
python3 tools/fdy_diff.py BEFORE AFTER
python3 tools/fdy_selftest.py      # verifies both against synthetic files
```

`fdy_census.py` says what a file contains: weight-record pages, every date
index run with its length and date span, how many of each run's pointers
resolve, a page census, and optionally every printable run in the file.

`fdy_diff.py` compares two files and reports, in order of usefulness: new
date index entries (which day a change was filed under, and its pointer),
new printable strings (whether names are stored inline), then the changed
byte ranges with before-and-after hex. Dumps are aligned to slot boundaries
and self-identify when they parse as a record we already understand.

## Capture protocol

The decisive advantage is a working FitDay PC install, because it turns
guesswork into a controlled experiment: make one known change, save, and
diff the bytes.

Use a **brand-new profile**, not a real one. An almost-empty file makes each
diff a handful of bytes instead of a haystack. A real long-running file is
the validation set for the end, once the layout is believed.

Close FitDay before copying the file each time — it may buffer writes.

| # | In FitDay | Save a copy as |
|---|-----------|----------------|
| 0 | New profile, nothing entered | `00-empty.fdy` |
| 1 | One weight, distinctive value | `01-weight.fdy` |
| 2 | One food, distinctive quantity | `02-food.fdy` |
| 3 | A second food, same day | `03-food-same-day.fdy` |
| 4 | A food on a different day | `04-food-other-day.fdy` |
| 5 | Change food #1's quantity, nothing else | `05-food-qty.fdy` |
| 6 | One exercise entry, distinctive duration | `06-exercise.fdy` |
| 7 | A **custom** food named `QQZZXX Test`, known calories and macros | `07-custom-food.fdy` |

Keep a plain text file beside them recording exactly what was entered and
what FitDay *displayed back* — dates, names, quantities, the calorie and
macro figures it computed. That display output is the ground truth. The
weight decode is trustworthy because it was checked at 803/803 weights and
143/143 notes against independently confirmed values; nothing else should be
believed on weaker evidence.

Pick numbers unlikely to occur by chance in a binary — 137, 187.3, 43
minutes. They are greppable; 1 and 100 are not.

One change per capture. Two at once and you are guessing again.

## Reading the results

Run `fdy_diff.py` on each consecutive pair. What to look for:

- **Step 7 produces no new printable runs.** Then food names are not inline,
  foods are IDs into FitDay's own database, and names are probably not
  recoverable from the user's file. Worth knowing before investing further.
- **A new date index entry appears with a pointer.** That pointer plus the
  changed byte range in the same diff is the record — and the offset it
  landed at tells you which pages that section's virtual space is made of,
  which is item (1) above.
- **Step 5 changes a small number of bytes in place.** Those bytes are the
  quantity field. Comparing against what FitDay displayed gives its units
  and scale directly.
