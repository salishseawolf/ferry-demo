//! Washington State Ferries terminals, as the server reports them (the
//! route network, `/live/sailings`, `/live/notices`): where each is, which
//! way its slip opens, how much drive-up space its upcoming sailings have
//! left, and the notices posted for it.

use std::collections::{BTreeSet, HashMap};

use ferry_model::api::{LiveNotices, LiveSailings, Network};

use crate::geo::{EARTH_RADIUS_M, LonLat, distance_m, initial_bearing_deg};

/// A sailing with this many drive-up spaces or fewer is nearly full.
pub const NEARLY_FULL_CARS: u32 = 10;
/// A sailing counts as upcoming until this long (ms) after its scheduled
/// departure: boats often leave a few minutes late.
const DEPARTED_AFTER_MS: f64 = 5.0 * 60_000.0;
/// A terminal's symbols (its anchor and notice dot) are to scale with the
/// terrain at full size while the view spans `SYMBOL_FULL_VIEW_M` or more
/// corner to corner, shrinking as it zooms in to `SYMBOL_LEAST_SCALE` of
/// that by `SYMBOL_LEAST_VIEW_M`, so they don't swamp the slips up close.
const SYMBOL_FULL_VIEW_M: f64 = 54_590.0;
const SYMBOL_LEAST_VIEW_M: f64 = 8_590.0;
const SYMBOL_LEAST_SCALE: f64 = 0.1;

/// How large a terminal's symbols are, as a share of full size, in a view
/// spanning `view_m` corner to corner: the same share for each step in
/// zoom (log scale) between the two clamps.
pub fn symbol_scale(view_m: f64) -> f64 {
    let t = (SYMBOL_FULL_VIEW_M / view_m).ln() / (SYMBOL_FULL_VIEW_M / SYMBOL_LEAST_VIEW_M).ln();
    SYMBOL_LEAST_SCALE.powf(t.clamp(0.0, 1.0))
}

pub use ferry_model::terminal::Terminal;

/// One departure from a terminal, with its drive-up space.
#[derive(Clone, Debug, PartialEq)]
pub struct Sailing {
    pub departs_ms: f64,
    pub cancelled: bool,
    pub vessel: String,
    /// Where it goes: one terminal, or several on island routes.
    pub to: String,
    /// Drive-up spaces left and the boat's capacity, where WSF shows them.
    pub drive_up: Option<u32>,
    pub capacity: Option<u32>,
}

impl Sailing {
    /// The share of drive-up space left, 0 to 1, if known.
    pub fn space_left(&self) -> Option<f64> {
        match (self.drive_up, self.capacity) {
            (Some(left), Some(max)) if max > 0 => Some((f64::from(left) / f64::from(max)).min(1.0)),
            _ => None,
        }
    }

    pub fn nearly_full(&self) -> bool {
        self.drive_up.is_some_and(|left| left <= NEARLY_FULL_CARS)
    }
}

/// A notice WSF has posted for a terminal: a closure, delays, a broken
/// elevator. `text` is WSF's HTML made plain, paragraphs apart.
#[derive(Clone, Debug, PartialEq)]
pub struct Bulletin {
    pub title: String,
    pub text: String,
    pub updated_ms: f64,
}

/// Each terminal's sailings (by id, soonest first) from a `/live/sailings`
/// document, naming boats from `vessel_names` and destinations from
/// `terminal_names`. A sailing to several stops shares one boat's space,
/// so the tightest count stands for it.
pub fn sailings_from(
    doc: &LiveSailings,
    vessel_names: &HashMap<u32, String>,
    terminal_names: &HashMap<u32, String>,
) -> HashMap<u32, Vec<Sailing>> {
    let mut out: HashMap<u32, Vec<Sailing>> = HashMap::new();
    for s in &doc.sailings {
        let to: Vec<String> = s
            .destinations
            .iter()
            .map(|d| {
                terminal_names
                    .get(&d.terminal)
                    .cloned()
                    .unwrap_or_else(|| format!("Terminal {}", d.terminal))
            })
            .collect();
        out.entry(s.terminal).or_default().push(Sailing {
            departs_ms: s.departs_ms as f64,
            cancelled: s.cancelled,
            vessel: s
                .vessel
                .and_then(|v| vessel_names.get(&v).cloned())
                .unwrap_or_default(),
            to: to.join(" / "),
            drive_up: s.destinations.iter().filter_map(|d| d.drive_up).min(),
            capacity: s.capacity,
        });
    }
    for sailings in out.values_mut() {
        sailings.sort_by(|a, b| a.departs_ms.total_cmp(&b.departs_ms));
    }
    out
}

