//! The documents the server's API serves under `/api/v1/`, as JSON. The
//! server builds them; the app reads them. Times are UTC milliseconds
//! since the Unix epoch, in fields ending `_ms`; boats and terminals go
//! by WSF's ids, legs by their OpenStreetMap way id, and the routes riders
//! know by a short id such as `fauntleroy-vashon-southworth`.

use serde::{Deserialize, Serialize};

/// `GET /network`: the terminals, the legs between them, and the routes
/// riders know. Changes rarely.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Network {
    pub terminals: Vec<NetworkTerminal>,
    pub legs: Vec<NetworkLeg>,
    pub routes: Vec<NetworkRoute>,
    /// Credits the data's licenses require, to show with the map.
    pub attribution: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkTerminal {
    pub id: u32,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    /// Which way the slip opens (degrees from north), as learned from
    /// boats leaving it; None until one has been seen.
    pub slip_facing_deg: Option<f64>,
}

/// A leg of a route as OpenStreetMap draws it, between two terminals (or,
/// for a slip approach, from a terminal back to itself).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkLeg {
    pub id: u64,
    /// The terminals at its ends, if both reach one.
    pub ends: Option<[u32; 2]>,
    pub length_m: f64,
    /// Cubic Béziers end to end, each four [lon, lat] control points.
    pub curves: Vec<[[f64; 2]; 4]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkRoute {
    pub id: String,
    pub name: String,
    pub terminals: Vec<u32>,
    /// The legs it's made of, by id, slip approaches included.
    pub legs: Vec<u64>,
}

/// `GET /vessels`: each boat's details. Changes rarely.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vessels {
    pub vessels: Vec<VesselInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VesselInfo {
    pub id: u32,
    pub name: String,
    pub class: Option<String>,
    pub length_m: Option<f64>,
    pub beam_m: Option<f64>,
    pub car_capacity: Option<u32>,
}

/// How current a live document is, at its top.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// When the server last fetched it from WSF successfully.
    pub fetched_ms: i64,
    /// When the server expects to have newer data (for display; clients
    /// time their next request from the response's Cache-Control and Age).
    pub next_update_ms: i64,
    /// Older than twice its usual interval.
    pub stale: bool,
    /// Why the last fetch failed, if it did; the data is then the last
    /// good copy.
    pub error: Option<String>,
}

/// `GET /live`: all three live documents, for a client's first load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Live {
    pub vessels: LiveVessels,
    pub sailings: LiveSailings,
    pub notices: LiveNotices,
}

/// `GET /live/vessels`: where each boat is and what it's doing (~10 s).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveVessels {
    pub source: Source,
    pub vessels: Vec<LiveVessel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveVessel {
    pub id: u32,
    pub lon: f64,
    pub lat: f64,
    pub speed_kn: f64,
    /// Degrees clockwise from true north.
    pub heading_deg: f64,
    pub at_dock: bool,
    pub in_service: bool,
    /// Terminal ids: where it's leaving or left from, and where it's bound.
    pub departing: Option<u32>,
    pub arriving: Option<u32>,
    pub scheduled_departure_ms: Option<i64>,
    pub left_dock_ms: Option<i64>,
    pub eta_ms: Option<i64>,
    /// When the position was taken.
    pub reported_ms: i64,
    /// WSF's service note, when it's in effect.
    pub note: Option<String>,
    /// Its reports over the last few minutes, oldest first.
    pub wake: Vec<WakePoint>,
}

/// A past report: when (ms), longitude and latitude, as a JSON array.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WakePoint(pub i64, pub f64, pub f64);

/// `GET /live/sailings`: drive-up space on upcoming departures (~60 s).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveSailings {
    pub source: Source,
    pub sailings: Vec<Sailing>,
}

