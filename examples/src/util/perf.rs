//! A rolling frame-rate meter.
//!
//! Every demo shows an FPS readout in its status bar. Most of these techniques
//! have a real cost (a 47-blob autotile lookup per cell, a per-cell hillshade,
//! a depth sort over every visible tile), and the whole point of a gallery is
//! being able to see what that cost is on each backend.

use std::time::Duration;

/// Number of frames averaged over. At 60fps this is a half-second window:
/// long enough that the number doesn't flicker, short enough that it reacts
/// when you pan into an expensive part of the map.
const WINDOW: usize = 30;

/// Rolling average of the last [`WINDOW`] frame deltas.
#[derive(Debug, Clone)]
pub struct FpsMeter {
    samples: [f32; WINDOW],
    next: usize,
    filled: usize,
}

impl Default for FpsMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl FpsMeter {
    /// A meter with no samples yet. [`fps`](Self::fps) returns 0 until the
    /// first [`record`](Self::record).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            samples: [0.0; WINDOW],
            next: 0,
            filled: 0,
        }
    }

    /// Record one frame's delta.
    pub const fn record(&mut self, delta: Duration) {
        self.samples[self.next] = delta.as_secs_f32();
        self.next = (self.next + 1) % WINDOW;
        if self.filled < WINDOW {
            self.filled += 1;
        }
    }

    /// Frames per second averaged over the window, or 0 before the first
    /// sample (and for a degenerate all-zero window, which the headless
    /// backend's synthetic clock can produce).
    #[must_use]
    pub fn fps(&self) -> f32 {
        if self.filled == 0 {
            return 0.0;
        }
        let total: f32 = self.samples[..self.filled].iter().sum();
        if total <= 0.0 {
            return 0.0;
        }
        self.filled as f32 / total
    }
}

#[cfg(test)]
mod tests {
    use super::{FpsMeter, WINDOW};
    use std::time::Duration;

    #[test]
    fn reports_zero_before_any_sample() {
        assert!((FpsMeter::new().fps() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn averages_a_steady_frame_rate() {
        let mut meter = FpsMeter::new();
        for _ in 0..WINDOW {
            meter.record(Duration::from_millis(20));
        }
        assert!((meter.fps() - 50.0).abs() < 0.01, "got {}", meter.fps());
    }

    #[test]
    fn window_rolls_over_and_forgets_old_samples() {
        let mut meter = FpsMeter::new();
        for _ in 0..WINDOW {
            meter.record(Duration::from_millis(100));
        }
        for _ in 0..WINDOW {
            meter.record(Duration::from_millis(10));
        }
        assert!((meter.fps() - 100.0).abs() < 0.01, "got {}", meter.fps());
    }

    #[test]
    fn zero_deltas_do_not_divide_by_zero() {
        let mut meter = FpsMeter::new();
        meter.record(Duration::ZERO);
        assert!(meter.fps().is_finite());
    }
}
