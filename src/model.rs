//! Core domain types.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// One weigh-in. At most one per calendar day — the day *is* the identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub date: NaiveDate,
    pub weight_lb: f64,
    #[serde(default)]
    pub memo: String,
    #[serde(default = "Source::manual")]
    pub source: Source,
}

/// Where a reading came from. Kept so a Withings sync never silently
/// overwrites something typed by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Manual,
    Withings,
    Fitday,
    Import,
}

impl Source {
    pub fn manual() -> Self {
        Source::Manual
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Manual => "manual",
            Source::Withings => "withings",
            Source::Fitday => "fitday",
            Source::Import => "import",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "withings" => Source::Withings,
            "fitday" => Source::Fitday,
            "import" => Source::Import,
            _ => Source::Manual,
        }
    }
}

/// A single target: lose (or gain) to `target_lb`, optionally by
/// `target_date`, counted from `start_lb` on `start_date`. `id` orders
/// goals — the highest `id` is the one currently being pursued; anything
/// older is history, evaluated against what actually happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: i64,
    pub target_lb: f64,
    pub target_date: Option<NaiveDate>,
    pub start_lb: f64,
    pub start_date: NaiveDate,
}

/// How a goal turned out, judged against the entries logged after it
/// started. Computed fresh from the data every time — nothing here is
/// stored, so it never goes stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    /// The one currently being pursued, not yet hit, deadline not passed
    /// (or none set).
    Active,
    /// The target weight was reached at some point on or after the start
    /// date — regardless of whether that was before or after the deadline.
    Achieved,
    /// The target date passed without the target weight ever being hit.
    Missed,
    /// A newer goal replaced this one before it was hit or its deadline
    /// passed.
    Superseded,
}

/// A goal plus its computed outcome — what the UI actually renders.
#[derive(Debug, Clone, Serialize)]
pub struct GoalView {
    #[serde(flatten)]
    pub goal: Goal,
    /// True for the single most recently created goal.
    pub current: bool,
    pub status: GoalStatus,
    /// The date the target weight was first reached, if it ever was.
    pub hit_date: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub count: usize,
    pub first: Option<NaiveDate>,
    pub last: Option<NaiveDate>,
    pub current: Option<f64>,
    /// 7-day moving average at the most recent reading — the number that
    /// actually reflects where you are, free of daily noise.
    pub trend_now: Option<f64>,
    pub min: Option<f64>,
    pub min_date: Option<NaiveDate>,
    pub max: Option<f64>,
    pub max_date: Option<NaiveDate>,
    pub change_7d: Option<f64>,
    pub change_30d: Option<f64>,
    pub change_90d: Option<f64>,
    pub change_365d: Option<f64>,
    /// Pounds per week, from a least-squares fit over the last 30 days.
    pub rate_lb_per_week: Option<f64>,
    /// The goal currently being pursued, if any.
    pub goal: Option<Goal>,
    /// Projected date of hitting the goal at the current rate.
    pub goal_eta: Option<NaiveDate>,
}
