//! Where "today" comes from. Racing days are Melbourne days, and every tool that defaults a
//! date, decides whether a race has been run, or looks ahead for a stable asks the clock
//! rather than the system, so tests and rehearsals can fix the calendar.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Australia::Melbourne;

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    /// The current date in Melbourne, which is the racing calendar's day.
    fn today(&self) -> NaiveDate {
        self.now().with_timezone(&Melbourne).date_naive()
    }
}

/// The real time.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock stopped at one instant, for tests and for rehearsing the demo fixture on the day
/// its story is set.
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// The real clock, unless `TRACKSIDE_TODAY` names a date (YYYY-MM-DD): then that day at
/// 9 am Melbourne time, for running the demo fixture as if it were the day before its
/// Caulfield Cup card. Never set it on the deployed server, whose data is live.
pub fn from_env() -> Arc<dyn Clock> {
    let Ok(text) = std::env::var("TRACKSIDE_TODAY") else {
        return Arc::new(SystemClock);
    };
    match NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d") {
        Ok(day) => {
            let nine = NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default();
            let at = Melbourne
                .from_local_datetime(&day.and_time(nine))
                .single()
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            tracing::warn!(%day, "TRACKSIDE_TODAY set: the clock is fixed, as for a rehearsal");
            Arc::new(FixedClock(at))
        }
        Err(_) => {
            tracing::warn!(%text, "TRACKSIDE_TODAY is not a YYYY-MM-DD date; using the real clock");
            Arc::new(SystemClock)
        }
    }
}
