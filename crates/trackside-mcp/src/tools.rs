//! The Trackside tool surface. Every tool answers in two layers: a short spoken-style text
//! block for voice, and `structured_content` for screens and agents. No prices, ever.

use std::sync::Arc;

use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use rmcp::{
    handler::server::wrapper::Parameters, model::*, schemars, service::RequestContext, tool,
    tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;

use trackside_core::{
    engagements_in, horse_key, looks_like_track_code, norm, prize_total, run_style, spoken_money,
    spring_carnival_2026, venue_matches, Engagement, FeatureRace, Meeting, NameMatch, RaceCard,
    RaceRef, Record, RunStyle, SectionalHighlight, Store, SOURCE_RACING_AUSTRALIA,
    SOURCE_SECTIONALS,
};

use trackside_core::{names, HorseForm};

use crate::auth::Caller;
use crate::clock::Clock;
use crate::memory::{Memory, Profile};
use crate::summary::Summariser;
use crate::telemetry::{self, Sink};

/// How many days past the asked date a stable report looks for each horse's next run.
/// Racing Australia publishes fields two to three days ahead, so four days covers every
/// field that exists.
const LOOK_AHEAD_DAYS: u64 = 4;

#[derive(Clone)]
pub struct Trackside {
    store: Arc<dyn Store>,
    /// What each listener asked Trackside to remember, keyed by the signed-in account (see
    /// `caller_key`): followed horses, home state, when they last heard their stable report.
    memory: Arc<dyn Memory>,
    /// Rewords `explain_race` for the ear when Bedrock is configured (see `summary.rs`).
    summariser: Option<Arc<Summariser>>,
    /// Where "today" comes from (see `clock.rs`).
    clock: Arc<dyn Clock>,
    /// Where each call's metric line goes, when metrics are on (see `telemetry.rs`).
    telemetry: Option<Arc<dyn Sink>>,
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
    /// The race's name as the listener said it, e.g. "Caulfield Cup" or "the Epsom". Give
    /// this, or a venue and race number.
    pub race: Option<String>,
    /// Venue name, e.g. "Caulfield" or "Flemington". With race_number, names the race;
    /// with race, narrows a name to that venue.
    pub venue: Option<String>,
    /// Race number on the card, e.g. 8. Needed with venue when no race name is given.
    pub race_number: Option<u32>,
    /// Date as YYYY-MM-DD. Defaults to today in Australia/Melbourne; with a race name, the
    /// days around today unless a date is given.
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
/// A tool's input schema with only what every host's model reads: no `$schema` line, no
/// nullable type arrays (an optional field is simply not required) and no integer formats or
/// bounds. MCP Inspector flags each of those as not portable across model providers.
fn portable<T: schemars::JsonSchema + std::any::Any>() -> Arc<rmcp::model::JsonObject> {
    let mut schema = rmcp::handler::server::common::schema_for_input::<T>()
        .unwrap_or_else(|e| panic!("input schema for {}: {e}", std::any::type_name::<T>()))
        .as_ref()
        .clone();
    schema.remove("$schema");
    if let Some(fields) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        for field in fields
            .values_mut()
            .filter_map(serde_json::Value::as_object_mut)
        {
            if let Some(serde_json::Value::Array(types)) = field.get("type") {
                let kept: Vec<_> = types.iter().filter(|t| *t != "null").cloned().collect();
                if let [one] = kept.as_slice() {
                    field.insert("type".into(), one.clone());
                }
            }
            field.remove("format");
            field.remove("minimum");
        }
    }
    Arc::new(schema)
}

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

/// The date a listener asked about, or `today` when they named none.
fn parse_date(s: &Option<String>, today: NaiveDate) -> Result<NaiveDate, McpError> {
    match s.as_deref() {
        None => Ok(today),
        Some(text) => parse_ymd(text),
    }
}

fn parse_opt_date(s: &Option<String>) -> Result<Option<NaiveDate>, McpError> {
    s.as_deref().map(parse_ymd).transpose()
}

fn parse_ymd(text: &str) -> Result<NaiveDate, McpError> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| McpError::invalid_params(format!("date must be YYYY-MM-DD, got {text}"), None))
}

/// "today", "tomorrow", or the weekday ("on Saturday") for a day within the week after `from`.
fn relative_day(date: NaiveDate, from: NaiveDate) -> String {
    match (date - from).num_days() {
        0 => "today".into(),
        1 => "tomorrow".into(),
        2..=6 => format!("on {}", date.format("%A")),
        _ => format!("on {}", date.format("%A %-d %B")),
    }
}

/// One horse's next run as a stable report says it, dated relative to `from`, with the
/// start already in the listener's words (`at`, none when there is no time yet):
/// "runs on Saturday in the Caulfield Cup, race 8 at Caulfield at 5 pm, barrier 4, with
/// J. Example up", or that it has been scratched.
fn next_run_words(e: &Engagement, from: NaiveDate, at: Option<String>) -> String {
    let when = relative_day(e.date, from);
    let race = if e.race_name.is_empty() {
        format!("race {} at {}", e.race_number, e.venue)
    } else {
        format!(
            "{}, race {} at {}",
            with_article(&e.race_name),
            e.race_number,
            e.venue
        )
    };
    if e.scratched {
        return format!("has been scratched from {race} {when}");
    }
    let at = at.map(|at| format!(" at {at}")).unwrap_or_default();
    let barrier = e
        .barrier
        .map(|b| format!(", barrier {b}"))
        .unwrap_or_default();
    let rider = if e.jockey.trim().is_empty() {
        String::new()
    } else {
        format!(", with {} up", e.jockey)
    };
    format!("runs {when} in {race}{at}{barrier}{rider}")
}

/// What a listener hears when the data behind an answer can't be read. The detail (an S3
/// key, a DynamoDB error) goes to the log with the call's request id, never to the host.
pub const INTERNAL_SPOKEN: &str =
    "Trackside couldn't read its data just now; try again in a moment.";

fn internal(err: anyhow::Error) -> McpError {
    tracing::error!(error = ?err, "tool call failed reading its data");
    McpError::internal_error(INTERNAL_SPOKEN, None)
}

fn answer(spoken: String, structured: serde_json::Value) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(spoken)]);
    result.structured_content = Some(structured);
    result
}

/// "the Manikato Stakes, a Group 1 race over 1200 metres", or just "a race over 1200 metres"
/// when the race has no name once its wagering sponsor is removed.
pub(crate) fn named_race(card: &RaceCard) -> String {
    if card.name.is_empty() {
        describe_race(card)
    } else {
        format!("{}, {}", with_article(&card.name), describe_race(card))
    }
}

/// "the Manikato Stakes", but "The Galaxy" as it is: a name that already starts with an
/// article doesn't get a second one.
fn with_article(name: &str) -> String {
    let starts_with_the = name
        .get(..4)
        .is_some_and(|lead| lead.eq_ignore_ascii_case("the "));
    if starts_with_the {
        name.to_string()
    } else {
        format!("the {name}")
    }
}

/// The race's name, or "Race 2 at Flemington" when it has none.
fn race_title(card: &RaceCard, venue: &str) -> String {
    if card.name.is_empty() {
        format!("Race {} at {venue}", card.race_number)
    } else {
        card.name.clone()
    }
}

/// " (Turnbull Stakes)", or nothing for a race with no name.
fn in_brackets(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else {
        format!(" ({name})")
    }
}

/// "a Group 1 race over 1200 metres": grade and class only when known, so an empty field
/// never leaves a gap in the sentence.
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

/// A published start time said aloud: Racing Australia writes "4:25PM" and "5:00PM", the
/// fixture "15:40"; a listener hears "4:25 pm", "5 pm" and "3:40 pm". Anything else is read
/// as written.
pub(crate) fn spoken_time(t: &str) -> String {
    let t = t.trim();
    let upper = t.to_ascii_uppercase();
    let (clock, half) = if let Some(c) = upper.strip_suffix("PM") {
        (c.trim(), Some("pm"))
    } else if let Some(c) = upper.strip_suffix("AM") {
        (c.trim(), Some("am"))
    } else {
        (upper.as_str(), None)
    };
    let Some((h, m)) = clock.split_once(':') else {
        return t.to_string();
    };
    let (Ok(h), Ok(m)) = (h.parse::<u32>(), m.parse::<u32>()) else {
        return t.to_string();
    };
    if h > 23 || m > 59 {
        return t.to_string();
    }
    let (hour, half) = match half {
        Some(half) => (h, half),
        None if h == 0 => (12, "am"),
        None if h < 12 => (h, "am"),
        None if h == 12 => (12, "pm"),
        None => (h - 12, "pm"),
    };
    if m == 0 {
        format!("{hour} {half}")
    } else {
        format!("{hour}:{m:02} {half}")
    }
}

/// When a race starts, for one listener: the venue's clock, the listener's own clock when
/// their home state keeps a different time that day, and how far off the jump is when the
/// race is today. Built by `Trackside::start`.
pub(crate) struct Start {
    /// "5 pm"; `None` when the published start isn't a time yet ("TBA", empty).
    venue: Option<String>,
    /// ("4 pm", "Queensland time") when the listener's clock differs from the venue's.
    home: Option<(String, &'static str)>,
    /// Minutes from now to the jump, only for a race on the listener's current day.
    minutes_until: Option<i64>,
    venue_hhmm: Option<String>,
    home_hhmm: Option<String>,
    venue_state: String,
    home_state: Option<String>,
}

impl Start {
    /// "4 pm Queensland time, 5 pm at the track", or just "5 pm"; `None` with no time yet.
    fn long(&self) -> Option<String> {
        let venue = self.venue.as_ref()?;
        Some(match &self.home {
            Some((home, label)) => format!("{home} {label}, {venue} at the track"),
            None => venue.clone(),
        })
    }

    /// "4 pm Queensland time", or just "5 pm": for lines that are already long.
    fn short(&self) -> Option<String> {
        let venue = self.venue.as_ref()?;
        Some(match &self.home {
            Some((home, label)) => format!("{home} {label}"),
            None => venue.clone(),
        })
    }

    /// ", due to jump in about 25 minutes", ", which jumped about an hour ago; ask me for
    /// the result", or nothing when the race isn't within three hours of now.
    fn relative(&self) -> String {
        match self.minutes_until {
            Some(m) if m > 2 && m <= 180 => format!(", due to jump in about {}", about(m)),
            Some(m) if (-2..=2).contains(&m) => ", jumping about now".to_string(),
            Some(m) if (-180..-2).contains(&m) => {
                format!(
                    ", which jumped about {} ago; ask me for the result",
                    about(m)
                )
            }
            _ => String::new(),
        }
    }

    /// The listener's clock time as "HH:MM", for screens, when it differs.
    fn start_home(&self) -> Option<String> {
        self.home_hhmm.clone()
    }

