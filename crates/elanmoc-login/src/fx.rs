//! Animation controller for the login window.
//!
//! One struct owns every time-varying effect: breathing and scanning pulse,
//! one-shot color flash, shake offset, touch ripple. The window queries it
//! per frame and repaints only while something moves.

/// Pulse mode of the fingerprint mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulse {
    Off,
    Breathe,
    Scan,
}

/// Effect timestamps. Queries take `now` so frames stay consistent.
pub struct Fx {
    pulse: Pulse,
    pulse_start: std::time::Instant,
    flash_at: Option<std::time::Instant>,
    flash_color: egui::Color32,
    shake_at: Option<std::time::Instant>,
    ripple_at: Option<std::time::Instant>,
}

impl Fx {
    /// All effects off.
    pub fn new() -> Self {
        Self {
            pulse: Pulse::Off,
            pulse_start: std::time::Instant::now(),
            flash_at: None,
            flash_color: egui::Color32::TRANSPARENT,
            shake_at: None,
            ripple_at: None,
        }
    }

    /// Switch pulse mode, restarting its phase.
    pub fn set_pulse(&mut self, pulse: Pulse) {
        self.pulse = pulse;
        self.pulse_start = std::time::Instant::now();
    }

    /// One-shot color overlay for 700 ms.
    pub fn flash(&mut self, color: egui::Color32) {
        self.flash_color = color;
        self.flash_at = Some(std::time::Instant::now());
    }

    /// Damped horizontal shake for 450 ms.
    pub fn shake(&mut self) {
        self.shake_at = Some(std::time::Instant::now());
    }

    /// Expanding ring for 800 ms.
    pub fn ripple(&mut self) {
        self.ripple_at = Some(std::time::Instant::now());
    }

    /// Icon opacity multiplier for the pulse mode.
    pub fn pulse_alpha(&self, now: std::time::Instant) -> f32 {
        let t = now
            .saturating_duration_since(self.pulse_start)
            .as_secs_f32();
        match self.pulse {
            Pulse::Off => 1.0,
            Pulse::Breathe => 0.775 + 0.225 * (std::f32::consts::TAU * t / 2.4).sin(),
            Pulse::Scan => 0.825 + 0.175 * (std::f32::consts::TAU * t / 0.9).sin(),
        }
    }

    /// Active flash color, if any.
    pub fn flash_now(&self, now: std::time::Instant) -> Option<egui::Color32> {
        let at = self.flash_at?;
        let t = now.saturating_duration_since(at).as_secs_f32();
        if t < 0.7 {
            Some(self.flash_color)
        } else {
            None
        }
    }

    /// Horizontal shake offset in pixels, else zero.
    pub fn shake_dx(&self, now: std::time::Instant) -> f32 {
        let Some(at) = self.shake_at else {
            return 0.0;
        };
        let t = now.saturating_duration_since(at).as_secs_f32();
        if t >= 0.45 {
            return 0.0;
        }
        12.0 * (1.0 - t / 0.45) * (std::f32::consts::TAU * 3.0 * t / 0.45).sin()
    }

    /// Ripple radius and alpha, while one runs.
    pub fn ripple_now(&self, now: std::time::Instant) -> Option<(f32, f32)> {
        let at = self.ripple_at?;
        let t = now.saturating_duration_since(at).as_secs_f32();
        if t >= 0.8 {
            return None;
        }
        let k = t / 0.8;
        Some((46.0 * k, 1.0 - k))
    }

    /// Whether any effect needs another frame.
    pub fn animating(&self, now: std::time::Instant) -> bool {
        if !matches!(self.pulse, Pulse::Off) {
            return true;
        }
        if self.flash_now(now).is_some() {
            return true;
        }
        if self.ripple_now(now).is_some() {
            return true;
        }
        self.shake_at.is_some_and(|at| {
            now.saturating_duration_since(at).as_secs_f32() < 0.45
        })
    }
}
