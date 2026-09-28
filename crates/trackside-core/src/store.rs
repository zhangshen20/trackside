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
}

fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace(['\'', '-', '.'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl FixtureStore {
    pub fn from_fixture(fixture: Fixture) -> Self {
        let form_by_horse = fixture
            .form
            .iter()
            .enumerate()
            .map(|(i, f)| (norm(&f.horse), i))
            .collect();
        Self {
            fixture,
            form_by_horse,
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("reading fixture {}", path.as_ref().display()))?;
        let fixture: Fixture = serde_json::from_str(&text).context("parsing fixture JSON")?;
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
        let venue = norm(venue);
        Ok(self
            .fixture
            .meetings
            .iter()
            .filter(|m| m.date == date && norm(&m.venue) == venue)
            .flat_map(|m| m.races.iter())
            .find(|r| r.race_number == race_number)
            .cloned())
    }

    async fn horse_form(&self, horse: &str) -> Result<Option<HorseForm>> {
        Ok(self
            .form_by_horse
            .get(&norm(horse))
            .map(|&i| self.fixture.form[i].clone()))
    }

    async fn race_result(
        &self,
        date: NaiveDate,
        venue: &str,
        race_number: u32,
    ) -> Result<Option<RaceResult>> {
        let venue = norm(venue);
        Ok(self
            .fixture
            .results
            .iter()
            .find(|r| r.date == date && norm(&r.venue) == venue && r.race_number == race_number)
            .cloned())
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