    fn json(&self) -> serde_json::Value {
        json!({
            "venue_time": self.venue_hhmm,
            "venue_state": self.venue_state,
            "home_state": self.home_state,
            "home_time": self.home_hhmm,
            "label": self.home.as_ref().map(|(_, l)| *l),
            "minutes_until": self.minutes_until,
        })
    }
}

/// A span of minutes the way a person says it: "a few minutes", "25 minutes", "an hour",
/// "an hour and a half", "2 hours".
fn about(minutes: i64) -> String {
    match minutes.abs() {
        m if m < 3 => "a few minutes".into(),
        m if m < 58 => format!("{} minutes", ((m + 2) / 5 * 5).max(5)),
        m if m < 75 => "an hour".into(),
        m if m < 105 => "an hour and a half".into(),
        m if m < 135 => "2 hours".into(),
        m if m < 165 => "2 and a half hours".into(),
        _ => "3 hours".into(),
    }
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
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            memory,
            summariser,
            clock,
            telemetry: None,
        }
    }

    /// Sends each tool call's metric line to `sink`.
    pub fn with_telemetry(mut self, sink: Option<Arc<dyn Sink>>) -> Self {
        self.telemetry = sink;
        self
    }

    /// Today's date in Melbourne, the racing calendar's day.
    fn today(&self) -> NaiveDate {
        self.clock.today()
    }

    #[tool(
        title = "Race meetings",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<DateArgs>(),
        description = "List the Australian thoroughbred race meetings on a date, with track condition and the first race time. Use for questions like 'what racing is on today' or 'is there racing at Flemington on Saturday'."
    )]
    async fn list_meetings(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<DateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date, self.today())?;
        let mut meetings = self.store.meetings(date).await.map_err(internal)?;
        if let Some(state) = args.state.as_deref() {
            meetings.retain(|m| m.state.eq_ignore_ascii_case(state));
        }
        if meetings.is_empty() {
            let day = date.format("%A %-d %B");
            let spoken = if date < self.today() {
                format!("I don't have any meetings on file for {day}.")
            } else {
                format!("I don't have any meetings on {day} yet. Fields are published two to three days ahead.")
            };
            return Ok(answer(
                spoken,
                json!({ "date": date, "meetings": [], "following": [] }),
            ));
        }
        // A listener with a home state hears its meetings in full and the rest by name, and
        // start times in their own clock when it differs from the venue's.
        let profile = self.profile(&ctx).await;
        let home_state = profile.home_state.clone();
        let home = match args.state {
            Some(_) => None,
            None => home_state.clone(),
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
        let mut spoken = full
            .iter()
            .map(|m| self.meeting_line(m, home_state.as_deref()))
            .collect::<Vec<_>>()
            .join(" ");
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
        // The listener's own horses that day, each with its race and start in their clock.
        let following = followed_in(
            &profile,
            meetings
                .iter()
                .flat_map(|m| &m.races)
                .flat_map(|r| &r.runners)
                .map(|x| x.horse.as_str()),
        );
        let past = date < self.today();
        let mut running: Vec<(Vec<String>, String)> = Vec::new();
        let mut scratched: Vec<String> = Vec::new();
        for m in &meetings {
            for r in &m.races {
                let entered = |want_scratched: bool| -> Vec<String> {
                    following
                        .iter()
                        .filter(|h| {
                            r.runners.iter().any(|x| {
                                x.scratched == want_scratched && horse_key(&x.horse) == horse_key(h)
                            })
                        })
                        .cloned()
                        .collect()
                };
                let race = format!("race {} at {}", r.race_number, m.venue);
                for h in entered(true) {
                    scratched.push(format!("{h} from {race}"));
                }
                let horses = entered(false);
                if horses.is_empty() {
                    continue;
                }
                let at = self
                    .start(m.date, &r.start_local, &m.state, home_state.as_deref())
                    .short()
                    .map(|t| format!(" at {t}"))
                    .unwrap_or_default();
                running.push((horses, format!("{race}{at}")));
            }
        }
        let mut yours = match running.as_slice() {
            [] => String::new(),
            [(horses, race)] => match horses.as_slice() {
                [one] => format!(
                    " Your horse {one} {} in {race}.",
                    if past { "ran" } else { "runs" }
                ),
                many => format!(
                    " Your horses {} {} in {race}.",
                    spoken_list(many),
                    if past { "ran" } else { "run" }
                ),
            },
            many => format!(
                " Your horses {}: {}.",
                if past { "ran" } else { "are running" },
                spoken_list(
                    &many
                        .iter()
                        .map(|(horses, race)| format!("{} in {race}", spoken_list(horses)))
                        .collect::<Vec<_>>()
                )
            ),
        };
        match scratched.as_slice() {
            [] => {}
            [one] => yours.push_str(&format!(" Your horse {one} has been scratched.")),
            many => yours.push_str(&format!(
                " Your horses {} have been scratched.",
                spoken_list(many)
            )),
        }
        Ok(answer(
            format!(
                "According to {SOURCE_RACING_AUSTRALIA}, on {}: {spoken}{yours}",
                date.format("%A %-d %B")
            ),
            json!({ "date": date, "source": SOURCE_RACING_AUSTRALIA, "home_state": home_state, "following": following, "meetings": meetings.iter().map(|m| json!({
                "venue": m.venue, "state": m.state, "track_condition": m.track_condition, "rail": m.rail,
                "races": m.races.iter().map(|r| json!({
                    "race_number": r.race_number, "name": r.name, "start_local": r.start_local,
                    "start_home": self.start(m.date, &r.start_local, &m.state, home_state.as_deref()).start_home(),
                    "distance_m": r.distance_m, "grade": r.grade,
                })).collect::<Vec<_>>()
            })).collect::<Vec<_>>() }),
        ))
    }

    #[tool(
        title = "Race card",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<RaceArgs>(),
        meta = app_meta(),
        description = "The race card for one race: name, distance, class, prize and the full field with barriers, jockeys, trainers and weights. Names the listener's followed horses when they are in the field. A race can be asked for by name (race), or by venue and race number. Use for 'who is running in the Caulfield Cup' or 'who is running in race 8 at Caulfield'."
    )]
    async fn get_race_card(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<RaceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let RaceAt {
            date,
            venue,
            race_number,
            note,
        } = match self.locate_race(&args, RaceTool::Card).await? {
            Ok(at) => at,
            Err(reply) => return Ok(reply),
        };
        let Some(card) = self
            .store
            .race_card(date, &venue, race_number)
            .await
            .map_err(internal)?
        else {
            return Ok(with_note(
                &note,
                self.missing_race(date, &venue, race_number, false).await,
            ));
        };
        let runners = card.runners.iter().filter(|r| !r.scratched).count();
        let scratched: Vec<_> = card
            .runners
            .iter()
            .filter(|r| r.scratched)
            .map(|r| r.horse.clone())
            .collect();
        // The jump in the listener's own clock when their state keeps a different time.
        let venue_state = self
            .store
            .meetings(date)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|m| venue_matches(&m.venue, &venue))
            .map(|m| m.state)
            .unwrap_or_default();
        let profile = self.profile(&ctx).await;
        let home = profile.home_state.clone();
        let start = self.start(date, &card.start_local, &venue_state, home.as_deref());
        let jump = match start.long() {
            Some(at) => format!(", jumping at {at}{}", start.relative()),
            None if card.start_local.trim().is_empty() => String::new(),
            None => ", start time to be confirmed".to_string(),
        };
        let mut spoken = format!(
            "{note}Race {} at {} is {}. {} runners{jump}.",
            card.race_number,
            venue,
            named_race(&card),
            runners
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
        // The listener's own horses in the field, last, so the card reads as it always has.
        let following = followed_in(&profile, card.runners.iter().map(|r| r.horse.as_str()));
        spoken.push_str(&your_runners(&card, &following));
        // Where each runner usually settles, for screens to draw a map of the field.
        let styles: serde_json::Map<String, serde_json::Value> = self
            .run_styles(&card)
            .await
            .into_iter()
            .filter_map(|(_, horse, st)| Some((horse, json!(st?.style))))
            .collect();
        Ok(answer(
            spoken,
            json!({ "found": true, "date": date, "venue": venue, "source": SOURCE_RACING_AUSTRALIA, "card": card, "jump": start.json(), "run_styles": styles, "following": following }),
        ))
    }

    #[tool(
        title = "Horse form",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<HorseArgs>(),
        meta = app_meta(),
        description = "A horse's form: career record, first-up and track-condition records, and its recent starts with margins and last-600m times. Use for 'how has Sample Stayer been going' or 'has it won on a soft track'."
    )]
    async fn horse_form(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<HorseArgs>,
    ) -> Result<CallToolResult, McpError> {
        let (form, heard_as) = match self.find_horse(&args.horse).await? {
            HorseLookup::Found(form, heard_as) => (form, heard_as),
            HorseLookup::Unsure(names) => {
                return Ok(self.horse_did_you_mean(&args.horse, &names).await)
            }
            HorseLookup::Missing => {
                return Ok(answer(
                    format!("I don't have form on file for {}.", args.horse),
                    json!({ "found": false }),
                ))
            }
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
                    .map(|d| format!(" over {d} metres"))
                    .unwrap_or_default();
                let track = if s.condition.is_empty() {
                    String::new()
                } else {
                    format!(" on a {} track", s.condition)
                };
                // Where it was at the 800 tells how the run unfolded.
                let settled = match s.pos_800 {
                    Some(1) => ", after leading at the 800".to_string(),
                    Some(p) if p > 1 => format!(", from {} at the 800", trackside_core::ordinal(p)),
                    _ => String::new(),
                };
                format!(
                    "{}{}{distance}{track} it {}{}{settled}",
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
        let style = run_style(&form.starts);
        let habit = style
            .as_ref()
            .map(|st| format!(" In its races it {}.", st.phrase))
            .unwrap_or_default();
        let following = !followed_in(&self.profile(&ctx).await, [form.horse.as_str()]).is_empty();
        let spoken = format!(
            "{}{}, trained by {}. Career {}.{first_up}{going}{habit}{recent} Source: {SOURCE_RACING_AUSTRALIA}.{}",
            heard_note(&heard_as, &form.horse),
            form.horse,
            form.trainer,
            form.career.summary(),
            if following { " You're following it." } else { "" }
        );
        Ok(answer(
            spoken,
            json!({ "found": true, "source": SOURCE_RACING_AUSTRALIA, "heard_as": heard_as, "form": form, "run_style": style, "following": following }),
        ))
    }

    #[tool(
        title = "Explain a race",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<RaceArgs>(),
        meta = app_meta(),
        description = "Explain a race in plain language for a newcomer: what it is, why it matters and which runners bring the strongest form. No betting or prices. Use for 'tell me about the Caulfield Cup' or 'explain race 8 at Flemington'. A race can be asked for by name (race), or by venue and race number."
    )]
    async fn explain_race(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<RaceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let RaceAt {
            date,
            venue,
            race_number,
            note,
        } = match self.locate_race(&args, RaceTool::Explain).await? {
            Ok(at) => at,
            Err(reply) => return Ok(reply),
        };
        let Some(card) = self
            .store
            .race_card(date, &venue, race_number)
            .await
            .map_err(internal)?
        else {
            return Ok(with_note(
                &note,
                self.missing_race(date, &venue, race_number, false).await,
            ));
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
        // Recent form: most wins in the last-10 string first, ties broken by rating. Only a
        // runner with a start and a win on that string has form to speak of; a first-starter
        // never hears "0 wins in its last 0 starts".
        let mut ranked: Vec<_> = card.runners.iter().filter(|r| !r.scratched).collect();
        ranked.sort_by(|a, b| {
            let wa = a.last10.matches('1').count();
            let wb = b.last10.matches('1').count();
            wb.cmp(&wa).then(b.rating.cmp(&a.rating))
        });
        let formed: Vec<_> = ranked
            .iter()
            .filter(|r| r.last10.chars().any(|c| c.is_ascii_digit()) && r.last10.contains('1'))
            .take(3)
            .copied()
            .collect();
        let form_words = formed
            .iter()
            .map(|r| {
                let wins = r.last10.matches('1').count();
                format!(
                    "{} ({wins} win{} in its last {} starts)",
                    r.horse,
                    if wins == 1 { "" } else { "s" },
                    r.last10.chars().filter(|c| c.is_ascii_digit()).count()
                )
            })
            .collect::<Vec<_>>();
        let track = self.track_condition(date, &venue).await;
        let styles = self.run_styles(&card).await;
        let pace = pace_sentence(&styles);
        let title = race_title(&card, &venue);
        let form_sentence = if form_words.is_empty() {
            String::new()
        } else {
            format!(
                " The strongest recent form belongs to {}.",
                spoken_list(&form_words)
            )
        };
        let template = format!("{title}: {why} Track is {track}.{form_sentence}{pace}");
        // Bedrock gets the same facts the template uses, plus the field, and nothing else.
        let facts = json!({
            "race": title, "venue": venue, "date": date.format("%A %-d %B").to_string(),
            "race_number": card.race_number, "distance_m": card.distance_m, "grade": card.grade,
            "class": card.class, "prize_total": prize_total(&card.prize).map(spoken_money),
            "why_it_matters": why, "track_condition": track,
            "feature_race": feature.is_some(),
            "strongest_recent_form": formed.iter().map(|r| json!({
                "horse": r.horse, "wins_in_recent_starts": r.last10.matches('1').count(),
                "recent_starts": r.last10.chars().filter(|c| c.is_ascii_digit()).count(),
                "jockey": r.jockey, "trainer": r.trainer, "barrier": r.barrier,
            })).collect::<Vec<_>>(),
            "field_size": ranked.len(),
            "where_they_usually_settle": styles.iter().filter_map(|(_, horse, st)| st.as_ref().map(|st| json!({ "horse": horse, "past_run_style": st.phrase }))).collect::<Vec<_>>(),
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
        // The listener's own horses are said after the explanation and never reach the model.
        let following = followed_in(
            &self.profile(&ctx).await,
            card.runners.iter().map(|r| r.horse.as_str()),
        );
        let spoken = format!(
            "{note}{explanation} Source: {SOURCE_RACING_AUSTRALIA}.{}",
            your_runners(&card, &following)
        );
        Ok(answer(
            spoken,
            json!({ "found": true, "race": title, "why": why, "strongest_recent_form": formed.iter().map(|r| r.horse.clone()).collect::<Vec<_>>(), "explanation": explanation, "written_by": written_by, "facts": facts, "source": SOURCE_RACING_AUSTRALIA, "following": following }),
        ))
    }

    #[tool(
        title = "Race result",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<RaceArgs>(),
        meta = app_meta(),
        description = "The result of a race: placings, margins, winning time and the fastest last 600 metres from sectional timing. Use for 'who won race 7 at Flemington', 'who won the Caulfield Cup' or 'who ran the fastest last 600'. A race can be asked for by name (race), or by venue and race number."
    )]
    async fn race_result(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<RaceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let RaceAt {
            date,
            venue,
            race_number,
            note,
        } = match self.locate_race(&args, RaceTool::Result).await? {
            Ok(at) => at,
            Err(reply) => return Ok(reply),
        };
        let Some(result) = self
            .store
            .race_result(date, &venue, race_number)
            .await
            .map_err(internal)?
        else {
            return Ok(with_note(
                &note,
                self.missing_race(date, &venue, race_number, true).await,
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
        // How the race was run: each runner's position at the 800 (from its form line, once
        // Racing Australia publishes it) and last 600 m (sectional timing).
        let mut run = Vec::new();
        // Each finisher's lengths from the winner, from its form line (a result's own margin
        // may be to the horse in front, so it is used only for the runner-up).
        let mut from_winner = Vec::new();
        for p in result.placings.iter().filter(|p| p.position >= 1) {
            let start = self
                .store
                .horse_form(&p.horse)
                .await
                .map_err(internal)?
                .and_then(|f| f.starts.into_iter().find(|s| s.date == result.date));
            from_winner.push(
                start
                    .as_ref()
                    .and_then(|s| s.margin_lengths)
                    .or(p.margin_lengths.filter(|_| p.position == 2)),
            );
            run.push(RunLine {
                position: p.position,
                horse: p.horse.clone(),
                pos_800: start.as_ref().and_then(|s| s.pos_800),
                pos_400: start.as_ref().and_then(|s| s.pos_400),
                last_600_s: p.last_600_s.or(start.as_ref().and_then(|s| s.last_600_s)),
                story: start.as_ref().and_then(|s| s.run_story()),
            });
        }
        let story = how_it_was_run(&run, result.fastest_last_600.as_ref());
        let following = followed_in(
            &self.profile(&ctx).await,
            result.placings.iter().map(|p| p.horse.as_str()),
        );
        let yours: Vec<(String, u32, Option<f64>, Option<u32>)> = following
            .iter()
            .filter_map(|h| {
                let i = run
                    .iter()
                    .position(|r| horse_key(&r.horse) == horse_key(h))?;
                Some((h.clone(), run[i].position, from_winner[i], run[i].pos_800))
            })
            .collect();
        let spoken = format!(
            "{note}Race {} at {} on {}: {placings}.{} Time {}.{story} Source: {SOURCE_RACING_AUSTRALIA}.{}",
            result.race_number,
            result.venue,
            date.format("%-d %B"),
            result
                .track_condition
                .as_ref()
                .map(|t| format!(" Track {t}."))
                .unwrap_or_default(),
            result
                .winning_time
                .clone()
                .unwrap_or_else(|| "not recorded".into()),
            your_finishers(&yours)
        );
        Ok(answer(
            spoken,
            json!({ "found": true, "source": [SOURCE_RACING_AUSTRALIA, SOURCE_SECTIONALS], "result": result, "run": run, "following": following }),
        ))
    }

    #[tool(
        title = "Jockey or trainer record",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<PersonArgs>(),
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
        let mut stats = self
            .store
            .person_stats(&args.name, &role, from, to)
            .await
            .map_err(internal)?;
        let mut heard_as = None;
        if stats.is_none() {
            // Not that spelling: try the names that sound like it, or ask.
            let similar = self
                .store
                .similar_people(&args.name, &role, 3)
                .await
                .map_err(internal)?;
            match similar.as_slice() {
                [] => {}
                [one] => {
                    stats = self
                        .store
                        .person_stats(one, &role, from, to)
                        .await
                        .map_err(internal)?;
                    heard_as = stats.is_some().then(|| args.name.clone());
                }
                many => return Ok(did_you_mean(&args.name, many)),
            }
        }
        let Some(stats) = stats else {
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
            "{}{} {}: {}, a {strike:.0} percent strike rate. Source: {SOURCE_RACING_AUSTRALIA}.",
            heard_note(&heard_as, &stats.name),
            capitalise(&role),
            stats.name,
            stats.record.summary()
        );
        Ok(answer(
            spoken,
            json!({ "found": true, "source": SOURCE_RACING_AUSTRALIA, "heard_as": heard_as, "stats": stats }),
        ))
    }

    #[tool(
        title = "Follow a horse",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<HorseArgs>(),
        description = "Follow a horse. Trackside remembers the horses each listener follows across sessions, says when the horse runs next, and reports how it went. Use for 'follow Sample Stayer' or 'add it to my stable'."
    )]
    async fn follow_horse(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<HorseArgs>,
    ) -> Result<CallToolResult, McpError> {
        let (form, heard_as) = match self.find_horse(args.horse.trim()).await? {
            HorseLookup::Found(form, heard_as) => (form, heard_as),
            HorseLookup::Unsure(names) => return Ok(self.horse_did_you_mean(&args.horse, &names).await),
            HorseLookup::Missing => return Ok(answer(
                format!("I can't find a horse called {} in the form guide, so I haven't added it. Check the spelling, or ask me who's running in a race.", args.horse.trim()),
                json!({ "found": false }),
            )),
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
        let today = self.today();
        profile.last_checked.get_or_insert(today);
        self.memory.save(&user, &profile).await.map_err(internal)?;
        let n = profile.horses.len();
        let remember = if first && self.memory.durable() {
            " I'll remember your stable next time, and tell you how they've gone since you last asked."
        } else {
            ""
        };
        // The horse's next run, when the fields already hold one.
        let next = self
            .store
            .engagements(&name, today, today + Days::new(LOOK_AHEAD_DAYS))
            .await
            .map_err(internal)?
            .into_iter()
            .next();
        let start = next.as_ref().map(|e| {
            self.start(
                e.date,
                &e.start_local,
                &e.state,
                profile.home_state.as_deref(),
            )
        });
        let coming = match (&next, &start) {
            (Some(e), Some(s)) if e.scratched => {
                format!(" {name} {}.", next_run_words(e, today, s.short()))
            }
            (Some(e), Some(s)) => format!(
                " {name} {}{}; ask me after the race and I'll tell you how it went.",
                next_run_words(e, today, s.short()),
                s.relative()
            ),
            _ => String::new(),
        };
        let next = next.map(|e| {
            let mut v = json!(e);
            v["start_home"] = json!(start.as_ref().and_then(Start::start_home));
            v
        });
        Ok(answer(
            format!(
                "{}Following {name}. You now follow {n} horse{}.{remember}{coming}",
                heard_note(&heard_as, &name),
                if n == 1 { "" } else { "s" }
            ),
            json!({ "found": true, "stable": profile.horses, "next": next, "remembered": self.memory.durable() }),
        ))
    }

    #[tool(
        title = "Unfollow a horse",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<HorseArgs>(),
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
        let found = profile
            .horses
            .iter()
            .position(|h| horse_key(h) == key)
            .or_else(|| {
                let best = names::best(&args.horse, profile.horses.iter().map(String::as_str))?;
                profile.horses.iter().position(|h| h == best)
            });
        let Some(i) = found else {
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
        title = "My stable",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false),
        input_schema = portable::<DateArgs>(),
        meta = app_meta(),
        description = "The horses the user follows and what's new for them: how they have run since the user last asked (remembered across sessions), today's engagements and results, and each horse's next run in the coming days. Use for 'what's happening with my stable', 'any of my horses running today', 'when does my horse run next' or 'how did my horses go'."
    )]
    async fn my_stable(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<DateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let date = parse_date(&args.date, self.today())?;
        let user = caller_key(&ctx);
        let mut profile = self.memory.load(&user).await.map_err(internal)?;
        let stable = profile.horses.clone();
        let home_state = profile.home_state.clone();
        let home = home_state.as_deref();
        if stable.is_empty() {
            return Ok(answer(
                "You aren't following any horses yet. Say 'follow' and a horse's name to start."
                    .into(),
                json!({ "stable": [] }),
            ));
        }
        let meetings = self.store.meetings(date).await.map_err(internal)?;
        // For a day that hasn't passed, the fields for the days after it: where each horse
        // not engaged on the asked day runs next. A past day's report is about that day.
        let today = self.today();
        let look_ahead_to = date + Days::new(LOOK_AHEAD_DAYS);
        let mut ahead: Vec<Meeting> = Vec::new();
        if date >= today {
            let mut day = date + Days::new(1);
            while day <= look_ahead_to {
                ahead.extend(self.store.meetings(day).await.map_err(internal)?);
                day = day + Days::new(1);
            }
        }
        let mut lines = Vec::new();
        let mut engagements = Vec::new();
        let mut upcoming: Vec<serde_json::Value> = Vec::new();
        // What happened between the last report and this day, from each horse's form. Nothing
        // to catch up on until a day has passed since the last report.
        let heard_up_to = date.min(today);
        let since = profile.last_checked.filter(|d| *d < heard_up_to);
        let mut catch_up = Vec::new();
        if let Some(since) = since {
            let mut heard = Vec::new();
            for horse in &stable {
                let Some(form) = self.store.horse_form(horse).await.map_err(internal)? else {
                    continue;
                };
                // From the day of the last check itself: a race run that day may have had
                // no result when the listener asked.
                let mut runs: Vec<_> = form
                    .starts
                    .iter()
                    .filter(|s| s.date >= since && s.date < date)
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
                                "{horse} {outcome} in race {} at {}{} on {}",
                                r.race_number,
                                m.venue,
                                in_brackets(&r.name),
                                res.date.format("%-d %b")
                            ));
                            engagements.push(json!({ "horse": horse, "venue": m.venue, "race_number": r.race_number, "start_local": r.start_local, "finished": placing.map(|p| p.position) }));
                            continue;
                        }
                        if runner.scratched {
                            lines.push(format!(
                                "{horse} has been scratched from race {} at {}{}",
                                r.race_number,
                                m.venue,
                                in_brackets(&r.name)
                            ));
                            engagements.push(json!({ "horse": horse, "venue": m.venue, "race_number": r.race_number, "start_local": r.start_local, "scratched": true }));
                            continue;
                        }
                        let start = self.start(date, &r.start_local, &m.state, home);
                        lines.push(format!(
                            "{} runs in race {} at {}{}{}, barrier {}, {} up{}",
                            horse,
                            r.race_number,
                            m.venue,
                            in_brackets(&r.name),
                            start
                                .short()
                                .map(|at| format!(" at {at}"))
                                .unwrap_or_default(),
                            runner
                                .barrier
                                .map(|b| b.to_string())
                                .unwrap_or_else(|| "TBA".into()),
                            runner.jockey,
                            start.relative()
                        ));
                        engagements.push(json!({ "horse": horse, "venue": m.venue, "race_number": r.race_number, "start_local": r.start_local, "start_home": start.start_home() }));
                    }
                }
            }
            if found {
                continue;
            }
            // Not engaged that day: its next run, when the fields hold one.
            if let Some(mut e) = engagements_in(&ahead, horse).into_iter().next() {
                let start = self.start(e.date, &e.start_local, &e.state, home);
                lines.push(format!(
                    "{horse} {}",
                    next_run_words(&e, date, start.short())
                ));
                // Named as the listener's stable names it, so a screen can match the two.
                e.horse = horse.clone();
                let mut v = json!(e);
                v["start_home"] = json!(start.start_home());
                upcoming.push(v);
                continue;
            }
            // Horses already covered by the catch-up don't need their last start again.
            if catch_up.iter().any(|c| c["horse"] == horse.as_str()) {
                continue;
            }
            let not_engaged = if date >= today {
                format!(
                    "{horse} isn't engaged on {} or in any field through {}",
                    date.format("%-d %b"),
                    look_ahead_to.format("%-d %b")
                )
            } else {
                format!("{horse} isn't engaged on {}", date.format("%-d %b"))
            };
            let last = self
                .store
                .horse_form(horse)
                .await
                .map_err(internal)?
                .and_then(|f| f.starts.first().cloned());
            match last {
                Some(s) => lines.push(format!(
                    "{not_engaged}; last start it {}{} on {}",
                    finish_words(s.finish, None),
                    at_venue(&s.venue),
                    s.date.format("%-d %b")
                )),
                None => lines.push(not_engaged),
            }
        }
        // The next report starts from here; asking about a future card doesn't move it.
        if profile.last_checked.is_none_or(|d| d < heard_up_to) {
            profile.last_checked = Some(heard_up_to);
            self.memory.save(&user, &profile).await.map_err(internal)?;
        }
        Ok(answer(
            format!("{}. Source: {SOURCE_RACING_AUSTRALIA}.", lines.join(". ")),
            json!({ "date": date, "stable": stable, "since": since, "catch_up": catch_up, "engagements": engagements, "upcoming": upcoming, "remembered": self.memory.durable() }),
        ))
    }

    #[tool(
        title = "Next race",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<NextRaceArgs>(),
        description = "The next race to jump, anywhere in Australia or in one state or at one venue: how far off it is, its start in the listener's own clock, and the races after it. Racing in the listener's remembered home state is read first. Use for 'what's the next race', 'what's next', 'when's the next race at Randwick', 'what's jumping now' or 'what's on next in Queensland'."
    )]
    async fn next_race(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<NextRaceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let state = match args.state.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(s) => {
                let s = s.to_ascii_uppercase();
                if !STATES.contains(&s.as_str()) {
                    return Err(McpError::invalid_params(
                        format!("state must be one of {}", STATES.join(", ")),
                        None,
                    ));
                }
                Some(s)
            }
        };
        let home = self
            .profile(&ctx)
            .await
            .home_state
            .map(|h| h.trim().to_ascii_uppercase())
            .filter(|h| STATES.contains(&h.as_str()));
        let home = home.as_deref();
        let today = self.today();
        let tomorrow = today + Days::new(1);
        // The venue as the day's meetings name it: today's first, then tomorrow's.
        let mut venue = None;
        if let Some(heard) = args
            .venue
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            let mut named = heard.to_string();
            for day in [today, tomorrow] {
                let resolved = match self.resolve_venue(day, heard).await {
                    Ok(v) => v,
                    Err(ask) => return Ok(ask),
                };
                let meetings = self.store.meetings(day).await.map_err(internal)?;
                if meetings.iter().any(|m| venue_matches(&m.venue, &resolved)) {
                    named = resolved;
                    break;
                }
            }
            venue = Some(named);
        }
        let filter = RaceFilter {
            state: state.as_deref(),
            venue: venue.as_deref(),
        };
        let scope = match (&venue, &state) {
            (Some(v), _) => format!(" at {v}"),
            (None, Some(s)) => format!(" in {s}"),
            (None, None) => String::new(),
        };
        let now = self.clock.now();
        let (racing_today, left) = self.races_from(today, &filter, Some(now)).await?;
        let ahead = if left.is_empty() {
            self.races_from(tomorrow, &filter, None).await?.1
        } else {
            Vec::new()
        };
        // Nothing today or tomorrow: the next day in the fields' window that has racing.
        let mut later = Vec::new();
        if left.is_empty() && ahead.is_empty() {
            let mut day = tomorrow + Days::new(1);
            while later.is_empty() && day <= today + Days::new(LOOK_AHEAD_DAYS) {
                later = self.races_from(day, &filter, None).await?.1;
                day = day + Days::new(1);
            }
        }
        let done = if racing_today {
            format!("Racing{scope} is done for today")
        } else {
            format!("There's no racing{scope} on file for today")
        };
        let none = if racing_today {
            format!("{done}, and I don't have any racing on file for tomorrow.")
        } else {
            format!("I don't have any racing{scope} on file for today or tomorrow.")
        };
        let mut anywhere = None;
        let (spoken, next, then, when): (String, Option<&NextUp>, Vec<&NextUp>, &str) =
            if let Some(first) = left.first() {
                // A listener with a home state hears the next race there first, when the next
                // anywhere is somewhere else and their own state still has racing today.
                let near = home
                    .filter(|_| filter.is_open())
                    .filter(|h| first.state != *h)
                    .and_then(|h| left.iter().find(|u| u.state == h));
                let spoken = match near {
                    Some(near) => {
                        let at = self.start(today, &near.race.start_local, &near.state, home);
                        let there = self.start(today, &first.race.start_local, &first.state, home);
                        anywhere = Some(first);
                        format!(
                            "The next race near you is race {} at {}{}, at {}; the next anywhere is race {} at {} at {}.",
                            near.race.race_number,
                            near.venue,
                            at.relative(),
                            at_words(&at, home, false),
                            first.race.race_number,
                            first.venue,
                            at_words(&there, home, false),
                        )
                    }
                    None => {
                        let at = self.start(today, &first.race.start_local, &first.state, home);
                        let mut s = format!(
                            "The next race{scope} is race {} at {}{}{}, at {}.",
                            first.race.race_number,
                            first.venue,
                            race_words(&first.race),
                            at.relative(),
                            at_words(&at, home, true),
                        );
                        if let Some(after) = left.get(1) {
                            let at = self.start(today, &after.race.start_local, &after.state, home);
                            s.push_str(&format!(
                                " After that, race {} at {} at {}.",
                                after.race.race_number,
                                after.venue,
                                at_words(&at, home, false),
                            ));
                        }
                        s
                    }
                };
                let next = near.unwrap_or(first);
                let then = left
                    .iter()
                    .filter(|u| !std::ptr::eq(*u, next))
                    .take(3)
                    .collect();
                (spoken, Some(next), then, "today")
            } else if let Some(first) = ahead.first() {
                let at = self.start(tomorrow, &first.race.start_local, &first.state, home);
                (
                    format!(
                        "{done}; tomorrow's first race is race {} at {} at {}.",
                        first.race.race_number,
                        first.venue,
                        at_words(&at, home, true),
                    ),
                    Some(first),
                    ahead.iter().skip(1).take(3).collect(),
                    "tomorrow",
                )
            } else if let Some(first) = later.first() {
                let at = self.start(first.date, &first.race.start_local, &first.state, home);
                (
                    format!(
                        "{none} The next racing I have is on {}, starting with race {} at {} at {}.",
                        first.date.format("%A %-d %B"),
                        first.race.race_number,
                        first.venue,
                        at_words(&at, home, true),
                    ),
                    Some(first),
                    later.iter().skip(1).take(3).collect(),
                    "later",
                )
            } else {
                (none, None, Vec::new(), "none")
            };
        Ok(answer(
            format!("{spoken} Source: {SOURCE_RACING_AUSTRALIA}."),
            json!({
                "found": next.is_some(),
                "when": when,
                "next": next.map(|u| self.next_json(u, home)),
                "then": then.iter().map(|u| self.next_json(u, home)).collect::<Vec<_>>(),
                "next_anywhere": anywhere.map(|u| self.next_json(u, home)),
                "home_state": home,
                "state": state,
                "venue": venue,
            }),
        ))
    }

    #[tool(
        title = "Home state",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        input_schema = portable::<StateArgs>(),
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
        title = "Forget me",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
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
        title = "Spring Carnival guide",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        input_schema = portable::<CarnivalArgs>(),
        description = "The 2026 Spring Racing Carnival as it stands today: the next feature race with its date and, once fields are out, its runners and jump time; the latest feature result; and what follows. Give a race by name for that feature alone. Use for 'what's on this carnival', 'when's the Cox Plate' or 'who won the Caulfield Cup'."
    )]
    async fn carnival_guide(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(args): Parameters<CarnivalArgs>,
    ) -> Result<CallToolResult, McpError> {
        let today = parse_date(&args.date, self.today())?;
        let home = self.profile(&ctx).await.home_state;
        if let Some(heard) = args.race.as_deref().filter(|r| !r.trim().is_empty()) {
            let feature = match trackside_core::resolve_feature(heard) {
                Ok(f) => f,
                Err(could_be) if !could_be.is_empty() => {
                    let names: Vec<String> = could_be.iter().map(|n| with_article(n)).collect();
                    return Ok(did_you_mean(heard, &names));
                }
                Err(_) => {
                    let all: Vec<String> = spring_carnival_2026()
                        .iter()
                        .map(|f| with_article(f.name))
                        .collect();
                    return Ok(answer(
                        format!(
                            "{} isn't one of the Spring Carnival's feature races. They are {}.",
                            heard.trim(),
                            spoken_list(&all)
                        ),
                        json!({ "found": false, "date": today, "heard": heard.trim(), "features": spring_carnival_2026().iter().map(|f| f.name).collect::<Vec<_>>() }),
                    ));
                }
            };
            let status = self.feature_status(feature, today, home.as_deref()).await?;
            let source = if status.quotes_store() {
                format!(" Source: {SOURCE_RACING_AUSTRALIA}.")
            } else {
                String::new()
            };
            let race = status.json(today);
            return Ok(answer(
                format!("{}{source}", status.said(today)),
                json!({ "found": true, "date": today, "race": race, "races": [race], "source": SOURCE_RACING_AUSTRALIA }),
            ));
        }
        let mut statuses = Vec::new();
        for feature in spring_carnival_2026() {
            statuses.push(self.feature_status(feature, today, home.as_deref()).await?);
        }
        let spoken = carnival_words(&statuses, today);
        let next = statuses.iter().find(|s| s.status != "run");
        let latest = statuses.iter().rev().find(|s| s.status == "run");
        Ok(answer(
            spoken,
            json!({
                "date": today,
                "next": next.map(|s| s.feature.name),
                "latest": latest.map(|s| s.feature.name),
                "carnival_over": next.is_none(),
                "races": statuses.iter().map(|s| s.json(today)).collect::<Vec<_>>(),
                "source": SOURCE_RACING_AUSTRALIA,
            }),
        ))
    }
}

