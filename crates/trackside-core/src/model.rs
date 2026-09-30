use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Where a fact came from. Every tool answer names its source.
pub const SOURCE_RACING_AUSTRALIA: &str = "Racing Australia";
pub const SOURCE_SECTIONALS: &str = "state racing bodies and racing.com sectional timing";

/// Wagering brands that sponsor races and venues. Trackside never names them: "Sportsbet
/// Longreach" is Longreach and the "TAB Handicap" is a handicap.
pub const BOOKMAKER_BRANDS: &[&str] = &[
    "sportsbet",
    "ladbrokes",
    "tab",
    "tabtouch",
    "neds",
    "pointsbet",
    "bet365",
    "betfair",
    "unibet",
    "betr",
    "bluebet",
    "palmerbet",
    "picklebet",
    "topsport",
    "dabble",
    "boombet",
    "betdeluxe",
    "betright",
    "elitebet",
    "betestate",
    "tabcorp",
];

/// Remove wagering brands from a race, venue or class name, keeping the rest as written.
/// Horse and people's names are never passed through this.
pub fn without_bookmakers(s: &str) -> String {
    let kept: Vec<&str> = s
        .split_whitespace()
        .filter(|w| {
            let bare: String = w
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            !BOOKMAKER_BRANDS.contains(&bare.as_str())
        })
        .collect();
    kept.join(" ")
        .trim_matches(|c: char| c == '-' || c == ',' || c.is_whitespace())
        .to_string()
}

/// A race meeting on one day at one venue.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Meeting {
    pub date: NaiveDate,
    pub state: String,
    pub venue: String,
    pub track_condition: Option<String>,
    pub rail: Option<String>,
    pub weather: Option<String>,
    pub races: Vec<RaceCard>,
}

/// The card for one race: conditions plus the field.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RaceCard {
    pub race_number: u32,
    pub name: String,
    /// Local start time as published, e.g. "14:40".
    pub start_local: String,
    pub distance_m: Option<u32>,
    pub class: String,
    pub prize: String,
    /// Group 1/2/3, Listed, or empty for non-black-type races.
    pub grade: String,
    pub runners: Vec<Runner>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Runner {
    pub number: u32,
    pub horse: String,
    pub jockey: String,
    pub trainer: String,
    pub barrier: Option<u32>,
    pub weight_kg: Option<f64>,
    pub rating: Option<i64>,
    /// The runner's last ten results, most recent last, e.g. "x3121".
    pub last10: String,
    pub scratched: bool,
}

/// Starts-wins-seconds-thirds, as in "10: 2-2-0".
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Record {
    pub starts: u32,
    pub wins: u32,
    pub seconds: u32,
    pub thirds: u32,
}

fn pl(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

impl Record {
    pub fn summary(&self) -> String {
        format!(
            "{} start{}: {} win{}, {} second{}, {} third{}",
            self.starts,
            pl(self.starts),
            self.wins,
            pl(self.wins),
            self.seconds,
            pl(self.seconds),
            self.thirds,
            pl(self.thirds)
        )
    }
}

/// One past start. Price fluctuations are intentionally absent.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PastStart {
    pub date: NaiveDate,
    pub venue: String,
    pub distance_m: Option<u32>,
    pub condition: String,
    pub class: String,
    pub finish: Option<u32>,
    pub starters: Option<u32>,
    pub margin_lengths: Option<f64>,
    pub jockey: String,
    pub weight_kg: Option<f64>,
    pub barrier: Option<u32>,
    pub time: String,
    pub last_600_s: Option<f64>,
    pub pos_800: Option<u32>,
    pub pos_400: Option<u32>,
}

