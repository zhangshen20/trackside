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

/// Wagering products, promotions and broadcast brands that sponsors write into race names
/// ("TAB ONE POOL Edward Manifold Stakes", "HKJC World Pool Paris Lane Stakes", "MORE ON
/// TOTE WIN Benchmark 78 Handicap", "LADBROKES ODDS BOOST Handicap", "SKY RACING Class 3
/// Plate"). Removed whole, longest first so that a phrase goes before any shorter phrase
/// inside it, and before the brand words.
pub const WAGERING_PHRASES: &[&str] = &[
    "tote win + 10% in october",
    "tote win +10% in october",
    "+ 10% in october",
    "+10% in october",
    "new betslip",
    "place extra",
    "more from tote with",
    "more on tote win",
    "same race multi",
    "same game multi",
    "best tote plus",
    "bet with mates",
    "each way extra",
    "extra winnings",
    "back yourself",
    "punter assist",
    "early payout",
    "bet builder",
    "hosted pots",
    "price boost",
    "odds boost",
    "world pool",
    "sky racing",
    "multiplier",
    "bonus bets",
    "money back",
    "power play",
    "bet boost",
    "best tote",
    "bonus bet",
    "one pool",
    "tote win",
    "cash out",
    "top tote",
    "top fluc",
    "cash in",
    "hkjc",
    "tote",
];

/// A venue's name without its wagering sponsor. Sponsored grounds are named "<brand> Park
/// <town>" ("Picklebet Park Warwick", "bet365 Park Kilmore"), so once the brand is gone the
/// sponsor's "Park" goes with it and the venue is the town. A ground whose own name has a Park
/// in it ("Aquis Park Gold Coast", "Pioneer Park") keeps it.
pub fn venue_name(raw: &str) -> String {
    let cleaned = without_bookmakers(raw);
    let lost_words = raw.split_whitespace().count() > cleaned.split_whitespace().count();
    let mut words = cleaned.split_whitespace();
    match (words.next(), lost_words) {
        (Some(first), true) if first.eq_ignore_ascii_case("park") => {
            let rest: Vec<&str> = words.collect();
            if rest.is_empty() {
                cleaned
            } else {
                rest.join(" ")
            }
        }
        _ => cleaned,
    }
}

