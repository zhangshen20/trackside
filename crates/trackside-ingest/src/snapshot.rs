//! Builds a Trackside snapshot (the `Fixture` shape the server loads) from the archive for a
//! range of days: Racing Australia fields and form, official results for finishing order and
//! riders, and the sectional canon for margins, times and the fastest last 600 m.
//!
//! Nothing price-shaped is read: the results wire type declares no dividend or odds fields,
//! and the form parser drops fluctuations, so the snapshot is odds-free by construction.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use futures::stream::{self, StreamExt};
use serde::Serialize;

use trackside_core::*;

use crate::archive::{Archive, Bucket};
use crate::{clean_horse, form_from_json, meeting_from_fields, title_case, wire};

/// How many archive reads run at once.
const PARALLEL_READS: usize = 48;

/// What went into a snapshot, per day, for the build log and the PR description.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DayReport {
    pub date: NaiveDate,
    pub meetings: usize,
    pub races: usize,
    pub runners: usize,
    pub form: usize,
    pub results: usize,
    pub sectional_highlights: usize,
    pub warnings: Vec<String>,
}

pub async fn build(
    archive: &dyn Archive,
    dates: &[NaiveDate],
) -> Result<(Fixture, Vec<DayReport>)> {
    let mut meetings = Vec::new();
    let mut results = Vec::new();
    // Latest day wins: a horse that ran twice this week keeps its most recent form.
    let mut form: BTreeMap<String, HorseForm> = BTreeMap::new();
    let mut reports = Vec::new();
    let mut dates = dates.to_vec();
    dates.sort();
    for date in dates {
        let day = build_day(archive, date)
            .await
            .with_context(|| format!("building {date}"))?;
        meetings.extend(day.meetings);
        results.extend(day.results);
        for f in day.form {
            form.insert(norm(&f.horse), f);
        }
        reports.push(day.report);
    }
    fold_results_into_form(&meetings, &results, &mut form);
    Ok((
        Fixture {
            meetings,
            form: form.into_values().collect(),
            results,
        },
        reports,
    ))
}

/// Racing Australia form is published before the race, so a horse that ran this week still
/// shows its previous start. Add each result in the snapshot as a start (and to the records)
/// when the form does not have it yet.
fn fold_results_into_form(
    meetings: &[Meeting],
    results: &[RaceResult],
    form: &mut BTreeMap<String, HorseForm>,
) {
    let mut results: Vec<&RaceResult> = results.iter().collect();
    results.sort_by_key(|r| r.date);
    for r in results {
        let card = meetings
            .iter()
            .find(|m| m.date == r.date && m.venue == r.venue)
            .and_then(|m| m.races.iter().find(|c| c.race_number == r.race_number));
        let condition = r.track_condition.clone().unwrap_or_default();
        for p in &r.placings {
            let Some(f) = form.get_mut(&norm(&p.horse)) else {
                continue;
            };
            if f.starts.iter().any(|s| s.date >= r.date) {
                continue;
            }
            let runner = card.and_then(|c| c.runners.iter().find(|x| x.number == p.number));
            let finish = (p.position > 0).then_some(p.position);
            f.starts.insert(
                0,
                PastStart {
                    date: r.date,
                    venue: r.venue.clone(),
                    distance_m: card.and_then(|c| c.distance_m),
                    condition: condition.clone(),
                    class: card.map(|c| c.class.clone()).unwrap_or_default(),
                    finish,
                    starters: Some(r.placings.len() as u32),
                    margin_lengths: p.margin_lengths,
                    jockey: p.jockey.clone(),
                    weight_kg: runner.and_then(|x| x.weight_kg),
                    barrier: runner.and_then(|x| x.barrier),
                    time: if finish == Some(1) {
                        r.winning_time.clone().unwrap_or_default()
                    } else {
                        String::new()
                    },
                    last_600_s: p.last_600_s.or_else(|| {
                        r.fastest_last_600
                            .as_ref()
                            .filter(|s| norm(&s.horse) == norm(&p.horse))
                            .map(|s| s.last_600_s)
                    }),
                    pos_800: None,
                    pos_400: None,
                },
            );
            let going = condition.to_ascii_lowercase();
            let mut records = vec![&mut f.career];
            if going.starts_with("good") || going.starts_with("firm") {
                records.push(&mut f.good);
            } else if going.starts_with("soft") {
                records.push(&mut f.soft);
            } else if going.starts_with("heavy") {
                records.push(&mut f.heavy);
            }
            for rec in records {
                rec.starts += 1;
                match finish {
                    Some(1) => rec.wins += 1,
                    Some(2) => rec.seconds += 1,
                    Some(3) => rec.thirds += 1,
                    _ => {}
                }
            }
        }
    }
}

