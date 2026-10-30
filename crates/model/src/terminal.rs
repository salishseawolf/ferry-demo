//! A Washington State Ferries terminal, as the route network sees it.

use crate::geo::LonLat;

#[derive(Clone, Debug, PartialEq)]
pub struct Terminal {
    pub id: u32,
    pub name: String,
    pub position: LonLat,
}
