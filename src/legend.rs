//! The color key panel: HTML describing the current color mode and the
//! overlays that are switched on.

use std::fmt::Write;

use h3o::Resolution;

use crate::palette::{self, Stop};
use crate::terminals::NEARLY_FULL_CARS;
use crate::view::{LAYER_CELLS, LAYER_FERRIES, STYLE_HYPSOMETRIC};

/// `fleet_status` says how current the ferry positions are.
pub fn html(
    style: u32,
    layers: u32,
    resolution: Resolution,
    exaggeration: f64,
    fleet_status: &str,
) -> String {
    let mut out = String::new();
    if layers & LAYER_FERRIES != 0 {
        let ink = palette::css(palette::VESSEL_INK);
        let paper = palette::css(palette::VESSEL_PAPER);
        let idle = palette::css(palette::VESSEL_IDLE);
        out += "<h2>Ferries</h2>";
        out += &symbol(&hull(&ink, "none", ""), "Under way");
        out += &symbol(&hull(&paper, &ink, ""), "At dock");
        out += &symbol(
            &hull(&paper, &idle, "stroke-dasharray=\"2 1.5\""),
            "Out of service",
        );
        out += &symbol(
            &format!("<path d=\"M2 7H23M23 4V10\" stroke=\"{ink}\" stroke-width=\"1.2\"/>"),
            "Where it will be in 3 minutes",
        );
        out += &symbol(
            &format!(
                "<defs><linearGradient id=\"wake\" gradientUnits=\"userSpaceOnUse\" \
                 x1=\"2\" x2=\"24\"><stop offset=\"0\" stop-color=\"{ink}\" \
                 stop-opacity=\"0\"/><stop offset=\"1\" stop-color=\"{ink}\" \
                 stop-opacity=\"0.6\"/></linearGradient></defs>\
                 <path d=\"M2 7H24\" stroke=\"url(#wake)\" stroke-width=\"3\"/>"
            ),
            "Its wake, the last 3 minutes",
        );
        out += &symbol(
            &format!(
                "<path d=\"M2 7H24\" stroke=\"{ink}\" stroke-opacity=\"0.6\" \
                 stroke-width=\"1.2\" stroke-dasharray=\"4.2 2.8\"/>"
            ),
            "Ferry route (OpenStreetMap)",
        );
        let warm = palette::css(palette::TERMINAL_NEARLY_FULL);
        out += &symbol(&anchor(&ink), "Terminal");
        out += &symbol(&gauge(&ink, 0.6), "Drive-up space left on its next sailing");
        let _ = write!(
            out,
            "{}",
            symbol(
                &gauge(&warm, 0.08),
                &format!("Nearly full: {NEARLY_FULL_CARS} cars or fewer")
            )
        );
        out += &symbol(&notice(&ink, &paper), "Notice from WSF: hover or click");
        let _ = write!(
            out,
            "<p>{}</p><p>Boats and terminals are drawn to scale with the \
             terrain, so they're small when zoomed out. Names appear as you zoom in; \
             hover a boat or terminal for details. Positions from Washington \
             State Ferries, carried forward between reports.</p>",
            escape(fleet_status)
        );
    }
    let meters = |v: f64| format!("{} m", signed(v));
    if style == STYLE_HYPSOMETRIC {
        out += "<h2>Terrain</h2>";
        out += &ramp(
            palette::TERRAIN,
            -3000.0,
            3200.0,
            &[-3000.0, -1500.0, 0.0, 1500.0, 3000.0],
            true,
            |v| format!("{} km", signed(v / 1000.0)),
        );
        out += "<p>Height above or below sea level.</p>";
    } else {
        out += "<h2>Land</h2>";
        out += &ramp(
            palette::CARTO_LAND,
            0.0,
            2800.0,
            &[500.0, 1500.0, 2500.0],
            true,
            |v| format!("{v:.0} m"),
        );
        out += "<h2>Water</h2>";
        // Puget Sound's depths, which the ferries cross; the open ocean
        // runs on past the end of the bar.
        out += &ramp(
            palette::CARTO_WATER,
            -300.0,
            0.0,
            &[-300.0, -200.0, -100.0, 0.0],
            true,
            meters,
        );
        out += &line(
            &palette::css(palette::CONTOUR),
            "Contours every 20, 100 and 500 m",
        );
        out += &line(
            &palette::css(palette::DEPTH_CONTOUR),
            "Depth contours every 10 and 50 m",
        );
        out += &line(&palette::css(palette::COASTLINE), "Shoreline");
        out += "<p>Finer contours, and smaller lakes and rivers, appear as \
                you zoom in. Water from ESA WorldCover; streams narrower \
                than about 20 m don't show.</p>";
    }
    let _ = write!(
        out,
        "<p>Relief is shaded by a sun in the northwest, at {exaggeration}× \
         vertical exaggeration.</p>"
    );
    out += &swatch(&palette::css(palette::BACKDROP), "Not loaded yet");
    if layers & LAYER_CELLS != 0 {
        out += "<h2>Hex grid</h2>";
        // Corner to corner is twice the edge length.
        let across_m = 2000.0 * resolution.edge_length_km();
        let across = if across_m >= 2000.0 {
            format!("{:.0} km", across_m / 1000.0)
        } else {
            format!("{:.0} m", (across_m / 10.0).round() * 10.0)
        };
        let _ = write!(
            out,
            "<p>Uber's H3 at resolution {resolution}: cells about {across} \
             across. Each finer resolution splits a cell into about seven, \
             and the map steps down to it as you zoom in.</p>"
        );
    }
    out
}