struct Day {
    meetings: Vec<Meeting>,
    form: Vec<HorseForm>,
    results: Vec<RaceResult>,
    report: DayReport,
}

async fn get_json<T: serde::de::DeserializeOwned>(
    archive: &dyn Archive,
    bucket: Bucket,
    key: &str,
) -> Result<Option<T>> {
    let Some(bytes) = archive.get(bucket, key).await? else {
        return Ok(None);
    };
    Ok(Some(
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {key}"))?,
    ))
}

async fn get_csv<T: serde::de::DeserializeOwned>(
    archive: &dyn Archive,
    key: &str,
) -> Result<Vec<T>> {
    let Some(bytes) = archive.get(Bucket::Sectional, key).await? else {
        return Ok(Vec::new());
    };
    csv::Reader::from_reader(bytes.as_slice())
        .deserialize()
        .collect::<Result<Vec<T>, _>>()
        .with_context(|| format!("parsing {key}"))
}

async fn build_day(archive: &dyn Archive, date: NaiveDate) -> Result<Day> {
    let d = date.format("%Y-%m-%d").to_string();
    let day = format!("HorseRacing/JSON/{d}/");
    let mut report = DayReport {
        date,
        ..Default::default()
    };

    // Fields: one file per meeting. The file name carries the venue token the form files use.
    let fields_prefix = format!("{day}Open/RA_Fields_{d}_");
    let mut meetings: Vec<(String, Meeting)> = Vec::new();
    for key in archive.list(Bucket::Racing, &fields_prefix).await? {
        let Some(rest) = key
            .strip_prefix(&fields_prefix)
            .and_then(|r| r.strip_suffix(".JSON"))
        else {
            continue;
        };
        let token = rest
            .split_once('_')
            .map(|(_, t)| t)
            .unwrap_or(rest)
            .to_string();
        let Some(bytes) = archive.get(Bucket::Racing, &key).await? else {
            continue;
        };
        match std::str::from_utf8(&bytes)
            .map_err(anyhow::Error::from)
            .and_then(meeting_from_fields)
        {
            Ok(m) => meetings.push((token, m)),
            Err(e) => report.warnings.push(format!("{key}: {e:#}")),
        }
    }

    // Track and weather from the official meetings list; the results list is final.
    let tab_meetings = match get_json::<wire::TabMeetings>(
        archive,
        Bucket::Racing,
        &format!("{day}Results_Meetings_{d}.JSON"),
    )
    .await?
    {
        Some(m) => m,
        None => get_json(
            archive,
            Bucket::Racing,
            &format!("{day}Open/Meetings_{d}.JSON"),
        )
        .await?
        .unwrap_or_default(),
    };
    let thoroughbred: Vec<&wire::TabMeeting> = tab_meetings
        .meetings
        .iter()
        .filter(|m| m.race_type == "R")
        .collect();
    for (_, m) in &mut meetings {
        if let Some(tm) = thoroughbred
            .iter()
            .find(|t| t.location == m.state && venue_matches(&t.meeting_name, &m.venue))
        {
            m.track_condition = tm.track_condition.as_deref().map(condition_words);
            m.weather = tm.weather_condition.as_deref().map(weather_words);
        }
    }

    // Form: one file per runner, `RA_Form_<date>_<TOKEN>_<race>_<number>.JSON`.
    let trainers: HashMap<(String, u32, u32), String> = meetings
        .iter()
        .flat_map(|(token, m)| {
            m.races.iter().flat_map(move |r| {
                r.runners
                    .iter()
                    .map(move |x| ((token.clone(), r.race_number, x.number), x.trainer.clone()))
            })
        })
        .collect();
    let form_prefix = format!("{day}Open/RA_Form_{d}_");
    let form_keys = archive.list(Bucket::Racing, &form_prefix).await?;
    let fetched: Vec<(String, Result<Option<Vec<u8>>>)> = stream::iter(form_keys)
        .map(|key| async move {
            let body = archive.get(Bucket::Racing, &key).await;
            (key, body)
        })
        .buffer_unordered(PARALLEL_READS)
        .collect()
        .await;
    let mut form = Vec::new();
    for (key, body) in fetched {
        let Some(bytes) = body? else { continue };
        let trainer = key
            .strip_prefix(&form_prefix)
            .and_then(|r| r.strip_suffix(".JSON"))
            .and_then(|r| {
                let mut parts = r.rsplitn(3, '_');
                let number = parts.next()?.parse().ok()?;
                let race = parts.next()?.parse().ok()?;
                let token = parts.next()?.to_string();
                trainers.get(&(token, race, number)).cloned()
            })
            .unwrap_or_default();
        match std::str::from_utf8(&bytes)
            .map_err(anyhow::Error::from)
            .and_then(|s| form_from_json(s, &trainer))
        {
            Ok(f) if !f.horse.is_empty() => form.push(f),
            Ok(_) => {}
            Err(e) => report.warnings.push(format!("{key}: {e:#}")),
        }
    }

    // Official results for thoroughbred races: `Results_OneRace_<date>_<MEETING>_<race>_R.JSON`.
    let results_prefix = format!("{day}Results_OneRace_{d}_");
    let mut tab_results: Vec<(String, wire::TabRaceResult)> = Vec::new();
    for key in archive.list(Bucket::Racing, &results_prefix).await? {
        let Some(name) = key
            .strip_prefix(&results_prefix)
            .and_then(|r| r.strip_suffix("_R.JSON"))
            .and_then(|r| r.rsplit_once('_'))
            .map(|(name, _)| name.to_string())
        else {
            continue;
        };
        match get_json::<wire::TabRaceResult>(archive, Bucket::Racing, &key).await {
            Ok(Some(r)) => tab_results.push((name, r)),
            Ok(None) => {}
            Err(e) => report.warnings.push(format!("{e:#}")),
        }
    }

    let sect_races: Vec<wire::SectionalRace> =
        get_csv(archive, &format!("canonical/sectional/{d}/races.csv")).await?;
    let sect_runners: Vec<wire::SectionalRunner> =
        get_csv(archive, &format!("canonical/sectional/{d}/runners.csv")).await?;

    // Meetings off the official list (picnics, some country tracks) take the rating the
    // sectional canon recorded for their first race.
    for (_, m) in &mut meetings {
        if m.track_condition.is_none() {
            m.track_condition = sect_races
                .iter()
                .find(|s| {
                    s.state == m.state
                        && (venue_matches(&s.venue_source, &m.venue)
                            || venue_matches(&s.venue_key, &m.venue))
                        && !s.track_condition.trim().is_empty()
                })
                .map(|s| s.track_condition.trim().to_string());
        }
    }

    let mut results = Vec::new();
    for (_, m) in &meetings {
        // A results meeting name is only trusted when the day's list places it in this state,
        // so "Belmont" (WA) never picks up "Belmont Park" (USA).
        let same_meeting = |name: &str| {
            venue_matches(name, &m.venue)
                && thoroughbred
                    .iter()
                    .any(|t| t.location == m.state && t.meeting_name.eq_ignore_ascii_case(name))
        };
        let same_venue = |state: &str, key: &str, source: &str| {
            state == m.state && (venue_matches(source, &m.venue) || venue_matches(key, &m.venue))
        };
        for race in &m.races {
            let tab = tab_results
                .iter()
                .find(|(name, r)| same_meeting(name) && r.race_number == race.race_number as i64)
                .map(|(_, r)| r);
            let sect: Vec<&wire::SectionalRunner> = sect_runners
                .iter()
                .filter(|s| {
                    s.race_number == race.race_number
                        && same_venue(&s.state, &s.venue_key, &s.venue_source)
                })
                .collect();
            let sect_race = sect_races.iter().find(|s| {
                s.race_number == race.race_number
                    && same_venue(&s.state, &s.venue_key, &s.venue_source)
            });
            if let Some(result) = race_result(date, m, race, tab, &sect, sect_race) {
                if result.fastest_last_600.is_some() {
                    report.sectional_highlights += 1;
                }
                results.push(result);
            }
        }
    }

    report.meetings = meetings.len();
    report.races = meetings.iter().map(|(_, m)| m.races.len()).sum();
    report.runners = meetings
        .iter()
        .flat_map(|(_, m)| &m.races)
        .map(|r| r.runners.len())
        .sum();
    report.form = form.len();
    report.results = results.len();
    report.warnings.truncate(20);
    let mut meetings: Vec<Meeting> = meetings.into_iter().map(|(_, m)| m).collect();
    meetings.sort_by(|a, b| (&a.state, &a.venue).cmp(&(&b.state, &b.venue)));
    Ok(Day {
        meetings,
        form,
        results,
        report,
    })
}