/// One departure from a terminal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sailing {
    pub terminal: u32,
    pub departs_ms: i64,
    pub vessel: Option<u32>,
    /// The boat's drive-up capacity.
    pub capacity: Option<u32>,
    pub cancelled: bool,
    /// Where it goes: one terminal, or several on island routes, each
    /// with its own space.
    pub destinations: Vec<Destination>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub terminal: u32,
    /// Drive-up spaces left, where WSF shows them (None: not shown,
    /// which isn't zero).
    pub drive_up: Option<u32>,
    /// More cars waiting than will fit (WSF's count was below zero; the
    /// space left is then 0).
    pub oversold: bool,
    /// Spaces still open to reservations, on routes that take them, where
    /// WSF shows them. Not on top of `drive_up`: WSF's two counts overlap.
    #[serde(default)]
    pub reservable: Option<u32>,
    /// Where oversold, how many cars were waiting beyond the space.
    #[serde(default)]
    pub oversold_by: Option<u32>,
}

/// `GET /live/notices`: the notices WSF has posted (~5 min).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveNotices {
    pub source: Source,
    pub notices: Vec<Notice>,
}

/// A notice, once, with every terminal it's posted at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub terminals: Vec<u32>,
    pub title: String,
    /// Plain text, paragraphs a blank line apart.
    pub text: String,
    pub updated_ms: Option<i64>,
}

/// `GET /stats/crossings?route=…&days=…`: how long crossings take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CrossingStats {
    pub route: String,
    pub days: u32,
    pub pairs: Vec<PairStats>,
}

/// Crossings between two terminals, either way, from completed trips.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairStats {
    pub terminals: [u32; 2],
    pub trips: u32,
    pub median_min: f64,
    pub p90_min: f64,
}

/// `GET /schedule?route=…`: a route's published sailings for the current
/// trip day and the next, oldest first. A day that's here with no
/// departures has none published; a day that's missing couldn't be loaded,
/// so nothing is known about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteSchedule {
    pub route: String,
    pub days: Vec<ScheduleDay>,
}

/// A route's sailings for one trip day (WSF's schedule day, which runs past
/// midnight), as departures from each of its terminals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleDay {
    /// The trip day, `2026-10-30`.
    pub trip_date: String,
    /// The season's schedule, as WSF names it ("Fall 2026").
    pub season: String,
    pub pdf_url: Option<String>,
    pub terminals: Vec<TerminalDepartures>,
}

/// One terminal's departures on a route, in time order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalDepartures {
    pub terminal: u32,
    pub departures: Vec<Departure>,
    /// Footnotes, plain text; departures point at them by index.
    pub notes: Vec<String>,
}

/// A scheduled sailing from a terminal, with the stops it makes in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Departure {
    pub departs_ms: i64,
    pub vessel_id: Option<u32>,
    pub vessel: String,
    pub stops: Vec<Stop>,
    pub loading: Loading,
    pub notes: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub terminal: u32,
    pub arrives_ms: Option<i64>,
}

/// `GET /sailings?route=…&terminal=…`: how a route's sailings from a terminal
/// went. Without `before_ms`: the current trip day's so far, and any later
/// sailing withdrawn. With it: the `limit` sailings before then, newest
/// first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteSailings {
    pub route: String,
    pub terminal: u32,
    pub sailings: Vec<SailingRecord>,
    /// Whether there are older sailings than these.
    pub more: bool,
}

/// A published sailing and how it went.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SailingRecord {
    /// WSF's trip day, `2026-10-30`.
    pub trip_date: String,
    pub departs_ms: i64,
    pub stops: Vec<u32>,
    /// The boat as last published, and as first published.
    pub vessel: String,
    pub first_vessel: String,
    pub outcome: SailingOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SailingOutcome {
    /// A boat ran it, this many minutes after its time (early if negative).
    Ran {
        left_ms: i64,
        vessel: String,
        late_min: i32,
    },
    /// WSF has it as no boat's sailing, but a boat off the schedule left
    /// around then for one of its stops.
    OffSchedule { left_ms: i64, vessel: String },
    /// WSF withdrew it, and nothing ran it.
    Cancelled { withdrawn_ms: i64 },
    /// Past its time, and it may yet go.
    Due,
    /// Still published, and no boat left for it while we were recording.
    NoRecord,
    /// We weren't recording then, so there's no telling.
    Unknown,
}

/// Who a sailing takes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Loading {
    Passengers,
    Vehicles,
    Both,
}

