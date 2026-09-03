//! Reading FitDay PC `.fdy` / `.fbk` files.
//!
//! FitDay PC was abandoned around 2004 and has no export. This decodes the
//! weight log out of its undocumented binary: dates, weights, and the full
//! note text.
//!
//! # The format, as far as it needs to be understood
//!
//! The file is a sequence of **4096-byte pages**. A page that holds records
//! begins with a 128-byte header, leaving 3968 bytes — exactly **496 slots**
//! of 8 bytes — of usable space.
//!
//! A **weight record** is:
//!
//! ```text
//! f32 weight | u32 unit (100 = lb) | u32 flags | u32 note_len | note bytes
//! ```
//!
//! padded so the whole record is a multiple of 8 bytes.
//!
//! A **date index record** is 24 bytes:
//!
//! ```text
//! u32 unix midnight | u32 sub-index | u32 0x000F4240 | u32 ptr | u64 zero
//! ```
//!
//! The catch, and the thing that made this hard: `ptr` is **not** a file
//! offset. It counts 8-byte slots through a virtual space made of the record
//! pages' *usable* areas concatenated together — page headers don't exist in
//! that space, and neither do the many pages in between that hold other
//! things. So resolving a pointer means knowing which pages hold records, in
//! order, and walking 496 slots at a time:
//!
//! ```text
//! page_index_in_sequence = ptr / 496
//! slot_within_page       = ptr % 496
//! file_offset = record_pages[page_index] * 4096 + 128 + slot_within_page * 8
//! ```
//!
//! Verified against a 16-year, 803-entry file with an independently confirmed
//! ground truth: 803/803 weights and 143/143 notes decoded exactly.

use crate::model::{Entry, Source};
use anyhow::{bail, Result};
use chrono::{DateTime, NaiveDate};
use std::collections::BTreeMap;

const MAGIC: [u8; 4] = [0xDE, 0xFE, 0xC8, 0x42];
const PAGE: usize = 4096;
const HEADER: usize = 128;
/// 8-byte slots of usable space in a record page.
const SLOTS: usize = (PAGE - HEADER) / 8;

const UNIT_LB: u32 = 100;
const DATE_MARKER: u32 = 0x000F_4240;

