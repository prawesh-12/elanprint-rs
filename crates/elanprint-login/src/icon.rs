//! The mark and the outcome glyphs, drawn with the painter.
//!
//! The mark is geometric, not an illustration of a finger: uniform arcs,
//! uniform gaps, one flat colour. It carries state through that colour, so
//! progress lives in the dots instead.

/// Concentric arcs in the mark. Fixed: the mark is not a progress indicator.
const ARCS: usize = 6;

pub struct Mark {
    pub size: f32,
    /// The whole mark is this one colour.
    pub colour: egui::Color32,
    /// Opacity multiplier for the pulse.
    pub alpha: f32,
}

impl Mark {
    pub fn new(size: f32, colour: egui::Color32) -> Self {
        Self {
            size,
            colour,
            alpha: 1.0,
        }
    }
}

pub fn mark(ui: &mut egui::Ui, mark: &Mark) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(mark.size, mark.size), egui::Sense::hover());
    let size = mark.size;
    let centre = rect.center();
    let stroke = egui::Stroke::new(
        (size * 0.055).max(2.0),
        fade(mark.colour, mark.alpha),
    );

    let inner = size * 0.13;
    let outer = size * 0.46;
    let step = (outer - inner) / (ARCS - 1) as f32;
    for i in 0..ARCS {
        let radius = inner + step * i as f32;
        let start = DOWN + GAP_HALF;
        let points = arc_span(centre, radius, start, TAU - 2.0 * GAP_HALF, 48);
        ui.painter().add(egui::Shape::line(points, stroke));
    }
    response
}

const TAU: f32 = std::f32::consts::TAU;
/// Screen y grows downward, so this points at the bottom of the mark.
const DOWN: f32 = std::f32::consts::FRAC_PI_2;
/// Half the opening at the bottom, the same on every arc.
const GAP_HALF: f32 = 0.55;

pub fn check(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let w = rect.width();
    let h = rect.height();
    let width = w.min(h) * 0.10;
    let p1 = egui::pos2(rect.min.x + w * 0.24, rect.min.y + h * 0.52);
    let p2 = egui::pos2(rect.min.x + w * 0.43, rect.min.y + h * 0.70);
    let p3 = egui::pos2(rect.min.x + w * 0.76, rect.min.y + h * 0.32);
    ui.painter().add(egui::Shape::line(
        vec![p1, p2, p3],
        egui::Stroke::new(width, color),
    ));
}

pub fn cross(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let w = rect.width();
    let h = rect.height();
    let width = w.min(h) * 0.10;
    let stroke = egui::Stroke::new(width, color);
    let (a, b) = (0.30, 0.70);
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

fn fade(color: egui::Color32, factor: f32) -> egui::Color32 {
    color.gamma_multiply(factor.clamp(0.0, 1.0))
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
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a = start + sweep * t;
        points.push(center + egui::vec2(a.cos(), a.sin()) * radius);
    }
    points
}
