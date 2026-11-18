use std::time::Duration;

use doppel_desktop::ax::tree::{ElementTarget, NodeIndex, SnapshotId};
use doppel_desktop::geometry::Point;
use doppel_desktop::mark::{Area, Lasts, Mark, MarkAt, Note, Shape};

use super::ParseError;
use crate::pb::common::v1 as common;
use crate::pb::daemon::v1 as daemon;

impl daemon::MarkScreen {
    /// Asks for `mark` on machine `name`.
    #[must_use]
    pub fn new(name: String, mark: &Mark) -> Self {
        let (shape, from) = match mark.shape() {
            Shape::Box => (daemon::MarkShape::Box, None),
            Shape::Dot => (daemon::MarkShape::Dot, None),
            Shape::Arrow { from } => (daemon::MarkShape::Arrow, from.map(Into::into)),
        };
        Self {
            name,
            shape: shape.into(),
            at: Some(match mark.at() {
                MarkAt::Point(at) => daemon::mark_screen::At::Point(at.into()),
                MarkAt::Area(area) => daemon::mark_screen::At::Area(rect(area)),
                MarkAt::Element(element) => daemon::mark_screen::At::Element(daemon::ElementOf {
                    snapshot_id: element.snapshot.get(),
                    node_index: element.node.get(),
                }),
            }),
            from,
            note: mark
                .note()
                .map(|note| note.as_str().to_owned())
                .unwrap_or_default(),
            lasts: Some(match mark.lasts() {
                Lasts::For(time) => daemon::mark_screen::Lasts::ForMs(millis(time)),
                Lasts::UntilActed(time) => daemon::mark_screen::Lasts::UntilActedMs(millis(time)),
            }),
        }
    }
}

fn millis(time: Duration) -> u32 {
    u32::try_from(time.as_millis()).unwrap_or(u32::MAX)
}

fn rect(area: Area) -> common::Rect {
    area.rect().into()
}

impl TryFrom<&daemon::MarkScreen> for Mark {
    type Error = ParseError;
    fn try_from(wire: &daemon::MarkScreen) -> Result<Self, ParseError> {
        let at = match wire.at.ok_or(ParseError::Missing("at"))? {
            daemon::mark_screen::At::Point(at) => MarkAt::Point(at.try_into()?),
            daemon::mark_screen::At::Area(area) => {
                let (origin, size) = (
                    area.origin.ok_or(ParseError::Missing("area.origin"))?,
                    area.size.ok_or(ParseError::Missing("area.size"))?,
                );
                MarkAt::Area(Area::new(origin.x, origin.y, size.width, size.height)?)
            }
            daemon::mark_screen::At::Element(element) => MarkAt::Element(ElementTarget {
                snapshot: SnapshotId::new(element.snapshot_id),
                node: NodeIndex::new(element.node_index),
            }),
        };
        let from = wire.from.map(Point::try_from).transpose()?;
        let unknown = ParseError::UnknownEnum {
            field: "shape",
            value: wire.shape,
        };
        let shape = match daemon::MarkShape::try_from(wire.shape).map_err(|_| unknown.clone())? {
            daemon::MarkShape::Box => Shape::Box,
            daemon::MarkShape::Dot => Shape::Dot,
            daemon::MarkShape::Arrow => Shape::Arrow { from },
            daemon::MarkShape::Unspecified => return Err(unknown),
        };
        if from.is_some() && !matches!(shape, Shape::Arrow { .. }) {
            return Err(ParseError::Invalid {
                field: "from",
                reason: "only an arrow points from somewhere".into(),
            });
        }
        let lasts = match wire.lasts.ok_or(ParseError::Missing("lasts"))? {
            daemon::mark_screen::Lasts::ForMs(ms) => Lasts::For(Duration::from_millis(ms.into())),
            daemon::mark_screen::Lasts::UntilActedMs(ms) => {
                Lasts::UntilActed(Duration::from_millis(ms.into()))
            }
        };
        Ok(Self::new(at, shape, Note::new(&wire.note)?, lasts)?)
    }
}

#[cfg(test)]
mod tests {
    use doppel_desktop::mark::DEFAULT_LASTS;

    use super::*;

    #[test]
    fn marks_round_trip() {
        let marks = [
            Mark::new(
                MarkAt::Element(ElementTarget {
                    snapshot: SnapshotId::new(7),
                    node: NodeIndex::new(3),
                }),
                Shape::Box,
                Note::new("this one").unwrap(),
                Lasts::UntilActed(DEFAULT_LASTS),
            )
            .unwrap(),
            Mark::new(
                MarkAt::Point(Point::new(4.0, 5.0).unwrap()),
                Shape::Arrow {
                    from: Some(Point::new(1.0, 2.0).unwrap()),
                },
                None,
                Lasts::For(Duration::from_secs(10)),
            )
            .unwrap(),
            Mark::new(
                MarkAt::Area(Area::new(1.0, 2.0, 3.0, 4.0).unwrap()),
                Shape::Dot,
                None,
                Lasts::For(Duration::from_secs(1)),
            )
            .unwrap(),
        ];
        for mark in marks {
            let wire = daemon::MarkScreen::new("live".into(), &mark);
            assert_eq!(wire.name, "live");
            assert_eq!(Mark::try_from(&wire).unwrap(), mark);
        }
    }

    #[test]
    fn bad_marks_are_refused() {
        let good = daemon::MarkScreen::new(
            "live".into(),
            &Mark::new(
                MarkAt::Point(Point::new(4.0, 5.0).unwrap()),
                Shape::Box,
                None,
                Lasts::For(DEFAULT_LASTS),
            )
            .unwrap(),
        );
        let refused = |change: fn(&mut daemon::MarkScreen)| {
            let mut wire = good.clone();
            change(&mut wire);
            Mark::try_from(&wire).is_err()
        };
        assert!(refused(|wire| wire.at = None));
        assert!(refused(|wire| wire.lasts = None));
        assert!(refused(|wire| wire.shape = 0));
        assert!(refused(
            |wire| wire.from = Some(common::Point { x: 1.0, y: 1.0 })
        ));
        assert!(refused(|wire| {
            wire.lasts = Some(daemon::mark_screen::Lasts::ForMs(61_000));
        }));
        assert!(refused(|wire| wire.note = "x".repeat(201)));
    }
}
