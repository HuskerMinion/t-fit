//! Derived numbers. Kept separate from storage so it is easy to unit-test.

use crate::model::{Entry, Goal, GoalStatus, GoalView, Stats};
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

pub fn compute(entries: &[Entry], goal: Option<Goal>) -> Stats {
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
    let goal_eta = match (trend_now.or(current), goal.as_ref().map(|g| g.target_lb), rate, today) {
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

/// The date the target weight was first reached on or after a goal's start
/// — the moment it actually happened, whatever the deadline said. `None` if
/// it never was (yet).
fn hit_date(entries: &[Entry], g: &Goal) -> Option<NaiveDate> {
    use std::cmp::Ordering;
    let losing = g.target_lb.partial_cmp(&g.start_lb).unwrap_or(Ordering::Equal);
    entries
        .iter()
        .filter(|e| e.date >= g.start_date)
        .find(|e| match losing {
            Ordering::Less => e.weight_lb <= g.target_lb,
            Ordering::Greater => e.weight_lb >= g.target_lb,
            Ordering::Equal => true, // target == start: already there
        })
        .map(|e| e.date)
}

/// Every goal (`goals`, newest first — see `Db::goals`) with its outcome
/// worked out from the entries logged since. Nothing about the outcome is
/// stored: a goal that looked missed yesterday reads as achieved today the
/// moment a qualifying weigh-in is logged, with no bookkeeping to update.
pub fn goal_views(entries: &[Entry], goals: &[Goal], today: NaiveDate) -> Vec<GoalView> {
    goals
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let current = i == 0;
            let hit = hit_date(entries, g);
            // Missed is a fact about the deadline, independent of whether a
            // newer goal has since been set — so it's checked before
            // Superseded. Superseded is reserved for a goal abandoned while
            // still theoretically open (no deadline yet, or one still
            // ahead).
            let status = if hit.is_some() {
                GoalStatus::Achieved
            } else if g.target_date.is_some_and(|d| d < today) {
                GoalStatus::Missed
            } else if !current {
                GoalStatus::Superseded
            } else {
                GoalStatus::Active
            };
            GoalView {
                goal: g.clone(),
                current,
                status,
                hit_date: hit,
            }
        })
        .collect()
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
        let s = compute(&[], None);
        assert_eq!(s.count, 0);
        assert!(s.current.is_none());
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn g(id: i64, target: f64, target_date: Option<&str>, start: f64, start_date: &str) -> Goal {
        Goal {
            id,
            target_lb: target,
            target_date: target_date.map(d),
            start_lb: start,
            start_date: d(start_date),
        }
    }

    #[test]
    fn a_goal_hit_after_its_deadline_still_counts_as_achieved() {
        // Late is still achieved, per the "any time after starting" rule —
        // the whole point of tracking start->hit separately from the
        // deadline.
        let entries = vec![
            e("2024-01-01", 200.0),
            e("2024-03-01", 189.5), // past the Feb 1 deadline, but under target
        ];
        let goal = g(1, 190.0, Some("2024-02-01"), 200.0, "2024-01-01");
        let views = goal_views(&entries, &[goal], d("2024-03-15"));
        assert_eq!(views[0].status, GoalStatus::Achieved);
        assert_eq!(views[0].hit_date, Some(d("2024-03-01")));
    }

    #[test]
    fn an_unhit_current_goal_past_its_deadline_is_missed() {
        let entries = vec![e("2024-01-01", 200.0), e("2024-02-15", 195.0)];
        let goal = g(1, 190.0, Some("2024-02-01"), 200.0, "2024-01-01");
        let views = goal_views(&entries, &[goal], d("2024-03-01"));
        assert_eq!(views[0].status, GoalStatus::Missed);
    }

    #[test]
    fn an_open_current_goal_not_yet_hit_is_active() {
        let entries = vec![e("2024-01-01", 200.0), e("2024-01-15", 197.0)];
        let goal = g(1, 190.0, None, 200.0, "2024-01-01");
        let views = goal_views(&entries, &[goal], d("2024-01-20"));
        assert_eq!(views[0].status, GoalStatus::Active);
    }

    #[test]
    fn an_older_open_ended_goal_is_superseded_by_a_newer_one() {
        // goals() returns newest-first, so index 0 is current. The older
        // goal has no deadline — still theoretically open when replaced —
        // so it reads as abandoned rather than failed.
        let entries = vec![e("2024-01-01", 200.0), e("2024-02-01", 197.0)];
        let newer = g(2, 180.0, None, 197.0, "2024-02-01");
        let older = g(1, 190.0, None, 200.0, "2024-01-01");
        let views = goal_views(&entries, &[newer, older], d("2024-02-10"));
        assert_eq!(views[0].status, GoalStatus::Active); // the newer one
        assert_eq!(views[1].status, GoalStatus::Superseded); // never hit, replaced
    }

    #[test]
    fn an_older_goal_past_its_own_deadline_reads_missed_even_after_being_superseded() {
        // Missed is a fact about the deadline that a later goal doesn't
        // erase — otherwise setting a new goal would quietly launder every
        // failure into a neutral "superseded".
        let entries = vec![e("2024-01-01", 200.0), e("2024-02-01", 197.0)];
        let newer = g(2, 180.0, None, 197.0, "2024-02-01");
        let older = g(1, 190.0, Some("2024-01-10"), 200.0, "2024-01-01");
        let views = goal_views(&entries, &[newer, older], d("2024-02-10"));
        assert_eq!(views[1].status, GoalStatus::Missed);
    }

    #[test]
    fn a_gain_goal_is_hit_going_up_not_down() {
        let entries = vec![e("2024-01-01", 150.0), e("2024-02-01", 161.0)];
        let goal = g(1, 160.0, None, 150.0, "2024-01-01");
        let views = goal_views(&entries, &[goal], d("2024-02-10"));
        assert_eq!(views[0].status, GoalStatus::Achieved);
    }
}
