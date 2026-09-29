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

/// A TAB results document for one race (`Results_OneRace_*.JSON`). Only the finishing order,
/// riders and scratchings are declared; the dividend and price blocks are never deserialised.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TabRaceResult {
    pub race_number: i64,
    pub race_status: String,
    pub scratchings: Vec<TabRunnerRef>,
    pub runners: Vec<TabRunner>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TabRunnerRef {
    pub runner_number: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TabRunner {
    pub runner_name: String,
    pub runner_number: i64,
    pub finishing_position: i64,
    pub rider_driver_name: String,
}

/// The day's meeting list (`Meetings_*.JSON`, `Results_Meetings_*.JSON`): track and weather.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TabMeetings {
    pub meetings: Vec<TabMeeting>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TabMeeting {
    pub meeting_name: String,
    pub location: String,
    pub race_type: String,
    pub weather_condition: Option<String>,
    pub track_condition: Option<String>,
}

/// One row of the sectional canon `races.csv`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SectionalRace {
    pub state: String,
    pub venue_key: String,
    pub venue_source: String,
    pub race_number: u32,
    pub track_condition: String,
    pub race_time_s: Option<f64>,
}

/// One row of the sectional canon `runners.csv`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SectionalRunner {
    pub state: String,
    pub venue_key: String,
    pub venue_source: String,
    pub race_number: u32,
    pub horse_name: String,
    pub saddle: Option<u32>,
    pub finish_position: Option<u32>,
    pub margin_l: Option<f64>,
    pub last_600_s: Option<f64>,
    pub source: String,
}
