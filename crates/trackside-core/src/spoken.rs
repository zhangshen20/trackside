//! Racing figures as a person says them aloud. Screens keep Racing Australia's decimals and
//! short forms; only the spoken text goes through here, so a listener hears "three-quarters
//! of a length" rather than "0.8 lengths" and "Victoria" rather than "VIC".

use crate::model::Record;

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// A whole number under a hundred in words ("fifty-eight"); figures from a hundred up.
pub fn number_words(n: u32) -> String {
    match n {
        0..=19 => ONES[n as usize].to_string(),
        20..=99 => match n % 10 {
            0 => TENS[(n / 10) as usize].to_string(),
            u => format!("{}-{}", TENS[(n / 10) as usize], ONES[u as usize]),
        },
        _ => n.to_string(),
    }
}

/// A count of lengths: words up to twenty, figures past it.
fn count_words(n: u32) -> String {
    if n <= 20 {
        number_words(n)
    } else {
        n.to_string()
    }
}

/// A margin in lengths as a race caller says it, to the nearest quarter: "a head" under a
/// quarter, "half a length", "a length and a quarter", "three and a half lengths". The
/// snapshot carries Racing Australia's decimal rather than its abbreviation, so short
/// margins are not guessed into a nose or a neck. Nothing for a margin that is not a number.
pub fn margin_words(lengths: f64) -> String {
    if !lengths.is_finite() || lengths < 0.0 {
        return String::new();
    }
    if lengths < 0.005 {
        return "a dead heat".into();
    }
    if lengths < 0.25 {
        return "a head".into();
    }
    // Past ten lengths nobody counts the quarters.
    let quarters = if lengths >= 10.0 {
        (lengths.round() as u32) * 4
    } else {
        (lengths * 4.0).round() as u32
    };
    let (whole, part) = (quarters / 4, quarters % 4);
    match (whole, part) {
        (0, 1) => "a quarter of a length".into(),
        (0, 2) => "half a length".into(),
        (0, 3) => "three-quarters of a length".into(),
        (1, 0) => "a length".into(),
        (1, 1) => "a length and a quarter".into(),
        (1, 2) => "a length and a half".into(),
        (1, 3) => "a length and three-quarters".into(),
        (n, 0) => format!("{} lengths", count_words(n)),
        (n, 1) => format!("{} and a quarter lengths", count_words(n)),
        (n, 2) => format!("{} and a half lengths", count_words(n)),
        (n, _) => format!("{} and three-quarter lengths", count_words(n)),
    }
}

/// Seconds as read off the clock: "8.24" is "eight point two four", "58" is "fifty-eight".
fn seconds_words(s: &str) -> Option<String> {
    let (whole, fraction) = match s.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (s, None),
    };
    let whole: u32 = whole.parse().ok()?;
    let mut out = number_words(whole);
    if let Some(f) = fraction {
        if f.is_empty() || !f.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let digits: Vec<String> = f
            .chars()
            .map(|c| number_words(c.to_digit(10).unwrap_or(0)))
            .collect();
        out.push_str(" point ");
        out.push_str(&digits.join(" "));
    }
    Some(out)
}

/// A race time as a caller reads it: "1:08.24" is "one minute eight point two four",
/// "2:02.41" "two minutes two point four one" and "58.31" "fifty-eight point three one".
/// Anything that is not a time comes back as written.
pub fn race_time_words(time: &str) -> String {
    let t = time.trim();
    let said = match t.split_once(':') {
        Some((m, s)) => m.parse::<u32>().ok().and_then(|m| {
            let s = seconds_words(s)?;
            let unit = if m == 1 { "minute" } else { "minutes" };
            Some(format!("{} {unit} {s}", number_words(m)))
        }),
        None => seconds_words(t),
    };
    said.unwrap_or_else(|| t.to_string())
}

/// A state or territory by name: "VIC" is "Victoria", "NT" "the Northern Territory" and
/// "ACT" "the ACT", the way people say it. Anything else comes back as written.
pub fn state_name(code: &str) -> String {
    match code.trim().to_ascii_uppercase().as_str() {
        "VIC" => "Victoria",
        "NSW" => "New South Wales",
        "QLD" => "Queensland",
        "SA" => "South Australia",
        "WA" => "Western Australia",
        "TAS" => "Tasmania",
        "NT" => "the Northern Territory",
        "ACT" => "the ACT",
        _ => return code.trim().to_string(),
    }
    .to_string()
}

/// "1200 metres": a distance is never read as a bare "m".
pub fn distance_words(metres: u32) -> String {
    format!("{metres} metres")
}

