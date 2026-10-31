//! The debug panel: frame timings, the view and what's loaded.

use web_sys::Element;

use crate::camera::view_fragment;
use crate::geo::group_thousands;
use crate::perf::{FrameStats, format_bytes};
use crate::renderer::TERRAIN_SLOTS;
use crate::terminals::symbol_scale;
use crate::view::LAYER_CELLS;

use super::dom::copy_to_clipboard;
use super::{App, LAYER_TOGGLES, STYLE_CHOICES};

impl App {
    /// The debug switch in the settings: the debug panel.
    pub(super) fn set_debug(&mut self, on: bool) {
        if on {
            self.advice_check
                .want("check", "api/v1/stats/advice".to_string());
        }
        self.set_perf_visible(on);
        self.redraw = true;
    }

    /// The debug panel's switch: the names' placement drawn over the map.
    pub(super) fn set_label_debug(&mut self, on: bool) {
        self.labels.set_debug(on);
        self.redraw = true;
    }

    fn set_perf_visible(&mut self, visible: bool) {
        self.perf.visible = visible;
        if visible {
            let _ = self.perf.card.remove_attribute("hidden");
            self.update_perf_panel();
        } else {
            let _ = self.perf.card.set_attribute("hidden", "");
        }
    }

    /// The performance panel's rows, as (label, value).
    fn perf_rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        let mut row = |label, value: String| rows.push((label, value));
        match self.perf.stats.summary() {
            _ if self.idle => row("FPS", "idle: redraws only when something changes".into()),
            Some(s) => {
                row("FPS", format!("{:.1}", s.fps));
                row(
                    "Frame time",
                    format!("{:.1} ms (worst {:.1})", s.frame_ms, s.worst_frame_ms),
                );
                row(
                    "CPU per frame",
                    format!("{:.2} ms (worst {:.2})", s.cpu_ms, s.worst_cpu_ms),
                );
            }
            None => row("FPS", "measuring".into()),
        }
        // What's needed to see what was seen: where the view is (its
        // middle, how far it spans corner to corner, as the terminal
        // symbols scale by, and the camera's height), a link that opens
        // it, the window, what's shown and selected, and when.
        let (center, m_per_px, reach_m) = self.view_scale();
        let km = |m: f64| {
            if m >= 10_000.0 {
                format!("{:.0} km", m / 1000.0)
            } else {
                format!("{:.2} km", m / 1000.0)
            }
        };
        row(
            "View",
            format!(
                "{}; {} corner to corner, {:.1} m/px",
                center.display(),
                km(2.0 * reach_m),
                m_per_px
            ),
        );
        row(
            "Altitude",
            format!(
                "{}; terminal symbols at {:.0}%",
                km(self.camera.altitude),
                100.0 * symbol_scale(2.0 * reach_m)
            ),
        );
        let fragment = view_fragment(center, self.camera.altitude);
        let page = web_sys::window()
            .and_then(|w| w.location().href().ok())
            .map(|href| href.split('#').next().unwrap_or_default().to_string())
            .unwrap_or_default();
        row("Link", format!("{page}#{fragment}"));
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        row(
            "Window",
            format!(
                "{} × {} CSS px, DPR {dpr}",
                self.canvas.client_width(),
                self.canvas.client_height()
            ),
        );
        let style = STYLE_CHOICES
            .iter()
            .find(|&&(_, s)| s == self.style)
            .map_or("?", |&(name, _)| name);
        let layers: Vec<&str> = LAYER_TOGGLES
            .iter()
            .filter(|&&(_, bit)| self.layers & bit != 0)
            .map(|&(id, _)| id.trim_start_matches("layer-"))
            .collect();
        row(
            "Map",
            format!(
                "{style}; layers: {}",
                if layers.is_empty() {
                    "none".to_string()
                } else {
                    layers.join(", ")
                }
            ),
        );
        row(
            "Selected",
            self.finder.entry().map_or_else(
                || "nothing".to_string(),
                |e| format!("{} {}", e.kind, e.name),
            ),
        );
        row("Data", self.fleet_status.clone());
        // How the arrival advice held lately, over all terminals.
        if let Some(check) = self.advice_check.get("check") {
            let n: u32 = check.terminals.iter().map(|t| t.sailings).sum();
            let weighted = |f: fn(&ferry_model::api::TerminalCheck) -> f64| {
                check
                    .terminals
                    .iter()
                    .map(|t| f(t) * f64::from(t.sailings))
                    .sum::<f64>()
                    / f64::from(n.max(1))
            };
            row(
                "Advice check",
                if n == 0 {
                    format!("no sailings to check in {} days", check.days)
                } else {
                    format!(
                        "held {:.0}% of {n} sailings (gave {:.0}%), {} days",
                        100.0 * weighted(|t| t.held),
                        100.0 * weighted(|t| t.predicted),
                        check.days
                    )
                },
            );
        }
        row(
            "Time",
            js_sys::Date::new_0()
                .to_iso_string()
                .as_string()
                .unwrap_or_default(),
        );
        if self.layers & LAYER_CELLS != 0 {
            row(
                "Hex grid",
                format!(
                    "resolution {}, {} cells",
                    self.hex.resolution,
                    group_thousands(self.hex.cells as u64)
                ),
            );
        }
        row(
            "Canvas",
            format!(
                "{} × {} device px",
                self.canvas.width(),
                self.canvas.height()
            ),
        );
        let t = &self.terrain;
        row(
            "Terrain",
            format!(
                "{} regions ({} on stand-ins) · {}/{TERRAIN_SLOTS} tiles cached · {} loading · {} \
                 available",
                t.drawn,
                t.stand_ins,
                t.cached(),
                t.loading(),
                group_thousands(t.available() as u64)
            ),
        );
        let c = self.renderer.counts;
        row("Draw calls", c.draw_calls.to_string());
        row("Triangles", group_thousands(c.triangles));
        row("Line segments", group_thousands(c.line_segments));
        row(
            "GPU memory",
            format!("≈ {}", format_bytes(self.renderer.gpu_bytes())),
        );
        rows
    }

    pub(super) fn update_perf_panel(&self) {
        let html: String = self
            .perf_rows()
            .iter()
            .map(|(label, value)| format!("<dt>{label}</dt><dd>{value}</dd>"))
            .collect();
        self.perf.body.set_inner_html(&format!("<dl>{html}</dl>"));
    }

    /// Copies the performance panel as text, with the browser it ran in.
    pub(super) fn copy_perf(&self) {
        let mut text = String::from("ferry-demo debug\n");
        for (label, value) in self.perf_rows() {
            text += &format!("{label}: {value}\n");
        }
        if let Some(agent) = web_sys::window().and_then(|w| w.navigator().user_agent().ok()) {
            text += &format!("Browser: {agent}\n");
        }
        copy_to_clipboard(&text, &self.perf.copy);
    }
}

/// The performance panel and the numbers behind it.
pub(super) struct Perf {
    pub(super) stats: FrameStats,
    pub(super) card: Element,
    pub(super) body: Element,
    pub(super) copy: Element,
    pub(super) visible: bool,
    /// When the panel last refreshed (animation frame time, ms).
    pub(super) shown_at: f64,
}
