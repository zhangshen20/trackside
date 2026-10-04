//! Turns the archived Racing Australia JSON into Trackside's model.
//!
//! The archive (written nightly by a separate pipeline, see `docs/prior-work.md`) holds one
//! `RA_Fields_<date>_<STATE>_<VENUE>.JSON` per meeting and one
//! `RA_Form_<date>_<VENUE>_<race>_<runner>.JSON` per runner. The wire structs below mirror
//! only the fields Trackside needs; unknown fields, including every price field, are ignored
//! by serde and never reach the model.

pub mod archive;
pub mod snapshot;
pub mod wire;

use anyhow::{Context, Result};
use chrono::NaiveDate;

use trackside_core::*;

/// Parse one `RA_Fields_*.JSON` document into a meeting.
pub fn meeting_from_fields(json: &str) -> Result<Meeting> {
    let f: wire::RaFields = serde_json::from_str(json).context("parsing RA_Fields JSON")?;
    let date = NaiveDate::parse_from_str(&f.date, "%Y-%m-%d")
        .with_context(|| format!("RA_Fields date {:?}", f.date))?;
    let races = f
        .races
        .iter()
        .map(|r| RaceCard {
            race_number: r.race_number.max(0) as u32,
            name: race_name(&r.race_name),
            start_local: r.start_local.trim().to_string(),
            distance_m: r.distance_m.and_then(|d| u32::try_from(d).ok()),
            class: without_bookmakers(&r.conditions),
            prize: without_bookmakers(&r.prize),
            grade: grade_from(&r.race_name, &r.conditions, &f.state),
            runners: r
                .runners
                .iter()
                .map(|x| Runner {
                    number: x.number.unwrap_or(0).max(0) as u32,
                    horse: clean_horse(&x.horse),
                    jockey: clean_person(&x.jockey),
                    trainer: x.trainer.trim().to_string(),
                    barrier: x.barrier.and_then(|b| u32::try_from(b).ok()),
                    weight_kg: x.weight_kg,
                    rating: x.hcp_rating,
                    last10: x.last10.trim().to_string(),
                    scratched: x.scratched,
                })
                .collect(),
        })
        .collect();
    Ok(Meeting {
        date,
        state: f.state.trim().to_string(),
        venue: title_case(&venue_name(&f.venue)),
        track_condition: None,
        rail: None,
        weather: None,
        races,
    })
}

/// Parse one `RA_Form_*.JSON` document into a horse's form. Price fluctuations are dropped.
pub fn form_from_json(json: &str, trainer: &str) -> Result<HorseForm> {
    let f: wire::RaHorseForm = serde_json::from_str(json).context("parsing RA_Form JSON")?;
    let rec = |r: &wire::RaRecord| Record {
        starts: r.starts.max(0) as u32,
        wins: r.wins.max(0) as u32,
        seconds: r.seconds.max(0) as u32,
        thirds: r.thirds.max(0) as u32,
    };
    let mut starts: Vec<PastStart> = f
        .starts
        .iter()
        .filter(|s| !s.trial)
        .filter_map(|s| {
            let date = parse_ra_date(&s.date)?;
            Some(PastStart {
                date,
                venue: venue_name(&s.track),
                distance_m: s.distance_m.and_then(|d| u32::try_from(d).ok()),
                condition: split_condition(&s.condition),
                class: without_bookmakers(&s.class),
                finish: s.finish.and_then(|v| u32::try_from(v).ok()),
                starters: s.starters.and_then(|v| u32::try_from(v).ok()),
                margin_lengths: s.margin_l,
                jockey: clean_person(&s.jockey),
                weight_kg: s.carried_kg.or(s.weight_kg),
                barrier: s.barrier.and_then(|v| u32::try_from(v).ok()),
                time: s.time.clone(),
                last_600_s: s.last_600,
                pos_800: s.pos_800.and_then(|v| u32::try_from(v).ok()),
                pos_400: s.pos_400.and_then(|v| u32::try_from(v).ok()),
            })
        })
        .filter(|s: &PastStart| !s.is_trial())
        .collect();
    starts.sort_by_key(|s| std::cmp::Reverse(s.date));
    Ok(HorseForm {
        horse: clean_horse(&f.horse),
        trainer: trainer.trim().to_string(),
        career: rec(&f.career),
        first_up: rec(&f.first_up),
        second_up: rec(&f.second_up),
        good: rec(&f.good),
        soft: rec(&f.soft),
        heavy: rec(&f.heavy),
        starts,
    })
}