impl Trackside {
    /// A horse by the name as heard: exactly, or the one name that sounds like it, or the
    /// few that might be meant.
    async fn find_horse(&self, heard: &str) -> Result<HorseLookup, McpError> {
        if let Some(form) = self.store.horse_form(heard).await.map_err(internal)? {
            return Ok(HorseLookup::Found(form, None));
        }
        let similar = self
            .store
            .similar_horses(heard, 3)
            .await
            .map_err(internal)?;
        let Some(name) = names::best(heard, similar.iter().map(String::as_str)) else {
            return Ok(match similar.is_empty() {
                true => HorseLookup::Missing,
                false => HorseLookup::Unsure(similar),
            });
        };
        Ok(match self.store.horse_form(name).await.map_err(internal)? {
            Some(form) => HorseLookup::Found(form, Some(heard.trim().to_string())),
            None => HorseLookup::Missing,
        })
    }

    /// The venue as the day's meetings name it: the one meeting the word matches, otherwise
    /// the one that sounds like it ("Cofield" is Caulfield). Unchanged when nothing does. When
    /// two meetings answer to the word ("Warwick" on a day Warwick Farm and Warwick both
    /// race), the error is the answer that asks which one, rather than a guess.
    async fn resolve_venue(&self, date: NaiveDate, heard: &str) -> Result<String, CallToolResult> {
        let meetings = self.store.meetings(date).await.unwrap_or_default();
        let matched = distinct(
            meetings
                .iter()
                .filter(|m| venue_matches(&m.venue, heard))
                .map(|m| m.venue.as_str()),
        );
        match matched.as_slice() {
            [] => {}
            // The meeting's own name: "Warwick" is Warwick Farm on a day only it races.
            [one] => return Ok(one.clone()),
            many => {
                // "Warwick Farm", said in full, also matches Warwick, whose name it contains;
                // the full name is meant. "Warwick" alone could be either, so ask.
                let said = norm(heard);
                let in_full = many.iter().filter(|v| norm(v) == said).count() == 1
                    && many.iter().all(|v| norm(v).len() <= said.len());
                return if in_full {
                    Ok(heard.to_string())
                } else {
                    Err(did_you_mean(heard, many))
                };
            }
        }
        // A venue answers to its full name and to its leading or trailing words
        // ("Rosehill" for "Rosehill Gardens", "Murray Bridge" for "Thomas Farms RC Murray Bridge").
        let mut forms: Vec<(String, &str)> = Vec::new();
        for m in &meetings {
            let words: Vec<&str> = m.venue.split_whitespace().collect();
            for k in 1..=words.len() {
                forms.push((words[..k].join(" "), &m.venue));
                forms.push((words[words.len() - k..].join(" "), &m.venue));
            }
        }
        let best = names::closest(heard, forms.iter().map(|(f, _)| f.as_str()), 8);
        let venues = distinct(
            best.iter()
                .filter(|(_, d)| *d == best[0].1)
                .filter_map(|(f, _)| forms.iter().find(|(x, _)| x == f).map(|(_, v)| *v)),
        );
        match venues.as_slice() {
            [] => Ok(heard.to_string()),
            [one] => Ok(one.clone()),
            many => Err(did_you_mean(heard, many)),
        }
    }

