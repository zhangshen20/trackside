//! Finding a name the way it was heard. Speech recognition splits and respells racing names:
//! "Jimmy's Star" for Jimmysstar, "Extra Galactic" for Extragalactic, "Cofield" for Caulfield,
//! "Jamie Car" for Jamie Kah. Names are compared by how they sound, after squashing out
//! spaces and punctuation, with a small edit distance allowed for what's left.

use crate::store::horse_key;

/// A name folded to an approximate sound: lower case, no spaces or punctuation, and common
/// English spellings of one sound written one way ("ph" and "f", "ck" and "k", double
/// letters), so that differently spelled homophones meet.
pub fn sound_key(name: &str) -> String {
    // Australian English drops a final r: "Miler" and "Myla" sound the same.
    let squashed: String = horse_key(name)
        .split_whitespace()
        .map(|w| match w.strip_suffix("er") {
            Some(stem) if !stem.is_empty() => format!("{stem}a"),
            _ => w.to_string(),
        })
        .collect::<String>()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let mut s = squashed;
    for (from, to) in [
        ("ph", "f"),
        ("ck", "k"),
        ("qu", "kw"),
        ("x", "ks"),
        ("wh", "w"),
        ("gh", "g"),
        ("kh", "k"),
        ("ee", "i"),
        ("ea", "i"),
        ("ie", "i"),
        ("ey", "i"),
        ("y", "i"),
        ("oo", "u"),
        ("ou", "u"),
        ("au", "o"),
        ("aw", "o"),
        ("ah", "a"),
        ("z", "s"),
    ] {
        s = s.replace(from, to);
    }
    // A hard c sounds like k; c before e, i or y like s.
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        let c = match (c, chars.get(i + 1)) {
            ('c', Some('e' | 'i')) => 's',
            ('c', _) => 'k',
            _ => c,
        };
        // Doubled letters sound single.
        if out.ends_with(c) {
            continue;
        }
        out.push(c);
    }
    // A silent final e ("Grande", "Lane").
    if out.len() > 3 && out.ends_with('e') {
        out.pop();
    }
    out
}

/// Levenshtein distance, for names of a few dozen letters.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// How far apart two names sound; `None` when too far to be the same name. Short names
/// must sound the same; longer ones may differ by one letter in five.
pub fn sounds_like(heard: &str, name: &str) -> Option<usize> {
    let (a, b) = (sound_key(heard), sound_key(name));
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let d = edit_distance(&a, &b);
    let allowed = (a.len().max(b.len()) / 5).min(3);
    (d <= allowed).then_some(d)
}

/// The names that sound like `heard`, closest first, at most `limit`.
pub fn closest<'a>(
    heard: &str,
    names: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<(&'a str, usize)> {
    let mut found: Vec<(&str, usize)> = names
        .into_iter()
        .filter_map(|n| sounds_like(heard, n).map(|d| (n, d)))
        .collect();
    found.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.len().cmp(&b.0.len())));
    found.dedup_by(|a, b| a.0 == b.0);
    found.truncate(limit);
    found
}

/// The one name `heard` must mean: the closest, when nothing else is as close.
pub fn best<'a>(heard: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    match closest(heard, names, 2).as_slice() {
        [(only, _)] => Some(only),
        [(first, d1), (_, d2)] if d1 < d2 => Some(first),
        _ => None,
    }
}

/// Words that name a kind of race rather than one race: "the Cup", "a Handicap", "the
/// Stakes". Said alone they could be any of dozens, so Trackside asks which one.
pub const GENERIC_RACE_WORDS: &[&str] = &[
    "a",
    "benchmark",
    "bm",
    "class",
    "classic",
    "cup",
    "derby",
    "final",
    "group",
    "guineas",
    "handicap",
    "hcp",
    "listed",
    "maiden",
    "mile",
    "novice",
    "oaks",
    "plate",
    "quality",
    "race",
    "restricted",
    "series",
    "sprint",
    "stakes",
    "the",
    "trophy",
    "welter",
];

/// A race name without a leading "the": "the Caulfield Cup" and "Caulfield Cup" are one race.
pub fn strip_the(name: &str) -> &str {
    let name = name.trim();
    match name.get(..4) {
        Some(lead) if lead.eq_ignore_ascii_case("the ") => name[4..].trim_start(),
        _ => name,
    }
}

/// Whether a race name heard is made only of generic words ("the Handicap", "Stakes",
/// "Maiden Plate"), so it names no race in particular.
pub fn is_generic_race_name(heard: &str) -> bool {
    let key = horse_key(heard);
    !key.is_empty()
        && key
            .split_whitespace()
            .all(|w| GENERIC_RACE_WORDS.contains(&w))
}

/// The words of a race name that tell it apart, generic words dropped, so "Manikato" finds
/// the Manikato Stakes.
fn race_core(name: &str) -> Vec<String> {
    horse_key(strip_the(name))
        .split_whitespace()
        .filter(|w| !GENERIC_RACE_WORDS.contains(w))
        .map(str::to_string)
        .collect()
}

