//! Fingerprint mark, drawn with the painter.
//!
//! The mark is the whole feedback surface: one ridge lights per accepted
//! sample, a ripple fires on every reply the chip sends, and a slow sweep
//! runs while the sensor is waiting. A person watching it can tell the
//! reader is alive without reading a word.

/// Ridge arcs drawn, one per enroll sample.
pub const RIDGES: u8 = 8;

/// Everything the mark needs for one frame.
pub struct Mark {
    /// Side of the square the mark is drawn in.
    pub size: f32,
    /// Ridges lit from the inside out.
    pub lit: u8,
    /// Ridges in total, so a different stage count still fills the mark.
    pub total: u8,
    /// Unlit ridge colour.
    pub base: egui::Color32,
    /// Lit ridge colour.
    pub accent: egui::Color32,
    /// Expanding ring, 0 to 1, drawn on every chip reply.
    pub ripple: Option<f32>,
    /// Scan band position, 0 to 1 top to bottom, while waiting for a finger.
    pub sweep: Option<f32>,
    /// Opacity multiplier for the breathing pulse.
    pub alpha: f32,
}

impl Mark {
    /// A mark with nothing lit and no animation.
    pub fn new(size: f32, base: egui::Color32, accent: egui::Color32) -> Self {
        Self {
            size,
            lit: 0,
            total: RIDGES,
            base,
            accent,
            ripple: None,
            sweep: None,
            alpha: 1.0,
        }
    }
}

/// Draw the fingerprint and return its response.
pub fn fingerprint(ui: &mut egui::Ui, mark: &Mark) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(mark.size, mark.size), egui::Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    let size = mark.size;
    let focus = rect.center() + egui::vec2(0.0, size * 0.13);
    let ridges = mark.total.max(1);
    let width = (size * 0.036).max(1.6);

    if let Some(t) = mark.sweep {
        draw_sweep(&painter, rect, t, mark.accent);
    }

    let mut idx = 0u8;
    while idx < ridges {
        let step = f32::from(idx) / f32::from(ridges);
        let radius = size * (0.075 + 0.345 * step);
        let half_gap = 0.42 + 0.46 * step;
        let leg = size * 0.11 * step;
        let on = idx < mark.lit;
        let colour = if on { mark.accent } else { mark.base };
        let colour = fade(colour, if on { 1.0 } else { 0.55 } * mark.alpha);
        let mut points = arc_points(focus, radius, half_gap, 44);
        if leg > 0.0 {
            if let Some(first) = points.first().copied() {
                points.insert(0, first + egui::vec2(0.0, leg));
            }
            if let Some(last) = points.last().copied() {
                points.push(last + egui::vec2(0.0, leg));
            }
        }
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(width, colour),
        ));
        idx += 1;
    }

    let core = if mark.lit > 0 { mark.accent } else { mark.base };
    painter.circle_filled(focus, (size * 0.035).max(1.5), fade(core, mark.alpha));

    if let Some(t) = mark.ripple {
        let radius = size * (0.10 + 0.46 * t);
        let colour = fade(mark.accent, (1.0 - t) * 0.85);
        painter.circle_stroke(
            focus,
            radius,
            egui::Stroke::new((size * 0.022).max(1.2), colour),
        );
    }

    response
}

/// Horizontal scan band, so a waiting sensor never looks frozen.
fn draw_sweep(painter: &egui::Painter, rect: egui::Rect, t: f32, colour: egui::Color32) {
    let y = rect.min.y + rect.height() * t;
    let band = rect.height() * 0.06;
    let mut step = -3i32;
    while step <= 3 {
        let offset = band * step as f32 * 0.5;
        let dist = (step.abs() as f32) / 3.0;
        let line_y = y + offset;
        if line_y < rect.min.y || line_y > rect.max.y {
            step += 1;
            continue;
        }
        painter.line_segment(
            [
                egui::pos2(rect.min.x, line_y),
                egui::pos2(rect.max.x, line_y),
            ],
            egui::Stroke::new(1.0, fade(colour, 0.20 * (1.0 - dist))),
        );
        step += 1;
    }
}

/// Checkmark polyline inside rect.
pub fn check(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let w = rect.width();
    let h = rect.height();
    let width = w.min(h) * 0.12;
    let p1 = egui::pos2(rect.min.x + w * 0.22, rect.min.y + h * 0.53);
    let p2 = egui::pos2(rect.min.x + w * 0.43, rect.min.y + h * 0.70);
    let p3 = egui::pos2(rect.min.x + w * 0.78, rect.min.y + h * 0.31);
    ui.painter().add(egui::Shape::line(
        vec![p1, p2, p3],
        egui::Stroke::new(width, color),
    ));
}

/// Cross polyline inside rect, for a refused finger.
pub fn cross(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let w = rect.width();
    let h = rect.height();
    let width = w.min(h) * 0.12;
    let stroke = egui::Stroke::new(width, color);
    let (a, b) = (0.28, 0.72);
    ui.painter().line_segment(
        [
            egui::pos2(rect.min.x + w * a, rect.min.y + h * a),
            egui::pos2(rect.min.x + w * b, rect.min.y + h * b),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(rect.min.x + w * b, rect.min.y + h * a),
            egui::pos2(rect.min.x + w * a, rect.min.y + h * b),
        ],
        stroke,
    );
}

/// Progress ring around the mark, filled clockwise from the top.
pub fn ring(ui: &egui::Ui, rect: egui::Rect, done: u8, total: u8, track: egui::Color32, fill: egui::Color32) {
    let centre = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5;
    let width = (radius * 0.07).max(2.0);
    let full = arc_span(centre, radius, 0.0, std::f32::consts::TAU, 96);
    ui.painter()
        .add(egui::Shape::line(full, egui::Stroke::new(width, track)));
    if total == 0 || done == 0 {
        return;
    }
    let frac = (f32::from(done) / f32::from(total)).clamp(0.0, 1.0);
    let start = -std::f32::consts::FRAC_PI_2;
    let points = arc_span(centre, radius, start, std::f32::consts::TAU * frac, 96);
    ui.painter()
        .add(egui::Shape::line(points, egui::Stroke::new(width, fill)));
}

fn fade(color: egui::Color32, factor: f32) -> egui::Color32 {
    color.gamma_multiply(factor.clamp(0.0, 1.0))
}

/// Arc wrapping the top, gap below.
fn arc_points(center: egui::Pos2, radius: f32, half_gap: f32, steps: usize) -> Vec<egui::Pos2> {
    let down = std::f32::consts::FRAC_PI_2;
    let from = down + half_gap;
    let to = down - half_gap + std::f32::consts::TAU;
    arc_span(center, radius, from, to - from, steps)
}

/// `steps` points along `sweep` radians from `start`.
fn arc_span(
    center: egui::Pos2,
    radius: f32,
    start: f32,
    sweep: f32,
    steps: usize,
) -> Vec<egui::Pos2> {
    let mut points = Vec::with_capacity(steps + 1);
    let mut i = 0;
    while i <= steps {
        let t = i as f32 / steps as f32;
        let a = start + sweep * t;
        points.push(center + egui::vec2(a.cos(), a.sin()) * radius);
        i += 1;
    }
    points
}
