//! The scene's reel in the VMM's words, for `doppel-vmm export` to burn in.

use doppel_machine::geometry::{Rect, Size};
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::vmm::v1 as wire;
use doppel_scene::film::{Corner, Glyph, Line, Picture, Reel, Shape, Spot};

fn spot(at: Spot) -> wire::Spot {
    wire::Spot { x: at.x, y: at.y }
}

fn rect(rect: Rect) -> common::Rect {
    common::Rect {
        origin: Some(common::Point {
            x: rect.left(),
            y: rect.top(),
        }),
        size: Some(common::Size {
            width: rect.width(),
            height: rect.height(),
        }),
    }
}

fn shape(shape: &Shape) -> wire::Shape {
    use wire::shape::Kind;
    let (colour, opacity, kind) = match shape {
        Shape::Frame {
            rect,
            colour,
            wash,
            width,
            opacity,
        } => (
            *colour,
            *opacity,
            Kind::Frame(wire::FrameShape {
                rect: Some(self::rect(*rect)),
                wash: *wash,
                width: *width,
            }),
        ),
        Shape::Ring {
            centre,
            radius,
            colour,
            wash,
            width,
            opacity,
        } => (
            *colour,
            *opacity,
            Kind::Ring(wire::RingShape {
                centre: Some(spot(*centre)),
                radius: *radius,
                wash: *wash,
                width: *width,
            }),
        ),
        Shape::Stroke {
            from,
            to,
            colour,
            width,
            line,
            opacity,
        } => (
            *colour,
            *opacity,
            Kind::Stroke(wire::StrokeShape {
                from: Some(spot(*from)),
                to: Some(spot(*to)),
                width: *width,
                line: match line {
                    Line::Solid => wire::StrokeLine::Solid,
                    Line::Dashed => wire::StrokeLine::Dashed,
                    Line::Arrow => wire::StrokeLine::Arrow,
                }
                .into(),
            }),
        ),
        Shape::Pointer {
            tip,
            colour,
            opacity,
        } => (
            *colour,
            *opacity,
            Kind::Pointer(wire::PointerShape {
                tip: Some(spot(*tip)),
            }),
        ),
        Shape::Glyph {
            centre,
            glyph,
            colour,
            size,
            opacity,
        } => (
            *colour,
            *opacity,
            Kind::Glyph(wire::GlyphShape {
                centre: Some(spot(*centre)),
                glyph: match glyph {
                    Glyph::Press => wire::Glyph::Press,
                    Glyph::Type => wire::Glyph::Type,
                    Glyph::Keys => wire::Glyph::Keys,
                    Glyph::Command => wire::Glyph::Command,
                    Glyph::Wait => wire::Glyph::Wait,
                    Glyph::Look => wire::Glyph::Look,
                    Glyph::Point => wire::Glyph::Point,
                    Glyph::Drag => wire::Glyph::Drag,
                    Glyph::Scroll => wire::Glyph::Scroll,
                    Glyph::Machine => wire::Glyph::Machine,
                    Glyph::Checkpoint => wire::Glyph::Checkpoint,
                    Glyph::Rollback => wire::Glyph::Rollback,
                    Glyph::Done => wire::Glyph::Done,
                }
                .into(),
                size: *size,
            }),
        ),
        Shape::Veil {
            holes,
            colour,
            opacity,
        } => (
            *colour,
            *opacity,
            Kind::Veil(wire::VeilShape {
                holes: holes.iter().map(|hole| self::rect(*hole)).collect(),
            }),
        ),
        Shape::Brackets {
            rect,
            arm,
            colour,
            width,
            opacity,
        } => (
            *colour,
            *opacity,
            Kind::Brackets(wire::BracketsShape {
                rect: Some(self::rect(*rect)),
                arm: *arm,
                width: *width,
            }),
        ),
        Shape::Tag {
            at,
            corner,
            text,
            ink,
            plate,
            opacity,
        } => (
            *plate,
            *opacity,
            Kind::Tag(wire::TagShape {
                at: Some(spot(*at)),
                corner: match corner {
                    Corner::TopLeft => wire::TagCorner::TopLeft,
                    Corner::TopRight => wire::TagCorner::TopRight,
                    Corner::BottomLeft => wire::TagCorner::BottomLeft,
                }
                .into(),
                text: text.clone(),
                ink: ink.hex(),
            }),
        ),
    };
    wire::Shape {
        opacity,
        colour: colour.hex(),
        kind: Some(kind),
    }
}

fn picture(picture: &Picture, video_from: f64) -> wire::Picture {
    wire::Picture {
        shapes: picture.shapes.iter().map(shape).collect(),
        video_ms: (picture.at - video_from).max(0.0),
    }
}

/// `reel`, drawn over a display of `display`, as the VMM reads it; its
/// video started at `video_from` in the session's time.
pub(crate) fn to_wire(reel: &Reel, display: Size, video_from: f64) -> wire::Reel {
    wire::Reel {
        fps: reel.fps,
        display: Some(common::Size {
            width: display.width(),
            height: display.height(),
        }),
        pictures: reel
            .pictures
            .iter()
            .map(|shown| picture(shown, video_from))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use doppel_machine::geometry::Rect;
    use doppel_scene::film::MARKED;

    use super::*;

    #[test]
    fn every_shape_keeps_its_colour_opacity_and_place() {
        let reel = Reel {
            fps: 30,
            pictures: vec![Picture {
                shapes: vec![Shape::Frame {
                    rect: Rect::new(10.0, 20.0, 30.0, 40.0).unwrap(),
                    colour: MARKED,
                    wash: 0.2,
                    width: 2.0,
                    opacity: 0.5,
                }],
                at: 1500.0,
            }],
        };
        let wire = to_wire(&reel, Size::new(1280.0, 800.0).unwrap(), 500.0);
        assert_eq!(wire.pictures[0].video_ms, 1000.0);
        assert_eq!(wire.fps, 30);
        assert_eq!(wire.display.unwrap().width, 1280.0);
        let shape = &wire.pictures[0].shapes[0];
        assert_eq!((shape.colour, shape.opacity), (0xff_5ca8, 0.5));
        let Some(wire::shape::Kind::Frame(frame)) = &shape.kind else {
            panic!("{shape:?}");
        };
        let rect = frame.rect.unwrap();
        assert_eq!(
            (rect.origin.unwrap().x, rect.size.unwrap().height),
            (10.0, 40.0)
        );
    }
}