/// How well a heard race name matches a published one, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameMatch {
    /// The same name, apart from case, punctuation and a leading "the".
    Exact,
    /// The whole name sounds alike, this many sounds apart ("Cofield Cup").
    Sound(usize),
    /// Only the telling words sound alike, the generic ones left out ("Manikato" for the
    /// Manikato Stakes, "Cox Plate" for a sponsor's "Sponsor Cox Plate").
    Core(usize),
    /// Generic words only ("the Cup"), all found in the name: fits too many races to take
    /// without a venue or a day.
    Words,
}

impl NameMatch {
    pub fn is_exact(self) -> bool {
        self == NameMatch::Exact
    }
}

/// How `heard` matches the race called `name`, or `None` when it doesn't. The whole name's
/// sound is tried before generic words are dropped, so "Caulfield Cup" matches the Caulfield
/// Cup as a whole and the Caulfield Guineas only through their shared "Caulfield"; a caller
/// keeps only the best tier it finds, and the Guineas never comes into it.
pub fn race_name_match(heard: &str, name: &str) -> Option<NameMatch> {
    let (h, n) = (horse_key(strip_the(heard)), horse_key(strip_the(name)));
    if h.is_empty() || n.is_empty() {
        return None;
    }
    if h == n {
        return Some(NameMatch::Exact);
    }
    if let Some(d) = sounds_like(&h, &n) {
        return Some(NameMatch::Sound(d));
    }
    let heard_core = race_core(heard).join(" ");
    if heard_core.is_empty() {
        // Generic words alone ("the Cup") fit any race whose name has them all; a caller
        // narrows these by venue or day before taking one.
        let name_words: Vec<&str> = n.split_whitespace().collect();
        return h
            .split_whitespace()
            .filter(|w| !matches!(*w, "the" | "a"))
            .all(|w| name_words.contains(&w))
            .then_some(NameMatch::Words);
    }
    let words = race_core(name);
    if words.is_empty() {
        return None;
    }
    // The core, or its trailing words: a sponsor's name leads, the race's own name follows.
    (0..words.len())
        .filter_map(|i| sounds_like(&heard_core, &words[i..].join(" ")))
        .min()
        .map(NameMatch::Core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn race_names_match_whole_before_their_parts() {
        assert_eq!(
            race_name_match("the Caulfield Cup", "Caulfield Cup"),
            Some(NameMatch::Exact)
        );
        assert!(matches!(
            race_name_match("Cofield Cup", "Caulfield Cup"),
            Some(NameMatch::Sound(_))
        ));
        // Whole names that differ meet only on their telling words, a tier below.
        assert!(matches!(
            race_name_match("Caulfield Cup", "Caulfield Guineas"),
            Some(NameMatch::Core(_))
        ));
        assert!(
            race_name_match("Cofield Cup", "Caulfield Cup")
                < race_name_match("Cofield Cup", "Caulfield Guineas")
        );
        assert_eq!(
            race_name_match("Manikato", "Manikato Stakes"),
            Some(NameMatch::Core(0))
        );
        assert_eq!(
            race_name_match("Cox Plate", "Sponsor Name Cox Plate"),
            Some(NameMatch::Core(0))
        );
        assert_eq!(
            race_name_match("the Cup", "Caulfield Cup"),
            Some(NameMatch::Words)
        );
        assert_eq!(race_name_match("Stakes", "Caulfield Cup"), None);
        assert_eq!(race_name_match("Epsom", "Caulfield Cup"), None);
        assert_eq!(strip_the("The Galaxy"), "Galaxy");
        assert_eq!(strip_the("Theodore Stakes"), "Theodore Stakes");
        for heard in [
            "the Handicap",
            "the Cup",
            "Stakes",
            "Plate",
            "Maiden",
            "maiden plate",
        ] {
            assert!(is_generic_race_name(heard), "{heard}");
        }
        for heard in ["Epsom", "Caulfield Cup", "the Galaxy", ""] {
            assert!(!is_generic_race_name(heard), "{heard}");
        }
    }

    #[test]
    fn misheard_names_sound_alike() {
        for (heard, name) in [
            ("Jimmy's Star", "Jimmysstar (NZ)"),
            ("Extra Galactic", "Extragalactic"),
            ("Cofield", "Caulfield"),
            ("Flemmington", "Flemington"),
            ("Moony Valley", "Moonee Valley"),
            ("Jamie Car", "Jamie Kah"),
            ("Mark Zarah", "Mark Zahra"),
            ("Demo Myla", "Demo Miler"),
            ("Ciaron Maher", "Ciaron Maher"),
        ] {
            assert!(sounds_like(heard, name).is_some(), "{heard} / {name}");
        }
        for (heard, name) in [
            ("Caulfield", "Flemington"),
            ("Jimmy", "Jimmysstar"),
            ("Kah", "Jamie Kah"),
        ] {
            assert!(sounds_like(heard, name).is_none(), "{heard} / {name}");
        }
    }

    #[test]
    fn the_best_match_must_be_clear() {
        let names = ["Extragalactic", "Extra Time", "Galactic Star"];
        assert_eq!(best("extra galactic", names), Some("Extragalactic"));
        // Two horses that sound the same: ask, don't guess.
        assert_eq!(
            best("Sea Biscuit", ["Seabiscuit", "Sea Biscuit (NZ)"]),
            None
        );
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
