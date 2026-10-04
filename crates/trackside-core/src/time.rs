//! Clock time across Australia's states. Racing Australia publishes a race's start in the
//! venue's local time; a listener in another state hears it in their own, and only when the
//! two clocks differ on that date (Brisbane and Melbourne agree until daylight saving starts
//! in October, then sit an hour apart).

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

/// The time zone a state's racing runs on. Broken Hill, which keeps South Australian time
/// inside New South Wales, is out of scope.
pub fn state_tz(state: &str) -> Option<Tz> {
    Some(match state.trim().to_ascii_uppercase().as_str() {
        "VIC" => chrono_tz::Australia::Melbourne,
        "NSW" => chrono_tz::Australia::Sydney,
        "ACT" => chrono_tz::Australia::Canberra,
        "TAS" => chrono_tz::Australia::Hobart,
        "QLD" => chrono_tz::Australia::Brisbane,
        "SA" => chrono_tz::Australia::Adelaide,
        "WA" => chrono_tz::Australia::Perth,
        "NT" => chrono_tz::Australia::Darwin,
        _ => return None,
    })
}

/// How a listener names their own clock: "4 pm Queensland time".
pub fn zone_label(state: &str) -> Option<&'static str> {
    Some(match state.trim().to_ascii_uppercase().as_str() {
        "VIC" => "Melbourne time",
        "NSW" => "Sydney time",
        "ACT" => "Canberra time",
        "TAS" => "Tasmanian time",
        "QLD" => "Queensland time",
        "SA" => "Adelaide time",
        "WA" => "Perth time",
        "NT" => "Darwin time",
        _ => return None,
    })
}

/// A published start as a clock time: "4:25PM" and "5:00PM" (Racing Australia), "15:40" and
/// "0:30" (24-hour), "4:25 pm". `None` for "TBA", empty text and anything else.
pub fn parse_start_local(s: &str) -> Option<NaiveTime> {
    let upper = s.trim().to_ascii_uppercase();
    let (clock, half) = if let Some(c) = upper.strip_suffix("PM") {
        (c.trim(), Some(true))
    } else if let Some(c) = upper.strip_suffix("AM") {
        (c.trim(), Some(false))
    } else {
        (upper.as_str(), None)
    };
    let (h, m) = clock.split_once(':')?;
    let (h, m) = (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?);
    let h = match half {
        Some(pm) if (1..=12).contains(&h) => (h % 12) + if pm { 12 } else { 0 },
        Some(_) => return None,
        None => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// The instant a race starts, from its date, published start and the venue's state.
pub fn start_instant(
    date: NaiveDate,
    start_local: &str,
    venue_state: &str,
) -> Option<DateTime<Utc>> {
    let t = parse_start_local(start_local)?;
    let zone = state_tz(venue_state)?;
    zone.from_local_datetime(&date.and_time(t))
        .earliest()
        .map(|at| at.with_timezone(&Utc))
}

/// A start in the listener's own clock: the time, and whether it differs from the venue's
/// clock that day (when it doesn't, there is nothing to say). `None` when either state is
/// unknown or the start isn't a time.
pub fn in_home_zone(
    date: NaiveDate,
    start_local: &str,
    venue_state: &str,
    home_state: &str,
) -> Option<(NaiveTime, bool)> {
    let venue = parse_start_local(start_local)?;
    let at = start_instant(date, start_local, venue_state)?;
    let home = at.with_timezone(&state_tz(home_state)?).time();
    Some((home, home != venue))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    #[test]
    fn published_starts_parse_as_clock_times() {
        assert_eq!(parse_start_local("4:25PM"), Some(t("16:25")));
        assert_eq!(parse_start_local("5:00PM"), Some(t("17:00")));
        assert_eq!(parse_start_local("12:10PM"), Some(t("12:10")));
        assert_eq!(parse_start_local("12:30AM"), Some(t("00:30")));
        assert_eq!(parse_start_local("11:05 am"), Some(t("11:05")));
        assert_eq!(parse_start_local("15:40"), Some(t("15:40")));
        assert_eq!(parse_start_local("0:30"), Some(t("00:30")));
        assert_eq!(parse_start_local("TBA"), None);
        assert_eq!(parse_start_local(""), None);
        assert_eq!(parse_start_local("13:00PM"), None);
        assert_eq!(parse_start_local("25:00"), None);
    }

    #[test]
    fn start_times_follow_the_listeners_state() {
        // A 5 pm Caulfield race on Caulfield Cup day, after daylight saving has started.
        let cup = d("2026-10-17");
        assert_eq!(
            in_home_zone(cup, "17:00", "VIC", "QLD"),
            Some((t("16:00"), true))
        );
        assert_eq!(
            in_home_zone(cup, "17:00", "VIC", "SA"),
            Some((t("16:30"), true))
        );
        assert_eq!(
            in_home_zone(cup, "17:00", "VIC", "WA"),
            Some((t("14:00"), true))
        );
        assert_eq!(
            in_home_zone(cup, "17:00", "VIC", "NT"),
            Some((t("15:30"), true))
        );
        assert_eq!(
            in_home_zone(cup, "17:00", "VIC", "NSW"),
            Some((t("17:00"), false))
        );
        assert_eq!(
            in_home_zone(cup, "17:00", "VIC", "TAS"),
            Some((t("17:00"), false))
        );
        // Before daylight saving, Brisbane and Melbourne clocks agree.
        assert_eq!(
            in_home_zone(d("2026-09-26"), "15:40", "VIC", "QLD"),
            Some((t("15:40"), false))
        );
        // A Queensland race heard in Victoria during daylight saving.
        assert_eq!(
            in_home_zone(cup, "12:10PM", "QLD", "VIC"),
            Some((t("13:10"), true))
        );
        // Nothing to convert.
        assert_eq!(in_home_zone(cup, "TBA", "VIC", "QLD"), None);
        assert_eq!(in_home_zone(cup, "17:00", "VIC", "UK"), None);
        assert_eq!(in_home_zone(cup, "17:00", "", "QLD"), None);
    }

    #[test]
    fn a_start_is_an_instant() {
        // 5 pm in Melbourne on 17 October 2026 (AEDT, UTC+11) is 6 am UTC.
        let at = start_instant(d("2026-10-17"), "17:00", "VIC").unwrap();
        assert_eq!(at.to_rfc3339(), "2026-10-17T06:00:00+00:00");
        // 12:10 pm in Brisbane (AEST, UTC+10) the same day is 2:10 am UTC.
        let at = start_instant(d("2026-10-17"), "12:10PM", "QLD").unwrap();
        assert_eq!(at.to_rfc3339(), "2026-10-17T02:10:00+00:00");
        assert_eq!(start_instant(d("2026-10-17"), "TBA", "VIC"), None);
    }

    #[test]
    fn zones_have_names() {
        assert_eq!(zone_label("qld"), Some("Queensland time"));
        assert_eq!(zone_label("WA"), Some("Perth time"));
        assert_eq!(zone_label("TAS"), Some("Tasmanian time"));
        assert_eq!(zone_label("NZ"), None);
        assert!(state_tz("ACT").is_some());
    }
}
