//! Performance statistics: frame rate and timings over a sliding window.

use std::collections::VecDeque;

/// Frames older than this (ms) drop out of the statistics.
const WINDOW_MS: f64 = 1000.0;

/// Per-frame timings for the last [`WINDOW_MS`].
#[derive(Default)]
pub struct FrameStats {
    /// (frame start time, CPU time spent on the frame), both in ms.
    frames: VecDeque<(f64, f64)>,
}

#[derive(Debug, PartialEq)]
pub struct Summary {
    pub fps: f64,
    /// Time between frames: mean and worst (ms).
    pub frame_ms: f64,
    pub worst_frame_ms: f64,
    /// Our own work per frame: mean and worst (ms).
    pub cpu_ms: f64,
    pub worst_cpu_ms: f64,
}

impl FrameStats {
    /// Records a frame that started at `time_ms` (the animation frame
    /// timestamp) and took `cpu_ms` of our own work.
    pub fn record(&mut self, time_ms: f64, cpu_ms: f64) {
        self.frames.push_back((time_ms, cpu_ms));
        while self
            .frames
            .front()
            .is_some_and(|&(t, _)| time_ms - t > WINDOW_MS)
        {
            self.frames.pop_front();
        }
    }

    /// Forgets the frames so far: after a pause in drawing (the map redraws
    /// only when something changes), the gap isn't one slow frame.
    pub fn pause(&mut self) {
        self.frames.clear();
    }

    /// Statistics over the window; None until there are two frames.
    pub fn summary(&self) -> Option<Summary> {
        if self.frames.len() < 2 {
            return None;
        }
        let first = self.frames.front()?.0;
        let last = self.frames.back()?.0;
        let intervals = self.frames.len() - 1;
        let span = last - first;
        let worst_frame_ms = self
            .frames
            .iter()
            .zip(self.frames.iter().skip(1))
            .map(|(a, b)| b.0 - a.0)
            .fold(0.0, f64::max);
        let cpu: Vec<f64> = self.frames.iter().map(|f| f.1).collect();
        Some(Summary {
            fps: if span > 0.0 {
                1000.0 * intervals as f64 / span
            } else {
                0.0
            },
            frame_ms: span / intervals as f64,
            worst_frame_ms,
            cpu_ms: cpu.iter().sum::<f64>() / cpu.len() as f64,
            worst_cpu_ms: cpu.iter().copied().fold(0.0, f64::max),
        })
    }
}

/// e.g. `41.3 MB`, `512 KB`.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.0} KB", b / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_sixty_fps() {
        let mut s = FrameStats::default();
        assert!(s.summary().is_none());
        for i in 0..120 {
            s.record(i as f64 * 1000.0 / 60.0, 2.0);
        }
        let sum = s.summary().unwrap();
        assert!((sum.fps - 60.0).abs() < 0.01, "{sum:?}");
        assert!((sum.frame_ms - 16.667).abs() < 0.01);
        assert_eq!(sum.cpu_ms, 2.0);
    }

    #[test]
    fn window_drops_old_frames_and_tracks_worst() {
        let mut s = FrameStats::default();
        // A slow stretch long ago, then a hitch, then steady 30 fps.
        s.record(0.0, 50.0);
        s.record(100.0, 50.0);
        let mut t = 2000.0;
        s.record(t, 1.0);
        t += 100.0;
        s.record(t, 9.0);
        for _ in 0..20 {
            t += 1000.0 / 30.0;
            s.record(t, 1.0);
        }
        let sum = s.summary().unwrap();
        assert!(sum.worst_cpu_ms == 9.0, "old 50 ms frames dropped: {sum:?}");
        assert!((sum.worst_frame_ms - 100.0).abs() < 1e-9, "{sum:?}");
    }

    #[test]
    fn bytes() {
        assert_eq!(format_bytes(512 * 1024), "512 KB");
        assert_eq!(format_bytes(41 * 1024 * 1024 + 300 * 1024), "41.3 MB");
    }

    #[test]
    fn pause_starts_afresh() {
        let mut stats = FrameStats::default();
        stats.record(0.0, 1.0);
        stats.record(16.0, 1.0);
        stats.pause();
        assert_eq!(stats.summary(), None);
        stats.record(500.0, 1.0);
        stats.record(516.0, 1.0);
        assert_eq!(stats.summary().unwrap().worst_frame_ms, 16.0);
    }
}