/// A horizontal gradient bar over `lo..hi` with labeled ticks. With
/// `open_ends`, values can run past the bar (the map shows them in the end
/// colors), so the end ticks read "≤ lo" and "≥ hi".
fn ramp(
    stops: &[Stop],
    lo: f64,
    hi: f64,
    ticks: &[f64],
    open_ends: bool,
    label: impl Fn(f64) -> String,
) -> String {
    let at = |v: f64| 100.0 * (v - lo) / (hi - lo);
    let gradient: Vec<String> = stops
        .iter()
        .map(|s| format!("{} {:.2}%", palette::css(s.srgb), at(s.value)))
        .collect();
    let mut out = format!(
        "<div class=\"bar\" style=\"background:linear-gradient(to right, {})\"></div>",
        gradient.join(", ")
    );
    out += "<div class=\"ticks\">";
    for &t in ticks {
        let prefix = match t {
            t if open_ends && t <= lo => "\u{2264}",
            t if open_ends && t >= hi => "\u{2265}",
            _ => "",
        };
        let _ = write!(
            out,
            "<span style=\"left:{:.2}%\">{prefix}{}</span>",
            at(t),
            label(t)
        );
    }
    out += "</div>";
    out
}

/// `+1.5` or `−4` with a real minus sign, trimming `.0`.
fn signed(v: f64) -> String {
    let body = if v.fract() == 0.0 {
        format!("{:.0}", v.abs())
    } else {
        format!("{:.1}", v.abs())
    };
    match v {
        v if v < 0.0 => format!("\u{2212}{body}"),
        v if v > 0.0 => format!("+{body}"),
        _ => body,
    }
}