/// Remove wagering brands and products from a race, venue or class name, keeping the rest
/// as written. Horse and people's names are never passed through this. A name that was
/// nothing but wagering words comes back empty, and the tools then say "race 2" instead.
pub fn without_bookmakers(s: &str) -> String {
    let s = without_phrases(s, WAGERING_PHRASES);
    // A percentage in a race name is what is left of a wagering promotion ("Ladbrokes Tote
    // Win +10% in October Plate" once the brand and product are gone): it goes too.
    let s = s
        .split_whitespace()
        .filter(|w| !is_percentage(w))
        .collect::<Vec<_>>()
        .join(" ");
    let s = s.as_str();
    let is_brand = |w: &str| {
        let bare: String = w
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        BOOKMAKER_BRANDS.contains(&bare.as_str())
    };
    // A brand can also be hyphenated onto a name ("SPORTSBET-BALLARAT"); other hyphens stay.
    let words: Vec<String> = s
        .split_whitespace()
        .map(|w| {
            w.split('-')
                .filter(|part| !is_brand(part))
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect();
    // A dash, plus, ampersand or colon standing alone stays only between two words that were
    // both kept ("0 - 65 Handicap", "Colts & Geldings"); one left over from a brand
    // ("Ladbrokes - Fast Form Plate", "Sportsbet & Neds Plate") goes. Joined onto a word
    // they are part of it: "3YO+" and "Benchmark 64+" keep their plus.
    let is_word = |w: &str| {
        !w.trim_matches(|c| matches!(c, '-' | '+' | '&' | ':'))
            .is_empty()
    };
    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    for (i, w) in words.iter().enumerate() {
        if !is_word(w) {
            let between = kept.last().is_some_and(|k| is_word(k))
                && words.get(i + 1).is_some_and(|n| is_word(n))
                && i > 0
                && is_word(&words[i - 1]);
            if between && !w.is_empty() {
                kept.push(w);
            }
            continue;
        }
        kept.push(w);
    }
    kept.join(" ")
        .trim_matches(|c: char| matches!(c, '-' | ',') || c.is_whitespace())
        .to_string()
}

/// "+10%", "10%", "+ 10%" once split: a number with a percent sign, and an optional plus.
fn is_percentage(w: &str) -> bool {
    let digits = w.trim_start_matches('+').trim_end_matches('%');
    w.ends_with('%') && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Remove every occurrence of each phrase, whatever its case, when it stands as whole words.
fn without_phrases(s: &str, phrases: &[&str]) -> String {
    let mut out = s.to_string();
    for phrase in phrases {
        loop {
            let lower = out.to_ascii_lowercase();
            let Some(at) = lower.match_indices(*phrase).map(|(i, _)| i).find(|&i| {
                let before = lower[..i].chars().next_back();
                let after = lower[i + phrase.len()..].chars().next();
                !before.is_some_and(|c| c.is_ascii_alphanumeric())
                    && !after.is_some_and(|c| c.is_ascii_alphanumeric())
            }) else {
                break;
            };
            out.replace_range(at..at + phrase.len(), " ");
        }
    }
    out
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

/// One horse's place in a published field: where and when it runs next (or ran), as a
/// stable report says it. Built from the meetings, see `Store::engagements`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Engagement {
    pub horse: String,
    pub date: NaiveDate,
    pub state: String,
    pub venue: String,
    pub race_number: u32,
    pub race_name: String,
    pub start_local: String,
    pub number: u32,
    pub barrier: Option<u32>,
    pub jockey: String,
    pub scratched: bool,
}

/// Every field in `meetings` that names `horse`, in meeting order.
pub fn engagements_in(meetings: &[Meeting], horse: &str) -> Vec<Engagement> {
    let key = crate::store::horse_key(horse);
    let mut out = Vec::new();
    for m in meetings {
        for r in &m.races {
            if let Some(x) = r
                .runners
                .iter()
                .find(|x| crate::store::horse_key(&x.horse) == key)
            {
                out.push(Engagement {
                    horse: x.horse.clone(),
                    date: m.date,
                    state: m.state.clone(),
                    venue: m.venue.clone(),
                    race_number: r.race_number,
                    race_name: r.name.clone(),
                    start_local: r.start_local.clone(),
                    number: x.number,
                    barrier: x.barrier,
                    jockey: x.jockey.clone(),
                    scratched: x.scratched,
                });
            }
        }
    }
    out
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

impl Record {
    /// The record as it is said aloud: "5 wins from 14 starts, with 3 seconds and 2 thirds",
    /// or "one start for one win". Screens read the numbers themselves.
    pub fn summary(&self) -> String {
        crate::spoken::record_words(self)
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

/// "1st", "2nd", "11th".
pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Where a horse usually settles in its races, from its positions at the 800 m in recent
/// starts. It describes past runs, not what will happen next time.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunStyle {
    /// "leader", "on-pace", "midfield" or "back".
    pub style: String,
    /// "usually leads", "usually races on the pace", ...
    pub phrase: String,
    /// Starts the style was read from.
    pub runs: u32,
    /// Average position at the 800 m in those starts.
    pub average_800: f64,
}

/// The run style from up to the last six starts with a position at the 800 m in a field of
/// five or more; none with fewer than two such starts.
pub fn run_style(starts: &[PastStart]) -> Option<RunStyle> {
    let runs: Vec<(f64, f64)> = starts
        .iter()
        .filter(|s| !s.is_trial())
        .filter_map(|s| {
            let pos = s.pos_800? as f64;
            let n = s.starters.filter(|&n| n >= 5)? as f64;
            (pos >= 1.0 && pos <= n).then_some((pos, (pos - 1.0) / (n - 1.0)))
        })
        .take(6)
        .collect();
    if runs.len() < 2 {
        return None;
    }
    let count = runs.len() as f64;
    let average_800 = runs.iter().map(|r| r.0).sum::<f64>() / count;
    let relative = runs.iter().map(|r| r.1).sum::<f64>() / count;
    let (style, phrase) = if average_800 <= 1.6 || relative <= 0.1 {
        ("leader", "usually leads or sits right on the lead")
    } else if relative <= 0.35 {
        ("on-pace", "usually races on the pace")
    } else if relative <= 0.65 {
        ("midfield", "usually settles midfield")
    } else {
        ("back", "usually settles back in the field and runs on")
    };
    Some(RunStyle {
        style: style.into(),
        phrase: phrase.into(),
        runs: runs.len() as u32,
        average_800: (average_800 * 10.0).round() / 10.0,
    })
}

impl PastStart {
    /// How this start was run, from its position at the 800 m and its finish: "came from
    /// 9th at the 800", "led at the 800", "dropped back from 2nd at the 800".
    pub fn run_story(&self) -> Option<String> {
        let at = self.pos_800?;
        let finish = self.finish?;
        if at == 0 || finish == 0 {
            return None;
        }
        Some(if at == 1 {
            if finish == 1 {
                "led at the 800 and kept going".to_string()
            } else {
                "led at the 800".to_string()
            }
        } else if at >= finish + 3 {
            format!("came from {} at the 800", ordinal(at))
        } else if finish >= at + 4 {
            format!("was {} at the 800 but dropped back", ordinal(at))
        } else {
            format!("was {} at the 800", ordinal(at))
        })
    }

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
    /// This runner's last 600 m in seconds, from sectional timing when available.
    pub last_600_s: Option<f64>,
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
    /// The blurb cut to a phrase a guide can say in one breath: "the race that stops a
    /// nation".
    pub tagline: &'static str,
}

pub fn spring_carnival_2026() -> Vec<FeatureRace> {
    let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
    vec![
        FeatureRace { name: "Caulfield Guineas", date: d(2026, 10, 10), venue: "Caulfield", grade: "Group 1", distance_m: 1600, blurb: "The premier mile for three-year-olds; a proving ground for future stallions.", tagline: "the premier mile for three-year-olds" },
        FeatureRace { name: "Caulfield Cup", date: d(2026, 10, 17), venue: "Caulfield", grade: "Group 1", distance_m: 2400, blurb: "Australia's richest handicap over 2400 metres and the traditional lead-up to the Melbourne Cup.", tagline: "the 2400 metre handicap that leads to the Melbourne Cup" },
        FeatureRace { name: "Cox Plate", date: d(2026, 10, 24), venue: "Moonee Valley", grade: "Group 1", distance_m: 2040, blurb: "The weight-for-age championship of Australasia, run on the tight Moonee Valley circuit.", tagline: "the weight-for-age championship of Australasia" },
        FeatureRace { name: "Victoria Derby", date: d(2026, 10, 31), venue: "Flemington", grade: "Group 1", distance_m: 2500, blurb: "The classic for three-year-olds that opens Melbourne Cup week.", tagline: "the classic for three-year-olds that opens Cup week" },
        FeatureRace { name: "Melbourne Cup", date: d(2026, 11, 3), venue: "Flemington", grade: "Group 1", distance_m: 3200, blurb: "The race that stops a nation: a 3200 metre handicap run on the first Tuesday of November since 1861.", tagline: "the race that stops a nation" },
        FeatureRace { name: "VRC Oaks", date: d(2026, 11, 5), venue: "Flemington", grade: "Group 1", distance_m: 2500, blurb: "The fillies' classic on Oaks Day.", tagline: "the fillies' classic" },
        FeatureRace { name: "Champions Stakes", date: d(2026, 11, 7), venue: "Flemington", grade: "Group 1", distance_m: 2000, blurb: "The weight-for-age feature that closes the Flemington carnival.", tagline: "the weight-for-age feature that closes the carnival" },
    ]
}

/// The feature race a listener means, by its name as heard: "Cox Plate", "the Cox Plate",
/// "Cocks Plate", "the Derby", "Oaks". `Err` carries the features it could be: several when
/// the words fit more than one ("Caulfield", "the Cup"), none when nothing fits, so the
/// caller can ask rather than guess.
pub fn resolve_feature(heard: &str) -> Result<FeatureRace, Vec<&'static str>> {
    let features = spring_carnival_2026();
    let named = |name: &str| {
        features
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .ok_or_else(Vec::new)
    };
    let mut said = crate::store::norm(heard);
    if let Some(rest) = said.strip_prefix("the ") {
        said = rest.to_string();
    }
    for tail in [" 2026", " day", " race"] {
        if let Some(rest) = said.strip_suffix(tail) {
            said = rest.to_string();
        }
    }
    // Names the sponsors and the history books use for the same races.
    for (alias, name) in [
        ("kennedy oaks", "VRC Oaks"),
        ("crown oaks", "VRC Oaks"),
        ("vrc derby", "Victoria Derby"),
        ("ws cox plate", "Cox Plate"),
        ("w s cox plate", "Cox Plate"),
        ("vrc champions stakes", "Champions Stakes"),
    ] {
        if said == alias {
            return named(name);
        }
    }
    if said.is_empty() {
        return Err(vec![]);
    }
    if let Some(f) = features.iter().find(|f| crate::store::norm(f.name) == said) {
        return Ok(f.clone());
    }
    // Every word said is a word of the name: "Cox", "Derby", "Caulfield".
    let words: Vec<&str> = said.split_whitespace().collect();
    let containing: Vec<&FeatureRace> = features
        .iter()
        .filter(|f| {
            let name = crate::store::norm(f.name);
            let name_words: Vec<&str> = name.split_whitespace().collect();
            words.iter().all(|w| name_words.contains(w))
        })
        .collect();
    match containing.as_slice() {
        [one] => return Ok((*one).clone()),
        [] => {}
        many => return Err(many.iter().map(|f| f.name).collect()),
    }
    let names: Vec<&'static str> = features.iter().map(|f| f.name).collect();
    if let Some(name) = crate::names::best(&said, names.iter().copied()) {
        return named(name);
    }
    // The name said inside a longer phrase: "Moonee Valley Cox Plate", "Cox Plate at the
    // Valley". The longest run of words that is a feature's name wins.
    for len in (1..words.len()).rev() {
        for run in words.windows(len) {
            if let Some(name) = crate::names::best(&run.join(" "), names.iter().copied()) {
                return named(name);
            }
        }
    }
    Err(crate::names::closest(&said, names.iter().copied(), 3)
        .into_iter()
        .map(|(n, _)| n)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_races_are_found_the_way_they_are_said() {
        for (heard, name) in [
            ("Cox Plate", "Cox Plate"),
            ("the Cox Plate", "Cox Plate"),
            ("cocks plate", "Cox Plate"),
            ("Melbourne Cup", "Melbourne Cup"),
            ("melbourne cup day", "Melbourne Cup"),
            ("the Derby", "Victoria Derby"),
            ("Oaks", "VRC Oaks"),
            ("Kennedy Oaks", "VRC Oaks"),
            ("Caulfield Guineas", "Caulfield Guineas"),
            ("Cofield Cup", "Caulfield Cup"),
            ("Champions Stakes", "Champions Stakes"),
            ("Moony Valley Cox Plate", "Cox Plate"),
            ("the Melbourne Cup at Flemington", "Melbourne Cup"),
        ] {
            assert_eq!(resolve_feature(heard).map(|f| f.name), Ok(name), "{heard}");
        }
        assert_eq!(
            resolve_feature("Caulfield").map(|f| f.name),
            Err(vec!["Caulfield Guineas", "Caulfield Cup"])
        );
        assert_eq!(
            resolve_feature("the Cup").map(|f| f.name),
            Err(vec!["Caulfield Cup", "Melbourne Cup"])
        );
        assert_eq!(
            resolve_feature("Golden Slipper").map(|f| f.name),
            Err(vec![])
        );
    }

    #[test]
    fn every_feature_has_a_short_tagline() {
        for f in spring_carnival_2026() {
            assert!(f.tagline.split_whitespace().count() <= 10, "{}", f.name);
            assert!(f.tagline.starts_with("the "), "{}", f.name);
        }
    }

    #[test]
    fn bookmaker_brands_are_removed_from_names() {
        assert_eq!(without_bookmakers("SPORTSBET LONGREACH"), "LONGREACH");
        assert_eq!(without_bookmakers("SPORTSBET-BALLARAT"), "BALLARAT");
        assert_eq!(without_bookmakers("Come-by-chance"), "Come-by-chance");
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
    fn a_sponsors_park_goes_with_the_sponsor() {
        assert_eq!(venue_name("Picklebet Park Warwick"), "Warwick");
        assert_eq!(venue_name("bet365 Park Kilmore"), "Kilmore");
        assert_eq!(venue_name("SPORTSBET-BALLARAT"), "BALLARAT");
        assert_eq!(venue_name("Aquis Park Gold Coast"), "Aquis Park Gold Coast");
        assert_eq!(venue_name("Pioneer Park"), "Pioneer Park");
        assert_eq!(venue_name("Warwick Farm"), "Warwick Farm");
    }

    #[test]
    fn wagering_products_are_removed_from_race_names() {
        assert_eq!(
            without_bookmakers("TAB ONE POOL Edward Manifold Stakes"),
            "Edward Manifold Stakes"
        );
        assert_eq!(
            without_bookmakers("HKJC World Pool Paris Lane Stakes"),
            "Paris Lane Stakes"
        );
        assert_eq!(
            without_bookmakers("LADBROKES MORE ON TOTE WIN BENCHMARK 78 Handicap"),
            "BENCHMARK 78 Handicap"
        );
        assert_eq!(
            without_bookmakers("LADBROKES TOTE WIN + 10% IN OCTOBER BENCHMARK 85 Handicap"),
            "BENCHMARK 85 Handicap"
        );
        assert_eq!(
            without_bookmakers("MORE FROM TOTE WITH LADBROKES 0 - 65 Handicap"),
            "0 - 65 Handicap"
        );
        assert_eq!(without_bookmakers("TAB ONE POOL"), "");
        // Whole words only: a horse called Totem or a Pooley Stakes keep their names.
        assert_eq!(
            without_bookmakers("Pooley Totem Stakes"),
            "Pooley Totem Stakes"
        );
    }

    #[test]
    fn wagering_slogans_are_removed_from_race_names() {
        for (name, cleaned) in [
            ("LADBROKES ODDS BOOST HANDICAP", "HANDICAP"),
            ("Sportsbet Bet With Mates Handicap", "Handicap"),
            ("Same Race Multi Handicap", "Handicap"),
            ("Same Game Multi Plate", "Plate"),
            ("Cash Out Handicap", "Handicap"),
            ("Cash In Handicap", "Handicap"),
            ("Price Boost Handicap", "Handicap"),
            ("Each Way Extra Handicap", "Handicap"),
            ("Punter Assist Handicap", "Handicap"),
            ("Multiplier Handicap", "Handicap"),
            ("Back Yourself Handicap", "Handicap"),
            ("Extra Winnings Handicap", "Handicap"),
            ("Neds Bet Builder Handicap", "Handicap"),
            (
                "TAB Bonus Bets Benchmark 64 Handicap",
                "Benchmark 64 Handicap",
            ),
            ("SKY RACING Class 3 Plate", "Class 3 Plate"),
            ("Sportsbet & Neds Plate", "Plate"),
        ] {
            assert_eq!(without_bookmakers(name), cleaned, "{name}");
        }
    }

    #[test]
    fn a_plus_joined_to_a_condition_stays() {
        assert_eq!(
            without_bookmakers("Group 1. Handicap. 3YO+"),
            "Group 1. Handicap. 3YO+"
        );
        assert_eq!(without_bookmakers("TAB Benchmark 64+"), "Benchmark 64+");
        assert_eq!(without_bookmakers("Colts & Geldings"), "Colts & Geldings");
        // Standing alone next to a removed brand, the sign goes with it.
        assert_eq!(
            without_bookmakers("Fast Form Plate + Sportsbet"),
            "Fast Form Plate"
        );
        assert_eq!(without_bookmakers("TAB: Maiden Plate"), "Maiden Plate");
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
