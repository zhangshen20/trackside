//! The Trackside tool surface. Every tool answers in two layers: a short spoken-style text
//! block for voice, and `structured_content` for screens and agents. No prices, ever.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use chrono_tz::Australia::Melbourne;
use rmcp::{
    handler::server::wrapper::Parameters, model::*, schemars, service::RequestContext, tool,
    tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use trackside_core::{
    horse_key, looks_like_track_code, prize_total, spoken_money, spring_carnival_2026,
    venue_matches, RaceCard, Record, Store, SOURCE_RACING_AUSTRALIA, SOURCE_SECTIONALS,
};

use crate::auth::Caller;

#[derive(Clone)]
pub struct Trackside {
    store: Arc<dyn Store>,
    /// Horses each user follows, keyed by the signed-in account (see `caller_key`). Held in
    /// memory, so a cold start forgets it; durable storage is a separate change.
    stable: Stable,
}

pub type Stable = Arc<RwLock<HashMap<String, Vec<String>>>>;

/// Whose stable a request reads: the OAuth subject when the server checks tokens, or one
/// shared list when it runs without auth (local development).
fn caller_key(ctx: &RequestContext<RoleServer>) -> String {
    ctx.extensions
        .get::<http::request::Parts>()
        .and_then(|p| p.extensions.get::<Caller>())
        .map(|c| c.subject.clone())
        .unwrap_or_else(|| "local".into())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DateArgs {
    /// Date as YYYY-MM-DD. Defaults to today in Australia/Melbourne.
    pub date: Option<String>,
    /// Optional state filter: VIC, NSW, QLD, SA, WA, TAS, NT or ACT.
    pub state: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RaceArgs {
    /// Venue name, e.g. "Caulfield" or "Flemington".
    pub venue: String,
    /// Race number on the card, e.g. 8.
    pub race_number: u32,
    /// Date as YYYY-MM-DD. Defaults to today in Australia/Melbourne.
    pub date: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HorseArgs {
    /// The horse's name as it appears in the field.
    pub horse: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PersonArgs {
    /// The jockey's or trainer's name.
    pub name: String,
    /// "jockey" or "trainer".
    pub role: String,
    /// Start of the period, YYYY-MM-DD. Optional.
    pub from: Option<String>,
    /// End of the period, YYYY-MM-DD. Optional.
    pub to: Option<String>,
}

fn parse_date(s: &Option<String>) -> Result<NaiveDate, McpError> {
    match s {
        None => Ok(Utc::now().with_timezone(&Melbourne).date_naive()),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| {
            McpError::invalid_params(format!("date must be YYYY-MM-DD, got {text}"), None)
        }),
    }
}

fn parse_opt_date(s: &Option<String>) -> Result<Option<NaiveDate>, McpError> {
    s.as_ref().map(|_| parse_date(s)).transpose()
}

fn internal(err: anyhow::Error) -> McpError {
    McpError::internal_error(err.to_string(), None)
}

fn answer(spoken: String, structured: serde_json::Value) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(spoken)]);
    result.structured_content = Some(structured);
    result
}

/// "the Manikato Stakes, a Group 1 race over 1200 metres": grade and class only when known,
/// so an empty field never leaves a gap in the sentence.
fn describe_race(card: &RaceCard) -> String {
    let label = if !card.grade.is_empty() {
        card.grade.as_str()
    } else if card.class.len() <= 40 {
        card.class.as_str()
    } else {
        ""
    };
    let kind = match label.chars().next() {
        None => "a race".to_string(),
        Some(c) if "AEIOUaeiou".contains(c) => format!("an {label} race"),
        Some(_) => format!("a {label} race"),
    };
    let distance = card
        .distance_m
        .map(|d| format!(" over {d} metres"))
        .unwrap_or_default();
    let purse = prize_total(&card.prize)
        .map(|n| format!(", worth {}", spoken_money(n)))
        .unwrap_or_default();
    format!("{kind}{distance}{purse}")
}

/// "a Good 4 track" for a track code we couldn't name drops the venue rather than read
/// out letters like "CTRN".
fn at_venue(venue: &str) -> String {
    if venue.is_empty() || looks_like_track_code(venue) {
        String::new()
    } else {
        format!(" at {venue}")
    }
}

/// "good going 7 from 17, soft 5 from 13", leaving out conditions it has never raced on.
fn going_records(records: &[(&str, &Record)]) -> String {
    let said: Vec<_> = records
        .iter()
        .filter(|(_, r)| r.starts > 0)
        .map(|(name, r)| format!("{name} {} from {}", r.wins, r.starts))
        .collect();
    if said.is_empty() {
        String::new()
    } else {
        format!(" On {}.", said.join(", "))
    }
}

fn fmt_len(m: Option<f64>) -> String {
    m.map(|v| format!("{v:.1} lengths")).unwrap_or_default()
}

#[tool_router]
impl Trackside {
    pub fn new(store: Arc<dyn Store>, stable: Stable) -> Self {
        Self { store, stable }
    }

    #[tool(
        description = "List the Australian thoroughbred race meetings on a date, with track condition and the first race time. Use for questions like 'what racing is on today' or 'is there racing at Flemington on Saturday'."
    )]
    async fn list_meetings(
        &self,
        Parameters(args): Parameters<DateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date)?;
        let mut meetings = self.store.meetings(date).await.map_err(internal)?;
        if let Some(state) = args.state.as_deref() {
            meetings.retain(|m| m.state.eq_ignore_ascii_case(state));
        }
        if meetings.is_empty() {
            return Ok(answer(
                format!("I don't have any meetings on {} yet. Fields are published two to three days ahead.", date.format("%A %-d %B")),
                json!({ "date": date, "meetings": [] }),
            ));
        }
        let spoken = meetings
            .iter()
            .map(|m| {
                let first = m
                    .races
                    .first()
                    .map(|r| r.start_local.as_str())
                    .unwrap_or("time to be confirmed");
                format!(
                    "{} in {}: {} race{}, track {}, first race {}.",
                    m.venue,
                    m.state,
                    m.races.len(),
                    if m.races.len() == 1 { "" } else { "s" },
                    m.track_condition
                        .clone()
                        .unwrap_or_else(|| "not yet rated".into()),
                    first
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        Ok(answer(
            format!(
                "According to {SOURCE_RACING_AUSTRALIA}, on {}: {spoken}",
                date.format("%A %-d %B")
            ),
            json!({ "date": date, "source": SOURCE_RACING_AUSTRALIA, "meetings": meetings.iter().map(|m| json!({
                "venue": m.venue, "state": m.state, "track_condition": m.track_condition, "rail": m.rail,
                "races": m.races.iter().map(|r| json!({"race_number": r.race_number, "name": r.name, "start_local": r.start_local, "distance_m": r.distance_m, "grade": r.grade})).collect::<Vec<_>>()
            })).collect::<Vec<_>>() }),
        ))
    }

    #[tool(
        description = "The race card for one race: name, distance, class, prize and the full field with barriers, jockeys, trainers and weights. Use for 'who is running in race 8 at Caulfield'."
    )]
    async fn get_race_card(
        &self,
        Parameters(args): Parameters<RaceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date)?;
        let Some(card) = self
            .store
            .race_card(date, &args.venue, args.race_number)
            .await
            .map_err(internal)?
        else {
            return Ok(answer(
                format!(
                    "I can't find race {} at {} on {}.",
                    args.race_number, args.venue, date
                ),
                json!({ "found": false }),
            ));
        };
        let runners = card.runners.iter().filter(|r| !r.scratched).count();
        let scratched: Vec<_> = card
            .runners
            .iter()
            .filter(|r| r.scratched)
            .map(|r| r.horse.clone())
            .collect();
        let mut spoken = format!(
            "Race {} at {} is the {}, {}. {} runners, jumping at {}.",
            card.race_number,
            args.venue,
            card.name,
            describe_race(&card),
            runners,
            card.start_local
        );
        if !scratched.is_empty() {
            spoken.push_str(&format!(" Scratched: {}.", scratched.join(", ")));
        }
        let field = card
            .runners
            .iter()
            .filter(|r| !r.scratched)
            .map(|r| {
                format!(
                    "{} {} for {}, ridden by {}, barrier {}",
                    r.number,
                    r.horse,
                    r.trainer,
                    r.jockey,
                    r.barrier
                        .map(|b| b.to_string())
                        .unwrap_or_else(|| "TBA".into())
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        spoken.push_str(&format!(
            " The field: {field}. Source: {SOURCE_RACING_AUSTRALIA}."
        ));
        Ok(answer(
            spoken,
            json!({ "found": true, "date": date, "venue": args.venue, "source": SOURCE_RACING_AUSTRALIA, "card": card }),
        ))
    }

    #[tool(
        description = "A horse's form: career record, first-up and track-condition records, and its recent starts with margins and last-600m times. Use for 'how has Sample Stayer been going' or 'has it won on a soft track'."
    )]
    async fn horse_form(
        &self,
        Parameters(args): Parameters<HorseArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(form) = self.store.horse_form(&args.horse).await.map_err(internal)? else {
            return Ok(answer(
                format!("I don't have form on file for {}.", args.horse),
                json!({ "found": false }),
            ));
        };
        let recent = form
            .starts
            .iter()
            .take(3)
            .map(|s| {
                let place = match s.finish {
                    Some(1) => "won".to_string(),
                    Some(f) => format!(
                        "ran {}{} of {}",
                        f,
                        ordinal(f),
                        s.starters
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "the field".into())
                    ),
                    None => "ran".to_string(),
                };
                let margin = if s.finish == Some(1) {
                    fmt_len(s.margin_lengths).replace("lengths", "lengths clear")
                } else {
                    s.margin_lengths
                        .map(|m| format!("{m:.1} lengths off the winner"))
                        .unwrap_or_default()
                };
                let distance = s
                    .distance_m
                    .map(|d| format!(" over {d} m"))
                    .unwrap_or_default();
                let track = if s.condition.is_empty() {
                    String::new()
                } else {
                    format!(" on a {} track", s.condition)
                };
                format!(
                    "{}{}{distance}{track} it {}{}",
                    s.date.format("%-d %b"),
                    at_venue(&s.venue),
                    place,
                    if margin.is_empty() {
                        String::new()
                    } else {
                        format!(", {margin}")
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(". ");
        let first_up = if form.first_up.starts > 0 {
            format!(
                " First-up {} from {}.",
                form.first_up.wins, form.first_up.starts
            )
        } else {
            String::new()
        };
        let going = going_records(&[
            ("good going", &form.good),
            ("soft", &form.soft),
            ("heavy", &form.heavy),
        ]);
        let recent = if recent.is_empty() {
            String::new()
        } else {
            format!(" Recent starts: {recent}.")
        };
        let spoken = format!(
            "{}, trained by {}. Career {}.{first_up}{going}{recent} Source: {SOURCE_RACING_AUSTRALIA}.",
            form.horse,
            form.trainer,
            form.career.summary()
        );
        Ok(answer(
            spoken,
            json!({ "found": true, "source": SOURCE_RACING_AUSTRALIA, "form": form }),
        ))
    }

    #[tool(
        description = "Explain a race in plain language for a newcomer: what it is, why it matters and which runners bring the strongest form. No betting or prices. Use for 'tell me about the Caulfield Cup' or 'explain race 8'."
    )]
    async fn explain_race(
        &self,
        Parameters(args): Parameters<RaceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date)?;
        let Some(card) = self
            .store
            .race_card(date, &args.venue, args.race_number)
            .await
            .map_err(internal)?
        else {
            return Ok(answer(
                format!(
                    "I can't find race {} at {} on {}.",
                    args.race_number, args.venue, date
                ),
                json!({ "found": false }),
            ));
        };
        let feature = spring_carnival_2026()
            .into_iter()
            .find(|f| f.name.eq_ignore_ascii_case(&card.name) && f.date == date);
        let why = feature.map(|f| f.blurb.to_string()).unwrap_or_else(|| {
            let mut d = describe_race(&card);
            d[..1].make_ascii_uppercase();
            format!("{d}.")
        });
        // Contenders on form: most wins in the last-10 string, ties broken by rating.
        let mut ranked: Vec<_> = card.runners.iter().filter(|r| !r.scratched).collect();
        ranked.sort_by(|a, b| {
            let wa = a.last10.matches('1').count();
            let wb = b.last10.matches('1').count();
            wb.cmp(&wa).then(b.rating.cmp(&a.rating))
        });
        let contenders = ranked
            .iter()
            .take(3)
            .map(|r| {
                format!(
                    "{} ({} wins in its last {} starts)",
                    r.horse,
                    r.last10.matches('1').count(),
                    r.last10.chars().filter(|c| c.is_ascii_digit()).count()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let spoken = format!("{}: {why} Track is {}. On recent form the ones to watch are {contenders}. Source: {SOURCE_RACING_AUSTRALIA}.", card.name, self.track_condition(date, &args.venue).await);
        Ok(answer(
            spoken,
            json!({ "found": true, "race": card.name, "why": why, "contenders": ranked.iter().take(3).map(|r| r.horse.clone()).collect::<Vec<_>>(), "source": SOURCE_RACING_AUSTRALIA }),
        ))
    }

    #[tool(
        description = "The result of a race: placings, margins, winning time and the fastest last 600 metres from sectional timing. Use for 'who won race 7 at Flemington' or 'who ran the fastest last 600'."
    )]
    async fn race_result(
        &self,
        Parameters(args): Parameters<RaceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date)?;
        let Some(result) = self
            .store
            .race_result(date, &args.venue, args.race_number)
            .await
            .map_err(internal)?
        else {
            return Ok(answer(
                format!(
                    "No result yet for race {} at {} on {}.",
                    args.race_number, args.venue, date
                ),
                json!({ "found": false }),
            ));
        };
        // Voice reads the placegetters; the full finishing order stays in structured content.
        let placings = result
            .placings
            .iter()
            .filter(|p| (1..=4).contains(&p.position))
            .map(|p| {
                format!(
                    "{}{} {} ridden by {}{}",
                    p.position,
                    ordinal(p.position),
                    p.horse,
                    p.jockey,
                    p.margin_lengths
                        .map(|m| format!(" by {m:.1} lengths"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sectional = result
            .fastest_last_600
            .as_ref()
            .map(|s| {
                format!(
                    " Fastest last 600 metres: {} in {:.2} seconds, according to {}.",
                    s.horse, s.last_600_s, s.source
                )
            })
            .unwrap_or_default();
        let spoken = format!("Race {} at {} on {}: {placings}.{} Time {}.{sectional} Source: {SOURCE_RACING_AUSTRALIA}.", result.race_number, result.venue, date.format("%-d %B"), result.track_condition.as_ref().map(|t| format!(" Track {t}.")).unwrap_or_default(), result.winning_time.clone().unwrap_or_else(|| "not recorded".into()));
        Ok(answer(
            spoken,
            json!({ "found": true, "source": [SOURCE_RACING_AUSTRALIA, SOURCE_SECTIONALS], "result": result }),
        ))
    }

    #[tool(
        description = "Wins and places for a jockey or trainer over a period, counted from official results. Use for 'how is Jamie Kah going this spring'."
    )]
    async fn jockey_or_trainer_stats(
        &self,
        Parameters(args): Parameters<PersonArgs>,
    ) -> Result<CallToolResult, McpError> {
        let role = args.role.to_lowercase();
        if role != "jockey" && role != "trainer" {
            return Err(McpError::invalid_params(
                "role must be 'jockey' or 'trainer'",
                None,
            ));
        }
        let from = parse_opt_date(&args.from)?;
        let to = parse_opt_date(&args.to)?;
        let Some(stats) = self
            .store
            .person_stats(&args.name, &role, from, to)
            .await
            .map_err(internal)?
        else {
            return Ok(answer(
                format!("I have no results on file for {} {}.", role, args.name),
                json!({ "found": false }),
            ));
        };
        let strike = if stats.record.starts > 0 {
            100.0 * stats.record.wins as f64 / stats.record.starts as f64
        } else {
            0.0
        };
        let spoken = format!(
            "{} {}: {}, a {strike:.0} percent strike rate. Source: {SOURCE_RACING_AUSTRALIA}.",
            capitalise(&role),
            stats.name,
            stats.record.summary()
        );
        Ok(answer(
            spoken,
            json!({ "found": true, "source": SOURCE_RACING_AUSTRALIA, "stats": stats }),
        ))
    }

    #[tool(
        description = "Follow a horse. Trackside will report when it is in a field or has run. Use for 'follow Sample Stayer' or 'add it to my stable'."
    )]
    async fn follow_horse(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<HorseArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(form) = self
            .store
            .horse_form(args.horse.trim())
            .await
            .map_err(internal)?
        else {
            return Ok(answer(
                format!("I can't find a horse called {} in the form guide, so I haven't added it. Check the spelling, or ask me who's running in a race.", args.horse.trim()),
                json!({ "found": false }),
            ));
        };
        let name = form.horse;
        let mut stables = self.stable.write().await;
        let stable = stables.entry(caller_key(&ctx)).or_default();
        if !stable.iter().any(|h| horse_key(h) == horse_key(&name)) {
            stable.push(name.clone());
        }
        Ok(answer(
            format!(
                "Following {name}. You now follow {} horse{}.",
                stable.len(),
                if stable.len() == 1 { "" } else { "s" }
            ),
            json!({ "found": true, "stable": *stable }),
        ))
    }

    #[tool(
        description = "The horses the user follows, and anything new for them: today's engagements and their latest results. Use for 'what's happening with my stable' or 'any of my horses running today'."
    )]
    async fn my_stable(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<DateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date)?;
        let stable = self
            .stable
            .read()
            .await
            .get(&caller_key(&ctx))
            .cloned()
            .unwrap_or_default();
        if stable.is_empty() {
            return Ok(answer(
                "You aren't following any horses yet. Say 'follow' and a horse's name to start."
                    .into(),
                json!({ "stable": [] }),
            ));
        }
        let meetings = self.store.meetings(date).await.map_err(internal)?;
        let mut lines = Vec::new();
        let mut engagements = Vec::new();
        for horse in &stable {
            let mut found = false;
            for m in &meetings {
                for r in &m.races {
                    if let Some(runner) = r
                        .runners
                        .iter()
                        .find(|x| horse_key(&x.horse) == horse_key(horse))
                    {
                        found = true;
                        // A race already run reads as its result, not as an engagement.
                        let result = self
                            .store
                            .race_result(date, &m.venue, r.race_number)
                            .await
                            .map_err(internal)?;
                        let placing = result.as_ref().and_then(|res| {
                            res.placings
                                .iter()
                                .find(|p| horse_key(&p.horse) == horse_key(horse))
                        });
                        if let Some(res) = &result {
                            let outcome = match placing {
                                Some(p) if p.position == 1 => "won".to_string(),
                                Some(p) if p.position > 1 => {
                                    format!("ran {}{}", p.position, ordinal(p.position))
                                }
                                _ => "ran unplaced".to_string(),
                            };
                            lines.push(format!(
                                "{horse} {outcome} in race {} at {} ({}) on {}",
                                r.race_number,
                                m.venue,
                                r.name,
                                res.date.format("%-d %b")
                            ));
                            engagements.push(json!({ "horse": horse, "venue": m.venue, "race_number": r.race_number, "start_local": r.start_local, "finished": placing.map(|p| p.position) }));
                            continue;
                        }
                        lines.push(format!(
                            "{} runs in race {} at {} ({}) at {}, barrier {}, {} up",
                            horse,
                            r.race_number,
                            m.venue,
                            r.name,
                            r.start_local,
                            runner
                                .barrier
                                .map(|b| b.to_string())
                                .unwrap_or_else(|| "TBA".into()),
                            runner.jockey
                        ));
                        engagements.push(json!({ "horse": horse, "venue": m.venue, "race_number": r.race_number, "start_local": r.start_local }));
                    }
                }
            }
            if !found {
                let last = self
                    .store
                    .horse_form(horse)
                    .await
                    .map_err(internal)?
                    .and_then(|f| f.starts.first().cloned());
                match last {
                    Some(s) => lines.push(format!(
                        "{} isn't engaged on {}; last start it {}{} on {}",
                        horse,
                        date.format("%-d %b"),
                        s.finish
                            .map(|f| if f == 1 {
                                "won".to_string()
                            } else {
                                format!("ran {}{}", f, ordinal(f))
                            })
                            .unwrap_or_else(|| "ran".into()),
                        at_venue(&s.venue),
                        s.date.format("%-d %b")
                    )),
                    None => lines.push(format!(
                        "{horse} isn't engaged on {}",
                        date.format("%-d %b")
                    )),
                }
            }
        }
        Ok(answer(
            format!("{}. Source: {SOURCE_RACING_AUSTRALIA}.", lines.join(". ")),
            json!({ "date": date, "stable": stable, "engagements": engagements }),
        ))
    }

    #[tool(
        description = "The Spring Racing Carnival guide: the feature races, their dates, venues and what makes each one matter. Use for 'when is the Melbourne Cup' or 'what's on this carnival'."
    )]
    async fn carnival_guide(&self) -> Result<CallToolResult, McpError> {
        let races = spring_carnival_2026();
        let spoken = races
            .iter()
            .map(|f| {
                format!(
                    "{} on {} at {}, {} over {} metres: {}",
                    f.name,
                    f.date.format("%A %-d %B"),
                    f.venue,
                    f.grade,
                    f.distance_m,
                    f.blurb
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        Ok(answer(
            format!("The 2026 Melbourne Spring Racing Carnival. {spoken}"),
            json!({ "races": races }),
        ))
    }
}

impl Trackside {
    async fn track_condition(&self, date: NaiveDate, venue: &str) -> String {
        self.store
            .meetings(date)
            .await
            .ok()
            .and_then(|ms| ms.into_iter().find(|m| venue_matches(&m.venue, venue)))
            .and_then(|m| m.track_condition)
            .unwrap_or_else(|| "not yet rated".into())
    }
}

fn ordinal(n: u32) -> &'static str {
    match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[tool_handler]
impl ServerHandler for Trackside {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info({
                let mut info = Implementation::from_build_env();
                info.name = "trackside".into();
                info.title = Some("Trackside".into());
                info
            })
            .with_instructions(
                "Trackside is a form guide for Australian thoroughbred racing: meetings, race cards, horse form, results, sectional timing, jockey and trainer records and a Spring Carnival guide. It is a fan companion with no betting or prices; never ask it for odds or tips. Attribute facts to the source each answer names."
                    .to_string(),
            )
    }
}
