//! Washington State Ferries' API: fetching (with the access code, which
//! never appears in a log or an error), recording every answer in the raw
//! poll log, and parsing the answers. Or, for working offline and tests,
//! recorded answers played back (see `Replay`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use ferry_model::api::{Destination, LiveVessel, Loading, Notice, Sailing, VesselInfo};
use ferry_model::geo::LonLat;
use ferry_model::terminal::Terminal;
use ferry_model::text::{collapse_spaces, decode_entities, plain_text};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::db;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Where WSF's answers come from.
pub struct Wsf {
    upstream: Upstream,
    /// Where live answers are recorded (raw_polls).
    log: Option<PgPool>,
}

enum Upstream {
    Live {
        http: reqwest::Client,
        base: String,
        key: String,
    },
    Replay(Replay),
}

/// An answer, and when it was fetched (epoch ms).
pub struct Fetched {
    pub fetched_ms: i64,
    pub body: String,
}

impl Wsf {
    /// The live API at `base`, recording every answer in `log`.
    pub fn live(base: String, key: String, log: PgPool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent("ferry-demo")
            .build()?;
        Ok(Self {
            upstream: Upstream::Live { http, base, key },
            log: Some(log),
        })
    }

    /// Answers recorded in `file` (see `Replay`), not recorded again.
    pub fn replay(file: &Path) -> Result<Self> {
        Ok(Self {
            upstream: Upstream::Replay(Replay::load(file)?),
            log: None,
        })
    }

    /// The answer to `path` (such as `vessels/rest/vesselverbose`).
    /// Errors name the path only: the URL carries the access code.
    pub async fn get(&self, path: &str) -> Result<Fetched> {
        let fetched_ms = now_ms();
        match &self.upstream {
            Upstream::Replay(replay) => Ok(Fetched {
                fetched_ms,
                body: replay.next(path)?,
            }),
            Upstream::Live { http, base, key } => {
                let (status, result) = fetch(http, base, key, path).await;
                if let Some(pool) = &self.log {
                    record(pool, path, fetched_ms, status, &result).await;
                }
                Ok(Fetched {
                    fetched_ms,
                    body: result?,
                })
            }
        }
    }

    /// Whether answers are played back rather than fetched.
    pub fn is_replay(&self) -> bool {
        matches!(self.upstream, Upstream::Replay(_))
    }

    pub async fn terminals(&self) -> Result<Vec<Terminal>> {
        parse_terminals(&self.get("terminals/rest/terminallocations").await?.body)
    }

    pub async fn vessels(&self) -> Result<Vec<VesselInfo>> {
        parse_vessels(&self.get("vessels/rest/vesselverbose").await?.body)
    }

    /// When `service`'s (`vessels`, `terminals`) slowly changing data last
    /// changed (epoch ms).
    pub async fn flushed_ms(&self, service: &str) -> Result<i64> {
        let path = format!("{service}/rest/cacheflushdate");
        parse_flush_date(&self.get(&path).await?.body).ok_or_else(|| anyhow!("{path}: no date"))
    }
}

/// One request to WSF: the HTTP status, if any, and the body or why
/// there isn't one.
async fn fetch(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    path: &str,
) -> (Option<u16>, Result<String>) {
    let url =
        match reqwest::Url::parse_with_params(&format!("{base}{path}"), [("apiaccesscode", key)]) {
            Ok(url) => url,
            Err(e) => return (None, Err(anyhow!("{path}: {e}"))),
        };
    let response = match http.get(url).send().await {
        Ok(r) => r,
        Err(e) => return (None, Err(anyhow!("{path}: {}", e.without_url()))),
    };
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| anyhow!("{path}: {}", e.without_url()));
    let result = match body {
        Ok(_) if !status.is_success() => Err(anyhow!("{path}: HTTP {status}")),
        other => other,
    };
    (Some(status.as_u16()), result)
}

