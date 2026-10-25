//! Draggable temperature -> duty curve.
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind};

const SIZE: egui::Vec2 = egui::vec2(460.0, 210.0);
const PAD: f32 = 34.0;

/// Returns true when a point moved. `range` is the temperature axis in °C. With `allow_zero`, dragging
/// a point well below the floor snaps it to 0 % (fans stopped).
pub fn editor(
    ui: &mut egui::Ui,
    id: &str,
    points: &mut [(f32, u8)],
    floor: u8,
    allow_zero: bool,
    range: (f32, f32),
    current: Option<f32>,
) -> bool {
    let (outer, resp) = ui.allocate_exact_size(SIZE, Sense::click_and_drag());
    let plot = Rect::from_min_max(outer.min + egui::vec2(PAD, 6.0), outer.max - egui::vec2(8.0, 22.0));
    let (t0, t1) = range;
    let to_screen = |t: f32, d: f32| {
        Pos2::new(plot.left() + (t - t0) / (t1 - t0) * plot.width(), plot.bottom() - d / 100.0 * plot.height())
    };
    let from_screen = |p: Pos2| {
        let t = t0 + (p.x - plot.left()) / plot.width() * (t1 - t0);
        let d = (plot.bottom() - p.y) / plot.height() * 100.0;
        (t.clamp(t0, t1), d.clamp(0.0, 100.0))
    };
    let painter = ui.painter_at(outer);
    let weak = ui.visuals().weak_text_color();
    painter.rect_filled(plot, 4.0, ui.visuals().extreme_bg_color);

    // Region below the safety floor.
    let floor_y = to_screen(t0, floor as f32).y;
    painter.rect_filled(Rect::from_min_max(Pos2::new(plot.left(), floor_y), plot.max), 0.0, Color32::from_rgba_unmultiplied(200, 60, 60, 28));

    let font = egui::FontId::proportional(11.0);
    for d in [0, 25, 50, 75, 100] {
        let y = to_screen(t0, d as f32).y;
        painter.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(0.5, weak.gamma_multiply(0.4)));
        painter.text(Pos2::new(plot.left() - 4.0, y), egui::Align2::RIGHT_CENTER, format!("{d}%"), font.clone(), weak);
    }
    let step = if t1 - t0 > 50.0 { 10.0 } else { 5.0 };
    let mut t = t0;
    while t <= t1 + 0.01 {
        let x = to_screen(t, 0.0).x;
        painter.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(0.5, weak.gamma_multiply(0.4)));
        painter.text(Pos2::new(x, plot.bottom() + 3.0), egui::Align2::CENTER_TOP, format!("{t:.0}°"), font.clone(), weak);
        t += step;
    }
    if let Some(c) = current.filter(|c| (t0..=t1).contains(c)) {
        let x = to_screen(c, 0.0).x;
        painter.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.5, Color32::from_rgb(255, 170, 60)));
    }

    // Drag handling: grab the nearest point on press, keep it until release.
    let key = ui.scope_id().with(("curve-drag", id));
    let mut changed = false;
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let nearest = points
                .iter()
                .enumerate()
                .map(|(i, &(t, d))| (i, to_screen(t, d as f32).distance(p)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .filter(|&(_, dist)| dist < 18.0)
                .map(|(i, _)| i);
            ui.memory_mut(|m| m.data.insert_temp(key, nearest));
        }
    }
    let dragging: Option<usize> = ui.memory(|m| m.data.get_temp(key)).flatten();
    if let (Some(i), true, Some(p)) = (dragging, resp.dragged(), resp.interact_pointer_pos()) {
        let (t, d) = from_screen(p);
        // Keep points ordered: a point can't pass its neighbours.
        let lo = if i > 0 { points[i - 1].0 + 1.0 } else { t0 };
        let hi = if i + 1 < points.len() { points[i + 1].0 - 1.0 } else { t1 };
        let duty = if allow_zero && d < floor as f32 / 2.0 { 0 } else { (d.round() as u8).max(floor) };
        points[i] = (t.clamp(lo, hi.max(lo)).round(), duty);
        changed = true;
    }
    if resp.drag_stopped() {
        ui.memory_mut(|m| m.data.remove::<Option<usize>>(key));
    }

    let accent = Color32::from_rgb(0, 160, 255);
    let mut line = vec![to_screen(t0, points.first().map_or(100, |p| p.1) as f32)];
    line.extend(points.iter().map(|&(t, d)| to_screen(t, d as f32)));
    line.push(to_screen(t1, points.last().map_or(100, |p| p.1) as f32));
    painter.add(egui::Shape::line(line, Stroke::new(2.0, accent)));
    let hover = resp.hover_pos();
    for (i, &(t, d)) in points.iter().enumerate() {
        let c = to_screen(t, d as f32);
        let hot = dragging == Some(i) || hover.is_some_and(|h| h.distance(c) < 18.0);
        painter.circle_filled(c, if hot { 7.0 } else { 5.0 }, if hot { Color32::WHITE } else { accent });
        if hot {
            let label = if d == 0 { format!("{t:.0}° → stopped") } else { format!("{t:.0}° → {d}%") };
            painter.text(c + egui::vec2(8.0, -8.0), egui::Align2::LEFT_BOTTOM, label, font.clone(), Color32::WHITE);
        }
    }
    painter.rect_stroke(plot, 4.0, Stroke::new(1.0, weak.gamma_multiply(0.6)), StrokeKind::Inside);
    changed
}