/// The archive's file name for a meeting's fields, given its parts.
pub fn fields_file_name(date: NaiveDate, state: &str, venue: &str) -> String {
    format!(
        "RA_Fields_{}_{}_{}.JSON",
        date.format("%Y-%m-%d"),
        state,
        venue_token(venue)
    )
}

/// The archive's file name for one runner's form.
pub fn form_file_name(
    date: NaiveDate,
    venue: &str,
    race_number: u32,
    runner_number: u32,
) -> String {
    format!(
        "RA_Form_{}_{}_{}_{}.JSON",
        date.format("%Y-%m-%d"),
        venue_token(venue),
        race_number,
        runner_number
    )
}

/// Racing Australia dates read "18Mar26".
pub fn parse_ra_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%d%b%y").ok()
}

/// "Soft6" -> "Soft 6"; "Good" stays "Good".
fn split_condition(s: &str) -> String {
    let s = s.trim();
    match s.find(|c: char| c.is_ascii_digit()) {
        Some(i) if i > 0 => format!("{} {}", &s[..i], &s[i..]),
        _ => s.to_string(),
    }
}

/// Words that say a race name is still carrying a wagering message once the known brands
/// and products are gone. A name with one of these left in it is dropped whole, and the
/// server speaks the race by its number instead.
const WAGERING_WORDS: &[&str] = &[
    "odds", "bet", "bets", "betting", "wager", "wagering", "punt", "punter", "punters", "bookie",
    "bookies", "tote", "multi",
];

/// The race's name without wagering sponsors. Racing NSW and Queensland publish names in
/// capitals ("TAB EPSOM"); those are set in title case so a screen shows "Epsom".
fn race_name(raw: &str) -> String {
    let name = without_bookmakers(raw);
    let wagering = name.split_whitespace().any(|w| {
        let bare: String = w
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        WAGERING_WORDS.contains(&bare.as_str())
    });
    if wagering {
        return String::new();
    }
    if name.chars().any(|c| c.is_lowercase()) {
        name
    } else {
        title_case(&name)
    }
}

/// Australia's Group 1 races, as their names appear once sponsors are removed, lower case
/// with spaces and punctuation squashed out. The Racing Australia fields file carries no
/// grade and its conditions text is usually empty, so the name is what there is to go on.
/// Only Group 1 is listed: a wrong grade is worse than none, and the Group 1 label is the
/// one a listener asks about ("is the Epsom a Group 1?").
///
/// Some names are run in two states at different grades: Randwick's Queen Elizabeth Stakes
/// is a Group 1 and Flemington's a Group 3. Those carry the state whose race is the Group 1;
/// "" is a name that is one race wherever it is run.
const GROUP_ONE_RACES: &[(&str, &str)] = &[
    // New South Wales
    ("goldenslipper", ""),
    ("doncaster", ""),
    ("queenelizabethstakes", "NSW"),
    ("sydneycup", ""),
    ("tjsmithstakes", ""),
    ("australianderby", ""),
    ("australianoaks", ""),
    ("champagnestakes", ""),
    ("queenoftheturf", ""),
    ("allagedstakes", ""),
    ("rosehillguineas", ""),
    ("ranvetstakes", ""),
    ("georgeryderstakes", ""),
    ("thegalaxy", ""),
    ("vinerystudstakes", ""),
    ("coolmoreclassic", ""),
    ("randwickguineas", ""),
    ("canterburystakes", ""),
    ("chippingnortonstakes", ""),
    ("surroundstakes", ""),
    ("siresproducestakes", "NSW"),
    ("winxstakes", ""),
    ("goldenrose", ""),
    ("epsom", ""),
    ("metropolitan", ""),
    ("flightstakes", ""),
    ("springchampionstakes", ""),
    ("kingcharlesiiistakes", ""),
    // Victoria
    ("bluediamondstakes", ""),
    ("oakleighplate", ""),
    ("futuritystakes", ""),
    ("cforrstakes", ""),
    ("blackcaviarlightning", ""),
    ("lightning", "VIC"),
    ("australianguineas", ""),
    ("newmarkethandicap", ""),
    ("australiancup", ""),
    ("williamreidstakes", ""),
    ("memsiestakes", ""),
    ("makybedivastakes", ""),
    ("rupertclarkestakes", ""),
    ("underwoodstakes", ""),
    ("moirstakes", ""),
    ("turnbullstakes", ""),
    ("mightandpower", ""),
    ("caulfieldguineas", ""),
    ("toorakhandicap", ""),
    ("thousandguineas", ""),
    ("1000guineas", ""),
    ("caulfieldcup", ""),
    ("manikatostakes", ""),
    ("coxplate", ""),
    ("coolmorestudstakes", ""),
    ("victoriaderby", ""),
    ("empirerosestakes", ""),
    ("melbournecup", ""),
    ("vrcoaks", ""),
    ("kennedyoaks", ""),
    ("championsmile", ""),
    ("championssprint", ""),
    ("championsstakes", ""),
    // Queensland
    ("doomben10000", ""),
    ("doombencup", ""),
    ("kingsfordsmithcup", ""),
    ("stradbrokehandicap", ""),
    ("jjatkins", ""),
    ("queenslandderby", ""),
    ("queenslandoaks", ""),
    ("tattersallstiara", ""),
    // South Australia and Western Australia
    ("thegoodwood", ""),
    ("robertsangsterstakes", ""),
    ("australasianoaks", ""),
    ("railwaystakes", ""),
    ("winterbottomstakes", ""),
    ("northerlystakes", ""),
    ("kingstontownclassic", ""),
];

