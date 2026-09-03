//! Derived numbers. Kept separate from storage so it is easy to unit-test.

use crate::model::{Entry, Goal, Stats};
use chrono::{Duration, NaiveDate};

/// Centred-trailing moving average over a window of days (not samples), so
/// gaps in logging don't distort it.
pub fn moving_average(entries: &[Entry], window_days: i64) -> Vec<(NaiveDate, f64)> {
    let mut out = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let lo = e.date - Duration::days(window_days - 1);
        let mut sum = 0.0;
        let mut n = 0u32;
        for p in entries[..=i].iter().rev() {
            if p.date < lo {
                break;
            }
            sum += p.weight_lb;
            n += 1;
        }
        if n > 0 {
            out.push((e.date, sum / n as f64));
        }
    }
    out
}

/// Weight nearest to `target`, if any reading is within `tolerance` days.
fn near(entries: &[Entry], target: NaiveDate, tolerance: i64) -> Option<f64> {
    entries
        .iter()
        .filter(|e| (e.date - target).num_days().abs() <= tolerance)
        .min_by_key(|e| (e.date - target).num_days().abs())
        .map(|e| e.weight_lb)
}

/// Least-squares slope in pounds per week over the trailing `days`.
pub fn rate_lb_per_week(entries: &[Entry], days: i64) -> Option<f64> {
    let last = entries.last()?.date;
    let lo = last - Duration::days(days);
    let pts: Vec<(f64, f64)> = entries
        .iter()
        .filter(|e| e.date >= lo)
        .map(|e| ((e.date - lo).num_days() as f64, e.weight_lb))
        .collect();
    if pts.len() < 3 {
        return None;
    }
    let n = pts.len() as f64;
    let sx: f64 = pts.iter().map(|p| p.0).sum();
    let sy: f64 = pts.iter().map(|p| p.1).sum();
    let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-9 {
        return None;
    }
    Some(((n * sxy - sx * sy) / denom) * 7.0)
}

pub fn compute(entries: &[Entry], goal: Goal) -> Stats {
    let last = entries.last();
    let current = last.map(|e| e.weight_lb);
    let today = last.map(|e| e.date);

    let ma7 = moving_average(entries, 7);
    let trend_now = ma7.last().map(|(_, v)| *v);

    let lo = |d: i64| today.and_then(|t| near(entries, t - Duration::days(d), 10));
    let delta = |d: i64| match (current, lo(d)) {
        (Some(c), Some(p)) => Some(c - p),
        _ => None,
    };

    let min = entries
        .iter()
        .min_by(|a, b| a.weight_lb.total_cmp(&b.weight_lb));
    let max = entries
        .iter()
        .max_by(|a, b| a.weight_lb.total_cmp(&b.weight_lb));

    let rate = rate_lb_per_week(entries, 30);
    let goal_eta = match (trend_now.or(current), goal.target_lb, rate, today) {
        (Some(cur), Some(tgt), Some(r), Some(t)) if r.abs() > 0.05 => {
            let weeks = (tgt - cur) / r;
            if weeks > 0.0 && weeks < 520.0 {
                t.checked_add_signed(Duration::days((weeks * 7.0).round() as i64))
            } else {
                None
            }
        }
        _ => None,
    };

    Stats {
        count: entries.len(),
        first: entries.first().map(|e| e.date),
        last: today,
        current,
        trend_now,
        min: min.map(|e| e.weight_lb),
        min_date: min.map(|e| e.date),
        max: max.map(|e| e.weight_lb),
        max_date: max.map(|e| e.date),
        change_7d: delta(7),
        change_30d: delta(30),
        change_90d: delta(90),
        change_365d: delta(365),
        rate_lb_per_week: rate,
        goal,
        goal_eta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Source;

    fn e(d: &str, w: f64) -> Entry {
        Entry {
            date: NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap(),
            weight_lb: w,
            memo: String::new(),
            source: Source::Manual,
        }
    }

    #[test]
    fn moving_average_uses_a_day_window_not_a_sample_window() {
        // Two readings a month apart must not be averaged together.
        let v = vec![e("2024-01-01", 200.0), e("2024-02-01", 190.0)];
        let ma = moving_average(&v, 7);
        assert_eq!(ma.len(), 2);
        assert!((ma[1].1 - 190.0).abs() < 1e-9);
    }

    #[test]
    fn steady_loss_gives_a_negative_weekly_rate() {
        let mut v = Vec::new();
        for i in 0..30 {
            let d = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap() + Duration::days(i);
            v.push(Entry {
                date: d,
                weight_lb: 200.0 - i as f64 * 0.2,
                memo: String::new(),
                source: Source::Manual,
            });
        }
        let r = rate_lb_per_week(&v, 30).unwrap();
        assert!((r + 1.4).abs() < 0.05, "rate was {r}");
    }

    #[test]
    fn empty_history_does_not_panic() {
        let s = compute(&[], Goal::default());
        assert_eq!(s.count, 0);
        assert!(s.current.is_none());
    }
}