fn line(css: &str, label: &str) -> String {
    format!("<div class=\"row\"><i class=\"line\" style=\"background:{css}\"></i>{label}</div>")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A ferry hull lying east, as drawn on the map (markers.wgsl).
fn hull(fill: &str, stroke: &str, extra: &str) -> String {
    format!(
        "<path d=\"M2 7Q2 3 7 3H19Q24 3 24 7Q24 11 19 11H7Q2 11 2 7Z\" fill=\"{fill}\" \
         stroke=\"{stroke}\" stroke-width=\"1.2\" {extra}/>"
    )
}

/// A terminal's anchor, as drawn on the map (markers.wgsl).
fn anchor(ink: &str) -> String {
    format!(
        "<g fill=\"none\" stroke=\"{ink}\" stroke-width=\"1.2\" stroke-linecap=\"round\">\
         <circle cx=\"13\" cy=\"2.6\" r=\"1.3\"/><path d=\"M13 3.9V12.6M10.5 5.3H15.5\
         M8.6 8.6Q9.2 12.6 13 12.6Q16.8 12.6 17.4 8.6M8.6 8.6L8 10M17.4 8.6L18 10\"/></g>"
    )
}

/// A notice dot, as web.rs places it over the map (#notes in index.html).
fn notice(ink: &str, paper: &str) -> String {
    format!(
        "<circle cx=\"13\" cy=\"7\" r=\"5.5\" fill=\"{ink}\" stroke=\"{paper}\" \
         stroke-width=\"1\"/><text x=\"13\" y=\"9.6\" text-anchor=\"middle\" \
         font-family=\"Georgia, serif\" font-style=\"italic\" font-weight=\"600\" \
         font-size=\"7.5\" fill=\"{paper}\">i</text>"
    )
}

/// A space gauge on its own, lying across the chip, filled to `left`,
/// frame and all in `color`.
fn gauge(color: &str, left: f64) -> String {
    format!(
        "<rect x=\"3\" y=\"5\" width=\"20\" height=\"4\" fill=\"none\" stroke=\"{color}\" \
         stroke-width=\"0.8\"/><rect x=\"3\" y=\"5\" width=\"{:.1}\" height=\"4\" \
         fill=\"{color}\"/>",
        (20.0 * left).max(1.5)
    )
}

/// A small drawing on a chip of map paper, so ink symbols show on the
/// dark panel.
fn symbol(svg: &str, label: &str) -> String {
    format!(
        "<div class=\"row\"><svg class=\"chip\" viewBox=\"0 0 26 14\" aria-hidden=\"true\">\
         {svg}</svg>{label}</div>"
    )
}

fn swatch(css: &str, label: &str) -> String {
    format!("<div class=\"row\"><i class=\"swatch\" style=\"background:{css}\"></i>{label}</div>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_style_and_only_active_overlays() {
        use crate::view::STYLE_CARTOGRAPHIC;
        let carto = html(STYLE_CARTOGRAPHIC, 0, Resolution::Nine, 2.5, "");
        assert!(carto.contains("<h2>Land</h2>") && carto.contains("<h2>Water</h2>"));
        assert!(carto.contains("Contours every") && carto.contains("2.5×"));
        assert!(carto.contains("\u{2264}\u{2212}300 m"));
        assert!(!carto.contains("Hex grid") && !carto.contains("Ferries"));
        let hypso = html(STYLE_HYPSOMETRIC, LAYER_CELLS, Resolution::Nine, 1.0, "");
        assert!(hypso.contains("<h2>Terrain</h2>") && hypso.contains("+1.5 km"));
        assert!(hypso.contains("<h2>Hex grid</h2>") && hypso.contains("about 400 m across"));
        let coarse = html(STYLE_HYPSOMETRIC, LAYER_CELLS, Resolution::Four, 1.0, "");
        assert!(coarse.contains("resolution 4") && coarse.contains("about 52 km across"));
        let ferries = html(
            STYLE_CARTOGRAPHIC,
            LAYER_FERRIES,
            Resolution::Four,
            1.0,
            "Unavailable: <HTTP 502>",
        );
        assert!(ferries.starts_with("<h2>Ferries</h2>") && ferries.contains("Under way"));
        assert!(ferries.contains("<svg class=\"chip\"") && ferries.contains("3 minutes"));
        assert!(ferries.contains("<circle") && ferries.contains("Terminal"));
        assert!(ferries.contains("Ferry route (OpenStreetMap)"));
        assert!(ferries.contains("Nearly full: 10 cars or fewer"));
        assert!(ferries.contains("Notice from WSF"));
        assert!(ferries.contains("Unavailable: &lt;HTTP 502&gt;"));
    }

    #[test]
    fn ramp_positions_stops_by_value() {
        let stops = [
            Stop {
                value: 20.0,
                srgb: [0.0; 3],
            },
            Stop {
                value: 45.0,
                srgb: [0.5; 3],
            },
            Stop {
                value: 70.0,
                srgb: [1.0; 3],
            },
        ];
        let h = ramp(&stops, 20.0, 70.0, &[20.0, 45.0], true, |v| format!("{v}"));
        assert!(h.contains("0.00%") && h.contains("50.00%") && h.contains("100.00%"));
        assert!(h.contains("left:50.00%") && h.contains("\u{2264}20"));
    }
}