/// Words that make a race named after a Group 1 something else: its lead-up, its trial, a
/// race on its day, or a sponsor's message about it.
const NOT_THE_RACE_ITSELF: &[&str] = &[
    "prelude",
    "preview",
    "trial",
    "qualifier",
    "heat",
    "series",
    "consolation",
    "day",
    "eve",
    "week",
    "carnival",
    "season",
    "tour",
    "tickets",
    "sale",
    "book",
    "member",
];

/// The grade a race name or its conditions declare: "Group 1", "G1", "Gr 2", "Listed", or a
/// name on the Group 1 list, read with the state the meeting is in.
fn grade_from(name: &str, conditions: &str, state: &str) -> String {
    let text = format!("{name} {conditions}").to_ascii_lowercase();
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    for (i, t) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1).copied().unwrap_or("");
        let number = match (*t, next) {
            ("group" | "gr" | "g", "1" | "one") => Some(1),
            ("group" | "gr" | "g", "2" | "two") => Some(2),
            ("group" | "gr" | "g", "3" | "three") => Some(3),
            ("g1" | "gr1", _) => Some(1),
            ("g2" | "gr2", _) => Some(2),
            ("g3" | "gr3", _) => Some(3),
            _ => None,
        };
        if let Some(n) = number {
            return format!("Group {n}");
        }
        if *t == "listed" || *t == "lr" {
            return "Listed".to_string();
        }
    }
    let name = without_bookmakers(name).to_ascii_lowercase();
    let words: Vec<&str> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.iter().any(|w| NOT_THE_RACE_ITSELF.contains(w)) {
        return String::new();
    }
    // Sponsors come first ("YULONG GOLDEN ROSE"), so the race's own name ends the string, with
    // or without its generic last word ("Doncaster" and "Doncaster Mile"). A name that only
    // contains a Group 1's name ("Metropolitan Hotel Maiden Plate") is not that race.
    let squashed = words.concat();
    let stem = ["stakes", "handicap", "hcp", "mile"]
        .iter()
        .find_map(|w| squashed.strip_suffix(w))
        .unwrap_or(&squashed);
    let is_group_one = GROUP_ONE_RACES.iter().any(|(race, only_in)| {
        let before = [squashed.as_str(), stem]
            .iter()
            .find_map(|s| s.strip_suffix(race));
        // The Western Australian Derby, Oaks and Guineas are not the Australian ones.
        before.is_some_and(|b| !b.ends_with("western"))
            && (only_in.is_empty() || state.trim().eq_ignore_ascii_case(only_in))
    });
    if is_group_one {
        return "Group 1".to_string();
    }
    String::new()
}