/// Assemble one race's result. Finishing order and riders come from the official results;
/// positions past fourth, margins and times from the sectional canon when it covers the race.
fn race_result(
    date: NaiveDate,
    meeting: &Meeting,
    race: &RaceCard,
    tab: Option<&wire::TabRaceResult>,
    sect: &[&wire::SectionalRunner],
    sect_race: Option<&wire::SectionalRace>,
) -> Option<RaceResult> {
    let field = |number: u32| race.runners.iter().find(|r| r.number == number);
    let sect_for = |number: u32, horse: &str| {
        sect.iter()
            .find(|s| s.saddle == Some(number) || norm(&s.horse_name) == norm(horse))
    };

    let mut placings: Vec<Placing> = match tab {
        Some(tab) => tab
            .runners
            .iter()
            .filter(|r| {
                !tab.scratchings
                    .iter()
                    .any(|s| s.runner_number == r.runner_number)
            })
            .filter_map(|r| {
                let number = u32::try_from(r.runner_number).ok()?;
                let listed = field(number);
                let horse = listed
                    .map(|x| x.horse.clone())
                    .unwrap_or_else(|| clean_horse(&r.runner_name));
                let s = sect_for(number, &horse);
                let position = u32::try_from(r.finishing_position)
                    .ok()
                    .filter(|p| *p > 0)
                    .or(s.and_then(|s| s.finish_position))
                    .unwrap_or(0);
                Some(Placing {
                    position,
                    number,
                    horse,
                    jockey: rider(listed.map(|x| x.jockey.as_str()), &r.rider_driver_name),
                    margin_lengths: s.and_then(|s| s.margin_l).filter(|_| position != 1),
                    last_600_s: s.and_then(|s| s.last_600_s).filter(|t| *t > 0.0),
                })
            })
            .collect(),
        None => sect
            .iter()
            .filter_map(|s| {
                let position = s.finish_position?;
                let listed = s.saddle.and_then(field).or_else(|| {
                    race.runners
                        .iter()
                        .find(|r| norm(&r.horse) == norm(&s.horse_name))
                });
                Some(Placing {
                    position,
                    number: listed.map(|x| x.number).or(s.saddle).unwrap_or(0),
                    horse: listed
                        .map(|x| x.horse.clone())
                        .unwrap_or_else(|| clean_horse(&s.horse_name)),
                    jockey: listed.map(|x| x.jockey.clone()).unwrap_or_default(),
                    margin_lengths: s.margin_l.filter(|_| position != 1),
                    last_600_s: s.last_600_s.filter(|t| *t > 0.0),
                })
            })
            .collect(),
    };
    if !placings.iter().any(|p| p.position == 1) {
        return None;
    }
    placings.sort_by_key(|p| (p.position == 0, p.position, p.number));

    let fastest_last_600 = sect
        .iter()
        .filter_map(|s| Some((s.last_600_s?, s)))
        .filter(|(t, _)| *t > 0.0)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(t, s)| {
            let horse = s
                .saddle
                .and_then(field)
                .map(|x| x.horse.clone())
                .unwrap_or_else(|| clean_horse(&s.horse_name));
            SectionalHighlight {
                horse,
                last_600_s: t,
                source: sectional_source(&s.source),
            }
        });

    Some(RaceResult {
        date,
        venue: meeting.venue.clone(),
        race_number: race.race_number,
        placings,
        winning_time: sect_race.and_then(|s| s.race_time_s).map(race_time),
        track_condition: sect_race
            .map(|s| s.track_condition.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| meeting.track_condition.clone()),
        fastest_last_600,
    })
}

