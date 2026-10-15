//! Line charts drawn with egui's painter. (egui_plot 0.37 is built for egui 0.36, so Scana
//! draws its own.) One chart per value; charts share an x range and a hover position, so
//! pointing at one shows the same moment on all of them.

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

#[derive(Clone, Copy)]
pub struct ChartStyle {
    pub bg: Color32,
    pub grid: Color32,
    pub text: Color32,
    pub mark: Color32,
}

pub struct Chart<'a> {
    pub unit: &'a str,
    pub color: Color32,
    /// (x seconds, y), in x order.
    pub points: &'a [[f64; 2]],
}

/// A "nice" step near `raw`: 1, 2 or 5 times a power of ten.
pub fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let p = 10f64.powf(raw.log10().floor());
    let m = raw / p;
    let n = if m < 1.5 {
        1.0
    } else if m < 3.5 {
        2.0
    } else if m < 7.5 {
        5.0
    } else {
        10.0
    };
    n * p
}

fn label(v: f64, step: f64) -> String {
    let places = if step >= 1.0 {
        0
    } else if step >= 0.1 {
        1
    } else if step >= 0.01 {
        2
    } else {
        3
    };
    let s = format!("{v:.places$}");
    if s == "-0" { "0".into() } else { s }
}

/// The y range to show for the points inside [x0, x1], with a little room above and below.
pub fn y_range(points: &[[f64; 2]], x0: f64, x1: f64) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for p in points.iter().filter(|p| p[0] >= x0 && p[0] <= x1) {
        lo = lo.min(p[1]);
        hi = hi.max(p[1]);
    }
    if !lo.is_finite() {
        return (0.0, 1.0);
    }
    if hi - lo < 1e-9 {
        let pad = (hi.abs() * 0.05).max(0.5);
        return (lo - pad, hi + pad);
    }
    let pad = (hi - lo) * 0.08;
    (lo - pad, hi + pad)
}

