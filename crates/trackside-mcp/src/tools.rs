//! The Trackside tool surface. Every tool answers in two layers: a short spoken-style text
//! block for voice, and `structured_content` for screens and agents. No prices, ever.

use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use chrono_tz::Australia::Melbourne;
use rmcp::{
    handler::server::wrapper::Parameters, model::*, schemars, service::RequestContext, tool,
    tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;

use trackside_core::{
    horse_key, looks_like_track_code, prize_total, spoken_money, spring_carnival_2026,
    venue_matches, Meeting, RaceCard, Record, Store, SOURCE_RACING_AUSTRALIA, SOURCE_SECTIONALS,
};

use crate::auth::Caller;
use crate::memory::{Memory, Profile};
use crate::summary::Summariser;

#[derive(Clone)]
pub struct Trackside {
    store: Arc<dyn Store>,
    /// What each listener asked Trackside to remember, keyed by the signed-in account (see
    /// `caller_key`): followed horses, home state, when they last heard their stable report.
    memory: Arc<dyn Memory>,
    /// Rewords `explain_race` for the ear when Bedrock is configured (see `summary.rs`).
    summariser: Option<Arc<Summariser>>,
}

/// Whose profile a request reads: the OAuth subject when the server checks tokens, or one
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
pub struct StateArgs {
    /// The listener's home state: VIC, NSW, QLD, SA, WA, TAS, NT or ACT.
    pub state: String,
}

const STATES: &[&str] = &["VIC", "NSW", "QLD", "SA", "WA", "TAS", "NT", "ACT"];

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

/// Trackside's MCP App (the MCP Apps extension): one view that draws race cards, results,
/// form, explanations and a stable from a tool's structured content, and calls the tools back
/// through the host when a listener taps a horse.
pub const APP_URI: &str = "ui://trackside/race-card.html";
pub const APP_MIME: &str = "text/html;profile=mcp-app";
const APP_HTML: &str = include_str!("../static/race-card.html");

/// Points a tool at the MCP App. `ui/resourceUri` is the key earlier hosts read.
fn app_meta() -> MetaObject {
    MetaObject(
        json!({ "ui": { "resourceUri": APP_URI }, "ui/resourceUri": APP_URI })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    )
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

/// "Flemington in VIC: 10 races, track Good 4, first race 12:35."
fn meeting_line(m: &Meeting) -> String {
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
        m.track_condition.as_deref().unwrap_or("not yet rated"),
        first
    )
}

fn fmt_len(m: Option<f64>) -> String {
    m.map(|v| format!("{v:.1} lengths")).unwrap_or_default()
}

#[tool_router]
impl Trackside {
    pub fn new(
        store: Arc<dyn Store>,
        memory: Arc<dyn Memory>,
        summariser: Option<Arc<Summariser>>,
    ) -> Self {
        Self {
            store,
            memory,
            summariser,
        }
    }

    #[tool(
        description = "List the Australian thoroughbred race meetings on a date, with track condition and the first race time. Use for questions like 'what racing is on today' or 'is there racing at Flemington on Saturday'."
    )]
    async fn list_meetings(
        &self,
        ctx: RequestContext<RoleServer>,
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
        // A listener with a home state hears its meetings in full and the rest by name.
        let home = match args.state {
            Some(_) => None,
            None => self.profile(&ctx).await.home_state,
        };
        let is_home = |m: &&Meeting| {
            home.as_deref()
                .is_some_and(|h| m.state.eq_ignore_ascii_case(h))
        };
        meetings.sort_by_key(|m| !is_home(&m));
        let home_count = meetings.iter().filter(is_home).count();
        let (full, others) = match home_count {
            0 => (&meetings[..], &meetings[..0]),
            n => meetings.split_at(n),
        };
        let mut spoken = full.iter().map(meeting_line).collect::<Vec<_>>().join(" ");
        if let (Some(h), 0) = (&home, home_count) {
            spoken = format!("There's no racing in {h}. {spoken}");
        }
        if !others.is_empty() {
            let named = others
                .iter()
                .take(6)
                .map(|m| m.venue.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let more = match others.len().saturating_sub(6) {
                0 => String::new(),
                n => format!(" and {n} more"),
            };
            spoken.push_str(&format!(" Elsewhere: {named}{more}."));
        }
        Ok(answer(
            format!(
                "According to {SOURCE_RACING_AUSTRALIA}, on {}: {spoken}",
                date.format("%A %-d %B")
            ),
            json!({ "date": date, "source": SOURCE_RACING_AUSTRALIA, "home_state": home, "meetings": meetings.iter().map(|m| json!({
                "venue": m.venue, "state": m.state, "track_condition": m.track_condition, "rail": m.rail,
                "races": m.races.iter().map(|r| json!({"race_number": r.race_number, "name": r.name, "start_local": r.start_local, "distance_m": r.distance_m, "grade": r.grade})).collect::<Vec<_>>()
            })).collect::<Vec<_>>() }),
        ))
    }

    #[tool(
        meta = app_meta(),
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
            return Ok(self
                .missing_race(date, &args.venue, args.race_number, false)
                .await);
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
        meta = app_meta(),
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
        meta = app_meta(),
        description = "Explain a race in plain language for a newcomer: what it is, why it matters and which runners bring the strongest form. No betting or prices. Use for 'tell me about the Caulfield Cup' or 'explain race 8'. Given only a race's name, find its venue, date and race number with list_meetings first."
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
            return Ok(self
                .missing_race(date, &args.venue, args.race_number, false)
                .await);
        };
        let feature = spring_carnival_2026()
            .into_iter()
            .find(|f| f.name.eq_ignore_ascii_case(&card.name) && f.date == date);
        let why = feature
            .as_ref()
            .map(|f| f.blurb.to_string())
            .unwrap_or_else(|| {
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
        let track = self.track_condition(date, &args.venue).await;
        let template = format!(
            "{}: {why} Track is {track}. On recent form the ones to watch are {contenders}.",
            card.name
        );
        // Bedrock gets the same facts the template uses, plus the field, and nothing else.
        let facts = json!({
            "race": card.name, "venue": args.venue, "date": date.format("%A %-d %B").to_string(),
            "race_number": card.race_number, "distance_m": card.distance_m, "grade": card.grade,
            "class": card.class, "prize_total": prize_total(&card.prize).map(spoken_money),
            "why_it_matters": why, "track_condition": track,
            "feature_race": feature.is_some(),
            "strongest_recent_form": ranked.iter().take(3).map(|r| json!({
                "horse": r.horse, "wins_in_recent_starts": r.last10.matches('1').count(),
                "recent_starts": r.last10.chars().filter(|c| c.is_ascii_digit()).count(),
                "jockey": r.jockey, "trainer": r.trainer, "barrier": r.barrier,
            })).collect::<Vec<_>>(),
            "field_size": ranked.len(),
        });
        let (explanation, written_by) = match &self.summariser {
            None => (template, "template".to_string()),
            Some(s) => match s.explain(&facts).await {
                Ok(text) => (text, format!("bedrock:{}", s.model())),
                Err(err) => {
                    tracing::warn!(error = ?err, "Bedrock explanation failed; using the template");
                    (template, "template".to_string())
                }
            },
        };
        let spoken = format!("{explanation} Source: {SOURCE_RACING_AUSTRALIA}.");
        Ok(answer(
            spoken,
            json!({ "found": true, "race": card.name, "why": why, "contenders": ranked.iter().take(3).map(|r| r.horse.clone()).collect::<Vec<_>>(), "explanation": explanation, "written_by": written_by, "facts": facts, "source": SOURCE_RACING_AUSTRALIA }),
        ))
    }

    #[tool(
        meta = app_meta(),
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
            return Ok(self
                .missing_race(date, &args.venue, args.race_number, true)
                .await);
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
        description = "Follow a horse. Trackside remembers the horses each listener follows across sessions and reports when they are in a field or have run. Use for 'follow Sample Stayer' or 'add it to my stable'."
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
        let user = caller_key(&ctx);
        let mut profile = self.memory.load(&user).await.map_err(internal)?;
        let first = profile.horses.is_empty();
        if !profile
            .horses
            .iter()
            .any(|h| horse_key(h) == horse_key(&name))
        {
            profile.horses.push(name.clone());
        }
        // The first follow starts the clock for "since you last checked".
        profile.last_checked.get_or_insert_with(today);
        self.memory.save(&user, &profile).await.map_err(internal)?;
        let n = profile.horses.len();
        let remember = if first && self.memory.durable() {
            " I'll remember your stable next time, and tell you how they've gone since you last asked."
        } else {
            ""
        };
        Ok(answer(
            format!(
                "Following {name}. You now follow {n} horse{}.{remember}",
                if n == 1 { "" } else { "s" }
            ),
            json!({ "found": true, "stable": profile.horses, "remembered": self.memory.durable() }),
        ))
    }

    #[tool(
        description = "Stop following a horse. Use for 'unfollow Sample Stayer' or 'take it out of my stable'."
    )]
    async fn unfollow_horse(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<HorseArgs>,
    ) -> Result<CallToolResult, McpError> {
        let user = caller_key(&ctx);
        let mut profile = self.memory.load(&user).await.map_err(internal)?;
        let key = horse_key(&args.horse);
        let Some(i) = profile.horses.iter().position(|h| horse_key(h) == key) else {
            let following = match profile.horses.as_slice() {
                [] => "You aren't following any horses.".to_string(),
                hs => format!("You follow {}.", spoken_list(hs)),
            };
            return Ok(answer(
                format!("You weren't following {}. {following}", args.horse.trim()),
                json!({ "found": false, "stable": profile.horses }),
            ));
        };
        let name = profile.horses.remove(i);
        self.memory.save(&user, &profile).await.map_err(internal)?;
        let left = match profile.horses.len() {
            0 => "Your stable is empty now.".to_string(),
            n => format!("You follow {n} horse{}.", if n == 1 { "" } else { "s" }),
        };
        Ok(answer(
            format!("Stopped following {name}. {left}"),
            json!({ "found": true, "removed": name, "stable": profile.horses }),
        ))
    }

    #[tool(
        meta = app_meta(),
        description = "The horses the user follows and what's new for them: how they have run since the user last asked (remembered across sessions), today's engagements and results. Use for 'what's happening with my stable', 'any of my horses running today' or 'how did my horses go'."
    )]
    async fn my_stable(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<DateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date)?;
        let user = caller_key(&ctx);
        let mut profile = self.memory.load(&user).await.map_err(internal)?;
        let stable = profile.horses.clone();
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
        // What happened between the last report and this day, from each horse's form. Nothing
        // to catch up on until a day has passed since the last report.
        let heard_up_to = date.min(today());
        let since = profile.last_checked.filter(|d| *d < heard_up_to);
        let mut catch_up = Vec::new();
        if let Some(since) = since {
            let mut heard = Vec::new();
            for horse in &stable {
                let Some(form) = self.store.horse_form(horse).await.map_err(internal)? else {
                    continue;
                };
                let mut runs: Vec<_> = form
                    .starts
                    .iter()
                    .filter(|s| s.date > since && s.date < date)
                    .collect();
                runs.sort_by_key(|s| s.date);
                for s in runs {
                    heard.push(format!(
                        "{horse} {}{} on {}",
                        finish_words(s.finish, s.starters),
                        at_venue(&s.venue),
                        s.date.format("%A %-d %B")
                    ));
                    catch_up.push(json!({ "horse": horse, "date": s.date, "venue": s.venue, "finish": s.finish, "starters": s.starters, "distance_m": s.distance_m, "condition": s.condition }));
                }
            }
            let when = since.format("%A %-d %B");
            lines.push(if heard.is_empty() {
                format!("None of your horses have raced since you last checked on {when}")
            } else {
                format!("Since you last checked on {when}: {}", heard.join("; "))
            });
        }
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
            // Horses already covered by the catch-up don't need their last start again.
            if !found && !catch_up.iter().any(|c| c["horse"] == horse.as_str()) {
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
                        finish_words(s.finish, None),
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
        // The next report starts from here; asking about a future card doesn't move it.
        if profile.last_checked.is_none_or(|d| d < heard_up_to) {
            profile.last_checked = Some(heard_up_to);
            self.memory.save(&user, &profile).await.map_err(internal)?;
        }
        Ok(answer(
            format!("{}. Source: {SOURCE_RACING_AUSTRALIA}.", lines.join(". ")),
            json!({ "date": date, "stable": stable, "since": since, "catch_up": catch_up, "engagements": engagements, "remembered": self.memory.durable() }),
        ))
    }

    #[tool(
        description = "Remember the listener's home state, so meetings there are read out first and in full. Use for 'I'm in Sydney', 'I follow Queensland racing' or 'set my state to VIC'."
    )]
    async fn set_home_state(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<StateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let state = args.state.trim().to_ascii_uppercase();
        if !STATES.contains(&state.as_str()) {
            return Err(McpError::invalid_params(
                format!("state must be one of {}", STATES.join(", ")),
                None,
            ));
        }
        let user = caller_key(&ctx);
        let mut profile = self.memory.load(&user).await.map_err(internal)?;
        profile.home_state = Some(state.clone());
        self.memory.save(&user, &profile).await.map_err(internal)?;
        Ok(answer(
            format!("Got it. I'll read {state} meetings first from now on."),
            json!({ "home_state": state, "remembered": self.memory.durable() }),
        ))
    }

    #[tool(
        description = "Forget everything Trackside remembers about the listener: followed horses, home state and when they last checked. Use for 'forget me' or 'delete my data'."
    )]
    async fn forget_me(&self, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        self.memory
            .forget(&caller_key(&ctx))
            .await
            .map_err(internal)?;
        Ok(answer(
            "Done. I've forgotten your followed horses and your home state.".into(),
            json!({ "forgotten": true }),
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
    /// The caller's remembered profile; an empty one when memory can't be read, so a storage
    /// fault never stops a racing answer.
    async fn profile(&self, ctx: &RequestContext<RoleServer>) -> Profile {
        self.memory
            .load(&caller_key(ctx))
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(error = ?err, "reading the listener's profile failed");
                Profile::default()
            })
    }

    /// Why a race can't be found, in words that help the next question: no racing at that
    /// venue that day (and where there was), no such race on the card, or not run yet.
    async fn missing_race(
        &self,
        date: NaiveDate,
        venue: &str,
        race_number: u32,
        want_result: bool,
    ) -> CallToolResult {
        let day = date.format("%A %-d %B").to_string();
        let meetings = self.store.meetings(date).await.unwrap_or_default();
        let venues: Vec<String> = meetings.iter().map(|m| m.venue.clone()).collect();
        let structured = json!({ "found": false, "date": date, "venue": venue, "race_number": race_number, "meetings_that_day": venues });
        let Some(meeting) = meetings.iter().find(|m| venue_matches(&m.venue, venue)) else {
            let spoken = if venues.is_empty() {
                format!("I don't have any meetings on {day}. Fields are published two to three days ahead, and I only hold recent racing.")
            } else {
                // Voice gets a handful; the full list is in the structured content.
                let named = venues
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = match venues.len().saturating_sub(5) {
                    0 => String::new(),
                    n => format!(" and {n} more"),
                };
                format!("There was no racing at {venue} on {day}. Meetings that day include {named}{more}.")
            };
            return answer(spoken, structured);
        };
        if !meeting.races.iter().any(|r| r.race_number == race_number) {
            return answer(
                format!(
                    "{} on {day} has {} races, so there's no race {race_number}.",
                    meeting.venue,
                    meeting.races.len()
                ),
                structured,
            );
        }
        let spoken = match (want_result, date >= today()) {
            (true, true) => format!(
                "Race {race_number} at {} on {day} hasn't been run yet, or its result isn't in.",
                meeting.venue
            ),
            (true, false) => format!(
                "I don't have the result of race {race_number} at {} on {day}.",
                meeting.venue
            ),
            (false, _) => format!(
                "I can't find race {race_number} at {} on {day}.",
                meeting.venue
            ),
        };
        answer(spoken, structured)
    }

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

fn today() -> NaiveDate {
    Utc::now().with_timezone(&Melbourne).date_naive()
}

/// "won", "ran 3rd of 12", "ran 5th", or "ran" when the finish isn't known.
fn finish_words(finish: Option<u32>, starters: Option<u32>) -> String {
    match (finish, starters) {
        (Some(1), _) => "won".into(),
        (Some(f), Some(n)) => format!("ran {f}{} of {n}", ordinal(f)),
        (Some(f), None) => format!("ran {f}{}", ordinal(f)),
        (None, _) => "ran".into(),
    }
}

/// "A", "A and B", "A, B and C".
fn spoken_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
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
        let mut extensions = ExtensionCapabilities::new();
        extensions.insert(
            "io.modelcontextprotocol/ui".into(),
            json!({ "mimeTypes": [APP_MIME] })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_extensions_with(extensions)
                .enable_resources()
                .enable_tools()
                .build(),
        )
            .with_server_info({
                let mut info = Implementation::from_build_env();
                info.name = "trackside".into();
                info.title = Some("Trackside".into());
                info
            })
            .with_instructions(
                "Trackside is a form guide for Australian thoroughbred racing: meetings, race cards, horse form, results, sectional timing, jockey and trainer records and a Spring Carnival guide. It remembers each signed-in listener across sessions: the horses they follow, their home state, and what has happened to their horses since they last asked. It is a fan companion with no betting or prices; never ask it for odds or tips. Attribute facts to the source each answer names."
                    .to_string(),
            )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![Resource::new(
            APP_URI,
            "race-card",
        )
        .with_title("Trackside race card")
        .with_description(
            "Interactive race card, result, form and stable view for screens (MCP App)",
        )
        .with_mime_type(APP_MIME)]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != APP_URI {
            return Err(McpError::resource_not_found(
                format!("no resource {}", request.uri),
                None,
            ));
        }
        // Self-contained: no network access, so no CSP domains to declare.
        let meta = MetaObject(
            json!({ "ui": { "prefersBorder": false } })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        Ok(
            ReadResourceResult::new(vec![ResourceContents::text(APP_HTML, APP_URI)
                .with_mime_type(APP_MIME)
                .with_meta(meta)])
            .into(),
        )
    }
}
