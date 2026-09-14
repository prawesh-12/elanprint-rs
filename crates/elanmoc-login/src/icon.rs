//! Fingerprint mark and check overlay, drawn with the painter.

/// Fingerprint whorl: ridge arcs opening downward plus a focus dot.
pub fn fingerprint(ui: &mut egui::Ui, size: f32, color: egui::Color32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let focus = rect.center() + egui::vec2(0.0, size * 0.14);
    let width = (size * 0.055).max(1.5);
    let stroke = egui::Stroke::new(width, color);
    let radii = [0.09 * size, 0.155 * size, 0.22 * size, 0.285 * size];
    let gaps = [0.45, 0.55, 0.68, 0.85];
    let legs = [0.0, 0.03 * size, 0.07 * size, 0.11 * size];
    let mut idx = 0;
    while idx < radii.len() {
        let mut points = arc_points(focus, radii[idx], gaps[idx], 40);
        let leg = legs[idx];
        if leg > 0.0 {
            if let Some(first) = points.first().copied() {
                points.insert(0, first + egui::vec2(0.0, leg));
            }
            if let Some(last) = points.last().copied() {
                points.push(last + egui::vec2(0.0, leg));
            }
        }
        ui.painter().add(egui::Shape::line(points, stroke));
        idx += 1;
    }
    ui.painter()
        .circle_filled(focus, (size * 0.045).max(1.5), color);
    response
}

/// Checkmark polyline inside rect.
pub fn check(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let w = rect.width();
    let h = rect.height();
    let width = w.min(h) * 0.14;
    let p1 = egui::pos2(rect.min.x + w * 0.20, rect.min.y + h * 0.54);
    let p2 = egui::pos2(rect.min.x + w * 0.44, rect.min.y + h * 0.70);
    let p3 = egui::pos2(rect.min.x + w * 0.80, rect.min.y + h * 0.30);
    ui.painter().add(egui::Shape::line(
        vec![p1, p2, p3],
        egui::Stroke::new(width, color),
    ));
}

/// Arc wrapping the top, gap below.
fn arc_points(center: egui::Pos2, radius: f32, half_gap: f32, steps: usize) -> Vec<egui::Pos2> {
    let down = std::f32::consts::FRAC_PI_2;
    let from = down + half_gap;
    let to = down - half_gap + std::f32::consts::TAU;
    let mut points = Vec::with_capacity(steps + 1);
    let mut i = 0;
    while i <= steps {
        let t = i as f32 / steps as f32;
        let a = from + (to - from) * t;
        points.push(center + egui::vec2(a.cos(), a.sin()) * radius);
        i += 1;
    }
    points
}