/// Draws one chart. `hover_x` is shared between charts: read to draw the crosshair, written when
/// the pointer is over this chart. The response reports drags (for panning).
#[allow(clippy::too_many_arguments)]
pub fn chart(
    ui: &mut Ui,
    c: &Chart<'_>,
    x0: f64,
    x1: f64,
    height: f32,
    marks: &[f64],
    style: &ChartStyle,
    hover_x: &mut Option<f64>,
    x_labels: bool,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let font = FontId::proportional(12.0);
    let plot = Rect::from_min_max(
        pos2(rect.left() + 58.0, rect.top() + 4.0),
        pos2(rect.right() - 8.0, rect.bottom() - if x_labels { 20.0 } else { 6.0 }),
    );
    painter.rect_filled(rect, 6, style.bg);
    if plot.width() < 20.0 || plot.height() < 20.0 || x1 <= x0 {
        return resp;
    }
    let (y0, y1) = y_range(c.points, x0, x1);
    let to_screen = |x: f64, y: f64| -> Pos2 {
        let fx = ((x - x0) / (x1 - x0)) as f32;
        let fy = ((y - y0) / (y1 - y0)) as f32;
        pos2(plot.left() + fx * plot.width(), plot.bottom() - fy * plot.height())
    };

    // Grid and labels.
    let ystep = nice_step((y1 - y0) / 4.0);
    let mut v = (y0 / ystep).ceil() * ystep;
    while v <= y1 {
        let p = to_screen(x0, v);
        painter.line_segment([pos2(plot.left(), p.y), pos2(plot.right(), p.y)], Stroke::new(1.0, style.grid));
        painter.text(pos2(plot.left() - 6.0, p.y), Align2::RIGHT_CENTER, label(v, ystep), font.clone(), style.text);
        v += ystep;
    }
    let xstep = nice_step((x1 - x0) / 6.0);
    let mut t = (x0 / xstep).ceil() * xstep;
    while t <= x1 {
        let p = to_screen(t, y0);
        painter.line_segment([pos2(p.x, plot.top()), pos2(p.x, plot.bottom())], Stroke::new(1.0, style.grid));
        if x_labels {
            painter.text(
                pos2(p.x, plot.bottom() + 3.0),
                Align2::CENTER_TOP,
                format!("{} s", label(t, xstep)),
                font.clone(),
                style.text,
            );
        }
        t += xstep;
    }

    // The line: points closer than a pixel apart are skipped, so long histories stay fast.
    let start = c.points.partition_point(|p| p[0] < x0).saturating_sub(1);
    let end = (c.points.partition_point(|p| p[0] <= x1) + 1).min(c.points.len());
    let mut line: Vec<Pos2> = Vec::new();
    for p in &c.points[start..end] {
        let s = to_screen(p[0], p[1]);
        if line.last().is_none_or(|l| (s.x - l.x).abs() >= 0.75) {
            line.push(s);
        }
    }
    let clip = painter.with_clip_rect(plot);
    if line.len() >= 2 {
        clip.line(line, Stroke::new(2.0, c.color));
    } else if let Some(p) = line.first() {
        clip.circle_filled(*p, 2.5, c.color);
    }
    for &m in marks.iter().filter(|m| **m >= x0 && **m <= x1) {
        let p = to_screen(m, y0);
        clip.line_segment([pos2(p.x, plot.top()), pos2(p.x, plot.bottom())], Stroke::new(1.5, style.mark));
    }
    painter.rect_stroke(plot, 0, Stroke::new(1.0, style.grid), StrokeKind::Inside);

    // Hover: a crosshair and the nearest value, shared by every chart.
    if let Some(pos) = resp.hover_pos()
        && plot.contains(pos)
    {
        *hover_x = Some(x0 + f64::from((pos.x - plot.left()) / plot.width()) * (x1 - x0));
    }
    if let Some(hx) = *hover_x
        && hx >= x0
        && hx <= x1
        && !c.points.is_empty()
    {
        let i = c.points.partition_point(|p| p[0] < hx).min(c.points.len() - 1);
        let near = if i > 0 && (c.points[i - 1][0] - hx).abs() < (c.points[i][0] - hx).abs() {
            c.points[i - 1]
        } else {
            c.points[i]
        };
        let p = to_screen(near[0], near[1]);
        clip.line_segment([pos2(p.x, plot.top()), pos2(p.x, plot.bottom())], Stroke::new(1.0, style.text));
        clip.circle_filled(p, 4.0, c.color);
        let text = format!("{:.1} s: {} {}", near[0], label(near[1], ystep / 10.0), c.unit);
        let anchor = if p.x > plot.center().x { Align2::RIGHT_TOP } else { Align2::LEFT_TOP };
        let tp = pos2(if anchor == Align2::RIGHT_TOP { p.x - 8.0 } else { p.x + 8.0 }, plot.top() + 4.0);
        let r = painter.text(tp, anchor, &text, font.clone(), style.text);
        painter.rect_filled(r.expand(3.0), 3, style.bg);
        painter.text(tp, anchor, text, font, c.color);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_steps() {
        assert_eq!(nice_step(0.9), 1.0);
        assert_eq!(nice_step(2.6), 2.0);
        assert_eq!(nice_step(4.0), 5.0);
        assert_eq!(nice_step(80.0), 100.0);
        assert!((nice_step(0.034) - 0.02).abs() < 1e-12); // 3.4 x 0.01 rounds to 2 x 0.01
        assert!((nice_step(0.04) - 0.05).abs() < 1e-12);
        assert_eq!(nice_step(0.0), 1.0);
    }

    #[test]
    fn y_ranges() {
        let pts = [[0.0, 10.0], [1.0, 20.0], [2.0, 1000.0]];
        let (lo, hi) = y_range(&pts, 0.0, 1.5);
        assert!(lo < 10.0 && hi > 20.0 && hi < 25.0);
        let (lo, hi) = y_range(&[[0.0, 5.0]], 0.0, 1.0);
        assert!(lo < 5.0 && hi > 5.0);
        assert_eq!(y_range(&[], 0.0, 1.0), (0.0, 1.0));
    }
}
