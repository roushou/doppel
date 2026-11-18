use doppel_scene::film::{Corner, Glyph, Line, Picture, Shape, Spot};

use crate::pb::vmm::v1 as vmm;

impl From<Spot> for vmm::Spot {
    fn from(at: Spot) -> Self {
        Self { x: at.x, y: at.y }
    }
}

fn spot(at: Spot) -> vmm::Spot {
    at.into()
}

impl From<&Shape> for vmm::Shape {
    fn from(shape: &Shape) -> Self {
        use vmm::shape::Kind;
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
                Kind::Frame(vmm::FrameShape {
                    rect: Some((*rect).into()),
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
                Kind::Ring(vmm::RingShape {
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
                Kind::Stroke(vmm::StrokeShape {
                    from: Some(spot(*from)),
                    to: Some(spot(*to)),
                    width: *width,
                    line: match line {
                        Line::Solid => vmm::StrokeLine::Solid,
                        Line::Dashed => vmm::StrokeLine::Dashed,
                        Line::Arrow => vmm::StrokeLine::Arrow,
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
                Kind::Pointer(vmm::PointerShape {
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
                Kind::Glyph(vmm::GlyphShape {
                    centre: Some(spot(*centre)),
                    glyph: match glyph {
                        Glyph::Press => vmm::Glyph::Press,
                        Glyph::Type => vmm::Glyph::Type,
                        Glyph::Keys => vmm::Glyph::Keys,
                        Glyph::Command => vmm::Glyph::Command,
                        Glyph::Wait => vmm::Glyph::Wait,
                        Glyph::Look => vmm::Glyph::Look,
                        Glyph::Point => vmm::Glyph::Point,
                        Glyph::Drag => vmm::Glyph::Drag,
                        Glyph::Scroll => vmm::Glyph::Scroll,
                        Glyph::Machine => vmm::Glyph::Machine,
                        Glyph::Checkpoint => vmm::Glyph::Checkpoint,
                        Glyph::Rollback => vmm::Glyph::Rollback,
                        Glyph::Done => vmm::Glyph::Done,
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
                Kind::Veil(vmm::VeilShape {
                    holes: holes.iter().map(|hole| (*hole).into()).collect(),
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
                Kind::Brackets(vmm::BracketsShape {
                    rect: Some((*rect).into()),
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
                Kind::Tag(vmm::TagShape {
                    at: Some(spot(*at)),
                    corner: match corner {
                        Corner::TopLeft => vmm::TagCorner::TopLeft,
                        Corner::TopRight => vmm::TagCorner::TopRight,
                        Corner::BottomLeft => vmm::TagCorner::BottomLeft,
                    }
                    .into(),
                    text: text.clone(),
                    ink: ink.hex(),
                }),
            ),
        };
        vmm::Shape {
            opacity,
            colour: colour.hex(),
            kind: Some(kind),
        }
    }
}

impl From<&Picture> for vmm::Picture {
    fn from(picture: &Picture) -> Self {
        Self {
            shapes: picture.shapes.iter().map(Into::into).collect(),
            at_ms: picture.at,
        }
    }
}

#[cfg(test)]
mod tests {
    use doppel_desktop::geometry::Rect;
    use doppel_scene::film::MARKED;

    use super::*;

    #[test]
    fn every_shape_keeps_its_colour_opacity_and_place() {
        let picture = Picture {
            shapes: vec![Shape::Frame {
                rect: Rect::new(10.0, 20.0, 30.0, 40.0).unwrap(),
                colour: MARKED,
                wash: 0.2,
                width: 2.0,
                opacity: 0.5,
            }],
            at: 1500.0,
        };
        let wire = vmm::Picture::from(&picture);
        assert_eq!(wire.at_ms, 1500.0);
        let shape = &wire.shapes[0];
        assert_eq!((shape.colour, shape.opacity), (0xff_5ca8, 0.5));
        let Some(vmm::shape::Kind::Frame(frame)) = &shape.kind else {
            panic!("{shape:?}");
        };
        let rect = frame.rect.unwrap();
        assert_eq!(
            (rect.origin.unwrap().x, rect.size.unwrap().height),
            (10.0, 40.0)
        );
    }
}