    /// Each runner's usual run style from its form, in saddlecloth order.
    async fn run_styles(&self, card: &RaceCard) -> Vec<(u32, String, Option<RunStyle>)> {
        let mut out = Vec::new();
        for r in card.runners.iter().filter(|r| !r.scratched) {
            let style = self
                .store
                .horse_form(&r.horse)
                .await
                .ok()
                .flatten()
                .and_then(|f| run_style(&f.starts));
            out.push((r.number, r.horse.clone(), style));
        }
        out
    }

    /// When a race on `date` at a venue in `venue_state` starts, for a listener whose home
    /// state is `home` (none, or the venue's own, means the venue's clock alone).
    fn start(
        &self,
        date: NaiveDate,
        start_local: &str,
        venue_state: &str,
        home: Option<&str>,
    ) -> Start {
        let venue_time = trackside_core::parse_start_local(start_local);
        let venue = venue_time.map(|_| spoken_time(start_local));
        let home_state = home
            .map(|h| h.trim().to_ascii_uppercase())
            .filter(|h| !h.is_empty());
        let home = home_state.as_deref().and_then(|h| {
            let (at, differs) = trackside_core::in_home_zone(date, start_local, venue_state, h)?;
            let label = trackside_core::zone_label(h)?;
            differs.then_some((at, label))
        });
        let minutes_until = (date == self.today())
            .then(|| trackside_core::start_instant(date, start_local, venue_state))
            .flatten()
            .map(|at| (at - self.clock.now()).num_minutes());
        Start {
            venue,
            home: home
                .as_ref()
                .map(|(at, label)| (spoken_time(&at.format("%H:%M").to_string()), *label)),
            minutes_until,
            venue_hhmm: venue_time.map(|t| t.format("%H:%M").to_string()),
            home_hhmm: home.as_ref().map(|(at, _)| at.format("%H:%M").to_string()),
            venue_state: venue_state.to_string(),
            home_state,
        }
    }

