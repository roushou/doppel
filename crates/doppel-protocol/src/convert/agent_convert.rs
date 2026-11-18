use doppel_desktop::agent::{
    AgentStatus, BundleId, GuestEvent, Permission, ProcessId, RunningApp, Session,
};
use doppel_desktop::geometry::Size;
use doppel_desktop::screen::{ImageFormat, Screenshot};

use super::ParseError;
use crate::pb::agent::v1 as agent;
use crate::pb::common::v1 as common;

/// A display's size in points: both sides longer than zero.
impl TryFrom<common::Size> for Size {
    type Error = ParseError;
    fn try_from(s: common::Size) -> Result<Self, ParseError> {
        Self::new(s.width, s.height)
            .ok()
            .filter(|size| size.width() > 0.0 && size.height() > 0.0)
            .ok_or_else(|| ParseError::Invalid {
                field: "display_points",
                reason: "not a positive size".into(),
            })
    }
}

impl TryFrom<agent::AgentStatus> for AgentStatus {
    type Error = ParseError;
    fn try_from(wire: agent::AgentStatus) -> Result<Self, ParseError> {
        let session = match agent::SessionState::try_from(wire.session) {
            Ok(agent::SessionState::Active) => Session::Active,
            Ok(agent::SessionState::Locked) => Session::Locked,
            Ok(agent::SessionState::Unspecified) => Session::Unreported,
            Err(_) => Session::Unrecognized(wire.session),
        };
        Ok(Self {
            granted: Granted(wire.granted).into(),
            session,
            display: wire
                .display_points
                .ok_or(ParseError::Missing("display_points"))?
                .try_into()?,
            frontmost: wire.frontmost_app.map(RunningApp::from),
        })
    }
}

/// Permissions as the wire lists them; unknown values are kept.
struct Granted(Vec<i32>);

impl From<Granted> for Vec<Permission> {
    fn from(Granted(raw): Granted) -> Self {
        raw.into_iter().map(ParseError::permission).collect()
    }
}

impl From<agent::AgentEvent> for GuestEvent {
    /// Infallible: an event this build can't read is kept as unrecognized.
    fn from(event: agent::AgentEvent) -> Self {
        use agent::agent_event::Kind;
        match event.kind {
            Some(Kind::FrontmostAppChanged(changed)) => {
                Self::FrontmostAppChanged(changed.app.map(RunningApp::from))
            }
            Some(Kind::UiChanged(changed)) => Self::UiChanged(ProcessId::new(changed.pid)),
            Some(Kind::PermissionsChanged(changed)) => {
                Self::PermissionsChanged(Granted(changed.granted).into())
            }
            None => Self::Unrecognized,
        }
    }
}

impl From<agent::RunningApp> for RunningApp {
    /// Infallible: an empty bundle id means the process has none.
    fn from(app: agent::RunningApp) -> Self {
        Self {
            pid: ProcessId::new(app.pid),
            bundle_id: BundleId::new(app.bundle_id),
            name: app.name,
        }
    }
}

impl TryFrom<common::Image> for Screenshot {
    type Error = ParseError;
    fn try_from(wire: common::Image) -> Result<Self, ParseError> {
        let format = match common::ImageEncoding::try_from(wire.encoding) {
            Ok(common::ImageEncoding::Png) => ImageFormat::Png,
            Ok(common::ImageEncoding::Jpeg) => ImageFormat::Jpeg,
            Ok(common::ImageEncoding::Unspecified) | Err(_) => {
                return Err(ParseError::Invalid {
                    field: "encoding",
                    reason: format!("unknown encoding {}", wire.encoding),
                });
            }
        };
        if wire.data.is_empty() || wire.width_pixels == 0 || wire.height_pixels == 0 {
            return Err(ParseError::Invalid {
                field: "data",
                reason: "empty image".into(),
            });
        }
        Ok(Self {
            format,
            width_pixels: wire.width_pixels,
            height_pixels: wire.height_pixels,
            display: wire
                .display_points
                .ok_or(ParseError::Missing("display_points"))?
                .try_into()?,
            data: wire.data.to_vec(),
        })
    }
}

impl From<Size> for common::Size {
    fn from(display: Size) -> Self {
        Self {
            width: display.width(),
            height: display.height(),
        }
    }
}

impl From<&RunningApp> for agent::RunningApp {
    fn from(app: &RunningApp) -> Self {
        Self {
            pid: app.pid.get(),
            bundle_id: app
                .bundle_id
                .as_ref()
                .map_or_else(String::new, |id| id.as_str().to_owned()),
            name: app.name.clone(),
        }
    }
}

