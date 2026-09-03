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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Goal {
    pub target_lb: Option<f64>,
    pub target_date: Option<NaiveDate>,
    pub start_lb: Option<f64>,
    pub start_date: Option<NaiveDate>,
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
    pub goal: Goal,
    /// Projected date of hitting the goal at the current rate.
    pub goal_eta: Option<NaiveDate>,
}