/// Each terminal's notices (by id, newest first) from a `/live/notices`
/// document, which lists each once with the terminals posting it.
pub fn bulletins_from(doc: &LiveNotices) -> HashMap<u32, Vec<Bulletin>> {
    let mut out: HashMap<u32, Vec<Bulletin>> = HashMap::new();
    for n in &doc.notices {
        for &t in &n.terminals {
            out.entry(t).or_default().push(Bulletin {
                title: n.title.clone(),
                text: n.text.clone(),
                updated_ms: n.updated_ms.unwrap_or(0) as f64,
            });
        }
    }
    for bulletins in out.values_mut() {
        bulletins.sort_by(|a, b| b.updated_ms.total_cmp(&a.updated_ms));
    }
    out
}

/// The terminals, what's been learned about their slips, and their
/// sailings' space.
#[derive(Default)]
pub struct Terminals {
    pub list: Vec<Terminal>,
    /// Which way each slip opens (degrees), as the server has learned it.
    facing: HashMap<u32, f64>,
    /// Terminals each one has a route to.
    partners: HashMap<u32, BTreeSet<u32>>,
    space: HashMap<u32, Vec<Sailing>>,
    bulletins: HashMap<u32, Vec<Bulletin>>,
}

impl Terminals {
    pub fn set_space(&mut self, space: HashMap<u32, Vec<Sailing>>) {
        self.space = space;
    }

    pub fn set_bulletins(&mut self, bulletins: HashMap<u32, Vec<Bulletin>>) {
        self.bulletins = bulletins;
    }

    /// The notices posted for `t`, newest first.
    pub fn bulletins(&self, t: &Terminal) -> &[Bulletin] {
        self.bulletins.get(&t.id).map_or(&[], Vec::as_slice)
    }

    /// Takes in the route network: the terminals, their slips' learned
    /// directions, and which terminals each has a route to.
    pub fn set_network(&mut self, network: &Network) {
        self.list = network
            .terminals
            .iter()
            .map(|t| Terminal {
                id: t.id,
                name: t.name.clone(),
                position: LonLat::new(t.lon, t.lat),
            })
            .collect();
        self.facing = network
            .terminals
            .iter()
            .filter_map(|t| Some((t.id, t.slip_facing_deg?)))
            .collect();
        self.partners.clear();
        for [a, b] in network.legs.iter().filter_map(|l| l.ends) {
            if a != b {
                self.partners.entry(a).or_default().insert(b);
                self.partners.entry(b).or_default().insert(a);
            }
        }
    }

    /// Each terminal's name, by id.
    pub fn names(&self) -> HashMap<u32, String> {
        self.list.iter().map(|t| (t.id, t.name.clone())).collect()
    }

    /// Which way `t`'s slip opens (degrees from north): as learned from
    /// boats leaving it, or else toward its nearest route partner.
    pub fn facing_deg(&self, t: &Terminal) -> Option<f64> {
        if let Some(&heading) = self.facing.get(&t.id) {
            return Some(heading);
        }
        let partner = self
            .partners
            .get(&t.id)?
            .iter()
            .filter_map(|id| self.list.iter().find(|p| p.id == *id));
        let nearest = partner.min_by(|a, b| {
            let d = |p: &Terminal| distance_m(t.position, p.position, EARTH_RADIUS_M);
            d(a).total_cmp(&d(b))
        })?;
        initial_bearing_deg(t.position, nearest.position)
    }

    /// `t`'s sailings from `now_ms` on, cancelled ones included.
    pub fn upcoming(&self, t: &Terminal, now_ms: f64) -> Vec<&Sailing> {
        self.space
            .get(&t.id)
            .into_iter()
            .flatten()
            .filter(|s| s.departs_ms + DEPARTED_AFTER_MS >= now_ms)
            .collect()
    }

    /// `t`'s next sailing that hasn't been cancelled.
    pub fn next_sailing(&self, t: &Terminal, now_ms: f64) -> Option<&Sailing> {
        self.upcoming(t, now_ms).into_iter().find(|s| !s.cancelled)
    }