    /// "Flemington in VIC: 10 races, track Good 4, first race 12:35 pm." A listener in
    /// another state hears the first race in their own clock as well.
    fn meeting_line(&self, m: &Meeting, home: Option<&str>) -> String {
        let first = m
            .races
            .first()
            .and_then(|r| self.start(m.date, &r.start_local, &m.state, home).long())
            .unwrap_or_else(|| "time to be confirmed".to_string());
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
        let spoken = match (want_result, date >= self.today()) {
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

/// Where the field usually settles, from past runs: " On past runs, A usually leads, and B
/// and C usually settle back in the field." Empty when nobody's style is known.
fn pace_sentence(styles: &[(u32, String, Option<RunStyle>)]) -> String {
    let with = |style: &str| -> Vec<String> {
        styles
            .iter()
            .filter(|(_, _, st)| st.as_ref().is_some_and(|st| st.style == style))
            .map(|(_, h, _)| h.clone())
            .take(3) // a sentence, not the whole field; the screen has the rest
            .collect()
    };
    let (leaders, on_pace, back) = (with("leader"), with("on-pace"), with("back"));
    let verb = |names: &[String], one: &str, many: &str| {
        format!(
            "{} {}",
            spoken_list(names),
            if names.len() == 1 { one } else { many }
        )
    };
    let front = if !leaders.is_empty() {
        verb(&leaders, "usually leads", "usually lead")
    } else if !on_pace.is_empty() {
        verb(
            &on_pace,
            "usually races on the pace",
            "usually race on the pace",
        )
    } else {
        String::new()
    };
    let rear = (!back.is_empty()).then(|| {
        verb(
            &back,
            "usually settles back in the field",
            "usually settle back in the field",
        )
    });
    match (front.is_empty(), rear) {
        (true, None) => String::new(),
        (true, Some(r)) => format!(" On past runs, {r}."),
        (false, None) => format!(" On past runs, {front}."),
        (false, Some(r)) => format!(" On past runs, {front}, and {r}."),
    }
}

enum HorseLookup {
    /// The horse's form, and the name as heard when it wasn't spelt that way.
    Found(HorseForm, Option<String>),
    /// Several horses sound like it.
    Unsure(Vec<String>),
    Missing,
}

/// "Taking Jimmy Star as Jimmysstar. " when the name was matched by sound, so the listener
/// knows which horse they're hearing about.
fn heard_note(heard_as: &Option<String>, name: &str) -> String {
    match heard_as {
        Some(h) if horse_key(h) != horse_key(name) => format!("Taking {h} as {name}. "),
        _ => String::new(),
    }
}

fn did_you_mean(heard: &str, names: &[String]) -> CallToolResult {
    let options: Vec<(String, String)> = names.iter().map(|n| (n.clone(), String::new())).collect();
    did_you_mean_with(heard, &options)
}

/// "Did you mean" with a fact that tells each name apart, written to follow the name as
/// spoken (", trained by C. Trainer" or " at Caulfield on Saturday 17 October"). Names
/// with facts are set apart by ", or" so the last fact doesn't run into the next name.
fn did_you_mean_with(heard: &str, options: &[(String, String)]) -> CallToolResult {
    let names: Vec<String> = options.iter().map(|(n, _)| n.clone()).collect();
    let said: Vec<String> = options.iter().map(|(n, f)| format!("{n}{f}")).collect();
    let with_facts = options.iter().any(|(_, f)| !f.is_empty());
    let last_join = if with_facts { ", or " } else { " or " };
    let spoken = format!(
        "I couldn't place {}. Did you mean {}?",
        heard.trim(),
        match said.as_slice() {
            [one] => one.clone(),
            [init @ .., last] => format!("{}{last_join}{last}", init.join(", ")),
            [] => String::new(),
        }
    );
    let mut structured = json!({ "found": false, "did_you_mean": names });
    if with_facts {
        structured["facts"] = json!(options
            .iter()
            .map(|(_, f)| f.trim_start_matches(", ").trim())
            .collect::<Vec<_>>());
    }
    answer(spoken, structured)
}

/// The names in `names`, each once, in the order first seen.
fn distinct<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if !out.iter().any(|seen| seen == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// One runner's race, for "how it was run".
#[derive(Debug, serde::Serialize)]
pub(crate) struct RunLine {
    pub(crate) position: u32,
    pub(crate) horse: String,
    pub(crate) pos_800: Option<u32>,
    pub(crate) pos_400: Option<u32>,
    pub(crate) last_600_s: Option<f64>,
    pub(crate) story: Option<String>,
}

/// "34.9", "35.12": seconds as they'd be read.
fn secs(t: f64) -> String {
    let s = format!("{t:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A few spoken sentences on how the race unfolded: how the winner won, who ran home
/// fastest and from where, and any placegetter that came from well back. A sentence that
/// speaks a last-600 time names the sectional source; a position at the 800 is Racing
/// Australia's and stands on its own. Empty when there is neither a position at the 800 nor
/// a sectional to go on.
pub(crate) fn how_it_was_run(run: &[RunLine], fastest: Option<&SectionalHighlight>) -> String {
    // Each sentence, and whether it speaks a last-600 time.
    let mut said: Vec<(String, bool)> = Vec::new();
    let named = |h: &str| fastest.is_some_and(|f| horse_key(&f.horse) == horse_key(h));
    let same_time = |t: Option<f64>| {
        fastest
            .zip(t)
            .is_some_and(|(f, t)| (f.last_600_s - t).abs() < 0.005)
    };
    // Two runs clocked at the same time are equal-fastest, whichever one the feed names.
    let shared = run
        .iter()
        .filter(|r| named(&r.horse) || same_time(r.last_600_s))
        .count()
        > 1;
    let label = if shared { "equal-fastest" } else { "fastest" };
    if let Some(w) = run.iter().find(|r| r.position == 1) {
        let mut parts = Vec::new();
        if let Some(story) = &w.story {
            parts.push(story.clone());
        }
        let winner_fastest = fastest.filter(|_| named(&w.horse) || same_time(w.last_600_s));
        let timed = match (winner_fastest, w.last_600_s) {
            (Some(f), _) => {
                parts.push(format!(
                    "ran the {label} last 600 in the race, {} seconds",
                    secs(f.last_600_s)
                ));
                true
            }
            (None, Some(t)) => {
                parts.push(format!("ran its last 600 in {} seconds", secs(t)));
                true
            }
            _ => false,
        };
        if !parts.is_empty() {
            said.push((format!("{} {}", w.horse, parts.join(" and ")), timed));
        }
    }
    let winner_named = run.iter().any(|r| r.position == 1 && named(&r.horse));
    if let Some(f) = fastest.filter(|_| !winner_named) {
        let line = run.iter().find(|r| named(&r.horse));
        let tail: Vec<String> = [
            line.and_then(|r| r.pos_800)
                .filter(|&p| p > 1)
                .map(|p| format!("from {} at the 800", trackside_core::ordinal(p))),
            line.map(|r| format!("to finish {}", trackside_core::ordinal(r.position))),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut sentence = format!(
            "{} ran the {label} last 600, {} seconds",
            f.horse,
            secs(f.last_600_s)
        );
        if !tail.is_empty() {
            sentence.push_str(&format!(", {}", tail.join(" ")));
        }
        said.push((sentence, true));
    }
    for r in run
        .iter()
        .filter(|r| (2..=4).contains(&r.position) && !named(&r.horse))
    {
        if let Some(story) = r.story.as_ref().filter(|s| s.starts_with("came from")) {
            said.push((
                format!(
                    "{} {story} to run {}",
                    r.horse,
                    trackside_core::ordinal(r.position)
                ),
                false,
            ));
        }
    }
    if said.is_empty() {
        return String::new();
    }
    let source = fastest
        .map(|f| format!(", according to {}", f.source))
        .unwrap_or_default();
    let sentences: Vec<String> = said
        .into_iter()
        .map(|(s, timed)| if timed { format!("{s}{source}") } else { s })
        .collect();
    format!(" How it was run: {}.", sentences.join(". "))
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
    /// The router's call, timed and classified for the call's metric line, inside a span that
    /// carries the tool and the request id so any error logged during the call can be traced.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let router = Self::tool_router();
        // Only a tool the server has becomes a dimension; anything else a caller sends is
        // "unknown", so a metric can't carry a caller's text.
        let tool = if router.get(&request.name).is_some() {
            request.name.to_string()
        } else {
            "unknown".to_string()
        };
        let request_id = telemetry::request_id(context.extensions.get::<http::request::Parts>());
        let span = tracing::info_span!(
            "tool_call",
            tool = %tool,
            request_id = request_id.as_deref().unwrap_or("-")
        );
        let started = std::time::Instant::now();
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let (result, notes) =
            telemetry::noting(tracing::Instrument::instrument(router.call(tcc), span)).await;
        if let Some(sink) = &self.telemetry {
            let mut record = telemetry::Record::tool_call(
                &tool,
                telemetry::classify(&result),
                started.elapsed(),
            )
            .merge(notes);
            if let Some(id) = &request_id {
                record = record.property("RequestId", id);
            }
            record.emit(sink.as_ref());
        }
        result
    }

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

// ---- next_race ----

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NextRaceArgs {
    /// Only racing in this state: VIC, NSW, QLD, SA, WA, TAS, NT or ACT. Optional.
    pub state: Option<String>,
    /// Only racing at this venue, e.g. "Randwick". Optional.
    pub venue: Option<String>,
}

/// Which meetings a "next race" question is about: a state, a venue, or (neither) anywhere.
struct RaceFilter<'a> {
    state: Option<&'a str>,
    venue: Option<&'a str>,
}

impl RaceFilter<'_> {
    fn is_open(&self) -> bool {
        self.state.is_none() && self.venue.is_none()
    }

    fn takes(&self, m: &Meeting) -> bool {
        self.state.is_none_or(|s| m.state.eq_ignore_ascii_case(s))
            && self.venue.is_none_or(|v| venue_matches(&m.venue, v))
    }
}

/// One race still to jump, placed on the clock.
struct NextUp {
    date: NaiveDate,
    at: DateTime<Utc>,
    venue: String,
    state: String,
    race: RaceCard,
}

impl Trackside {
    /// The races on `date` the filter takes, in jump order, from `after` on (all of them when
    /// `after` is none), and whether the day had any racing the filter takes at all. A race
    /// whose published start isn't a time can't be placed, so it is left out.
    async fn races_from(
        &self,
        date: NaiveDate,
        filter: &RaceFilter<'_>,
        after: Option<DateTime<Utc>>,
    ) -> Result<(bool, Vec<NextUp>), McpError> {
        let meetings = self.store.meetings(date).await.map_err(internal)?;
        let mut any = false;
        let mut out = Vec::new();
        for m in meetings.into_iter().filter(|m| filter.takes(m)) {
            any |= !m.races.is_empty();
            for race in m.races {
                let Some(at) = trackside_core::start_instant(date, &race.start_local, &m.state)
                else {
                    continue;
                };
                if after.is_some_and(|now| at < now) {
                    continue;
                }
                out.push(NextUp {
                    date,
                    at,
                    venue: m.venue.clone(),
                    state: m.state.to_ascii_uppercase(),
                    race,
                });
            }
        }
        out.sort_by(|a, b| {
            (a.at, &a.venue, a.race.race_number).cmp(&(b.at, &b.venue, b.race.race_number))
        });
        Ok((any, out))
    }

    /// A race to jump as a screen draws it.
    fn next_json(&self, u: &NextUp, home: Option<&str>) -> serde_json::Value {
        let start = self.start(u.date, &u.race.start_local, &u.state, home);
        json!({
            "date": u.date,
            "venue": u.venue,
            "state": u.state,
            "race_number": u.race.race_number,
            "name": u.race.name,
            "distance_m": u.race.distance_m,
            "start_local": u.race.start_local,
            "start_utc": u.at.to_rfc3339(),
            "minutes_until": (u.at - self.clock.now()).num_minutes(),
            "home_time": start.start_home(),
            "home_state": home,
        })
    }
}

/// When a race jumps, always with a clock named so a listener anywhere knows which one:
/// "3:40 pm Melbourne time", or for a listener whose clock differs from the track's,
/// "2:40 pm Perth time, 3:40 pm at the track" (`long`) or "2:40 pm Perth time".
fn at_words(start: &Start, home: Option<&str>, long: bool) -> String {
    let Some(venue) = start.venue.as_ref() else {
        return "a time to be confirmed".into();
    };
    if start.home.is_some() {
        let said = if long { start.long() } else { start.short() };
        return said.unwrap_or_else(|| venue.clone());
    }
    match home
        .and_then(trackside_core::zone_label)
        .or_else(|| trackside_core::zone_label(&start.venue_state))
    {
        Some(label) => format!("{venue} {label}"),
        None => venue.clone(),
    }
}

/// ", the Caulfield Cup over 2400 metres", ", a race over 1200 metres", or nothing.
fn race_words(card: &RaceCard) -> String {
    let over = card
        .distance_m
        .map(|d| format!(" over {d} metres"))
        .unwrap_or_default();
    match (card.name.is_empty(), over.is_empty()) {
        (false, _) => format!(", {}{over}", with_article(&card.name)),
        (true, false) => format!(", a race{over}"),
        (true, true) => String::new(),
    }
}

// ---- The carnival guide, read from the store on the day it is asked ----

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CarnivalArgs {
    /// Date as YYYY-MM-DD to read the carnival from. Defaults to today in Australia/Melbourne.
    pub date: Option<String>,
    /// One feature race by name, e.g. "Cox Plate" or "Melbourne Cup", to hear about that race
    /// alone.
    pub race: Option<String>,
}

/// A feature's winner, as the result on file has it.
struct FeatureResult {
    winner: String,
    jockey: String,
    /// The winning margin: the second horse's margin, or the winner's own when a store gives it.
    margin_lengths: Option<f64>,
}

impl FeatureResult {
    fn from_result(r: &trackside_core::RaceResult) -> Option<Self> {
        let winner = r.placings.iter().find(|p| p.position == 1)?;
        let margin_lengths = winner
            .margin_lengths
            .filter(|m| *m > 0.0)
            .or_else(|| {
                r.placings
                    .iter()
                    .find(|p| p.position == 2)
                    .and_then(|p| p.margin_lengths)
            })
            .filter(|m| *m > 0.0);
        Some(Self {
            winner: winner.horse.clone(),
            jockey: winner.jockey.clone(),
            margin_lengths,
        })
    }

    /// "won by Sample Stayer, ridden by J. Example, by half a length".
    fn words(&self) -> String {
        let ridden = if self.jockey.trim().is_empty() {
            String::new()
        } else {
            format!(", ridden by {}", self.jockey.trim())
        };
        let by = self
            .margin_lengths
            .map(|m| format!(", by {}", margin_words(m)))
            .unwrap_or_default();
        format!("won by {}{ridden}{by}", self.winner)
    }
}

/// Where one feature race stands on a given day.
struct FeatureStatus {
    feature: trackside_core::FeatureRace,
    /// "run", "today", "ahead" (fields out) or "fields_not_out".
    status: &'static str,
    /// The venue as the day's meetings name it, or the guide's own name for it.
    venue: String,
    /// The feature's place on the published card, once it is out.
    race_number: Option<u32>,
    runners: Option<usize>,
    start_local: Option<String>,
    jump: Option<Start>,
    result: Option<FeatureResult>,
}

impl FeatureStatus {
    fn days_to_go(&self, today: NaiveDate) -> i64 {
        (self.feature.date - today).num_days()
    }

    /// Whether anything said comes from the store (a field or a result) rather than the guide.
    fn quotes_store(&self) -> bool {
        self.result.is_some() || self.runners.is_some()
    }

    /// " Fields are out: 14 runners, jumping at 5:05 pm." for a race ahead, " It's race 9,
    /// with 14 runners, jumping at 5:05 pm, due to jump in about 25 minutes." on the day.
    fn field_words(&self) -> String {
        let Some(n) = self.runners else {
            return if self.status == "today" {
                String::new()
            } else {
                " Fields aren't out yet.".into()
            };
        };
        let at = self
            .jump
            .as_ref()
            .and_then(Start::short)
            .map(|t| format!(", jumping at {t}"))
            .unwrap_or_default();
        let runners = format!("{n} runner{}", if n == 1 { "" } else { "s" });
        if self.status == "today" {
            let relative = self.jump.as_ref().map(Start::relative).unwrap_or_default();
            let race = self
                .race_number
                .map(|r| format!("race {r}, with "))
                .unwrap_or_default();
            format!(" It's {race}{runners}{at}{relative}.")
        } else {
            format!(" Fields are out: {runners}{at}.")
        }
    }

    /// The one or two sentences about this race alone, in the tense its date calls for.
    fn said(&self, today: NaiveDate) -> String {
        let f = &self.feature;
        let name = with_article(f.name);
        match self.status {
            "run" => match &self.result {
                Some(r) => format!(
                    "{}, {name} at {} was {}.",
                    capitalise(&past_when(f.date, today, true)),
                    self.venue,
                    r.words()
                ),
                None => format!(
                    "{} was run at {} {}; I don't have the result on file.",
                    capitalise(&name),
                    self.venue,
                    past_when(f.date, today, true)
                ),
            },
            "today" => format!(
                "Today is {} day at {}: {}.{}",
                f.name,
                self.venue,
                f.tagline,
                self.field_words()
            ),
            _ => format!(
                "{} is at {} {}: {}.{}",
                capitalise(&name),
                self.venue,
                ahead_when(f.date, today),
                f.tagline,
                self.field_words()
            ),
        }
    }

    fn json(&self, today: NaiveDate) -> serde_json::Value {
        let f = &self.feature;
        json!({
            "name": f.name,
            "date": f.date,
            "venue": self.venue,
            "grade": f.grade,
            "distance_m": f.distance_m,
            "blurb": f.blurb,
            "tagline": f.tagline,
            "status": self.status,
            "race_number": self.race_number,
            "runners": self.runners,
            "start_local": self.start_local,
            "jump": self.jump.as_ref().map(Start::json),
            "days_to_go": (self.status != "run").then(|| self.days_to_go(today)),
            "result": self.result.as_ref().map(|r| json!({
                "winner": r.winner,
                "jockey": r.jockey,
                "margin_lengths": r.margin_lengths,
                "margin": r.margin_lengths.map(margin_words),
            })),
            "said": self.said(today),
        })
    }
}

impl Trackside {
    /// Where `feature` stands on `today`, from the store: run (with its result when one is on
    /// file), today, ahead with its field out, or ahead with no field yet.
    async fn feature_status(
        &self,
        feature: trackside_core::FeatureRace,
        today: NaiveDate,
        home: Option<&str>,
    ) -> Result<FeatureStatus, McpError> {
        let meetings = self.store.meetings(feature.date).await.map_err(internal)?;
        let found = feature_card(&meetings, &feature);
        let result = match found {
            Some((m, card)) if feature.date <= today => self
                .store
                .race_result(feature.date, &m.venue, card.race_number)
                .await
                .map_err(internal)?
                .as_ref()
                .and_then(FeatureResult::from_result),
            _ => None,
        };
        let days = (feature.date - today).num_days();
        let field = found.filter(|(_, c)| c.runners.iter().any(|r| !r.scratched));
        let status = if result.is_some() || days < 0 {
            "run"
        } else if days == 0 {
            "today"
        } else if field.is_some() {
            "ahead"
        } else {
            "fields_not_out"
        };
        let live = field.filter(|_| status != "run");
        Ok(FeatureStatus {
            venue: found
                .map(|(m, _)| m.venue.clone())
                .unwrap_or_else(|| feature.venue.to_string()),
            race_number: found.map(|(_, c)| c.race_number),
            runners: live.map(|(_, c)| c.runners.iter().filter(|r| !r.scratched).count()),
            start_local: live
                .map(|(_, c)| c.start_local.clone())
                .filter(|s| !s.trim().is_empty()),
            jump: live.map(|(m, c)| self.start(feature.date, &c.start_local, &m.state, home)),
            status,
            result,
            feature,
        })
    }
}

/// The feature's race on its day's cards: by name (exactly, then within a sponsored name at
/// its venue), else the one Group 1 at its venue over its distance.
fn feature_card<'a>(
    meetings: &'a [Meeting],
    f: &trackside_core::FeatureRace,
) -> Option<(&'a Meeting, &'a RaceCard)> {
    let name = norm(f.name);
    let races = || {
        meetings
            .iter()
            .flat_map(|m| m.races.iter().map(move |r| (m, r)))
    };
    let at_venue = || races().filter(|(m, _)| venue_matches(&m.venue, f.venue));
    if let Some(found) = races().find(|(_, r)| norm(&r.name) == name) {
        return Some(found);
    }
    if let Some(found) =
        at_venue().find(|(_, r)| format!(" {} ", norm(&r.name)).contains(&format!(" {name} ")))
    {
        return Some(found);
    }
    let group_one = |grade: &str| matches!(norm(grade).replace(' ', "").as_str(), "group1" | "g1");
    let mut by_shape =
        at_venue().filter(|(_, r)| r.distance_m == Some(f.distance_m) && group_one(&r.grade));
    match (by_shape.next(), by_shape.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// A winning margin as a race caller says it: "a nose", "a head", "half a length", "2.3
/// lengths".
fn margin_words(m: f64) -> String {
    match m {
        m if m < 0.08 => "a nose".into(),
        m if m < 0.15 => "a short head".into(),
        m if m < 0.25 => "a head".into(),
        m if m < 0.4 => "a neck".into(),
        m if m < 0.65 => "half a length".into(),
        m if m < 0.9 => "three-quarters of a length".into(),
        m if m < 1.15 => "a length".into(),
        m => {
            let s = format!("{m:.1}");
            format!("{} lengths", s.trim_end_matches(".0"))
        }
    }
}

/// A day still to come: "tomorrow, Saturday 24 October", "this Saturday, 24 October, 4 days
/// away", "on Tuesday 3 November, 20 days away".
fn ahead_when(date: NaiveDate, today: NaiveDate) -> String {
    match (date - today).num_days() {
        i64::MIN..=0 => "today".into(),
        1 => format!("tomorrow, {}", date.format("%A %-d %B")),
        n @ 2..=6 => format!(
            "this {}, {}, {n} days away",
            date.format("%A"),
            date.format("%-d %B")
        ),
        n => format!("on {}, {n} days away", date.format("%A %-d %B")),
    }
}

/// A day gone by: "earlier today", "yesterday", "last Saturday" (with its date when `full`),
/// "on Tuesday 3 November".
fn past_when(date: NaiveDate, today: NaiveDate, full: bool) -> String {
    let dated = |s: String| {
        if full {
            format!("{s}, {}", date.format("%-d %B"))
        } else {
            s
        }
    };
    match (today - date).num_days() {
        i64::MIN..=0 => "earlier today".into(),
        1 => dated("yesterday".into()),
        2..=7 => dated(format!("last {}", date.format("%A"))),
        _ => format!("on {}", date.format("%A %-d %B")),
    }
}

/// "One", "Two" ... for a count said at the start of a sentence.
fn count_word(n: usize) -> String {
    match n {
        1 => "One".into(),
        2 => "Two".into(),
        3 => "Three".into(),
        4 => "Four".into(),
        5 => "Five".into(),
        6 => "Six".into(),
        n => n.to_string(),
    }
}

/// The whole guide in one breath: what is next, the latest result, and how many follow. Once
/// every feature has been run, each reads in the past tense with its winner.
fn carnival_words(statuses: &[FeatureStatus], today: NaiveDate) -> String {
    let source = if statuses.iter().any(FeatureStatus::quotes_store) {
        format!(" Source: {SOURCE_RACING_AUSTRALIA}.")
    } else {
        String::new()
    };
    let Some(next_at) = statuses.iter().position(|s| s.status != "run") else {
        let won: Vec<String> = statuses
            .iter()
            .filter_map(|s| s.result.as_ref().map(|r| (s, r)))
            .enumerate()
            .map(|(i, (s, r))| {
                let verb = if i == 0 { " went" } else { "" };
                format!("{}{verb} to {}", with_article(s.feature.name), r.winner)
            })
            .collect();
        let missing: Vec<String> = statuses
            .iter()
            .filter(|s| s.result.is_none())
            .map(|s| with_article(s.feature.name))
            .collect();
        let mut text = format!(
            "The 2026 Spring Carnival is over: all {} feature races have been run.",
            statuses.len()
        );
        if !won.is_empty() {
            text.push_str(&format!(" {}.", capitalise(&spoken_list(&won))));
        }
        match missing.as_slice() {
            [] => {}
            [one] => text.push_str(&format!(
                " {} was run, but I don't have its result on file.",
                capitalise(one)
            )),
            _ => text.push_str(&format!(
                " I don't have the results of {} on file.",
                spoken_list(&missing)
            )),
        }
        text.push_str(if won.is_empty() {
            " Ask me about any by name."
        } else {
            " Ask me about any by name for the rider and margin."
        });
        return format!("{text}{source}");
    };
    let next = &statuses[next_at];
    let f = &next.feature;
    let latest = statuses[..next_at].iter().rev().find(|s| s.status == "run");
    let lead = if next.status == "today" {
        format!("Today is {} day at {}", f.name, next.venue)
    } else {
        let opener = if latest.is_some() {
            "Next up is"
        } else {
            "The carnival opens with"
        };
        format!(
            "{opener} {} at {} {}",
            with_article(f.name),
            next.venue,
            ahead_when(f.date, today)
        )
    };
    let recent = latest
        .map(|s| match &s.result {
            Some(r) => format!(
                " {} {} was {}.",
                capitalise(&past_when(s.feature.date, today, false)),
                with_article(s.feature.name),
                r.words()
            ),
            None => format!(
                " {} was run {}; I don't have the result on file.",
                capitalise(&with_article(s.feature.name)),
                past_when(s.feature.date, today, false)
            ),
        })
        .unwrap_or_default();
    let after = &statuses[next_at + 1..];
    let follow = |ask: bool| match after {
        [] => " It's the last feature of the carnival.".to_string(),
        [last] => format!(
            " After that, {} on {} closes the carnival.",
            with_article(last.feature.name),
            last.feature.date.format("%A %-d %B")
        ),
        [.., last] => format!(
            " {} more follow, through to {} on {}{}.",
            count_word(after.len()),
            with_article(last.feature.name),
            last.feature.date.format("%A %-d %B"),
            if ask {
                "; ask me about any by name"
            } else {
                ""
            }
        ),
    };
    let field = next.field_words();
    let full = format!(
        "{lead}: {}.{field}{recent}{}{source}",
        f.tagline,
        follow(true)
    );
    if word_count(&full) <= 80 {
        return full;
    }
    // A long winner's name and a listener in another time zone can run past one breath: the
    // tagline and the invitation go first.
    format!("{lead}.{field}{recent}{}{source}", follow(false))
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// `reply` with `note` ("Taking Warwick as Warwick Farm. ") said before it.
fn with_note(note: &str, mut reply: CallToolResult) -> CallToolResult {
    if let Some(ContentBlock::Text(t)) = reply.content.first_mut() {
        t.text.insert_str(0, note);
    }
    reply
}

/// Which race tool is asking, for the words of an answer from the carnival guide.
#[derive(Clone, Copy, PartialEq)]
enum RaceTool {
    Card,
    Explain,
    Result,
}

/// Where a race is, found by its name or by venue and number, and what to say first about
/// how it was taken ("Taking Warwick as Warwick Farm. "); empty when it was said as named.
struct RaceAt {
    date: NaiveDate,
    venue: String,
    race_number: u32,
    note: String,
}

/// How many days back a race name is looked for in the fields; ahead, the fields reach
/// `LOOK_AHEAD_DAYS`.
const NAME_LOOK_BACK_DAYS: u64 = 7;

/// A race a name might mean: one in the published fields, or a carnival feature whose
/// field isn't out (or isn't held).
enum RaceCandidate {
    Field(RaceRef),
    Carnival(FeatureRace, NameMatch),
}

impl RaceCandidate {
    fn name(&self) -> &str {
        match self {
            Self::Field(r) => &r.name,
            Self::Carnival(f, _) => f.name,
        }
    }

    fn date(&self) -> NaiveDate {
        match self {
            Self::Field(r) => r.date,
            Self::Carnival(f, _) => f.date,
        }
    }

    fn venue(&self) -> &str {
        match self {
            Self::Field(r) => &r.venue,
            Self::Carnival(f, _) => f.venue,
        }
    }

    fn matched(&self) -> NameMatch {
        match self {
            Self::Field(r) => r.matched,
            Self::Carnival(_, m) => *m,
        }
    }

    fn graded(&self) -> bool {
        match self {
            Self::Field(r) => !r.grade.trim().is_empty(),
            Self::Carnival(f, _) => !f.grade.is_empty(),
        }
    }

    /// The closest name, then a graded race, then the day nearest `around`; a field before
    /// the guide's entry for the same race.
    fn rank(&self, around: NaiveDate) -> (NameMatch, bool, i64, bool) {
        (
            self.matched(),
            !self.graded(),
            (self.date() - around).num_days().abs(),
            matches!(self, Self::Carnival(..)),
        )
    }

    /// " at Caulfield on Saturday 17 October": what tells two races apart in "did you mean".
    fn where_and_when(&self) -> String {
        format!(
            " at {} on {}",
            self.venue(),
            self.date().format("%A %-d %B")
        )
    }
}

/// What a race name came to.
enum RaceLookup {
    At(RaceAt),
    /// An answer in itself: a question, a "did you mean", or a carnival race without a field.
    Reply(CallToolResult),
    /// No race by that name; the answer that says so.
    Missing(CallToolResult),
}

impl Trackside {
    /// The race a race tool was asked about: by name when `race` is given (narrowed by
    /// venue and date when those are given too), otherwise by venue and race number. The
    /// inner error is an answer to give instead; the outer one a call that names no race.
    async fn locate_race(
        &self,
        args: &RaceArgs,
        tool: RaceTool,
    ) -> Result<Result<RaceAt, CallToolResult>, McpError> {
        let asked_date = parse_opt_date(&args.date)?;
        let venue = args
            .venue
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let heard = args
            .race
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        let mut missing = None;
        if let Some(heard) = heard {
            match self.resolve_race(heard, asked_date, venue, tool).await? {
                RaceLookup::At(at) => return Ok(Ok(at)),
                RaceLookup::Reply(reply) => return Ok(Err(reply)),
                RaceLookup::Missing(reply) => missing = Some(reply),
            }
        }
        let (Some(venue), Some(race_number)) = (venue, args.race_number) else {
            // A name that found nothing, with no venue and number to fall back on.
            if let Some(reply) = missing {
                return Ok(Err(reply));
            }
            return Err(McpError::invalid_params(
                "Say which race: give its name as race (for example \"Caulfield Cup\"), or give venue and race_number.",
                None,
            ));
        };
        let date = asked_date.unwrap_or_else(|| self.today());
        let resolved = match self.resolve_venue(date, venue).await {
            Ok(v) => v,
            Err(ask) => return Ok(Err(ask)),
        };
        let note = if norm(&resolved) == norm(venue) {
            String::new()
        } else {
            format!("Taking {venue} as {resolved}. ")
        };
        Ok(Ok(RaceAt {
            date,
            venue: resolved,
            race_number,
            note,
        }))
    }

    /// A race by the name a listener said, across the fields from a week ago to as far
    /// ahead as fields go (or on `date` alone), and the carnival guide. An exact name wins,
    /// then a graded race, then the day nearest today; two different races that fit as well
    /// as each other are asked about, and generic words alone ("the Cup") are a question.
    async fn resolve_race(
        &self,
        heard: &str,
        date: Option<NaiveDate>,
        venue: Option<&str>,
        tool: RaceTool,
    ) -> Result<RaceLookup, McpError> {
        let today = self.today();
        if names::is_generic_race_name(heard) && date.is_none() && venue.is_none() {
            let word = horse_key(heard)
                .split_whitespace()
                .rfind(|w| !matches!(*w, "the" | "a"))
                .unwrap_or("race")
                .to_string();
            return Ok(RaceLookup::Reply(answer(
                format!("Which {word}? Say the venue or the day."),
                json!({ "found": false, "race": heard, "question": "which_race" }),
            )));
        }
        let around = date.unwrap_or(today);
        let window = match date {
            Some(d) => d..=d,
            None => {
                today
                    .checked_sub_days(Days::new(NAME_LOOK_BACK_DAYS))
                    .unwrap_or(today)
                    ..=today
                        .checked_add_days(Days::new(LOOK_AHEAD_DAYS))
                        .unwrap_or(today)
            }
        };
        let fields = self
            .store
            .find_races(heard, around, window.clone())
            .await
            .map_err(internal)?;
        let mut found: Vec<RaceCandidate> = Vec::new();
        for f in spring_carnival_2026() {
            if date.is_some_and(|d| d != f.date) {
                continue;
            }
            let Some(m) = names::race_name_match(heard, f.name) else {
                continue;
            };
            // The guide steps aside once the race's field is out.
            let in_fields = fields.iter().any(|r| {
                r.date == f.date
                    && venue_matches(&r.venue, f.venue)
                    && names::race_name_match(f.name, &r.name).is_some()
            });
            if !in_fields {
                found.push(RaceCandidate::Carnival(f, m));
            }
        }
        found.extend(fields.into_iter().map(RaceCandidate::Field));
        if let Some(v) = venue {
            found.retain(|c| {
                venue_matches(c.venue(), v) || names::sounds_like(v, c.venue()).is_some()
            });
        }
        found.sort_by_key(|c| c.rank(around));
        let Some(best) = found.first().map(RaceCandidate::matched) else {
            let spoken = match date {
                Some(d) => format!(
                    "I can't find a race called {heard} on {}. Say the venue and race number, or another day.",
                    d.format("%A %-d %B")
                ),
                None => format!(
                    "I can't find a race called {heard} in this week's fields or the carnival guide. Say the venue and race number, or the day it's on."
                ),
            };
            return Ok(RaceLookup::Missing(answer(
                spoken,
                json!({ "found": false, "race": heard, "searched": { "from": window.start(), "to": window.end() } }),
            )));
        };
        found.retain(|c| c.matched() == best);
        // One candidate per race name, best placed first.
        let mut distinct: Vec<&RaceCandidate> = Vec::new();
        for c in &found {
            if !distinct.iter().any(|d| norm(d.name()) == norm(c.name())) {
                distinct.push(c);
            }
        }
        let graded: Vec<&RaceCandidate> = distinct.iter().copied().filter(|c| c.graded()).collect();
        let chosen = match (distinct.as_slice(), graded.as_slice()) {
            ([one], _) => *one,
            (_, [one]) => *one,
            (many, _) => {
                let shown = &many[..many.len().min(3)];
                let options: Vec<(String, String)> = shown
                    .iter()
                    .map(|c| (with_article(c.name()), c.where_and_when()))
                    .collect();
                let mut reply = did_you_mean_with(heard, &options);
                if let Some(s) = reply.structured_content.as_mut() {
                    s["did_you_mean"] = json!(shown.iter().map(|c| c.name()).collect::<Vec<_>>());
                    s["candidates"] = json!(shown
                        .iter()
                        .map(|c| json!({
                            "name": c.name(),
                            "venue": c.venue(),
                            "date": c.date(),
                            "race_number": match c {
                                RaceCandidate::Field(r) => Some(r.race_number),
                                RaceCandidate::Carnival(..) => None,
                            },
                        }))
                        .collect::<Vec<_>>());
                }
                return Ok(RaceLookup::Reply(reply));
            }
        };
        match chosen {
            RaceCandidate::Field(r) => {
                let note = if r.matched.is_exact() {
                    String::new()
                } else {
                    format!(
                        "Taking {heard} as {}{}. ",
                        with_article(&r.name),
                        chosen.where_and_when()
                    )
                };
                Ok(RaceLookup::At(RaceAt {
                    date: r.date,
                    venue: r.venue.clone(),
                    race_number: r.race_number,
                    note,
                }))
            }
            RaceCandidate::Carnival(f, _) => {
                Ok(RaceLookup::Reply(self.carnival_answer(heard, f, tool)))
            }
        }
    }

    /// A carnival feature asked about before its field is out (or one this store doesn't
    /// hold), answered from the guide: when and where, and when the field comes.
    fn carnival_answer(&self, heard: &str, f: &FeatureRace, tool: RaceTool) -> CallToolResult {
        let today = self.today();
        let name = capitalise(&with_article(f.name));
        let day = f.date.format("%A %-d %B");
        let spoken = if f.date > today {
            let fields = if f.date.weekday() == chrono::Weekday::Sat {
                "on the Wednesday before"
            } else {
                "two to three days before"
            };
            match tool {
                RaceTool::Result => {
                    format!(
                        "{name} is on {day} at {}, so it hasn't been run yet.",
                        f.venue
                    )
                }
                RaceTool::Card => {
                    format!(
                        "{name} is on {day} at {}; fields come out {fields}.",
                        f.venue
                    )
                }
                RaceTool::Explain => format!(
                    "{name} is on {day} at {}; fields come out {fields}. {}",
                    f.venue, f.blurb
                ),
            }
        } else {
            let when = if f.date == today {
                "is today".to_string()
            } else {
                format!("was run on {day}")
            };
            let held = match tool {
                RaceTool::Result => "its result",
                RaceTool::Card | RaceTool::Explain => "its field",
            };
            format!(
                "{name} {when} at {}, but I don't have {held} on file.",
                f.venue
            )
        };
        answer(
            spoken,
            json!({ "found": false, "race": heard, "carnival": f }),
        )
    }

    /// "Did you mean" for horses that sound alike, each with its trainer when the store
    /// knows every one and they differ: "Grey Area, trained by C. Trainer, or Gray Area,
    /// trained by M. Yard".
    async fn horse_did_you_mean(&self, heard: &str, horses: &[String]) -> CallToolResult {
        let mut trainers = Vec::with_capacity(horses.len());
        for h in horses {
            let trainer = self
                .store
                .horse_form(h)
                .await
                .ok()
                .flatten()
                .map(|f| f.trainer.trim().to_string())
                .unwrap_or_default();
            trainers.push(trainer);
        }
        let tells_apart = trainers.iter().all(|t| !t.is_empty())
            && distinct(trainers.iter().map(String::as_str)).len() == trainers.len();
        let options: Vec<(String, String)> = horses
            .iter()
            .zip(&trainers)
            .map(|(h, t)| {
                let fact = if tells_apart {
                    format!(", trained by {t}")
                } else {
                    String::new()
                };
                (h.clone(), fact)
            })
            .collect();
        did_you_mean_with(heard, &options)
    }
}

/// The horses among `names` that the listener follows, spelt as their stable spells them and
/// in the order they were followed. Empty without a stable.
fn followed_in<'a>(profile: &Profile, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let keys: Vec<String> = names.into_iter().map(horse_key).collect();
    profile
        .horses
        .iter()
        .filter(|h| keys.contains(&horse_key(h)))
        .cloned()
        .collect()
}

/// " Your horse Sample Stayer is in it, barrier 4, with J. Example up." for the followed
/// horses in a field, then which of them have been scratched; empty when none are in it.
fn your_runners(card: &RaceCard, following: &[String]) -> String {
    let mut running = Vec::new();
    let mut scratched = Vec::new();
    for h in following {
        match card
            .runners
            .iter()
            .find(|r| horse_key(&r.horse) == horse_key(h))
        {
            Some(r) if r.scratched => scratched.push(h.clone()),
            Some(r) => running.push((h.clone(), r)),
            None => {}
        }
    }
    let details = |r: &trackside_core::Runner, joiner: &str| {
        let mut parts = Vec::new();
        if let Some(b) = r.barrier {
            parts.push(format!("barrier {b}"));
        }
        if !r.jockey.trim().is_empty() {
            parts.push(format!("with {} up", r.jockey.trim()));
        }
        parts.join(joiner)
    };
    let mut out = match running.as_slice() {
        [] => String::new(),
        [(h, r)] => match details(r, ", ") {
            d if d.is_empty() => format!(" Your horse {h} is in it."),
            d => format!(" Your horse {h} is in it, {d}."),
        },
        many => {
            let names: Vec<String> = many.iter().map(|(h, _)| h.clone()).collect();
            let each: Vec<String> = many
                .iter()
                .map(|(h, r)| match details(r, " ") {
                    d if d.is_empty() => h.clone(),
                    d => format!("{h} from {d}"),
                })
                .collect();
            format!(
                " Your horses {} are in it, {}.",
                spoken_list(&names),
                spoken_list(&each)
            )
        }
    };
    match scratched.as_slice() {
        [] => {}
        [one] => out.push_str(&format!(" Your horse {one} has been scratched.")),
        many => out.push_str(&format!(
            " Your horses {} have been scratched.",
            spoken_list(many)
        )),
    }
    out
}

/// How the listener's horses finished: " Your horse Demo Miler ran 2nd, 0.8 lengths from the
/// winner, and came from 9th at the 800." Each entry is (name in the stable, finishing
/// position, lengths from the winner, position at the 800). Empty when none ran.
fn your_finishers(yours: &[(String, u32, Option<f64>, Option<u32>)]) -> String {
    let clause = |position: u32, margin: Option<f64>, at_800: Option<u32>| {
        let mut words = if position == 1 {
            "won it".to_string()
        } else {
            format!("ran {position}{}", ordinal(position))
        };
        if let Some(m) = margin.filter(|_| position > 1) {
            words.push_str(&format!(", {m:.1} lengths from the winner"));
        }
        match at_800.filter(|&p| p > 0) {
            Some(1) => words.push_str(", and led at the 800"),
            Some(p) if p > position => words.push_str(&format!(
                ", and came from {} at the 800",
                trackside_core::ordinal(p)
            )),
            Some(p) => words.push_str(&format!(
                ", and was {} at the 800",
                trackside_core::ordinal(p)
            )),
            None => {}
        }
        words
    };
    match yours {
        [] => String::new(),
        [(h, position, margin, at_800)] => {
            format!(" Your horse {h} {}.", clause(*position, *margin, *at_800))
        }
        many => {
            let names: Vec<String> = many.iter().map(|(h, ..)| h.clone()).collect();
            let each: String = many
                .iter()
                .map(|(h, position, margin, at_800)| {
                    format!(" {h} {}.", clause(*position, *margin, *at_800))
                })
                .collect();
            format!(" Your horses {} were in it.{each}", spoken_list(&names))
        }
    }
}