/// Racing Australia writes "SAMPLE STAYER (NZ)"; keep the country tag, fix the case.
pub(crate) fn clean_horse(s: &str) -> String {
    let s = s.trim();
    let (name, tag) = match s.rfind(" (") {
        Some(i) if s.ends_with(')') => (&s[..i], &s[i..]),
        _ => (s, ""),
    };
    format!("{}{}", title_case(name), tag)
}

/// "Ms Emma Ly (a2/51kg), (late alt)" -> "Emma Ly": drop the honorific, the apprentice
/// claim and any trailing note, so the same rider matches across sources.
pub fn clean_person(s: &str) -> String {
    let s = s.split([',', '(']).next().unwrap_or_default().trim();
    let s = ["Ms ", "Mr ", "Mrs ", "Miss "]
        .iter()
        .find_map(|h| s.strip_prefix(h))
        .unwrap_or(s);
    s.trim().to_string()
}

/// Clubs, bodies, schemes and people written by their initials in race names. In capitals
/// they stay in capitals where an ordinary word is set in title case: "VRC Oaks", "TJ Smith
/// Stakes", "JJ Atkins".
const INITIALISMS: &[&str] = &[
    "vrc", "mrc", "mvrc", "atc", "stc", "ajc", "brc", "qtc", "sajc", "watc", "trc", "ttc", "crc",
    "rsl", "qtis", "bobs", "vobis", "tj", "jj", "cf", "wj", "bm",
];

/// Short words with no vowel that read as words all the same: "St Leger", "Mt Barker",
/// "Mr Brightside".
const NOT_INITIALISMS: &[&str] = &["st", "mt", "dr", "mr", "mrs", "ms", "jnr", "snr", "ltd"];

