//! The scene's reel in the VMM's words, for `doppel-vmm export` to burn in.

use std::path::PathBuf;

use doppel_machine::geometry::Size;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::vmm::v1 as wire;
use doppel_scene::film::Reel;

/// `reel`, drawn over a display of `display`, as the VMM reads it, over
/// `sources`: each video's path and when it started in the session's time.
pub(crate) fn to_wire(reel: &Reel, display: Size, sources: &[(PathBuf, f64)]) -> wire::Reel {
    wire::Reel {
        fps: reel.fps,
        display: Some(common::Size {
            width: display.width(),
            height: display.height(),
        }),
        pictures: reel.pictures.iter().map(Into::into).collect(),
        sources: sources
            .iter()
            .map(|(path, start_ms)| wire::Source {
                path: path.display().to_string(),
                start_ms: *start_ms,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use doppel_scene::film::Picture;

    use super::*;

    #[test]
    fn a_reel_keeps_its_pace_display_and_sources() {
        let reel = Reel {
            fps: 30,
            pictures: vec![Picture {
                shapes: Vec::new(),
                at: 1500.0,
            }],
        };
        let sources = [(PathBuf::from("/r/live-001.mp4"), 500.0)];
        let wire = to_wire(&reel, Size::new(1280.0, 800.0).unwrap(), &sources);
        assert_eq!(wire.pictures[0].at_ms, 1500.0);
        assert_eq!(
            (wire.sources[0].path.as_str(), wire.sources[0].start_ms),
            ("/r/live-001.mp4", 500.0)
        );
        assert_eq!(wire.fps, 30);
        assert_eq!(wire.display.unwrap().width, 1280.0);
    }
}