/// `GET /loads?vessel=…`: a boat's latest sailings and how full its car
/// deck was as each left the dock, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VesselLoads {
    pub vessel: u32,
    pub sailings: Vec<SailingLoad>,
}

/// How full a sailing's car deck was as it left: the share of its car
/// spaces taken (0 to 1), from WSF's last count of the space left.
/// Passengers aren't counted (WSF doesn't publish them).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SailingLoad {
    pub from: u32,
    pub to: Option<u32>,
    pub scheduled_ms: i64,
    pub left_ms: i64,
    pub load: f64,
    /// Car spaces, and those left.
    pub capacity: u32,
    pub free: u32,
    /// How long (min) before it left WSF last gave that count: the
    /// longer, the more cars may have boarded since.
    pub reading_age_min: f64,
}

/// `GET /stats/loads?route=…` (or `vessel=…`) `&days=…`: how full car
/// decks are, by direction and by hour of the day (Pacific time) on
/// weekdays and weekends, from sailings in the last `days`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoadStats {
    pub route: Option<String>,
    pub vessel: Option<u32>,
    pub days: u32,
    pub directions: Vec<DirectionLoads>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DirectionLoads {
    pub from: u32,
    pub to: u32,
    pub sailings: u32,
    pub median_load: f64,
    pub hours: Vec<HourLoads>,
}

/// Sailings scheduled to leave in one hour of the day (0 to 23) on
/// weekdays or weekends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HourLoads {
    pub weekend: bool,
    pub hour: u8,
    pub sailings: u32,
    pub median_load: f64,
    /// The share of them that left full (95% or more of the deck).
    pub full_share: f64,
}

/// `GET /advice?terminal=…`: when to arrive by car for each of a
/// terminal's sailings in the published schedule, from how sailings like
/// each filled before.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalAdvice {
    pub terminal: u32,
    /// On routes that take reservations, when those holding one should
    /// arrive and check in (WSF's guidance).
    pub reservation: Option<ReservationAdvice>,
    pub sailings: Vec<SailingAdvice>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReservationAdvice {
    /// Arrive this many minutes early, from the first to the second.
    pub arrive_min: [u32; 2],
    /// Check in at least this many minutes before, or lose the spot.
    pub check_in_min: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SailingAdvice {
    pub departs_ms: i64,
    pub advice: Advice,
    /// A holiday or special day (more time allowed in the advice).
    pub holiday: Option<String>,
}

/// When to arrive with a car, without a reservation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Advice {
    /// Where there's too little record to go on (or WSF doesn't count car
    /// space here: `counted` false), WSF's general guidance: arrive by
    /// then, `lead_min` early, for the reason given.
    Guidance {
        arrive_by_ms: i64,
        lead_min: u32,
        reason: GuidanceReason,
        counted: bool,
    },
    /// Sailings like it usually have room.
    Room { sailings: u32, basis: String },
    /// Arrive by then to get on with about `likely` chance. Sailings like
    /// it were usually full by `usually_full_ms`, where most filled; today
    /// it's filling `pace` times as fast as usual, where that's known.
    ArriveBy {
        arrive_by_ms: i64,
        likely: f64,
        usually_full_ms: Option<i64>,
        sailings: u32,
        /// The sailings it's from: "Fridays", "weekdays".
        basis: String,
        pace: Option<f64>,
    },
}

/// Which of WSF's general arrival guidance applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceReason {
    /// At least 20 minutes early.
    OffPeak,
    /// 30 to 60 minutes early at busy times.
    Busy,
    /// On routes that take reservations, 30 to 45 minutes early.
    Reservations,
}

/// `GET /stats/advice?days=…`: how the advice held, per terminal: for
/// each sailing in the last `days`, the advice from the other days' and
/// whether it had a space then.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdviceCheck {
    pub days: u32,
    pub terminals: Vec<TerminalCheck>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalCheck {
    pub terminal: u32,
    pub sailings: u32,
    /// The share of them with a space at the advised time, and the chance
    /// the advice gave, on average.
    pub held: f64,
    pub predicted: f64,
}

