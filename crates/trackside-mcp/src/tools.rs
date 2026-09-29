//! The Trackside tool surface. Every tool answers in two layers: a short spoken-style text
//! block for voice, and `structured_content` for screens and agents. No prices, ever.

use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use chrono_tz::Australia::Melbourne;
use rmcp::{
    handler::server::wrapper::Parameters, model::*, schemars, tool, tool_handler, tool_router,
    ErrorData as McpError, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use trackside_core::{
    norm, spring_carnival_2026, venue_matches, Store, SOURCE_RACING_AUSTRALIA, SOURCE_SECTIONALS,
};

#[derive(Clone)]
pub struct Trackside {
    store: Arc<dyn Store>,
    /// Horses the user follows. Shared by every session in this process for now; keyed by
    /// account (and persisted) once OAuth lands.
    stable: Stable,
}

pub type Stable = Arc<RwLock<Vec<String>>>;

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
            "Race {} at {} is the {}, {} over {} metres, {} runners, jumping at {}.",
            card.race_number,
            args.venue,
            card.name,
            card.class,
            card.distance_m
                .map(|d| d.to_string())
                .unwrap_or_else(|| "an unlisted distance".into()),
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
                format!(
                    "{} at {} over {} m on a {} track it {}{}",
                    s.date.format("%-d %b"),
                    s.venue,
                    s.distance_m.unwrap_or(0),
                    s.condition,
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
        let spoken = format!(
            "{}, trained by {}. Career {}. First-up {} from {}; on good going {} from {}, soft {} from {}, heavy {} from {}. Recent starts: {}. Source: {SOURCE_RACING_AUSTRALIA}.",
            form.horse, form.trainer, form.career.summary(), form.first_up.wins, form.first_up.starts, form.good.wins, form.good.starts, form.soft.wins, form.soft.starts, form.heavy.wins, form.heavy.starts, recent
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
            format!(
                "A {} over {} metres.",
                card.class,
                card.distance_m.unwrap_or(0)
            )
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
        Parameters(args): Parameters<HorseArgs>,
    ) -> Result<CallToolResult, McpError> {
        let name = args.horse.trim().to_string();
        let mut stable = self.stable.write().await;
        if !stable.iter().any(|h| h.eq_ignore_ascii_case(&name)) {
            stable.push(name.clone());
        }
        Ok(answer(
            format!(
                "Following {name}. You now follow {} horse{}.",
                stable.len(),
                if stable.len() == 1 { "" } else { "s" }
            ),
            json!({ "stable": *stable }),
        ))
    }

    #[tool(
        description = "The horses the user follows, and anything new for them: today's engagements and their latest results. Use for 'what's happening with my stable' or 'any of my horses running today'."
    )]
    async fn my_stable(
        &self,
        Parameters(args): Parameters<DateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date)?;
        let stable = self.stable.read().await.clone();
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
                    if let Some(runner) = r.runners.iter().find(|x| norm(&x.horse) == norm(horse)) {
                        found = true;
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
                        "{} isn't engaged on {}; last start it {} at {} on {}",
                        horse,
                        date.format("%-d %b"),
                        s.finish
                            .map(|f| if f == 1 {
                                "won".to_string()
                            } else {
                                format!("ran {}{}", f, ordinal(f))
                            })
                            .unwrap_or_else(|| "ran".into()),
                        s.venue,
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
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Trackside is a form guide for Australian thoroughbred racing: meetings, race cards, horse form, results, sectional timing, jockey and trainer records and a Spring Carnival guide. It is a fan companion with no betting or prices; never ask it for odds or tips. Attribute facts to the source each answer names."
                    .to_string(),
            )
    }
}