/// A track rating as a sentence carries it: "Good 4" is "a good track, rated Good 4" and a
/// rating with no number ("Synthetic") is "a synthetic track". Empty for no rating.
pub fn going_words(condition: &str) -> String {
    let c = condition.trim();
    if c.is_empty() {
        return String::new();
    }
    let word: String = c
        .chars()
        .take_while(|ch| ch.is_alphabetic() || *ch == ' ')
        .collect::<String>()
        .trim()
        .to_lowercase();
    if word.is_empty() {
        return format!("a track rated {c}");
    }
    let article = if word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    if c.chars().any(|ch| ch.is_ascii_digit()) {
        format!("{article} {word} track, rated {c}")
    } else {
        format!("{article} {word} track")
    }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        one.to_string()
    } else {
        format!("{n} {many}")
    }
}

/// "5 wins from 9", "1 win from 3", "no wins from 4".
pub fn wins_from(wins: u32, starts: u32) -> String {
    let wins = match wins {
        0 => "no wins".to_string(),
        1 => "1 win".to_string(),
        n => format!("{n} wins"),
    };
    format!("{wins} from {starts}")
}

/// A record the way a person reads it out: "12 wins from 30 starts, with 5 seconds and 4
/// thirds", "no wins from 6 starts, with a second", "one start for one win", "no starts yet".
pub fn record_words(r: &Record) -> String {
    if r.starts == 0 {
        return "no starts yet".into();
    }
    if r.starts == 1 {
        return match (r.wins, r.seconds, r.thirds) {
            (1, ..) => "one start for one win",
            (_, 1, _) => "one start for a second",
            (_, _, 1) => "one start for a third",
            _ => "one start without a placing",
        }
        .into();
    }
    let places: Vec<String> = [
        (r.seconds, plural(r.seconds, "a second", "seconds")),
        (r.thirds, plural(r.thirds, "a third", "thirds")),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(_, said)| said)
    .collect();
    let head = format!("{} starts", wins_from(r.wins, r.starts));
    match places.as_slice() {
        [] => head,
        [one] => format!("{head}, with {one}"),
        [a, b] => format!("{head}, with {a} and {b}"),
        _ => head,
    }
}

/// Racing Australia's word-like race-name abbreviations, written out so a voice reads words
/// rather than letters: "Super MDN PLT" is "Super Maiden Plate", "BM64 HCP" "Benchmark 64
/// Handicap", "CL1" "Class 1", "F&M" "Fillies and Mares", "SW+P" "Set Weights and
/// Penalties", "3YO+" "Three-Year-Old and Upwards". Matched word by word, whatever its case;
/// every other word stays as written.
pub fn race_words(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    // An age shared across an ampersand: "3 & 4YO" is "Three and Four-Year-Old".
    let shared_age = |i: usize| {
        let n: Option<u32> = words[i].parse().ok().filter(|n| (2..=9).contains(n));
        let age_after = words.get(i + 2).is_some_and(|a| {
            expand(a.trim_end_matches(['.', ',']), false).is_some_and(|e| e.contains("-Year-Old"))
        });
        n.filter(|_| words.get(i + 1) == Some(&"&") && age_after)
    };
    let mut skip_ampersand = false;
    for (i, w) in words.iter().enumerate() {
        if skip_ampersand {
            skip_ampersand = false;
            out.push("and".to_string());
            continue;
        }
        if let Some(n) = shared_age(i) {
            let mut said = number_words(n);
            said[..1].make_ascii_uppercase();
            out.push(said);
            skip_ampersand = true;
            continue;
        }
        // Keep a trailing full stop or comma ("Group 1. Handicap. 3YO+").
        let core = w.trim_end_matches(['.', ',']);
        let tail = &w[core.len()..];
        let next_is_number = words
            .get(i + 1)
            .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()));
        let said = expand(core, next_is_number).unwrap_or_else(|| core.to_string());
        out.push(format!("{said}{tail}"));
    }
    out.join(" ")
}