/// Adds an answer to the raw poll log; a failure to record is logged, not
/// passed on, so a database hiccup doesn't stop the live data.
async fn record(
    pool: &PgPool,
    path: &str,
    fetched_ms: i64,
    status: Option<u16>,
    result: &Result<String>,
) {
    let (body, error) = match result {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(json) => (Some(json), None),
            Err(_) => (None, Some("the answer wasn't JSON".to_string())),
        },
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    if let Err(e) = db::insert_raw(
        pool,
        path,
        fetched_ms,
        status,
        body.as_ref(),
        error.as_deref(),
    )
    .await
    {
        tracing::warn!(path, "couldn't record the answer: {e:#}");
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// One recorded answer, a line of a replay file (JSON lines).
#[derive(Serialize, Deserialize)]
pub struct Recorded {
    pub path: String,
    pub fetched_ms: i64,
    pub body: serde_json::Value,
}

/// Recorded answers (`ferry-server export-replay`), played back in order
/// for each path, around and around, as if just fetched.
pub struct Replay {
    answers: HashMap<String, (Vec<String>, Mutex<usize>)>,
}

impl Replay {
    pub fn load(file: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut answers: HashMap<String, (Vec<String>, Mutex<usize>)> = HashMap::new();
        for (n, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let r: Recorded =
                serde_json::from_str(line).with_context(|| format!("replay line {}", n + 1))?;
            answers
                .entry(r.path)
                .or_insert_with(|| (Vec::new(), Mutex::new(0)))
                .0
                .push(r.body.to_string());
        }
        Ok(Self { answers })
    }

    fn next(&self, path: &str) -> Result<String> {
        let (bodies, at) = self
            .answers
            .get(path)
            .ok_or_else(|| anyhow!("{path}: nothing recorded"))?;
        let mut at = at.lock().unwrap();
        let body = bodies[*at % bodies.len()].clone();
        *at += 1;
        Ok(body)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfLocation {
    #[serde(rename = "TerminalID")]
    terminal_id: u32,
    terminal_name: String,
    latitude: f64,
    longitude: f64,
}

pub fn parse_terminals(json: &str) -> Result<Vec<Terminal>> {
    let raw: Vec<WsfLocation> = serde_json::from_str(json)?;
    Ok(raw
        .into_iter()
        .map(|t| Terminal {
            id: t.terminal_id,
            name: t.terminal_name.trim().to_string(),
            position: LonLat::new(t.longitude, t.latitude),
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfVessel {
    #[serde(rename = "VesselID")]
    vessel_id: u32,
    vessel_name: String,
    class: Option<WsfClass>,
    length: Option<String>,
    beam: Option<String>,
    /// Standard car spaces: the capacity WSF quotes for a boat.
    reg_deck_space: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfClass {
    class_name: Option<String>,
}

pub fn parse_vessels(json: &str) -> Result<Vec<VesselInfo>> {
    let raw: Vec<WsfVessel> = serde_json::from_str(json)?;
    Ok(raw
        .into_iter()
        .map(|v| VesselInfo {
            id: v.vessel_id,
            name: v.vessel_name.trim().to_string(),
            class: v
                .class
                .and_then(|c| c.class_name)
                .map(|n| n.trim().to_string()),
            length_m: v.length.as_deref().and_then(feet_inches_m),
            beam_m: v.beam.as_deref().and_then(feet_inches_m),
            car_capacity: v.reg_deck_space,
        })
        .collect())
}

/// Meters from WSF's feet and inches, such as `382' 2"`, to 0.1 m (an
/// inch is 2.5 cm).
fn feet_inches_m(s: &str) -> Option<f64> {
    let (feet, rest) = s.split_once('\'')?;
    let inches = rest.trim().trim_end_matches('"').trim();
    let inches: f64 = if inches.is_empty() {
        0.0
    } else {
        inches.parse().ok()?
    };
    let m = (feet.trim().parse::<f64>().ok()? + inches / 12.0) * 0.3048;
    Some((m * 10.0).round() / 10.0)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfPosition {
    #[serde(rename = "VesselID")]
    vessel_id: u32,
    latitude: f64,
    longitude: f64,
    speed: f64,
    heading: f64,
    in_service: bool,
    at_dock: bool,
    #[serde(rename = "DepartingTerminalID")]
    departing_terminal_id: Option<u32>,
    #[serde(rename = "ArrivingTerminalID")]
    arriving_terminal_id: Option<u32>,
    scheduled_departure: Option<String>,
    left_dock: Option<String>,
    eta: Option<String>,
    time_stamp: String,
    vessel_watch_shut_msg: Option<String>,
    vessel_watch_shut_flag: Option<String>,
}

/// A vessellocations answer, each boat without its wake (see `live`).
pub fn parse_positions(json: &str) -> Result<Vec<LiveVessel>> {
    let raw: Vec<WsfPosition> = serde_json::from_str(json)?;
    let date = |s: Option<String>| s.as_deref().and_then(wsf_date_ms);
    Ok(raw
        .into_iter()
        .map(|v| LiveVessel {
            id: v.vessel_id,
            lon: v.longitude,
            lat: v.latitude,
            speed_kn: v.speed,
            heading_deg: v.heading,
            at_dock: v.at_dock,
            in_service: v.in_service,
            departing: v.departing_terminal_id,
            arriving: v.arriving_terminal_id,
            scheduled_departure_ms: date(v.scheduled_departure),
            left_dock_ms: date(v.left_dock),
            eta_ms: date(v.eta),
            reported_ms: wsf_date_ms(&v.time_stamp).unwrap_or(0),
            // The message is only in effect while the flag is up; it
            // lingers, stale, otherwise.
            note: v
                .vessel_watch_shut_msg
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty() && v.vessel_watch_shut_flag.as_deref() == Some("1")),
            wake: Vec::new(),
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfSpace {
    #[serde(rename = "TerminalID")]
    terminal_id: u32,
    departing_spaces: Vec<WsfDeparture>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfDeparture {
    departure: String,
    is_cancelled: bool,
    #[serde(rename = "VesselID")]
    vessel_id: Option<u32>,
    max_space_count: Option<u32>,
    space_for_arrival_terminals: Vec<WsfArrival>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfArrival {
    #[serde(rename = "TerminalID")]
    terminal_id: Option<u32>,
    terminal_name: String,
    #[serde(rename = "ArrivalTerminalIDs", default)]
    arrival_terminal_ids: Vec<u32>,
    display_drive_up_space: bool,
    /// Below zero when more cars are waiting than will fit.
    drive_up_space_count: Option<i32>,
    #[serde(default)]
    display_reservable_space: bool,
    reservable_space_count: Option<i32>,
}

/// A terminalsailingspace answer. Each arrival entry's space is shared by
/// the terminals it lists; its TerminalID can name the departing terminal,
/// and its name reads "Anacortes -> Friday Harbor", so destinations come
/// from its list of terminal ids, less the departing terminal, or failing
/// that, from the name after the arrow (matched against `terminals`).
pub fn parse_sailings(json: &str, terminals: &[Terminal]) -> Result<Vec<Sailing>> {
    let raw: Vec<WsfSpace> = serde_json::from_str(json)?;
    let by_name = |name: &str| {
        let name = name.rsplit("->").next().unwrap_or(name).trim();
        terminals
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .map(|t| t.id)
    };
    let mut out = Vec::new();
    for t in raw {
        for d in t.departing_spaces {
            let Some(departs_ms) = wsf_date_ms(&d.departure) else {
                continue;
            };
            let mut destinations: Vec<Destination> = Vec::new();
            for a in &d.space_for_arrival_terminals {
                let mut ids: Vec<u32> = a
                    .arrival_terminal_ids
                    .iter()
                    .copied()
                    .chain(a.terminal_id)
                    .filter(|&id| id != t.terminal_id)
                    .collect();
                if ids.is_empty() {
                    ids.extend(by_name(&a.terminal_name));
                }
                let count = a.drive_up_space_count.filter(|_| a.display_drive_up_space);
                let reservable = (a.reservable_space_count)
                    .filter(|_| a.display_reservable_space)
                    .map(|n| n.max(0).unsigned_abs());
                for id in ids {
                    if destinations.iter().any(|x| x.terminal == id) {
                        continue;
                    }
                    destinations.push(Destination {
                        terminal: id,
                        drive_up: count.map(|n| n.max(0).unsigned_abs()),
                        oversold: count.is_some_and(|n| n < 0),
                        reservable,
                        oversold_by: count.filter(|&n| n < 0).map(i32::unsigned_abs),
                    });
                }
            }
            out.push(Sailing {
                terminal: t.terminal_id,
                departs_ms,
                vessel: d.vessel_id,
                capacity: d.max_space_count,
                cancelled: d.is_cancelled,
                destinations,
            });
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfTerminalBulletins {
    #[serde(rename = "TerminalID")]
    terminal_id: u32,
    bulletins: Vec<WsfBulletin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfBulletin {
    bulletin_title: String,
    bulletin_text: Option<String>,
    bulletin_last_updated: Option<String>,
}

/// A terminalbulletins answer, as notices: WSF lists a notice once per
/// terminal it's posted at, so they're merged by title (the id is made
/// from it, so it holds when the text is edited), newest first.
pub fn parse_notices(json: &str) -> Result<Vec<Notice>> {
    let raw: Vec<WsfTerminalBulletins> = serde_json::from_str(json)?;
    let mut notices: Vec<Notice> = Vec::new();
    for t in raw {
        for b in t.bulletins {
            let title = collapse_spaces(&decode_entities(&b.bulletin_title));
            let updated_ms = b.bulletin_last_updated.as_deref().and_then(wsf_date_ms);
            match notices.iter_mut().find(|n| n.title == title) {
                Some(n) => {
                    if !n.terminals.contains(&t.terminal_id) {
                        n.terminals.push(t.terminal_id);
                    }
                    // The latest edit's text.
                    if updated_ms > n.updated_ms {
                        n.updated_ms = updated_ms;
                        n.text = plain_text(b.bulletin_text.as_deref().unwrap_or(""));
                    }
                }
                None => notices.push(Notice {
                    id: notice_id(&title),
                    terminals: vec![t.terminal_id],
                    text: plain_text(b.bulletin_text.as_deref().unwrap_or("")),
                    title,
                    updated_ms,
                }),
            }
        }
    }
    for n in &mut notices {
        n.terminals.sort_unstable();
    }
    notices.sort_by_key(|n| std::cmp::Reverse(n.updated_ms));
    Ok(notices)
}

/// A short, stable id for a notice, from its title (FNV-1a, so it's the
/// same in every build and run).
fn notice_id(title: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in title.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("n-{hash:016x}")
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfScheduleRoute {
    #[serde(rename = "RouteID")]
    route_id: u32,
}

/// A schedule `routes/{date}` answer: the ids of the routes sailing that
/// day.
pub fn parse_schedule_routes(json: &str) -> Result<Vec<u32>> {
    let raw: Vec<WsfScheduleRoute> = serde_json::from_str(json)?;
    Ok(raw.into_iter().map(|r| r.route_id).collect())
}

/// A route's schedule for a day as WSF gives it: each pair of terminals
/// with its sailings.
#[derive(Debug, PartialEq)]
pub struct DaySchedule {
    pub season: String,
    pub pdf_url: Option<String>,
    pub pairs: Vec<PairSailings>,
}

#[derive(Debug, PartialEq)]
pub struct PairSailings {
    pub from: u32,
    pub to: u32,
    /// Footnotes, plain text; sailings point at them by index.
    pub notes: Vec<String>,
    pub sailings: Vec<PairSailing>,
}

#[derive(Debug, PartialEq)]
pub struct PairSailing {
    pub departs_ms: i64,
    pub arrives_ms: Option<i64>,
    pub vessel_id: Option<u32>,
    pub vessel: String,
    pub loading: Loading,
    pub notes: Vec<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfSchedule {
    schedule_name: Option<String>,
    #[serde(rename = "SchedulePDFUrl")]
    schedule_pdf_url: Option<String>,
    terminal_combos: Vec<WsfCombo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfCombo {
    #[serde(rename = "DepartingTerminalID")]
    departing_terminal_id: u32,
    #[serde(rename = "ArrivingTerminalID")]
    arriving_terminal_id: u32,
    #[serde(default)]
    annotations: Vec<String>,
    times: Vec<WsfTime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsfTime {
    departing_time: String,
    arriving_time: Option<String>,
    /// 1 passengers, 2 vehicles, 3 both.
    loading_rule: Option<u8>,
    #[serde(rename = "VesselID")]
    vessel_id: Option<u32>,
    vessel_name: Option<String>,
    #[serde(default)]
    annotation_indexes: Vec<usize>,
}

/// A schedule `schedule/{date}/{route}` answer.
pub fn parse_schedule(json: &str) -> Result<DaySchedule> {
    let raw: WsfSchedule = serde_json::from_str(json)?;
    let text = |s: &str| collapse_spaces(&plain_text(s));
    Ok(DaySchedule {
        season: raw.schedule_name.map_or_else(String::new, |n| text(&n)),
        pdf_url: raw
            .schedule_pdf_url
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty()),
        pairs: raw
            .terminal_combos
            .into_iter()
            .map(|c| PairSailings {
                from: c.departing_terminal_id,
                to: c.arriving_terminal_id,
                notes: c.annotations.iter().map(|a| text(a)).collect(),
                sailings: c
                    .times
                    .into_iter()
                    .filter_map(|t| {
                        Some(PairSailing {
                            departs_ms: wsf_date_ms(&t.departing_time)?,
                            arrives_ms: t.arriving_time.as_deref().and_then(wsf_date_ms),
                            vessel_id: t.vessel_id.filter(|&id| id > 0),
                            vessel: t.vessel_name.map_or_else(String::new, |n| text(&n)),
                            loading: match t.loading_rule {
                                Some(1) => Loading::Passengers,
                                Some(2) => Loading::Vehicles,
                                _ => Loading::Both,
                            },
                            notes: t.annotation_indexes,
                        })
                    })
                    .collect(),
            })
            .collect(),
    })
}

/// A cacheflushdate answer: a JSON string holding a WSF date.
pub fn parse_flush_date(json: &str) -> Option<i64> {
    wsf_date_ms(&serde_json::from_str::<String>(json).ok()?)
}

/// Milliseconds since the epoch from WSF's `/Date(1793297887000-0700)/`
/// (the offset only says which zone it was written in).
pub fn wsf_date_ms(s: &str) -> Option<i64> {
    let digits = s.strip_prefix("/Date(")?;
    let end = digits
        .char_indices()
        .skip(1)
        .find(|&(_, c)| !c.is_ascii_digit())
        .map_or(digits.len(), |(i, _)| i);
    digits[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_terminals_trimmed() {
        let t = parse_terminals(
            r#"[{"TerminalID":11,"TerminalName":"Coupeville ","TerminalAbbrev":"KEY",
                 "Latitude":48.159,"Longitude":-122.672,"SortSeq":30}]"#,
        )
        .unwrap();
        assert_eq!(t[0].id, 11);
        assert_eq!(t[0].name, "Coupeville");
        assert_eq!(t[0].position, LonLat::new(-122.672, 48.159));
        assert!(parse_terminals(r#"{"Message":"bad code"}"#).is_err());
    }

    #[test]
    fn parses_vessel_details() {
        let v = parse_vessels(
            r#"[{"VesselID":1,"VesselName":"Cathlamet","Class":{"ClassID":10,
                 "ClassName":"Issaquah 130"},"Length":"328' 2\"","Beam":"78' 8\"",
                 "RegDeckSpace":124,"TallDeckSpace":25},
                {"VesselID":2,"VesselName":"Chelan","Class":null,"Length":null,
                 "Beam":"78'","RegDeckSpace":null}]"#,
        )
        .unwrap();
        assert_eq!(v[0].class.as_deref(), Some("Issaquah 130"));
        assert_eq!((v[0].length_m, v[0].beam_m), (Some(100.0), Some(24.0)));
        assert_eq!(v[0].car_capacity, Some(124));
        assert_eq!((v[1].class.as_deref(), v[1].length_m), (None, None));
        assert_eq!(v[1].beam_m, Some(23.8));
    }

    #[test]
    fn parses_schedules() {
        assert_eq!(
            parse_schedule_routes(r#"[{"RouteID":13,"RouteAbbrev":"f-v-s","Description":"x"}]"#)
                .unwrap(),
            [13]
        );
        let day = parse_schedule(
            r#"{"ScheduleID":201,"ScheduleName":"Fall 2026","ScheduleSeason":3,
                "SchedulePDFUrl":"https://wsdot.wa.gov/fall.pdf",
                "ScheduleStart":"/Date(1790665200000-0700)/","AllRoutes":[13],
                "TerminalCombos":[{"DepartingTerminalID":9,"DepartingTerminalName":"Fauntleroy",
                  "ArrivingTerminalID":22,"ArrivingTerminalName":"Vashon Island",
                  "SailingNotes":"","AnnotationsIVR":[],
                  "Annotations":["Via Southworth, <b>crossing time</b> 45 minutes"],
                  "Times":[{"DepartingTime":"/Date(1793404200000-0700)/",
                    "ArrivingTime":"/Date(1793406900000-0700)/","LoadingRule":3,
                    "VesselID":6,"VesselName":"Kitsap","VesselHandicapAccessible":true,
                    "VesselPositionNum":1,"Routes":[13],"AnnotationIndexes":[0]},
                   {"DepartingTime":"/Date(1793406000000-0700)/","ArrivingTime":null,
                    "LoadingRule":1,"VesselID":0,"VesselName":"Cathlamet ",
                    "VesselHandicapAccessible":true,"VesselPositionNum":2,"Routes":[13],
                    "AnnotationIndexes":[]}]}]}"#,
        )
        .unwrap();
        assert_eq!(day.season, "Fall 2026");
        assert_eq!(
            day.pdf_url.as_deref(),
            Some("https://wsdot.wa.gov/fall.pdf")
        );
        let pair = &day.pairs[0];
        assert_eq!((pair.from, pair.to), (9, 22));
        assert_eq!(pair.notes, ["Via Southworth, crossing time 45 minutes"]);
        assert_eq!(pair.sailings[0].vessel_id, Some(6));
        assert_eq!(pair.sailings[0].arrives_ms, Some(1_793_406_900_000));
        assert_eq!(pair.sailings[0].notes, [0]);
        assert_eq!(pair.sailings[1].vessel, "Cathlamet");
        assert_eq!(pair.sailings[1].vessel_id, None);
        assert_eq!(pair.sailings[1].loading, Loading::Passengers);
        assert!(parse_schedule(r#"{"Message":"bad code"}"#).is_err());
    }

    #[test]
    fn parses_dates() {
        assert_eq!(
            parse_flush_date(r#""\/Date(1793327076853-0700)\/""#),
            Some(1_793_327_076_853)
        );
        assert_eq!(wsf_date_ms("/Date(-1000)/"), Some(-1000));
        assert_eq!(parse_flush_date("null"), None);
        assert_eq!(wsf_date_ms("yesterday"), None);
    }

    fn terminal(id: u32, name: &str) -> Terminal {
        Terminal {
            id,
            name: name.to_string(),
            position: LonLat::new(-122.6, 48.5),
        }
    }

    #[test]
    fn parses_positions_with_notes_only_in_effect() {
        let v = parse_positions(
            r#"[{"VesselID":28,"VesselName":"Sealth","DepartingTerminalID":7,
                 "ArrivingTerminalID":4,"Latitude":47.6037,"Longitude":-122.3814,
                 "Speed":17.3,"Heading":256,"InService":true,"AtDock":false,
                 "LeftDock":"\/Date(1793297484000-0700)\/","Eta":"\/Date(1793300760000-0700)\/",
                 "ScheduledDeparture":"\/Date(1793297400000-0700)\/",
                 "TimeStamp":"\/Date(1793297887000-0700)\/",
                 "VesselWatchShutMsg":" Vessel information unavailable","VesselWatchShutFlag":"0"},
                {"VesselID":15,"Latitude":47.62,"Longitude":-122.51,"Speed":0,"Heading":291,
                 "InService":false,"AtDock":true,"DepartingTerminalID":null,"LeftDock":null,
                 "Eta":null,"ScheduledDeparture":null,"TimeStamp":"\/Date(1793297830000-0700)\/",
                 "VesselWatchShutMsg":"Out of service, lack of crew ","VesselWatchShutFlag":"1"}]"#,
        )
        .unwrap();
        assert_eq!((v[0].departing, v[0].arriving), (Some(7), Some(4)));
        assert_eq!(v[0].left_dock_ms, Some(1_793_297_484_000));
        assert_eq!(v[0].reported_ms, 1_793_297_887_000);
        assert_eq!(v[0].note, None);
        assert_eq!(v[1].note.as_deref(), Some("Out of service, lack of crew"));
        assert_eq!((v[1].departing, v[1].eta_ms), (None, None));
    }

    #[test]
    fn parses_space_per_destination() {
        let terminals = [
            terminal(1, "Anacortes"),
            terminal(10, "Friday Harbor"),
            terminal(13, "Lopez Island"),
            terminal(15, "Orcas Island"),
        ];
        let s = parse_sailings(
            r#"[{"TerminalID":1,"TerminalName":"Anacortes","DepartingSpaces":[
                 {"Departure":"\/Date(1793306100000-0700)\/","IsCancelled":false,
                  "VesselID":69,"VesselName":"Samish","MaxSpaceCount":141,
                  "SpaceForArrivalTerminals":[
                    {"TerminalID":1,"TerminalName":"Anacortes -> Friday Harbor",
                     "ArrivalTerminalIDs":[1,10],"DisplayDriveUpSpace":true,
                     "DriveUpSpaceCount":-16,"MaxSpaceCount":141},
                    {"TerminalID":13,"TerminalName":"Anacortes -> Lopez Island",
                     "ArrivalTerminalIDs":[13],"DisplayDriveUpSpace":true,
                     "DriveUpSpaceCount":26,"MaxSpaceCount":141,
                     "DisplayReservableSpace":true,"ReservableSpaceCount":12},
                    {"TerminalID":1,"TerminalName":"Anacortes -> Orcas Island",
                     "DisplayDriveUpSpace":false,"DriveUpSpaceCount":0}]},
                 {"Departure":"\/Date(1793310000000-0700)\/","IsCancelled":true,
                  "VesselID":null,"MaxSpaceCount":null,"SpaceForArrivalTerminals":[]}]}]"#,
            &terminals,
        )
        .unwrap();
        assert_eq!(s.len(), 2);
        let d = &s[0].destinations;
        let ids: Vec<u32> = d.iter().map(|x| x.terminal).collect();
        // The departing terminal dropped from the ids; Orcas by its name.
        assert_eq!(ids, [10, 13, 15]);
        assert_eq!((d[0].drive_up, d[0].oversold), (Some(0), true));
        assert_eq!((d[0].oversold_by, d[1].oversold_by), (Some(16), None));
        assert_eq!((d[1].drive_up, d[1].oversold), (Some(26), false));
        // Reservable space where WSF shows it; none where it doesn't say.
        assert_eq!((d[1].reservable, d[0].reservable), (Some(12), None));
        // Space WSF doesn't show is unknown, not zero.
        assert_eq!((d[2].drive_up, d[2].oversold), (None, false));
        assert_eq!((s[0].vessel, s[0].capacity), (Some(69), Some(141)));
        assert!(s[1].cancelled && s[1].destinations.is_empty());
    }

    #[test]
    fn merges_notices_posted_at_several_terminals() {
        let n = parse_notices(
            r#"[{"TerminalID":9,"Bulletins":[
                  {"BulletinTitle":"Two-boat schedule","BulletinText":"<p>Old text.</p>",
                   "BulletinLastUpdated":"\/Date(1793300000000-0700)\/"},
                  {"BulletinTitle":"Elevator &amp; ramp","BulletinText":null,
                   "BulletinLastUpdated":null}]},
                {"TerminalID":20,"Bulletins":[]},
                {"TerminalID":22,"Bulletins":[
                  {"BulletinTitle":"Two-boat schedule","BulletinText":"<p>New <b>text</b>.</p>",
                   "BulletinLastUpdated":"\/Date(1793311126713-0700)\/"}]}]"#,
        )
        .unwrap();
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].title, "Two-boat schedule");
        assert_eq!(n[0].terminals, [9, 22]);
        assert_eq!(n[0].text, "New text.");
        assert_eq!(n[0].updated_ms, Some(1_793_311_126_713));
        assert_eq!(n[1].title, "Elevator & ramp");
        assert_eq!(n[1].text, "");
        // Ids are stable: from the title alone.
        assert_eq!(n[0].id, notice_id("Two-boat schedule"));
        assert!(n[0].id.starts_with("n-") && n[0].id != n[1].id);
    }

    #[test]
    fn replays_answers_in_turn() {
        let replay = Replay::parse(
            r#"{"path":"a","fetched_ms":1,"body":[1]}
               {"path":"b","fetched_ms":2,"body":{"x":2}}

               {"path":"a","fetched_ms":3,"body":[3]}"#,
        )
        .unwrap();
        let next: Vec<String> = (0..3).map(|_| replay.next("a").unwrap()).collect();
        assert_eq!(next, ["[1]", "[3]", "[1]"]);
        assert_eq!(replay.next("b").unwrap(), r#"{"x":2}"#);
        assert!(replay.next("c").is_err());
        assert!(Replay::parse("not json").is_err());
    }
}
