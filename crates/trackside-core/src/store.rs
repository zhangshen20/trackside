use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::model::*;

/// Read-only access to racing facts. The MCP server only ever talks to this trait, so the
/// backing data (a JSON fixture today, the S3-fed store in production) is swappable.
#[async_trait]
pub trait Store: Send + Sync {
    async fn meetings(&self, date: NaiveDate) -> Result<Vec<Meeting>>;
    async fn race_card(
        &self,
        date: NaiveDate,
        venue: &str,
        race_number: u32,
    ) -> Result<Option<RaceCard>>;
    async fn horse_form(&self, horse: &str) -> Result<Option<HorseForm>>;
    async fn race_result(
        &self,
        date: NaiveDate,
        venue: &str,
        race_number: u32,
    ) -> Result<Option<RaceResult>>;
    async fn person_stats(
        &self,
        name: &str,
        role: &str,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> Result<Option<PersonStats>>;

    /// Where `horse` is engaged between `from` and `to` inclusive, soonest first: the fields
    /// published for those days scanned for its name. Fields come out two to three days
    /// ahead, so a short window is all a store can answer.
    async fn engagements(
        &self,
        horse: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<Engagement>> {
        let mut out = Vec::new();
        let mut day = from;
        while day <= to {
            out.extend(engagements_in(&self.meetings(day).await?, horse));
            match day.succ_opt() {
                Some(next) => day = next,
                None => break,
            }
        }
        Ok(out)
    }

    /// Horses whose names sound like `heard`, closest first, for "did you mean". The default
    /// store knows none.
    async fn similar_horses(&self, _heard: &str, _limit: usize) -> Result<Vec<String>> {
        Ok(vec![])
    }

    /// Jockeys or trainers whose names sound like `heard` (a surname alone will do),
    /// closest first.
    async fn similar_people(
        &self,
        _heard: &str,
        _role: &str,
        _limit: usize,
    ) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

/// The on-disk fixture shape: a bundle of meetings, form and results for demos and tests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fixture {
    pub meetings: Vec<Meeting>,
    pub form: Vec<HorseForm>,
    pub results: Vec<RaceResult>,
}

/// In-memory store loaded from a JSON fixture.
pub struct FixtureStore {
    fixture: Fixture,
    form_by_horse: HashMap<String, usize>,
    /// The same index keyed without country tags, for listeners who say "Jimmysstar" for
    /// "Jimmysstar (NZ)". A bare name shared by two horses points at the first one loaded.
    form_by_bare_name: HashMap<String, usize>,
}

/// Lower-cased, punctuation-free, single-spaced: the key every name lookup uses.
pub fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace(['\'', '.'], "")
        .replace(['-', ','], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A horse's name without its country tag, normalised: "Jimmysstar (NZ)" -> "jimmysstar".
pub fn horse_key(s: &str) -> String {
    let s = s.trim();
    let bare = match s.rfind(" (") {
        Some(i) if s.ends_with(')') => &s[..i],
        _ => s,
    };
    norm(bare)
}

/// Racing Australia's form lines name tracks by code ("CAUL", "CTRN"), not by name.
pub fn looks_like_track_code(s: &str) -> bool {
    (2..=5).contains(&s.len()) && s.chars().all(|c| c.is_ascii_uppercase())
}

/// Track codes seen in the archive whose names are certain. Everything else is learned
/// from the snapshot itself (see `resolve_track_codes`).
const KNOWN_TRACK_CODES: &[(&str, &str)] = &[("CAUL", "Caulfield"), ("FLEM", "Flemington")];

/// Tidy form for speaking: drop barrier trials, and replace track codes with the venue
/// names used everywhere else. A code is learned when a horse's form shows a start on a date
/// the snapshot also has a meeting for with that horse in the field.
fn tidy_form(fixture: &mut Fixture) {
    let mut names: HashMap<String, String> = KNOWN_TRACK_CODES
        .iter()
        .map(|(c, n)| (c.to_string(), n.to_string()))
        .collect();
    let mut ran_at: HashMap<(NaiveDate, String), &str> = HashMap::new();
    for m in &fixture.meetings {
        for r in &m.races {
            for x in &r.runners {
                ran_at.insert((m.date, horse_key(&x.horse)), &m.venue);
            }
        }
    }
    for f in &fixture.form {
        let key = horse_key(&f.horse);
        for s in &f.starts {
            if looks_like_track_code(&s.venue) && !names.contains_key(&s.venue) {
                if let Some(venue) = ran_at.get(&(s.date, key.clone())) {
                    names.insert(s.venue.clone(), venue.to_string());
                }
            }
        }
    }
    for f in &mut fixture.form {
        f.starts.retain(|s| !s.is_trial());
        for s in &mut f.starts {
            if let Some(name) = names.get(&s.venue) {
                s.venue = name.clone();
            }
        }
    }
}

/// The surnames in a person's name, one for each partner when it is a training partnership
/// ("Ben, Will & JD Hayes", "M. Price & M. Kent Jnr"), each alone and with the word before
/// it: "M. Price & M. Kent Jnr" gives "Price", "M. Price", "Jnr" and "Kent Jnr".
fn surnames(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in name.split([',', '&']).flat_map(|p| p.split(" and ")) {
        let words: Vec<&str> = part.split_whitespace().collect();
        if let Some(last) = words.last() {
            out.push(last.to_string());
        }
        if words.len() >= 2 {
            out.push(words[words.len() - 2..].join(" "));
        }
    }
    out
}

/// Venue names differ by source and sponsor: Racing Australia says "Rosehill Gardens" and
/// "Thomas Farms RC Murray Bridge", results feeds say "Rosehill" and "Murray Bridge", and
/// listeners say either. Two names match when one is the other's whole-word prefix or suffix.
pub fn venue_matches(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (long, short) = if a.len() >= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    long == short || long.starts_with(&format!("{short} ")) || long.ends_with(&format!(" {short}"))
}

impl FixtureStore {
    pub fn from_fixture(mut fixture: Fixture) -> Self {
        tidy_form(&mut fixture);
        let form_by_horse = fixture
            .form
            .iter()
            .enumerate()
            .map(|(i, f)| (norm(&f.horse), i))
            .collect();
        let mut form_by_bare_name = HashMap::new();
        for (i, f) in fixture.form.iter().enumerate() {
            form_by_bare_name.entry(horse_key(&f.horse)).or_insert(i);
        }
        Self {
            fixture,
            form_by_horse,
            form_by_bare_name,
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path.as_ref())
            .with_context(|| format!("reading fixture {}", path.as_ref().display()))?;
        Self::from_json(&bytes)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let fixture: Fixture = serde_json::from_slice(bytes).context("parsing fixture JSON")?;
        Ok(Self::from_fixture(fixture))
    }
}

#[async_trait]
impl Store for FixtureStore {
    async fn meetings(&self, date: NaiveDate) -> Result<Vec<Meeting>> {
        Ok(self
            .fixture
            .meetings
            .iter()
            .filter(|m| m.date == date)
            .cloned()
            .collect())
    }

    async fn race_card(
        &self,
        date: NaiveDate,
        venue: &str,
        race_number: u32,
    ) -> Result<Option<RaceCard>> {
        Ok(self
            .fixture
            .meetings
            .iter()
            .filter(|m| m.date == date && venue_matches(&m.venue, venue))
            .flat_map(|m| m.races.iter())
            .find(|r| r.race_number == race_number)
            .cloned())
    }

    async fn horse_form(&self, horse: &str) -> Result<Option<HorseForm>> {
        Ok(self
            .form_by_horse
            .get(&norm(horse))
            .or_else(|| self.form_by_bare_name.get(&horse_key(horse)))
            .map(|&i| self.fixture.form[i].clone()))
    }

    async fn race_result(
        &self,
        date: NaiveDate,
        venue: &str,
        race_number: u32,
    ) -> Result<Option<RaceResult>> {
        Ok(self
            .fixture
            .results
            .iter()
            .find(|r| {
                r.date == date && venue_matches(&r.venue, venue) && r.race_number == race_number
            })
            .cloned())
    }

    async fn similar_horses(&self, heard: &str, limit: usize) -> Result<Vec<String>> {
        Ok(crate::names::closest(
            heard,
            self.fixture.form.iter().map(|f| f.horse.as_str()),
            limit,
        )
        .into_iter()
        .map(|(n, _)| n.to_string())
        .collect())
    }

    async fn similar_people(&self, heard: &str, role: &str, limit: usize) -> Result<Vec<String>> {
        let mut names: Vec<&str> = Vec::new();
        for m in &self.fixture.meetings {
            for r in m.races.iter().flat_map(|r| r.runners.iter()) {
                names.push(if role == "trainer" {
                    &r.trainer
                } else {
                    &r.jockey
                });
            }
        }
        if role == "trainer" {
            names.extend(self.fixture.form.iter().map(|f| f.trainer.as_str()));
        } else {
            names.extend(
                self.fixture
                    .results
                    .iter()
                    .flat_map(|r| r.placings.iter().map(|p| p.jockey.as_str())),
            );
        }
        names.retain(|n| !n.trim().is_empty());
        names.sort_unstable();
        names.dedup();
        let found = crate::names::closest(heard, names.iter().copied(), limit);
        if !found.is_empty() || heard.split_whitespace().count() > 2 {
            return Ok(found.into_iter().map(|(n, _)| n.to_string()).collect());
        }
        // A surname alone ("how's Kah going") or with the word before it ("Kent Jnr"),
        // against every surname in the name: a training partnership has two or three.
        let mut by_surname: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| {
                surnames(n)
                    .iter()
                    .any(|s| crate::names::sounds_like(heard, s) == Some(0))
            })
            .collect();
        by_surname.truncate(limit);
        Ok(by_surname.into_iter().map(str::to_string).collect())
    }

    async fn person_stats(
        &self,
        name: &str,
        role: &str,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> Result<Option<PersonStats>> {
        // Counted from archived results: every placing carries the jockey; trainers come from
        // the horse's form. Enough for the fixture; the production store keeps a proper index.
        let wanted = norm(name);
        let mut record = Record::default();
        let mut seen = false;
        for result in &self.fixture.results {
            if from.is_some_and(|f| result.date < f) || to.is_some_and(|t| result.date > t) {
                continue;
            }
            for p in &result.placings {
                let matches = match role {
                    "jockey" => norm(&p.jockey) == wanted,
                    "trainer" => self
                        .form_by_horse
                        .get(&norm(&p.horse))
                        .is_some_and(|&i| norm(&self.fixture.form[i].trainer) == wanted),
                    _ => false,
                };
                if !matches {
                    continue;
                }
                seen = true;
                // Position 0 is a finisher outside the placings; it still counts as a start.
                record.starts += 1;
                match p.position {
                    1 => record.wins += 1,
                    2 => record.seconds += 1,
                    3 => record.thirds += 1,
                    _ => {}
                }
            }
        }
        Ok(seen.then(|| PersonStats {
            name: name.to_string(),
            role: role.to_string(),
            from,
            to,
            record,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venues_match_across_sponsors_and_sources() {
        assert!(venue_matches("Rosehill Gardens", "rosehill"));
        assert!(venue_matches(
            "Thomas Farms RC Murray Bridge",
            "MURRAY BRIDGE"
        ));
        assert!(venue_matches("Aquis Park Gold Coast", "Gold Coast"));
        assert!(venue_matches("Come-By-Chance,Picnic", "come by chance"));
        assert!(!venue_matches("Moonee Valley", "Valley Park"));
        assert!(!venue_matches("", "Caulfield"));
    }

    fn start(date: &str, venue: &str, condition: &str, class: &str) -> PastStart {
        PastStart {
            date: date.parse().unwrap(),
            venue: venue.into(),
            condition: condition.into(),
            class: class.into(),
            ..Default::default()
        }
    }

    fn snapshot() -> FixtureStore {
        let meeting = Meeting {
            date: "2026-09-22".parse().unwrap(),
            state: "VIC".into(),
            venue: "Sandown Hillside".into(),
            races: vec![RaceCard {
                race_number: 1,
                runners: vec![Runner {
                    horse: "Jimmysstar (NZ)".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        FixtureStore::from_fixture(Fixture {
            meetings: vec![meeting],
            form: vec![HorseForm {
                horse: "Jimmysstar (NZ)".into(),
                starts: vec![
                    start("2026-09-22", "SAND", "Good 4", "BM78"),
                    start("2026-09-14", "CTRN", "Jump", "Out - S5"),
                    start("2026-08-29", "CAUL", "Soft 6", "MEMSIE Group 1"),
                    start("2026-08-01", "SAND", "Good 3", "BM70"),
                    start("2026-07-01", "ZZZZ", "Good 3", "BM70"),
                ],
                ..Default::default()
            }],
            results: vec![],
        })
    }

    #[tokio::test]
    async fn horses_are_found_without_their_country_tag() {
        let store = snapshot();
        for name in ["Jimmysstar (NZ)", "jimmysstar", "JIMMYSSTAR"] {
            let form = store.horse_form(name).await.unwrap();
            assert_eq!(form.unwrap().horse, "Jimmysstar (NZ)", "{name}");
        }
        assert!(store.horse_form("Jimmy").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn trainers_are_found_by_surname_in_partnerships_too() {
        let runner = |trainer: &str| Runner {
            trainer: trainer.into(),
            ..Default::default()
        };
        let store = FixtureStore::from_fixture(Fixture {
            meetings: vec![Meeting {
                date: "2026-10-17".parse().unwrap(),
                races: vec![RaceCard {
                    race_number: 1,
                    runners: vec![
                        runner("C. Waller"),
                        runner("Ben, Will & JD Hayes"),
                        runner("M. Price & M. Kent Jnr"),
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        for (heard, want) in [
            ("Waller", "C. Waller"),
            ("Hayes", "Ben, Will & JD Hayes"),
            ("Price", "M. Price & M. Kent Jnr"),
            ("Kent Jnr", "M. Price & M. Kent Jnr"),
        ] {
            let found = store.similar_people(heard, "trainer", 3).await.unwrap();
            assert_eq!(found, vec![want.to_string()], "{heard}");
        }
        assert!(store
            .similar_people("Maher", "trainer", 3)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn form_drops_jump_outs_and_names_tracks() {
        let form = snapshot().horse_form("Jimmysstar").await.unwrap().unwrap();
        let venues: Vec<_> = form.starts.iter().map(|s| s.venue.as_str()).collect();
        // SAND is learned from the 22 Sep meeting; CAUL is known; ZZZZ stays a code.
        assert_eq!(
            venues,
            ["Sandown Hillside", "Caulfield", "Sandown Hillside", "ZZZZ"]
        );
    }
}