    /// Lines for the HUD at `now_ms`: the terminal and up to `limit` of its
    /// upcoming sailings, with times written by `clock`.
    pub fn describe(
        &self,
        t: &Terminal,
        now_ms: f64,
        limit: usize,
        clock: impl Fn(f64) -> String,
    ) -> Vec<String> {
        let mut lines = vec![format!("{} terminal", t.name)];
        match self.bulletins(t).len() {
            0 => {}
            1 => lines.push("1 notice posted".to_string()),
            n => lines.push(format!("{n} notices posted")),
        }
        let upcoming = self.upcoming(t, now_ms);
        if upcoming.is_empty() {
            lines.push("No sailing space reported".to_string());
        }
        for s in upcoming.into_iter().take(limit) {
            let space = if s.cancelled {
                "cancelled".to_string()
            } else {
                match (s.drive_up, s.capacity) {
                    (Some(left), Some(max)) => format!("{left} of {max} drive-up spaces"),
                    (Some(left), None) => format!("{left} drive-up spaces"),
                    _ => "space not shown".to_string(),
                }
            };
            lines.push(format!("{} to {} · {space}", clock(s.departs_ms), s.to));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinks_symbols_between_the_clamps() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(symbol_scale(200_000.0), 1.0));
        assert!(near(symbol_scale(54_590.0), 1.0));
        assert!(near(symbol_scale(8_590.0), 0.1));
        assert!(near(symbol_scale(1_000.0), 0.1));
        // Halfway in zoom (the geometric mean), halfway in scale.
        let mid = (54_590.0_f64 * 8_590.0).sqrt();
        assert!(near(symbol_scale(mid), 0.1_f64.sqrt()));
    }