/// The official results name the rider who actually rode, in capitals. Keep the listed
/// spelling when it is the same person (it has the right case, e.g. "McDonald").
fn rider(listed: Option<&str>, official: &str) -> String {
    let official = official.trim();
    match listed {
        Some(l) if official.is_empty() || norm(l) == norm(official) => l.to_string(),
        _ => title_case(official),
    }
}

/// "SOFT5" -> "Soft 5".
fn condition_words(s: &str) -> String {
    let t = title_case(s.trim());
    match t.find(|c: char| c.is_ascii_digit()) {
        Some(i) if i > 0 => format!("{} {}", &t[..i], &t[i..]),
        _ => t,
    }
}

fn weather_words(s: &str) -> String {
    match s.trim() {
        "FINE" => "Fine".into(),
        "OCAST" => "Overcast".into(),
        "SHWRY" => "Showery".into(),
        "RAIN" => "Raining".into(),
        "CLDY" => "Cloudy".into(),
        "HOT" => "Hot".into(),
        other => title_case(other),
    }
}

fn sectional_source(s: &str) -> String {
    match s.trim() {
        "racingcom" => "racing.com".into(),
        "racingnsw" | "nsw" => "Racing NSW".into(),
        "qld" => "Racing Queensland".into(),
        "racingsa" | "sa" => "Racing SA".into(),
        "wa" | "racingwa" => "Racing and Wagering WA".into(),
        "racingaustralia" => SOURCE_RACING_AUSTRALIA.into(),
        other => other.to_string(),
    }
}

