//! The toolbar's small symbols, drawn with lines in the widget's own
//! colour, so every skin reaches them and no font needs the characters.

use egui::{Painter, Pos2, Rect, Stroke, pos2, vec2};

#[derive(Clone, Copy)]
pub enum Glyph {
    Back,
    Forward,
    Up,
    /// Between path segments.
    Separator,
    Icons,
    List,
    Columns,
    /// Unmounting a share.
    Eject,
    /// Closing a panel.
    Close,
}

pub fn paint(painter: &Painter, rect: Rect, glyph: Glyph, stroke: Stroke) {
    let c = rect.center();
    let s = rect.height().min(rect.width()) * 0.22;
    let stroke = Stroke::new(stroke.width.max(1.5), stroke.color);
    let chevron = |points: [Pos2; 3]| {
        painter.line(points.to_vec(), stroke);
    };
    match glyph {
        Glyph::Back => chevron([pos2(c.x + s * 0.5, c.y - s), pos2(c.x - s * 0.5, c.y), pos2(c.x + s * 0.5, c.y + s)]),
        Glyph::Forward | Glyph::Separator => {
            let s = if matches!(glyph, Glyph::Separator) { s * 0.7 } else { s };
            chevron([pos2(c.x - s * 0.5, c.y - s), pos2(c.x + s * 0.5, c.y), pos2(c.x - s * 0.5, c.y + s)])
        }
        Glyph::Close => {
            let s = s * 0.8;
            painter.line_segment([pos2(c.x - s, c.y - s), pos2(c.x + s, c.y + s)], stroke);
            painter.line_segment([pos2(c.x - s, c.y + s), pos2(c.x + s, c.y - s)], stroke);
        }
        Glyph::Up => chevron([pos2(c.x - s, c.y + s * 0.5), pos2(c.x, c.y - s * 0.5), pos2(c.x + s, c.y + s * 0.5)]),
        Glyph::Icons => {
            let cell = s * 0.85;
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let centre = c + vec2(dx, dy) * cell * 0.65;
                painter.rect_stroke(
                    Rect::from_center_size(centre, vec2(cell, cell)),
                    1.0,
                    Stroke::new(1.2, stroke.color),
                    egui::StrokeKind::Middle,
                );
            }
        }
        Glyph::Columns => {
            let (w, h) = (s * 0.75, s * 2.0);
            for dx in [-1.0, 0.0, 1.0] {
                let centre = c + vec2(dx * w, 0.0);
                painter.rect_stroke(
                    Rect::from_center_size(centre, vec2(w, h)),
                    0.0,
                    Stroke::new(1.2, stroke.color),
                    egui::StrokeKind::Middle,
                );
            }
        }
        Glyph::Eject => {
            let stroke = Stroke::new(1.2, stroke.color);
            let top = c.y - s * 1.0;
            let base = c.y + s * 0.25;
            painter.line(vec![pos2(c.x - s, base), pos2(c.x, top), pos2(c.x + s, base), pos2(c.x - s, base)], stroke);
            let y = c.y + s * 0.85;
            painter.line_segment([pos2(c.x - s, y), pos2(c.x + s, y)], stroke);
        }
        Glyph::List => {
            for dy in [-1.0, 0.0, 1.0] {
                let y = c.y + dy * s * 0.8;
                painter.line_segment([pos2(c.x - s * 1.1, y), pos2(c.x + s * 1.1, y)], Stroke::new(1.4, stroke.color));
            }
        }
    }
}