    const NETWORK: &str = r#"{"terminals":[
        {"id":7,"name":"Seattle","lon":-122.340472,"lat":47.602501,"slip_facing_deg":265},
        {"id":3,"name":"Bainbridge Island","lon":-122.509617,"lat":47.622339,"slip_facing_deg":null},
        {"id":4,"name":"Bremerton","lon":-122.624089,"lat":47.561847,"slip_facing_deg":null}],
      "legs":[{"id":1,"ends":[7,3],"length_m":14000,"curves":[]},
              {"id":2,"ends":[7,4],"length_m":24000,"curves":[]},
              {"id":3,"ends":[4,4],"length_m":200,"curves":[]}],
      "routes":[],"attribution":[]}"#;

    const SAILINGS: &str = r#"{
      "source":{"fetched_ms":0,"next_update_ms":0,"stale":false,"error":null},
      "sailings":[
        {"terminal":7,"departs_ms":1793306000000,"vessel":37,"capacity":202,"cancelled":false,
         "destinations":[{"terminal":3,"drive_up":null,"oversold":false}]},
        {"terminal":7,"departs_ms":1793300000000,"vessel":37,"capacity":202,"cancelled":false,
         "destinations":[{"terminal":3,"drive_up":8,"oversold":false}]},
        {"terminal":7,"departs_ms":1793303000000,"vessel":null,"capacity":202,"cancelled":true,
         "destinations":[{"terminal":3,"drive_up":0,"oversold":true}]},
        {"terminal":1,"departs_ms":1793300000000,"vessel":69,"capacity":141,"cancelled":false,
         "destinations":[{"terminal":13,"drive_up":30,"oversold":false},
                         {"terminal":10,"drive_up":26,"oversold":false}]}]}"#;

    fn terminals() -> Terminals {
        let mut t = Terminals::default();
        t.set_network(&serde_json::from_str(NETWORK).unwrap());
        let boats = HashMap::from([(37, "Wenatchee".to_string())]);
        let names = HashMap::from([
            (3, "Bainbridge Island".to_string()),
            (13, "Lopez Island".to_string()),
        ]);
        t.set_space(sailings_from(
            &serde_json::from_str(SAILINGS).unwrap(),
            &boats,
            &names,
        ));
        t
    }

    #[test]
    fn reads_the_network_and_sailings() {
        let t = terminals();
        assert_eq!(t.list[0].name, "Seattle");
        assert_eq!(t.list[0].position, LonLat::new(-122.340472, 47.602501));
        assert_eq!(t.names()[&4], "Bremerton");
        let seattle = &t.space[&7];
        // Soonest first.
        let times: Vec<f64> = seattle.iter().map(|s| s.departs_ms).collect();
        assert_eq!(
            times,
            [
                1_793_300_000_000.0,
                1_793_303_000_000.0,
                1_793_306_000_000.0
            ]
        );
        assert_eq!(seattle[0].to, "Bainbridge Island");
        assert_eq!(seattle[0].vessel, "Wenatchee");
        assert_eq!(
            (seattle[0].drive_up, seattle[0].capacity),
            (Some(8), Some(202))
        );
        assert!(seattle[0].nearly_full());
        assert!((seattle[0].space_left().unwrap() - 8.0 / 202.0).abs() < 1e-9);
        // Space not shown is unknown, not zero.
        assert_eq!(seattle[2].drive_up, None);
        assert_eq!(seattle[2].space_left(), None);
        // Several stops: all named, the tightest count standing for them;
        // a terminal not yet named is called by its id.
        let anacortes = &t.space[&1][0];
        assert_eq!(anacortes.to, "Lopez Island / Terminal 10");
        assert_eq!(anacortes.drive_up, Some(26));
        assert_eq!(anacortes.vessel, "");
    }

    #[test]
    fn next_sailing_skips_cancelled_and_departed() {
        let t = terminals();
        let seattle = &t.list[0];
        // A few minutes after the first departure it still counts.
        let now = 1_793_300_000_000.0 + 3.0 * 60_000.0;
        assert_eq!(
            t.next_sailing(seattle, now).unwrap().departs_ms,
            1_793_300_000_000.0
        );
        // Past that, the cancelled one is listed but skipped.
        let later = 1_793_300_000_000.0 + 10.0 * 60_000.0;
        assert_eq!(t.upcoming(seattle, later).len(), 2);
        assert_eq!(
            t.next_sailing(seattle, later).unwrap().departs_ms,
            1_793_306_000_000.0
        );
        assert_eq!(t.next_sailing(&t.list[1], later), None);
    }

    #[test]
    fn slips_face_as_learned_or_toward_partners() {
        let t = terminals();
        // Learned by the server.
        assert_eq!(t.facing_deg(&t.list[0]), Some(265.0));
        // Bainbridge's partner is Seattle, to the east.
        let bainbridge = t.facing_deg(&t.list[1]).unwrap();
        assert!((80.0..110.0).contains(&bainbridge), "{bainbridge}");
        // Bremerton's is Seattle too (its slip approach doesn't count).
        let bremerton = t.facing_deg(&t.list[2]).unwrap();
        assert!((60.0..100.0).contains(&bremerton), "{bremerton}");
        assert_eq!(Terminals::default().facing_deg(&t.list[0]), None);
    }

    #[test]
    fn describes_upcoming_sailings() {
        let t = terminals();
        let clock = |ms: f64| format!("t{}", (ms / 1000.0) as u64 % 10_000);
        let lines = t.describe(&t.list[0], 1_793_299_000_000.0, 3, clock);
        assert_eq!(
            lines,
            [
                "Seattle terminal",
                "t0 to Bainbridge Island · 8 of 202 drive-up spaces",
                "t3000 to Bainbridge Island · cancelled",
                "t6000 to Bainbridge Island · space not shown",
            ]
        );
        assert_eq!(
            t.describe(&t.list[1], 0.0, 3, clock)[1],
            "No sailing space reported"
        );
    }

    #[test]
    fn reads_notices_for_each_terminal() {
        let doc: LiveNotices = serde_json::from_str(
            r#"{"source":{"fetched_ms":0,"next_update_ms":0,"stale":false,"error":null},
                "notices":[
                  {"id":"n-1","terminals":[7],"title":"Older","text":"Old.","updated_ms":1793000000000},
                  {"id":"n-2","terminals":[7,3],"title":"Running late","text":"25 minutes.",
                   "updated_ms":1793312004067}]}"#,
        )
        .unwrap();
        let mut t = terminals();
        t.set_bulletins(bulletins_from(&doc));
        let seattle = t.bulletins(&t.list[0]);
        let titles: Vec<&str> = seattle.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles, ["Running late", "Older"]);
        assert_eq!(t.bulletins(&t.list[1]).len(), 1);
        assert!(t.bulletins(&t.list[2]).is_empty());
        let lines = t.describe(&t.list[0], 1_793_299_000_000.0, 1, |_| String::new());
        assert_eq!(lines[1], "2 notices posted");
    }
}