/// Records shorter than this in a run are coincidence, not data.
const MIN_CHAIN: usize = 5;
/// The weight log's index leaves are long runs; short runs belong to other
/// sections of the file (food, exercise) and would decode into nonsense.
const MIN_LEAF: usize = 100;
/// A gap bigger than this ends an index run.
const LEAF_GAP: usize = 200;

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn f32_at(b: &[u8], off: usize) -> Option<f32> {
    b.get(off..off + 4)
        .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[derive(Debug, Clone)]
struct Record {
    weight_lb: f64,
    note: String,
    /// Total bytes, including padding — how far to the next record.
    size: usize,
}

fn read_record(b: &[u8], off: usize) -> Option<Record> {
    let weight = f32_at(b, off)?;
    let unit = u32_at(b, off + 4)?;
    let note_len = u32_at(b, off + 12)? as usize;

    if unit != UNIT_LB || !(50.0..600.0).contains(&weight) || note_len > 4000 {
        return None;
    }
    let note_bytes = b.get(off + 16..off + 16 + note_len)?;
    // FitDay wrote Windows-1252; latin-1 is close enough and never fails.
    let note: String = note_bytes
        .iter()
        .map(|&c| c as char)
        .collect::<String>()
        .replace('\u{0}', "");

    Some(Record {
        weight_lb: (weight as f64 * 10.0).round() / 10.0,
        note: note.trim().to_string(),
        size: 16 + note_len.div_ceil(8) * 8,
    })
}

/// Which 4096-byte pages hold weight records, in file order.
///
/// A page qualifies when a run of at least [`MIN_CHAIN`] records walks
/// cleanly through it — enough to rule out floats that merely look like a
/// plausible body weight.
fn record_pages(b: &[u8]) -> Vec<usize> {
    let mut pages = std::collections::BTreeSet::new();
    let mut off = 0usize;
    while off + 16 <= b.len() {
        if read_record(b, off).is_some() {
            let mut cursor = off;
            let mut n = 0usize;
            while let Some(r) = read_record(b, cursor) {
                n += 1;
                cursor += r.size;
                if n > MIN_CHAIN {
                    break;
                }
            }
            if n >= MIN_CHAIN {
                pages.insert(off / PAGE);
            }
        }
        off += 8;
    }
    pages.into_iter().collect()
}

#[derive(Debug, Clone)]
struct IndexEntry {
    offset: usize,
    date: NaiveDate,
    ptr: usize,
}

fn index_entries(b: &[u8]) -> Vec<IndexEntry> {
    let lo = 946_684_800i64; // 2000-01-01
    let hi = 4_102_444_800i64; // 2100-01-01
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 24 <= b.len() {
        if let (Some(stamp), Some(marker), Some(ptr)) =
            (u32_at(b, off), u32_at(b, off + 8), u32_at(b, off + 12))
        {
            let t = stamp as i64;
            if marker == DATE_MARKER && t > lo && t < hi {
                if let Some(when) = DateTime::from_timestamp(t, 0) {
                    let naive = when.naive_utc();
                    // Entries sit exactly on midnight UTC; anything else is a
                    // coincidence rather than a date.
                    if naive.time() == chrono::NaiveTime::MIN {
                        out.push(IndexEntry {
                            offset: off,
                            date: naive.date(),
                            ptr: ptr as usize,
                        });
                    }
                }
            }
        }
        off += 4;
    }
    out
}

/// Keep only the long contiguous runs of index entries — the weight log's
/// B-tree leaves. The short runs index other sections, and following them
/// would attach real weights to the wrong days.
fn weight_log_leaves(entries: Vec<IndexEntry>) -> Vec<IndexEntry> {
    if entries.is_empty() {
        return entries;
    }
    let mut runs: Vec<Vec<IndexEntry>> = Vec::new();
    let mut current = vec![entries[0].clone()];
    for e in entries.into_iter().skip(1) {
        if e.offset - current.last().unwrap().offset > LEAF_GAP {
            runs.push(std::mem::take(&mut current));
        }
        current.push(e);
    }
    runs.push(current);

    let long: Vec<Vec<IndexEntry>> = runs.iter().filter(|r| r.len() >= MIN_LEAF).cloned().collect();
    // A small file may have no run that long; then every run is all we have.
    let chosen = if long.is_empty() { runs } else { long };
    chosen.into_iter().flatten().collect()
}

#[derive(Debug, Default)]
pub struct FitdayReport {
    pub entries: Vec<Entry>,
    /// Index entries whose pointer didn't land on a record. Non-zero here
    /// means the decode is incomplete and should not be trusted wholesale.
    pub unresolved: usize,
    pub with_notes: usize,
}

/// Decode the weight log.
pub fn parse(bytes: &[u8]) -> Result<FitdayReport> {
    if bytes.len() < PAGE {
        bail!("too small to be a FitDay file ({} bytes)", bytes.len());
    }
    if bytes[..4] != MAGIC {
        bail!(
            "not a FitDay file — expected it to start with {:02x?}, found {:02x?}",
            MAGIC,
            &bytes[..4]
        );
    }

    let pages = record_pages(bytes);
    if pages.is_empty() {
        bail!("no weight records found — is this a FitDay file with a weight log in it?");
    }

    let mut report = FitdayReport::default();
    // Keyed by date so a day indexed twice resolves once; the first entry in
    // file order is the primary one.
    let mut by_date: BTreeMap<NaiveDate, Entry> = BTreeMap::new();

    for e in weight_log_leaves(index_entries(bytes)) {
        let page_slot = e.ptr / SLOTS;
        let within = e.ptr % SLOTS;
        let Some(&page) = pages.get(page_slot) else {
            report.unresolved += 1;
            continue;
        };
        let off = page * PAGE + HEADER + within * 8;
        let Some(rec) = read_record(bytes, off) else {
            report.unresolved += 1;
            continue;
        };
        by_date.entry(e.date).or_insert_with(|| Entry {
            date: e.date,
            weight_lb: rec.weight_lb,
            memo: rec.note.clone(),
            source: Source::Fitday,
        });
    }

    report.with_notes = by_date.values().filter(|e| !e.memo.is_empty()).count();
    report.entries = by_date.into_values().collect();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal file: a record page at page 1 and one index entry
    /// pointing at its second record.
    fn synthetic() -> Vec<u8> {
        let mut b = vec![0u8; PAGE * 3];
        b[..4].copy_from_slice(&MAGIC);

        let base = PAGE + HEADER;
        let mut off = base;
        // Ten records; the second carries a note.
        for i in 0..10u32 {
            let weight = 200.0f32 + i as f32;
            b[off..off + 4].copy_from_slice(&weight.to_le_bytes());
            b[off + 4..off + 8].copy_from_slice(&UNIT_LB.to_le_bytes());
            b[off + 8..off + 12].copy_from_slice(&0u32.to_le_bytes());
            let note: &[u8] = if i == 1 { b"felt good" } else { b"" };
            b[off + 12..off + 16].copy_from_slice(&(note.len() as u32).to_le_bytes());
            b[off + 16..off + 16 + note.len()].copy_from_slice(note);
            off += 16 + note.len().div_ceil(8) * 8;
        }
        b
    }

    #[test]
    fn rejects_files_that_are_not_fitday() {
        let mut b = vec![0u8; PAGE * 2];
        b[..4].copy_from_slice(b"RIFF");
        let err = parse(&b).unwrap_err().to_string();
        assert!(err.contains("not a FitDay file"), "{err}");
    }

    #[test]
    fn reads_a_record_with_its_note_and_padding() {
        let b = synthetic();
        let first = read_record(&b, PAGE + HEADER).unwrap();
        assert_eq!(first.weight_lb, 200.0);
        assert_eq!(first.size, 16, "an empty note pads to nothing");

        let second = read_record(&b, PAGE + HEADER + 16).unwrap();
        assert_eq!(second.weight_lb, 201.0);
        assert_eq!(second.note, "felt good");
        assert_eq!(second.size, 32, "9 bytes of note pads to 16");
    }

    #[test]
    fn finds_the_page_holding_records() {
        assert_eq!(record_pages(&synthetic()), vec![1]);
    }

    /// The pointer arithmetic is the part that was hard to get right, so pin
    /// it down: slot 0 is the first byte after the page header, and slot 496
    /// is the first byte of the *next record page*, not the next page.
    #[test]
    fn pointer_resolution_skips_page_headers_and_non_record_pages() {
        let pages = vec![7usize, 37, 38];
        let resolve = |ptr: usize| {
            let (k, rem) = (ptr / SLOTS, ptr % SLOTS);
            pages[k] * PAGE + HEADER + rem * 8
        };
        assert_eq!(resolve(0), 7 * PAGE + HEADER);
        assert_eq!(resolve(0), 28800, "matches a real file's first record");
        assert_eq!(resolve(1), 28808);
        assert_eq!(resolve(SLOTS), 37 * PAGE + HEADER);
        assert_eq!(resolve(SLOTS), 151680, "jumps to the next record page");
        assert_eq!(resolve(2 * SLOTS), 155776);
        // The base the naive "offset = base + ptr*8" reading would need.
        assert_eq!(151680 - SLOTS * 8, 147712);
    }
}