/// 77.07 -> "1:17.07".
fn race_time(secs: f64) -> String {
    let whole = secs.floor() as u64;
    let hundredths = ((secs - whole as f64) * 100.0).round() as u64;
    let (whole, hundredths) = if hundredths == 100 {
        (whole + 1, 0)
    } else {
        (whole, hundredths)
    };
    if whole >= 60 {
        format!("{}:{:02}.{:02}", whole / 60, whole % 60, hundredths)
    } else {
        format!("{whole}.{hundredths:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::MemoryArchive;

    const D: &str = "2026-09-26";

    fn archive() -> MemoryArchive {
        let mut a = MemoryArchive::default();
        let day = format!("HorseRacing/JSON/{D}/");
        a.put(
            Bucket::Racing,
            &format!("{day}Open/RA_Fields_{D}_NSW_ROSEHILL_GARDENS.JSON"),
            r#"{"date":"2026-09-26","state":"NSW","venue":"Rosehill Gardens","races":[{"race_number":1,"race_name":"TAB MIDWAY HANDICAP","start_local":"11:55AM","distance_m":1300,"prize":"$100,000","conditions":"BenchMark 78","runners":[
              {"number":2,"horse":"MISS SPACEGIRL","trainer":"A Yard","jockey":"Ms Jane Rider (a2/50kg)","barrier":2,"weight_kg":56.0,"last10":"x21","scratched":false},
              {"number":14,"horse":"CALL ME SASSY","trainer":"B Yard","jockey":"Tom McDonald","barrier":3,"weight_kg":58.0,"last10":"311","scratched":false},
              {"number":7,"horse":"LATE SCRATCHING","trainer":"C Yard","jockey":"Sam Other","barrier":9,"weight_kg":55.0,"last10":"5","scratched":false}]}]}"#,
        );
        a.put(
            Bucket::Racing,
            &format!("{day}Open/RA_Form_{D}_ROSEHILL_GARDENS_1_14.JSON"),
            r#"{"horse":"CALL ME SASSY","career":{"starts":5,"wins":2,"seconds":1,"thirds":1},"starts":[{"trial":false,"finish":1,"starters":10,"track":"ROSE","date":"12Sep26","distance_m":1200,"condition":"Good4","class":"BM72","jockey":"Tom McDonald","prices":[3.1,2.9],"raw":"$3.10/$2.90"}]}"#,
        );
        a.put(
            Bucket::Racing,
            &format!("{day}Results_Meetings_{D}.JSON"),
            r#"{"meetings":[{"meetingName":"ROSEHILL","location":"NSW","raceType":"R","trackCondition":"GOOD4","weatherCondition":"OCAST","exoticPools":[{"poolTotal":1000}]},
                            {"meetingName":"ROSEHILL","location":"NSW","raceType":"H","trackCondition":"GOOD"}]}"#,
        );
        a.put(
            Bucket::Racing,
            &format!("{day}Results_OneRace_{D}_ROSEHILL_1_R.JSON"),
            r#"{"raceNumber":1,"raceStatus":"Paying","scratchings":[{"runnerNumber":7}],"dividends":[{"amount":9.9}],"runners":[
              {"runnerName":"MISS SPACEGIRL","runnerNumber":2,"finishingPosition":2,"riderDriverName":"JANE RIDER","fixedOdds":{"returnWin":4.2}},
              {"runnerName":"CALL ME SASSY","runnerNumber":14,"finishingPosition":1,"riderDriverName":"TOM MCDONALD","fixedOdds":{"returnWin":3.1}},
              {"runnerName":"LATE SCRATCHING","runnerNumber":7,"finishingPosition":0,"riderDriverName":"SAM OTHER","fixedOdds":{"returnWin":21}}]}"#,
        );
        a.put(
            Bucket::Sectional,
            &format!("canonical/sectional/{D}/races.csv"),
            "race_date,state,venue_key,venue_source,race_number,race_key,race_name,distance_m,track_condition,rail,race_time_s,race_last_600_s\n\
             2026-09-26,NSW,ROSEHILL,Rosehill Gardens,1,k,MIDWAY,1300,Good 4,True,77.07,34.77\n",
        );
        a.put(
            Bucket::Sectional,
            &format!("canonical/sectional/{D}/runners.csv"),
            "race_date,state,venue_key,venue_source,race_number,horse_name,saddle,finish_position,margin_l,last_600_s,source\n\
             2026-09-26,NSW,ROSEHILL,Rosehill Gardens,1,CALL ME SASSY,14,1,,34.59,racingnsw\n\
             2026-09-26,NSW,ROSEHILL,Rosehill Gardens,1,MISS SPACEGIRL,2,2,0.1,34.01,racingnsw\n",
        );
        a
    }

    #[tokio::test]
    async fn a_day_becomes_meetings_form_and_results_without_prices() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let (fx, reports) = build(&archive(), &[date]).await.unwrap();

        let m = &fx.meetings[0];
        assert_eq!(m.venue, "Rosehill Gardens");
        assert_eq!(m.races[0].name, "MIDWAY HANDICAP", "sponsor dropped");
        assert_eq!(m.track_condition.as_deref(), Some("Good 4"));
        assert_eq!(m.weather.as_deref(), Some("Overcast"));
        assert_eq!(m.races[0].runners[0].jockey, "Jane Rider");

        assert_eq!(fx.form.len(), 1);
        assert_eq!(fx.form[0].trainer, "B Yard");
        let latest = &fx.form[0].starts[0];
        assert_eq!(
            (latest.date, latest.finish, latest.venue.as_str()),
            (date, Some(1), "Rosehill Gardens"),
            "the day's win is folded into form published before it"
        );
        assert_eq!(latest.time, "1:17.07");
        assert_eq!(fx.form[0].career.starts, 6);
        assert_eq!(fx.form[0].career.wins, 3);

        let r = &fx.results[0];
        let order: Vec<_> = r
            .placings
            .iter()
            .map(|p| (p.position, p.horse.as_str()))
            .collect();
        assert_eq!(
            order,
            [(1, "Call Me Sassy"), (2, "Miss Spacegirl")],
            "scratched runner dropped"
        );
        assert_eq!(r.placings[0].jockey, "Tom McDonald", "listed spelling kept");
        assert_eq!(r.placings[0].margin_lengths, None);
        assert_eq!(r.placings[1].margin_lengths, Some(0.1));
        assert_eq!(r.winning_time.as_deref(), Some("1:17.07"));
        let fastest = r.fastest_last_600.as_ref().unwrap();
        assert_eq!(
            (fastest.horse.as_str(), fastest.last_600_s),
            ("Miss Spacegirl", 34.01)
        );
        assert_eq!(fastest.source, "Racing NSW");
        assert_eq!(
            r.placings
                .iter()
                .find(|p| p.horse == "Miss Spacegirl")
                .unwrap()
                .last_600_s,
            Some(34.01),
            "each runner keeps its own last 600"
        );

        assert_eq!(reports[0].results, 1);
        assert!(reports[0].warnings.is_empty(), "{:?}", reports[0].warnings);

        let json = serde_json::to_string(&fx).unwrap().to_lowercase();
        for banned in ["price", "odds", "dividend", "pool", "return", "tab "] {
            assert!(!json.contains(banned), "{banned} leaked into the snapshot");
        }
    }

    #[test]
    fn times_read_like_a_race_caller() {
        assert_eq!(race_time(77.07), "1:17.07");
        assert_eq!(race_time(58.56), "58.56");
        assert_eq!(race_time(119.999), "2:00.00");
        assert_eq!(condition_words("SOFT5"), "Soft 5");
        assert_eq!(condition_words("GOOD"), "Good");
    }
}
