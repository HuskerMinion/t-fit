# Getting your data out of FitDay PC

FitDay PC (Version 1.0) was abandoned around 2004 and the online service shut down in 2022.
There is no export button, no CSV, no API. If you have a decade of weigh-ins
in it, they're sitting inside an undocumented binary on a machine running
software that will never be updated again.

You can have them back:

```
t-fit --import-fitday anameFit.fdy
```

That's it. Dates, weights, and the full text of every note.

```
read 803 days from anameFit.fdy (143 with notes)
  2009-09-14 → 2025-01-23
  added 803, already present 0
```

## Just want a CSV?

If you'd rather not build t-fit, `fitday_export.py` is a standalone copy of
the same decoder with no dependencies beyond Python 3:

```
python3 fitday_export.py anameFit.fdy -o weight_history.csv
```

The two produce identical output — that's checked, not assumed.

## Where's my file?

Typically `Documents\<name>.fdy`. A `.fbk` backup works too.

**Copy it somewhere else and work on the copy.** Never point a tool at a file
FitDay has open. Nothing here writes to your `.fdy`, but there's no reason to
find out what happens when FitDay flushes over the top of a read.

One trap with backups: FitDay's "Backup Account" is a plain file copy, so the
`.fbk`'s timestamp is the *source file's* modification time, not when you made
the backup. Mine looked current and was two years stale. Check the date range
in the output against what FitDay shows you.

## Checking it worked

The importer prints the day count, the date range, and how many notes it
recovered. Compare those against FitDay's own Weight Log before trusting them.

If it prints a warning about unresolved index entries, **stop and open an
issue** with the counts. That means the decode is incomplete on your file, and
the safe assumption is that some days are wrong rather than merely missing.
It doesn't happen on any file tested so far, but the format was reverse
engineered from a small number of examples and I'd rather hear about it.

## Notes are better than the printout

FitDay's own Print truncates notes to the column width — about 47 characters.
The binary holds the whole thing. The longest note recovered from my file was
302 characters; printing it gave me the first sentence and a half.

If you previously escaped FitDay via the print-to-PDF route, it's worth
re-importing from the binary just for the notes. `--import-fitday` won't
overwrite days you already have, so it's safe to run over an existing
database — but it also won't *upgrade* a truncated note. To replace what you
have, add `--overwrite`.

## How it works

[FORMAT.md](FORMAT.md) documents the file format: the record layout, the date
index, and the pointer arithmetic that makes the two fit together. That last
part is the reason this took two attempts.

Verified against a 16-year, 803-entry file with an independently established
ground truth: **803/803 weights and 143/143 notes decoded exactly.**

## Is this legal?

It's your own data, in a file on your own disk, from software that no longer
has a vendor. There's no copy protection here to circumvent — just an
undocumented format.
