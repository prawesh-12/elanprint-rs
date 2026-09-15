//! Animation controller for the login window.
//!
//! One struct owns every time-varying effect. The window queries it per
//! frame and repaints only while something moves.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulse {
    Off,
    Breathe,
}

/// Effect timestamps. Queries take `now` so frames stay consistent.
pub struct Fx {
    pulse: Pulse,
    pulse_start: std::time::Instant,
    flash_at: Option<std::time::Instant>,
    flash_color: egui::Color32,
    shake_at: Option<std::time::Instant>,
    ripple_at: Option<std::time::Instant>,
    sweep_from: Option<std::time::Instant>,
}

impl Fx {
    pub fn new() -> Self {
        Self {
            pulse: Pulse::Off,
            pulse_start: std::time::Instant::now(),
            flash_at: None,
            flash_color: egui::Color32::TRANSPARENT,
            shake_at: None,
            ripple_at: None,
            sweep_from: None,
        }
    }

    /// One expanding ring, 600 ms. Fired on every reply the chip sends.
    pub fn ripple(&mut self) {
        self.ripple_at = Some(std::time::Instant::now());
    }

    /// Ring progress 0 to 1, if one is running.
    pub fn ripple_now(&self, now: std::time::Instant) -> Option<f32> {
        let at = self.ripple_at?;
        let t = now.saturating_duration_since(at).as_secs_f32() / 0.6;
        (t < 1.0).then_some(t)
    }

    pub fn set_sweep(&mut self, on: bool) {
        match (on, self.sweep_from) {
            (true, None) => self.sweep_from = Some(std::time::Instant::now()),
            (false, _) => self.sweep_from = None,
            _ => {}
        }
    }

    /// Band position 0 to 1, if the sweep is running.
    pub fn sweep_now(&self, now: std::time::Instant) -> Option<f32> {
        let from = self.sweep_from?;
        let t = now.saturating_duration_since(from).as_secs_f32();
        Some((t / 1.9).fract())
    }

    /// Restarts the phase.
    pub fn set_pulse(&mut self, pulse: Pulse) {
        self.pulse = pulse;
        self.pulse_start = std::time::Instant::now();
    }

    /// One-shot color overlay for 150 ms.
    pub fn flash(&mut self, color: egui::Color32) {
        self.flash_color = color;
        self.flash_at = Some(std::time::Instant::now());
    }

    /// Damped horizontal shake for 450 ms.
    pub fn shake(&mut self) {
        self.shake_at = Some(std::time::Instant::now());
    }

    pub fn pulse_alpha(&self, now: std::time::Instant) -> f32 {
        let t = now
            .saturating_duration_since(self.pulse_start)
            .as_secs_f32();
        match self.pulse {
            Pulse::Off => 1.0,
            Pulse::Breathe => 0.775 + 0.225 * (std::f32::consts::TAU * t / 1.5).sin(),
        }
    }

    pub fn flash_now(&self, now: std::time::Instant) -> Option<egui::Color32> {
        let at = self.flash_at?;
        let t = now.saturating_duration_since(at).as_secs_f32();
        if t < 0.15 {
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

    pub fn fast_animating(&self, now: std::time::Instant) -> bool {
        if self.flash_now(now).is_some() || self.ripple_now(now).is_some() {
            return true;
        }
        if self.sweep_from.is_some() {
            return true;
        }
        self.shake_at.is_some_and(|at| {
            now.saturating_duration_since(at).as_secs_f32() < 0.45
        })
    }
}
