use doppel_desktop::geometry::Rect;
use doppel_desktop::geometry::Size;
use doppel_desktop::screen_text::{AccurateFailed, Confidence, Recognizer, ScreenText, TextLine};

use super::ParseError;
use crate::pb::common::v1 as common;
use crate::pb::vmm::v1 as vmm;

impl TryFrom<common::Rect> for Rect {
    type Error = ParseError;
    fn try_from(rect: common::Rect) -> Result<Self, ParseError> {
        let origin = rect.origin.ok_or(ParseError::Missing("frame.origin"))?;
        let size = rect.size.ok_or(ParseError::Missing("frame.size"))?;
        Self::new(origin.x, origin.y, size.width, size.height).map_err(|error| {
            ParseError::Invalid {
                field: "frame",
                reason: error.to_string(),
            }
        })
    }
}

impl TryFrom<vmm::TextLine> for TextLine {
    type Error = ParseError;
    fn try_from(line: vmm::TextLine) -> Result<Self, ParseError> {
        let words = line
            .words
            .into_iter()
            .map(|word| {
                let frame = word.frame.ok_or(ParseError::Missing("words.frame"))?;
                Ok((word.text, frame.try_into()?))
            })
            .collect::<Result<_, ParseError>>()?;
        Ok(Self::new(
            line.text,
            line.frame.ok_or(ParseError::Missing("frame"))?.try_into()?,
            Confidence::new(line.confidence)?,
            words,
        )?)
    }
}

impl TryFrom<vmm::RecognizedText> for ScreenText {
    type Error = ParseError;
    fn try_from(wire: vmm::RecognizedText) -> Result<Self, ParseError> {
        let display: Size = wire
            .display_points
            .ok_or(ParseError::Missing("display_points"))?
            .try_into()?;
        let lines = wire
            .lines
            .into_iter()
            .map(TextLine::try_from)
            .collect::<Result<_, _>>()?;
        let recognizer = match AccurateFailed::new(wire.accurate_failed) {
            None => Recognizer::Accurate,
            Some(why) => Recognizer::Fast(why),
        };
        Ok(Self::new(display, lines).read_by(recognizer))
    }
}

impl From<Rect> for common::Rect {
    fn from(rect: Rect) -> Self {
        Self {
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
}

impl From<&ScreenText> for vmm::RecognizedText {
    fn from(text: &ScreenText) -> Self {
        Self {
            display_points: Some(text.display().into()),
            lines: text
                .lines()
                .iter()
                .map(|line| vmm::TextLine {
                    text: line.text().to_owned(),
                    frame: Some(line.region().into()),
                    confidence: line.confidence().get(),
                    words: line
                        .words()
                        .map(|(word, region)| vmm::TextWord {
                            text: word.to_owned(),
                            frame: Some(region.into()),
                        })
                        .collect(),
                })
                .collect(),
            accurate_failed: match text.recognizer() {
                Recognizer::Accurate => String::new(),
                Recognizer::Fast(why) => why.as_str().to_owned(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Option<common::Rect> {
        Some(common::Rect {
            origin: Some(common::Point { x, y }),
            size: Some(common::Size { width, height }),
        })
    }

    fn wire(words: Vec<vmm::TextWord>, confidence: f32) -> vmm::RecognizedText {
        vmm::RecognizedText {
            display_points: Some(common::Size {
                width: 1280.0,
                height: 800.0,
            }),
            lines: vec![vmm::TextLine {
                text: "Cancel Save".into(),
                frame: rect(10.0, 20.0, 110.0, 20.0),
                confidence,
                words,
            }],
            accurate_failed: String::new(),
        }
    }

    #[test]
    fn recognized_text_round_trips() {
        let text = ScreenText::try_from(wire(
            vec![
                vmm::TextWord {
                    text: "Cancel".into(),
                    frame: rect(10.0, 20.0, 50.0, 12.0),
                },
                vmm::TextWord {
                    text: "Save".into(),
                    frame: rect(64.0, 20.0, 50.0, 12.0),
                },
            ],
            0.75,
        ))
        .unwrap();
        assert_eq!(
            ScreenText::try_from(vmm::RecognizedText::from(&text)),
            Ok(text)
        );
    }

    #[test]
    fn word_frames_carry_through_to_matches() {
        let text = ScreenText::try_from(wire(
            vec![
                vmm::TextWord {
                    text: "Cancel".into(),
                    frame: rect(10.0, 20.0, 60.0, 20.0),
                },
                vmm::TextWord {
                    text: "Save".into(),
                    frame: rect(80.0, 20.0, 40.0, 20.0),
                },
            ],
            0.9,
        ))
        .unwrap();
        let found = text.find(&"save".parse().unwrap());
        assert_eq!(
            (found[0].region.center().x(), found[0].region.center().y()),
            (100.0, 30.0)
        );
    }

    #[test]
    fn the_fast_recognizer_and_its_reason_round_trip() {
        let mut fast = wire(vec![], 0.9);
        fast.accurate_failed = "e5rtError 13".into();
        let text = ScreenText::try_from(fast).unwrap();
        assert_eq!(
            text.recognizer(),
            &Recognizer::Fast(AccurateFailed::new("e5rtError 13").unwrap())
        );
        assert_eq!(
            vmm::RecognizedText::from(&text).accurate_failed,
            "e5rtError 13"
        );
        let accurate = ScreenText::try_from(wire(vec![], 0.9)).unwrap();
        assert_eq!(accurate.recognizer(), &Recognizer::Accurate);
    }

    #[test]
    fn malformed_recognition_is_refused() {
        assert!(ScreenText::try_from(wire(vec![], 1.5)).is_err());
        let stray = vmm::TextWord {
            text: "Open".into(),
            frame: rect(0.0, 0.0, 1.0, 1.0),
        };
        assert!(ScreenText::try_from(wire(vec![stray], 0.9)).is_err());
        let mut no_frame = wire(vec![], 0.9);
        no_frame.lines[0].frame = None;
        assert!(ScreenText::try_from(no_frame).is_err());
    }
}