impl PastStart {
    /// Barrier trials and jump-outs are practice, not starts. Racing Australia marks most of
    /// them, but some jump-outs arrive unmarked, as class "Out - S5" on a "Jump" track.
    pub fn is_trial(&self) -> bool {
        let class = self.class.trim().to_ascii_lowercase();
        self.condition.trim().eq_ignore_ascii_case("jump")
            || class.starts_with("out -")
            || class.ends_with("-bt")
            || class.contains("trial")
            || class.contains("jump out")
            || class.contains("jumpout")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HorseForm {
    pub horse: String,
    pub trainer: String,
    pub career: Record,
    pub first_up: Record,
    pub second_up: Record,
    pub good: Record,
    pub soft: Record,
    pub heavy: Record,
    pub starts: Vec<PastStart>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Placing {
    pub position: u32,
    pub number: u32,
    pub horse: String,
    pub jockey: String,
    pub margin_lengths: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RaceResult {
    pub date: NaiveDate,
    pub venue: String,
    pub race_number: u32,
    pub placings: Vec<Placing>,
    pub winning_time: Option<String>,
    pub track_condition: Option<String>,
    /// Fastest last 600 m in the race, from sectional timing when available.
    pub fastest_last_600: Option<SectionalHighlight>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SectionalHighlight {
    pub horse: String,
    pub last_600_s: f64,
    pub source: String,
}

/// The headline purse from Racing Australia's prize text, which opens with the total and
/// runs on into the split: "$2,000,000.1st $1,200,000, 2nd ..." gives 2,000,000.
pub fn prize_total(prize: &str) -> Option<u64> {
    let s = prize.trim_start();
    let s = s.strip_prefix("Of ").unwrap_or(s).trim_start_matches('$');
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|&n| n > 0)
}

/// A purse as it would be said aloud: "$2 million", "$1.5 million", "$150,000".
pub fn spoken_money(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        let m = format!("{m:.2}");
        let m = m.trim_end_matches('0').trim_end_matches('.');
        return format!("${m} million");
    }
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("${out}")
}

/// Wins and places for a jockey or trainer over a period.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PersonStats {
    pub name: String,
    pub role: String,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub record: Record,
}

/// Spring Racing Carnival feature races. Static, checked 2026-09-28 against races.com.au.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeatureRace {
    pub name: &'static str,
    pub date: NaiveDate,
    pub venue: &'static str,
    pub grade: &'static str,
    pub distance_m: u32,
    pub blurb: &'static str,
}

pub fn spring_carnival_2026() -> Vec<FeatureRace> {
    let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
    vec![
        FeatureRace { name: "Caulfield Guineas", date: d(2026, 10, 10), venue: "Caulfield", grade: "Group 1", distance_m: 1600, blurb: "The premier mile for three-year-olds; a proving ground for future stallions." },
        FeatureRace { name: "Caulfield Cup", date: d(2026, 10, 17), venue: "Caulfield", grade: "Group 1", distance_m: 2400, blurb: "Australia's richest handicap over 2400 m and the traditional lead-up to the Melbourne Cup." },
        FeatureRace { name: "Cox Plate", date: d(2026, 10, 24), venue: "Moonee Valley", grade: "Group 1", distance_m: 2040, blurb: "The weight-for-age championship of Australasia, run on the tight Moonee Valley circuit." },
        FeatureRace { name: "Victoria Derby", date: d(2026, 10, 31), venue: "Flemington", grade: "Group 1", distance_m: 2500, blurb: "The classic for three-year-olds that opens Melbourne Cup week." },
        FeatureRace { name: "Melbourne Cup", date: d(2026, 11, 3), venue: "Flemington", grade: "Group 1", distance_m: 3200, blurb: "The race that stops a nation: a 3200 m handicap run on the first Tuesday of November since 1861." },
        FeatureRace { name: "VRC Oaks", date: d(2026, 11, 5), venue: "Flemington", grade: "Group 1", distance_m: 2500, blurb: "The fillies' classic on Oaks Day." },
        FeatureRace { name: "Champions Stakes", date: d(2026, 11, 7), venue: "Flemington", grade: "Group 1", distance_m: 2000, blurb: "The weight-for-age feature that closes the Flemington carnival." },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmaker_brands_are_removed_from_names() {
        assert_eq!(without_bookmakers("SPORTSBET LONGREACH"), "LONGREACH");
        assert_eq!(without_bookmakers("TAB HANDICAP"), "HANDICAP");
        assert_eq!(
            without_bookmakers("Ladbrokes - Fast Form Plate"),
            "Fast Form Plate"
        );
        assert_eq!(
            without_bookmakers("Bonus on prizemoney pools for TAB races."),
            "Bonus on prizemoney pools for races."
        );
        assert_eq!(without_bookmakers("Tableland Cup"), "Tableland Cup");
    }

    #[test]
    fn jump_outs_and_trials_are_not_starts() {
        let start = |condition: &str, class: &str| PastStart {
            condition: condition.into(),
            class: class.into(),
            ..Default::default()
        };
        assert!(start("Jump", "Out - S5").is_trial());
        assert!(start("Good 4", "OPEN-BT").is_trial());
        assert!(!start("Soft 6", "MEMSIE Group 1").is_trial());
        assert!(!start("Good 4", "").is_trial());
    }

    #[test]
    fn prize_text_reads_as_a_spoken_purse() {
        let text = "2,000,000.1st $1,200,000, 2nd $360,000, 3rd $180,000";
        assert_eq!(prize_total(text), Some(2_000_000));
        assert_eq!(prize_total("$5,000,000"), Some(5_000_000));
        assert_eq!(prize_total(""), None);
        assert_eq!(spoken_money(2_000_000), "$2 million");
        assert_eq!(spoken_money(1_500_000), "$1.5 million");
        assert_eq!(spoken_money(150_000), "$150,000");
    }
}