/// The body of every error response, with a matching HTTP status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: ErrorBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Short and stable, for code to match: `not_found`, `bad_request`.
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;

    /// Reads `json`, and checks writing it back gives the same document.
    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(json: &str) -> T {
        let doc: T = serde_json::from_str(json).unwrap();
        let again: T = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(doc, again);
        doc
    }

    #[test]
    fn reads_the_network_and_vessels() {
        let network: Network = round_trip(
            r#"{"terminals":[{"id":7,"name":"Seattle","lon":-122.3405,"lat":47.6025,
                 "slip_facing_deg":265},
                {"id":3,"name":"Bainbridge Island","lon":-122.5096,"lat":47.6223,
                 "slip_facing_deg":null}],
               "legs":[{"id":41229170,"ends":[7,3],"length_m":14210,
                 "curves":[[[-122.3398,47.6029],[-122.36,47.604],[-122.40,47.606],[-122.43,47.607]]]},
                {"id":5,"ends":null,"length_m":200,"curves":[]}],
               "routes":[{"id":"seattle-bainbridge-island","name":"Seattle / Bainbridge Island",
                 "terminals":[7,3],"legs":[41229170]}],
               "attribution":["Ferry routes © OpenStreetMap contributors (ODbL)"]}"#,
        );
        assert_eq!(network.legs[0].ends, Some([7, 3]));
        assert_eq!(network.legs[1].ends, None);
        assert_eq!(network.terminals[1].slip_facing_deg, None);
        let vessels: Vessels = round_trip(
            r#"{"vessels":[{"id":37,"name":"Wenatchee","class":"Jumbo Mark II",
                 "length_m":140.2,"beam_m":27.1,"car_capacity":202}]}"#,
        );
        assert_eq!(vessels.vessels[0].car_capacity, Some(202));
    }

    #[test]
    fn reads_live_documents() {
        let source = r#"{"fetched_ms":1793329998000,"next_update_ms":1793330008500,
                         "stale":false,"error":null}"#;
        let live: Live = round_trip(&format!(
            r#"{{"vessels":{{"source":{source},"vessels":[
                 {{"id":37,"lon":-122.4012,"lat":47.6061,"speed_kn":17.2,"heading_deg":265,
                  "at_dock":false,"in_service":true,"departing":7,"arriving":3,
                  "scheduled_departure_ms":1793329200000,"left_dock_ms":1793329320000,
                  "eta_ms":1793331300000,"reported_ms":1793329995000,"note":null,
                  "wake":[[1793329820000,-122.3851,47.6049]]}}]}},
               "sailings":{{"source":{source},"sailings":[
                 {{"terminal":1,"departs_ms":1793331000000,"vessel":69,"capacity":141,
                  "cancelled":false,"destinations":[
                    {{"terminal":10,"drive_up":0,"oversold":true}},
                    {{"terminal":15,"drive_up":null,"oversold":false}}]}}]}},
               "notices":{{"source":{source},"notices":[
                 {{"id":"b-2961","terminals":[9,22,20],"title":"Two-boat schedule",
                  "text":"Through Sunday.","updated_ms":1793311126713}}]}}}}"#
        ));
        let boat = &live.vessels.vessels[0];
        assert_eq!(
            boat.wake[0],
            WakePoint(1_793_329_820_000, -122.3851, 47.6049)
        );
        assert_eq!(boat.departing, Some(7));
        let sailing = &live.sailings.sailings[0];
        assert!(sailing.destinations[0].oversold);
        assert_eq!(sailing.destinations[1].drive_up, None);
        assert_eq!(live.notices.notices[0].terminals, [9, 22, 20]);
        assert_eq!(live.vessels.source.next_update_ms, 1_793_330_008_500);
    }

    #[test]
    fn reads_stats_and_errors() {
        let stats: CrossingStats = round_trip(
            r#"{"route":"seattle-bremerton","days":30,
               "pairs":[{"terminals":[7,4],"trips":412,"median_min":58,"p90_min":63}]}"#,
        );
        assert_eq!(stats.pairs[0].median_min, 58.0);
        let error: ApiError =
            round_trip(r#"{"error":{"code":"not_found","message":"No terminal 99"}}"#);
        assert_eq!(error.error.code, "not_found");
    }

    #[test]
    fn reads_advice() {
        let advice: TerminalAdvice = round_trip(
            r#"{"terminal":3,"trip_date":"2026-10-30","reservation":null,"sailings":[
                {"departs_ms":1793376900000,"holiday":null,"advice":{"kind":"arrive_by",
                  "arrive_by_ms":1793374200000,"likely":0.85,"usually_full_ms":1793374800000,
                  "sailings":7,"basis":"Fridays","pace":1.2}},
                {"departs_ms":1793380000000,"holiday":"Thanksgiving",
                  "advice":{"kind":"room","sailings":9,"basis":"weekdays"}},
                {"departs_ms":1793390000000,"holiday":null,"advice":{"kind":"guidance",
                  "arrive_by_ms":1793388800000,"lead_min":20,"reason":"off_peak",
                  "counted":true}}]}"#,
        );
        assert!(matches!(
            advice.sailings[0].advice,
            Advice::ArriveBy { sailings: 7, .. }
        ));
        let check: AdviceCheck = round_trip(
            r#"{"days":14,"terminals":[{"terminal":3,"sailings":40,"held":0.85,"predicted":0.83}]}"#,
        );
        assert_eq!(check.terminals[0].sailings, 40);
    }

    #[test]
    fn reads_loads() {
        let loads: VesselLoads = round_trip(
            r#"{"vessel":37,"sailings":[{"from":7,"to":3,"scheduled_ms":1793329200000,
                "left_ms":1793329320000,"load":0.82,"capacity":202,"free":36,
                "reading_age_min":1.5}]}"#,
        );
        assert_eq!(loads.sailings[0].free, 36);
        let stats: LoadStats = round_trip(
            r#"{"route":"seattle-bainbridge-island","vessel":null,"days":30,"directions":[
                {"from":3,"to":7,"sailings":40,"median_load":0.7,"hours":[
                  {"weekend":false,"hour":7,"sailings":10,"median_load":0.98,"full_share":0.8}]}]}"#,
        );
        assert!(stats.directions[0].hours[0].full_share > 0.5);
        // Older documents without reservable space still read.
        let d: Destination =
            serde_json::from_str(r#"{"terminal":3,"drive_up":4,"oversold":false}"#).unwrap();
        assert_eq!(d.reservable, None);
    }

    #[test]
    fn reads_sailing_records() {
        let doc: RouteSailings = round_trip(
            r#"{"route":"edmonds-kingston","terminal":12,"more":true,"sailings":[
                {"trip_date":"2026-10-30","departs_ms":1793394600000,"stops":[8],
                 "vessel":"Spokane","first_vessel":"Puyallup",
                 "outcome":{"kind":"ran","left_ms":1793394700000,"vessel":"Spokane","late_min":2}},
                {"trip_date":"2026-10-30","departs_ms":1793398200000,"stops":[8],
                 "vessel":"Spokane","first_vessel":"Spokane","outcome":{"kind":"no_record"}}]}"#,
        );
        assert!(matches!(
            doc.sailings[0].outcome,
            SailingOutcome::Ran { late_min: 2, .. }
        ));
        assert_eq!(doc.sailings[1].outcome, SailingOutcome::NoRecord);
    }

    #[test]
    fn reads_schedules() {
        let schedule: RouteSchedule = round_trip(
            r#"{"route":"fauntleroy-vashon-southworth","days":[{"trip_date":"2026-10-30",
               "season":"Fall 2026","pdf_url":null,"terminals":[
                 {"terminal":9,"notes":["Via Southworth"],"departures":[
                   {"departs_ms":1793394600000,"vessel_id":6,"vessel":"Kitsap",
                    "stops":[{"terminal":22,"arrives_ms":1793395800000},
                             {"terminal":20,"arrives_ms":null}],
                    "loading":"both","notes":[0]}]}]}]}"#,
        );
        let d = &schedule.days[0].terminals[0].departures[0];
        assert_eq!(d.loading, Loading::Both);
        assert_eq!(d.stops[1].arrives_ms, None);
    }
}