fn expand(word: &str, next_is_number: bool) -> Option<String> {
    let upper = word.to_ascii_uppercase();
    let fixed = match upper.as_str() {
        "MDN" => Some("Maiden"),
        "PLT" => Some("Plate"),
        "HCP" | "HCAP" => Some("Handicap"),
        "STKS" => Some("Stakes"),
        "QLTY" => Some("Quality"),
        "SW" => Some("Set Weights"),
        "SWP" | "SW+P" => Some("Set Weights and Penalties"),
        "F&M" => Some("Fillies and Mares"),
        "CL" if next_is_number => Some("Class"),
        "BM" if next_is_number => Some("Benchmark"),
        _ => None,
    };
    if let Some(f) = fixed {
        return Some(f.to_string());
    }
    // A class or benchmark joined to its number: "CL1", "BM64", "BM64+".
    for (short, long) in [("CL", "Class"), ("BM", "Benchmark")] {
        if let Some(rest) = upper.strip_prefix(short) {
            let digits = rest.trim_end_matches('+');
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let plus = &rest[digits.len()..];
                return Some(format!("{long} {digits}{plus}"));
            }
        }
    }
    // An age: "2YO" is "Two-Year-Old", "3YO+" "Three-Year-Old and Upwards".
    let (age, upwards) = match upper.strip_suffix('+') {
        Some(a) => (a, true),
        None => (upper.as_str(), false),
    };
    let n: u32 = age.strip_suffix("YO")?.parse().ok()?;
    if !(2..=9).contains(&n) {
        return None;
    }
    let mut said = number_words(n);
    said[..1].make_ascii_uppercase();
    let said = format!("{said}-Year-Old");
    Some(if upwards {
        format!("{said} and Upwards")
    } else {
        said
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figures_read_the_way_a_person_says_them() {
        for (lengths, said) in [
            (0.8, "three-quarters of a length"),
            (1.3, "a length and a quarter"),
            (3.5, "three and a half lengths"),
            (0.1, "a head"),
            (0.0, "a dead heat"),
            (1.0, "a length"),
            (2.0, "two lengths"),
            (0.5, "half a length"),
            (0.3, "a quarter of a length"),
            (1.5, "a length and a half"),
            (1.9, "two lengths"),
            (2.6, "two and a half lengths"),
            (2.8, "two and three-quarter lengths"),
            (12.4, "twelve lengths"),
            (25.0, "25 lengths"),
        ] {
            assert_eq!(margin_words(lengths), said, "{lengths}");
        }
        for (time, said) in [
            ("1:08.24", "one minute eight point two four"),
            ("58.31", "fifty-eight point three one"),
            ("2:02.41", "two minutes two point four one"),
            ("34.9", "thirty-four point nine"),
            ("1:36", "one minute thirty-six"),
            ("not a time", "not a time"),
        ] {
            assert_eq!(race_time_words(time), said, "{time}");
        }
        for (code, said) in [
            ("VIC", "Victoria"),
            ("NSW", "New South Wales"),
            ("QLD", "Queensland"),
            ("SA", "South Australia"),
            ("WA", "Western Australia"),
            ("tas", "Tasmania"),
            ("NT", "the Northern Territory"),
            ("ACT", "the ACT"),
            ("NZ", "NZ"),
        ] {
            assert_eq!(state_name(code), said, "{code}");
        }
        assert_eq!(distance_words(1200), "1200 metres");
        assert_eq!(going_words("Good 4"), "a good track, rated Good 4");
        assert_eq!(going_words("Soft 5"), "a soft track, rated Soft 5");
        assert_eq!(going_words("Synthetic"), "a synthetic track");
        assert_eq!(going_words(""), "");
    }

    #[test]
    fn records_read_as_sentences() {
        let r = |starts, wins, seconds, thirds| Record {
            starts,
            wins,
            seconds,
            thirds,
        };
        for (record, said) in [
            (
                r(30, 12, 5, 4),
                "12 wins from 30 starts, with 5 seconds and 4 thirds",
            ),
            (r(1, 1, 0, 0), "one start for one win"),
            (r(1, 0, 1, 0), "one start for a second"),
            (r(1, 0, 0, 0), "one start without a placing"),
            (r(6, 0, 1, 0), "no wins from 6 starts, with a second"),
            (r(4, 1, 0, 1), "1 win from 4 starts, with a third"),
            (r(3, 2, 0, 0), "2 wins from 3 starts"),
            (r(0, 0, 0, 0), "no starts yet"),
        ] {
            assert_eq!(record_words(&record), said);
        }
        assert_eq!(wins_from(5, 9), "5 wins from 9");
    }

    #[test]
    fn race_name_abbreviations_are_written_out() {
        for (short, long) in [
            ("Super MDN PLT", "Super Maiden Plate"),
            ("3 & 4yo Handicap", "Three and Four-Year-Old Handicap"),
            ("Pick 3 & Win", "Pick 3 & Win"),
            ("BM64 HCP", "Benchmark 64 Handicap"),
            ("BM 70 Hcap", "Benchmark 70 Handicap"),
            ("CL1 SW", "Class 1 Set Weights"),
            ("F&M SW+P", "Fillies and Mares Set Weights and Penalties"),
            ("3YO MDN", "Three-Year-Old Maiden"),
            (
                "Group 1. Handicap. 3YO+",
                "Group 1. Handicap. Three-Year-Old and Upwards",
            ),
            ("Qlty Stks", "Quality Stakes"),
            ("Benchmark 64+", "Benchmark 64+"),
            // Words that only start like an abbreviation stay as written.
            ("Caulfield Cup", "Caulfield Cup"),
            ("Swan Plate", "Swan Plate"),
        ] {
            assert_eq!(race_words(short), long, "{short}");
        }
    }
}