impl From<&AgentStatus> for agent::AgentStatus {
    fn from(status: &AgentStatus) -> Self {
        Self {
            granted: status
                .granted
                .iter()
                .map(|permission| match permission {
                    Permission::ScreenRecording => common::Permission::ScreenRecording as i32,
                    Permission::Accessibility => common::Permission::Accessibility as i32,
                    Permission::Unrecognized(raw) => *raw,
                })
                .collect(),
            session: match status.session {
                Session::Active => agent::SessionState::Active as i32,
                Session::Locked => agent::SessionState::Locked as i32,
                Session::Unreported => agent::SessionState::Unspecified as i32,
                Session::Unrecognized(raw) => raw,
            },
            display_points: Some(status.display.into()),
            frontmost_app: status.frontmost.as_ref().map(agent::RunningApp::from),
        }
    }
}

impl From<&Screenshot> for common::Image {
    fn from(shot: &Screenshot) -> Self {
        Self {
            encoding: match shot.format {
                ImageFormat::Png => common::ImageEncoding::Png,
                ImageFormat::Jpeg => common::ImageEncoding::Jpeg,
            } as i32,
            width_pixels: shot.width_pixels,
            height_pixels: shot.height_pixels,
            display_points: Some(shot.display.into()),
            data: shot.data.clone().into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_keep_what_this_build_cant_read() {
        use agent::agent_event::Kind;
        let event = |kind| GuestEvent::from(agent::AgentEvent { kind });
        assert_eq!(
            event(Some(Kind::UiChanged(agent::UiChanged { pid: 612 }))),
            GuestEvent::UiChanged(ProcessId::new(612))
        );
        assert_eq!(
            event(Some(Kind::FrontmostAppChanged(
                agent::FrontmostAppChanged { app: None }
            ))),
            GuestEvent::FrontmostAppChanged(None)
        );
        assert_eq!(
            event(Some(Kind::PermissionsChanged(agent::PermissionsChanged {
                granted: vec![42],
            }))),
            GuestEvent::PermissionsChanged(vec![Permission::Unrecognized(42)])
        );
        // A kind from a newer agent decodes to no kind at all.
        assert_eq!(event(None), GuestEvent::Unrecognized);
    }

    #[test]
    fn status_keeps_unknown_values() {
        let status = AgentStatus::try_from(agent::AgentStatus {
            granted: vec![common::Permission::Accessibility as i32, 42],
            session: 9,
            display_points: Some(common::Size {
                width: 1280.0,
                height: 800.0,
            }),
            frontmost_app: Some(agent::RunningApp {
                pid: 7,
                bundle_id: String::new(),
                name: "x".into(),
            }),
        })
        .unwrap();
        assert_eq!(
            status.granted,
            vec![Permission::Accessibility, Permission::Unrecognized(42)]
        );
        assert_eq!(status.session, Session::Unrecognized(9));
        assert_eq!(
            AgentStatus::try_from(agent::AgentStatus::from(&status)),
            Ok(status.clone()),
            "unknown values survive a round trip"
        );
        assert_eq!(status.frontmost.unwrap().bundle_id, None);
    }

    #[test]
    fn screenshots_round_trip() {
        let shot = Screenshot {
            format: ImageFormat::Jpeg,
            width_pixels: 2,
            height_pixels: 1,
            display: Size::new(1280.0, 800.0).unwrap(),
            data: vec![1, 2, 3],
        };
        assert_eq!(Screenshot::try_from(common::Image::from(&shot)), Ok(shot));
    }

    #[test]
    fn images_must_be_complete() {
        let good = common::Image {
            encoding: common::ImageEncoding::Png as i32,
            width_pixels: 2,
            height_pixels: 1,
            display_points: Some(common::Size {
                width: 1.0,
                height: 0.5,
            }),
            data: vec![1].into(),
        };
        assert!(Screenshot::try_from(good.clone()).is_ok());
        assert!(
            Screenshot::try_from(common::Image {
                data: Default::default(),
                ..good.clone()
            })
            .is_err()
        );
        assert!(
            Screenshot::try_from(common::Image {
                encoding: 0,
                ..good.clone()
            })
            .is_err()
        );
        assert!(
            Screenshot::try_from(common::Image {
                display_points: None,
                ..good
            })
            .is_err()
        );
    }
}