/// Capitals set as a name is written: "KING CHARLES III STAKES" is "King Charles III
/// Stakes", "VRC OAKS" is "VRC Oaks", "O'BRIEN" is "O'Brien", "MCGRATH" is "McGrath", and
/// "3YO" and "F&M" stay as they are.
pub(crate) fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(title_word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One word set in title case, unless the way it is written says it is not a plain word: a
/// condition that opens with a figure ("3YO", "0-58") or joins two with an ampersand
/// ("F&M"), or capitals that are a club's initials, a numeral in a royal name ("III") or
/// have no vowel at all ("TJ", "BM64"). The letter after a one-letter prefix and an
/// apostrophe, or after a leading Mc, is a capital too: O'Brien, D'Arcy, McGrath.
fn title_word(w: &str) -> String {
    let lower = w.to_lowercase();
    let letters: Vec<char> = w.chars().filter(|c| c.is_alphabetic()).collect();
    let in_capitals = !letters.is_empty() && letters.iter().all(|c| c.is_uppercase());
    let no_vowel = !letters
        .iter()
        .any(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y'));
    let as_written = w.starts_with(|c: char| c.is_ascii_digit())
        || w.contains('&')
        || (in_capitals
            && (INITIALISMS.contains(&lower.as_str())
                || is_roman_numeral(w)
                || (no_vowel && !NOT_INITIALISMS.contains(&lower.as_str()))));
    if as_written {
        return w.to_string();
    }
    let mut chars = lower.chars();
    let after_prefix =
        chars.next().is_some_and(|c| c.is_alphabetic()) && chars.next() == Some('\'');
    let capital_at = |i: usize| i == 0 || (i == 2 && (after_prefix || lower.starts_with("mc")));
    lower
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if capital_at(i) {
                c.to_uppercase().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// II to XXXIX in capitals, as in "King Charles III". A lone I, V or X is the same either way.
fn is_roman_numeral(w: &str) -> bool {
    let rest = w.trim_start_matches('X');
    w.len() >= 2
        && w.len() - rest.len() <= 3
        && matches!(
            rest,
            "" | "I" | "II" | "III" | "IV" | "V" | "VI" | "VII" | "VIII" | "IX"
        )
}

pub(crate) fn venue_token(venue: &str) -> String {
    venue.trim().to_uppercase().replace(' ', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELDS: &str = r#"{
      "date": "2026-10-17", "state": "VIC", "venue": "CAULFIELD", "key": "2026Oct17,VIC,Caulfield",
      "races": [{
        "race_number": 8, "race_name": "CAULFIELD CUP", "start_local": "5:00PM", "distance_m": 2400,
        "prize": "$5,000,000", "conditions": "Group 1. Handicap. 3YO+", "track_name": "Caulfield", "track_type": "Turf", "field_limit": "18",
        "runners": [
          {"number": 1, "last10": "x21131", "horse": "SAMPLE STAYER (NZ)", "trainer": "C Trainer", "jockey": "J Example", "barrier": 4, "weight_kg": 57.5, "hcp_rating": 108, "scratched": false, "form_url": "x"},
          {"number": 2, "last10": "4x2212", "horse": "DEMO MILER", "trainer": "M Yard", "jockey": "A Rider", "barrier": 11, "weight_kg": 55.0, "hcp_rating": 104, "scratched": true, "form_url": "x"}
        ]
      }]
    }"#;

    const FORM: &str = r#"{
      "date": "2026-10-17", "venue": "CAULFIELD", "race_number": 8, "number": 1, "horse": "SAMPLE STAYER (NZ)", "horse_code": "ABC=",
      "career": {"starts": 14, "wins": 5, "seconds": 3, "thirds": 2}, "prizemoney": 1234567.0,
      "first_up": {"starts": 3, "wins": 1, "seconds": 1, "thirds": 0}, "second_up": {"starts": 3, "wins": 2, "seconds": 0, "thirds": 0},
      "good": {"starts": 9, "wins": 4, "seconds": 2, "thirds": 1}, "soft": {"starts": 4, "wins": 1, "seconds": 1, "thirds": 1}, "heavy": {"starts": 1, "wins": 0, "seconds": 0, "thirds": 0},
      "starts": [
        {"trial": false, "finish": 3, "starters": 10, "track": "CAUL", "date": "05Sep26", "distance_m": 1800, "condition": "Soft5", "class": "Group 3", "prize": 200000.0, "jockey": "J Example", "weight_kg": 58.0, "carried_kg": null, "barrier": 2, "rating": 106, "time": "1:50.12", "last_600": 35.6, "margin_l": 1.5, "pos_800": 7, "pos_400": 6, "prices": [4.0, 4.2, 4.6], "raw": "..."},
        {"trial": false, "finish": 1, "starters": 12, "track": "FLEM", "date": "26Sep26", "distance_m": 2000, "condition": "Good4", "class": "Group 2", "prize": 300000.0, "jockey": "J Example (a)", "weight_kg": 59.0, "carried_kg": 57.0, "barrier": 6, "rating": 108, "time": "2:02.41", "last_600": 34.9, "margin_l": 0.8, "pos_800": 5, "pos_400": 3, "prices": [3.5], "raw": "..."},
        {"trial": true, "finish": 2, "starters": 6, "track": "FLEM", "date": "12Sep26", "distance_m": 1000, "condition": "Good4", "class": "OPEN-BT", "prize": null, "jockey": "", "weight_kg": 0.0, "carried_kg": null, "barrier": 0, "rating": null, "time": "0:58.00", "last_600": null, "margin_l": 1.0, "pos_800": null, "pos_400": null, "prices": [], "raw": "..."}
      ],
      "form_text": "…"
    }"#;

    #[test]
    fn fields_become_a_meeting_with_grades_and_case_fixed() {
        let m = meeting_from_fields(FIELDS).unwrap();
        assert_eq!(m.venue, "Caulfield");
        assert_eq!(m.date, NaiveDate::from_ymd_opt(2026, 10, 17).unwrap());
        let r = &m.races[0];
        assert_eq!(
            (
                r.race_number,
                r.name.as_str(),
                r.grade.as_str(),
                r.distance_m
            ),
            (8, "Caulfield Cup", "Group 1", Some(2400))
        );
        assert_eq!(r.runners[0].horse, "Sample Stayer (NZ)");
        assert_eq!(r.runners[0].rating, Some(108));
        assert!(r.runners[1].scratched);
    }

    #[test]
    fn group_ones_are_known_by_name_and_other_grades_by_their_marker() {
        for (name, conditions, state, grade) in [
            ("TAB EPSOM", "", "NSW", "Group 1"),
            ("ASAHI SUPER DRY METROPOLITAN", "", "NSW", "Group 1"),
            ("TAB Turnbull Stakes", "", "VIC", "Group 1"),
            ("Ladbrokes Manikato Stakes", "", "VIC", "Group 1"),
            ("YULONG GOLDEN ROSE", "", "NSW", "Group 1"),
            ("Penfolds Victoria Derby", "", "VIC", "Group 1"),
            ("Lexus Melbourne Cup", "", "VIC", "Group 1"),
            ("T J Smith Stakes", "", "NSW", "Group 1"),
            ("Sportsbet Caulfield Guineas Prelude", "", "VIC", ""),
            (
                "SAVE THE DATE WARWICK CUP SAT 10 OCT Maiden Plate",
                "",
                "QLD",
                "",
            ),
            ("Darley Maribyrnong Trial Stakes", "", "VIC", ""),
            ("Howden Super Impose Stakes", "", "VIC", ""),
            ("Metropolitan Hotel Maiden Plate", "", "NSW", ""),
            ("Doncaster Mile", "", "NSW", "Group 1"),
            ("Doomben 10,000", "", "QLD", "Group 1"),
            ("Gilgai Stakes", "G2 Open Handicap", "VIC", "Group 2"),
            (
                "Paris Lane Stakes",
                "3YO+ Gr 3 Set Weights",
                "VIC",
                "Group 3",
            ),
            ("Heritage Stakes", "LR. Quality", "VIC", "Listed"),
            // A name shared by a Group 1 and a lesser race in another state.
            ("Paramount+ Queen Elizabeth Stakes", "", "VIC", ""),
            ("QUEEN ELIZABETH STAKES", "", "VIC", ""),
            ("QUEEN ELIZABETH STAKES", "", "NSW", "Group 1"),
            ("VRC Sires' Produce Stakes", "", "VIC", ""),
            ("SIRES' PRODUCE STAKES", "", "NSW", "Group 1"),
            ("Lightning Stakes", "", "SA", ""),
            ("Lightning Stakes", "", "VIC", "Group 1"),
            ("Black Caviar Lightning", "", "VIC", "Group 1"),
            // The Western Australian classics are not the Australian ones.
            ("Western Australian Derby", "", "WA", ""),
            ("Western Australian Oaks", "", "WA", ""),
            ("Western Australian Guineas", "", "WA", ""),
            ("Australian Derby", "", "NSW", "Group 1"),
            ("Australian Oaks", "", "NSW", "Group 1"),
            ("Australian Guineas", "", "VIC", "Group 1"),
            ("South Australian Derby", "", "SA", "Group 1"),
            ("Queensland Derby", "", "QLD", "Group 1"),
            // The race's own name is what counts, with or without its generic last word.
            ("Sportsbet Might And Power", "", "VIC", "Group 1"),
            ("Might And Power Stakes", "", "VIC", "Group 1"),
        ] {
            assert_eq!(
                grade_from(name, conditions, state),
                grade,
                "{name} / {conditions} / {state}"
            );
        }
        assert_eq!(
            race_name("TAB ONE POOL Edward Manifold Stakes"),
            "Edward Manifold Stakes"
        );
        assert_eq!(race_name("TAB ONE POOL"), "");
        assert_eq!(
            race_name("ARROWFIELD BREEDERS' PLATE"),
            "Arrowfield Breeders' Plate"
        );
        assert_eq!(
            race_name("Howden Super Impose Stakes"),
            "Howden Super Impose Stakes"
        );
    }

    #[test]
    fn race_names_lose_wagering_slogans_or_go_altogether() {
        for (raw, name) in [
            ("LADBROKES ODDS BOOST HANDICAP", "Handicap"),
            ("SPORTSBET BET WITH MATES HANDICAP", "Handicap"),
            ("Same Race Multi Handicap", "Handicap"),
            ("Cash Out Handicap", "Handicap"),
            ("PRICE BOOST HANDICAP", "Handicap"),
            ("Each Way Extra Handicap", "Handicap"),
            ("Punter Assist Handicap", "Handicap"),
            ("Multiplier Handicap", "Handicap"),
            ("SKY RACING CLASS 3 PLATE", "Class 3 Plate"),
            // A wagering word the scrubbing did not know is reason to drop the whole name.
            ("BET NOW HANDICAP", ""),
            ("Punters Club Handicap", ""),
            ("MULTI MANIA BENCHMARK 64 Handicap", ""),
            ("Fixed Odds Plate", ""),
            ("Bookies Bag Handicap", ""),
            // Whole words only: Betty and Totem are not wagering.
            ("BETTY'S PLATE", "Betty's Plate"),
            ("Totem Handicap", "Totem Handicap"),
        ] {
            assert_eq!(race_name(raw), name, "{raw}");
        }
    }

    #[test]
    fn capitals_keep_initials_numerals_and_conditions_as_written() {
        for (raw, cased) in [
            ("CAULFIELD CUP", "Caulfield Cup"),
            ("TAB KING CHARLES III STAKES", "King Charles III Stakes"),
            ("TJ SMITH STAKES", "TJ Smith Stakes"),
            ("JJ ATKINS", "JJ Atkins"),
            ("VRC OAKS", "VRC Oaks"),
            ("ATC CUP", "ATC Cup"),
            ("O'BRIEN STAKES", "O'Brien Stakes"),
            ("D'ARCY HANDICAP", "D'Arcy Handicap"),
            ("3YO MAIDEN PLATE", "3YO Maiden Plate"),
            ("2YO F&M HANDICAP", "2YO F&M Handicap"),
            ("BM64 HANDICAP", "BM64 Handicap"),
            (
                "MCGRATH ESTATE AGENTS HANDICAP",
                "McGrath Estate Agents Handicap",
            ),
            ("ST LEGER", "St Leger"),
            ("ARROWFIELD BREEDERS' PLATE", "Arrowfield Breeders' Plate"),
        ] {
            assert_eq!(race_name(raw), cased, "{raw}");
        }
        assert_eq!(title_case("I AM INVINCIBLE"), "I Am Invincible");
        assert_eq!(title_case("Tom MCDONALD"), "Tom McDonald");
        assert_eq!(title_case("JD HAYES"), "JD Hayes");
        assert_eq!(title_case("SOFT5"), "Soft5");
        assert_eq!(title_case("LIV BYRNE"), "Liv Byrne");
    }

    #[test]
    fn form_drops_prices_and_trials_and_sorts_newest_first() {
        let f = form_from_json(FORM, "C Trainer").unwrap();
        assert_eq!(f.horse, "Sample Stayer (NZ)");
        assert_eq!(
            f.career,
            Record {
                starts: 14,
                wins: 5,
                seconds: 3,
                thirds: 2
            }
        );
        assert_eq!(f.starts.len(), 2, "trials are not starts");
        assert_eq!(
            f.starts[0].date,
            NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
        );
        assert_eq!(
            f.starts[0].weight_kg,
            Some(57.0),
            "carried weight wins over the allotted weight"
        );
        assert_eq!(f.starts[1].condition, "Soft 5");
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("price"), "no price field survives ingest");
    }

    #[test]
    fn people_lose_honorifics_and_claims() {
        assert_eq!(clean_person("Ms Emma Ly (a2/51kg)"), "Emma Ly");
        assert_eq!(
            clean_person("Ms Claire Ramsbotham (a0/52kg), (late alt)"),
            "Claire Ramsbotham"
        );
        assert_eq!(clean_person("Adrian Layt"), "Adrian Layt");
        assert_eq!(clean_person(""), "");
    }

    #[test]
    fn archive_file_names_match_the_pipeline() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 17).unwrap();
        assert_eq!(
            fields_file_name(d, "VIC", "Caulfield"),
            "RA_Fields_2026-10-17_VIC_CAULFIELD.JSON"
        );
        assert_eq!(
            form_file_name(d, "Moonee Valley", 9, 3),
            "RA_Form_2026-10-17_MOONEE_VALLEY_9_3.JSON"
        );
    }
}
