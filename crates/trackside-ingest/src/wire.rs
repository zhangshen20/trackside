//! Wire shapes of the archived Racing Australia JSON. Only the fields Trackside reads are
//! declared; everything else (notably `prices`) is ignored on deserialisation.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RaFields {
    pub date: String,
    pub state: String,
    pub venue: String,
    pub races: Vec<RaFieldRace>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RaFieldRace {
    pub race_number: i64,
    pub race_name: String,
    pub start_local: String,
    pub distance_m: Option<i64>,
    pub prize: String,
    pub conditions: String,
    pub runners: Vec<RaFieldRunner>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RaFieldRunner {
    pub number: Option<i64>,
    pub last10: String,
    pub horse: String,
    pub trainer: String,
    pub jockey: String,
    pub barrier: Option<i64>,
    pub weight_kg: Option<f64>,
    pub hcp_rating: Option<i64>,
    pub scratched: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RaRecord {
    pub starts: i64,
    pub wins: i64,
    pub seconds: i64,
    pub thirds: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RaStart {
    pub trial: bool,
    pub finish: Option<i64>,
    pub starters: Option<i64>,
    pub track: String,
    pub date: String,
    pub distance_m: Option<i64>,
    pub condition: String,
    pub class: String,
    pub jockey: String,
    pub weight_kg: Option<f64>,
    pub carried_kg: Option<f64>,
    pub barrier: Option<i64>,
    pub time: String,
    pub last_600: Option<f64>,
    pub margin_l: Option<f64>,
    pub pos_800: Option<i64>,
    pub pos_400: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RaHorseForm {
    pub horse: String,
    pub career: RaRecord,
    pub first_up: RaRecord,
    pub second_up: RaRecord,
    pub good: RaRecord,
    pub soft: RaRecord,
    pub heavy: RaRecord,
    pub starts: Vec<RaStart>,
}
