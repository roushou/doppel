//! Text recognized on a display (OCR), for apps whose accessibility tree is
//! missing or useless. Recognition happens in the shell; this module holds
//! the result, finds text in it, and renders it for a model.
//!
//! Everything is in display points with a top-left origin, so a match's
//! point goes straight into a click.

use std::fmt::Write as _;
use std::ops::Range;

use crate::geometry::{Rect, Size};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScreenTextError {
    #[error("confidence must be between 0 and 1 (got {0})")]
    InvalidConfidence(String),
    #[error("word {word:?} doesn't occur in its line {line:?} after the previous word")]
    WordNotInLine { word: String, line: String },
    #[error("the text to find is empty")]
    EmptyQuery,
}

/// A recognized box's vertical middle.
fn middle(rect: Rect) -> f64 {
    rect.top() + rect.height() / 2.0
}

/// Whether two boxes sit on one row: their middles are closer than half
/// the shorter one's height. Recognized boxes of one row (a menu bar)
/// differ by fractions of a point, so sorting by y alone scrambles them.
fn shares_row(a: Rect, b: Rect) -> bool {
    (middle(a) - middle(b)).abs() < a.height().min(b.height()) / 2.0
}

/// How sure recognition is of a line, 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f32);

impl Confidence {
    /// Below this, a line is marked as a guess.
    const UNSURE: Self = Self(0.5);

    /// # Errors
    /// [`ScreenTextError::InvalidConfidence`].
    pub fn new(value: f32) -> Result<Self, ScreenTextError> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ScreenTextError::InvalidConfidence(value.to_string()))
        }
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// A word of a line: where it sits in the line's text and on the display.
#[derive(Debug, Clone, PartialEq)]
struct Word {
    span: Range<usize>,
    region: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    text: String,
    region: Rect,
    confidence: Confidence,
    words: Vec<Word>,
}

impl TextLine {
    /// `words` are the line's words in order, each with its region.
    ///
    /// # Errors
    /// [`ScreenTextError::WordNotInLine`] when a word isn't found in `text`
    /// after the one before it.
    pub fn new(
        text: String,
        region: Rect,
        confidence: Confidence,
        words: Vec<(String, Rect)>,
    ) -> Result<Self, ScreenTextError> {
        let mut from = 0;
        let words = words
            .into_iter()
            .map(|(word, region)| {
                let start = text
                    .get(from..)
                    .and_then(|rest| rest.find(word.as_str()))
                    .map(|at| from + at)
                    .ok_or_else(|| ScreenTextError::WordNotInLine {
                        word: word.clone(),
                        line: text.clone(),
                    })?;
                from = start + word.len();
                Ok(Word {
                    span: start..from,
                    region,
                })
            })
            .collect::<Result<_, ScreenTextError>>()?;
        Ok(Self {
            text,
            region,
            confidence,
            words,
        })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn region(&self) -> Rect {
        self.region
    }

    #[must_use]
    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }

    /// The words in order, each with its region.
    pub fn words(&self) -> impl Iterator<Item = (&str, Rect)> {
        self.words
            .iter()
            .map(|word| (&self.text[word.span.clone()], word.region))
    }

    /// The region of the words a byte range of the text touches; the whole
    /// line when no word box covers it.
    fn region_of(&self, span: &Range<usize>) -> Rect {
        self.words
            .iter()
            .filter(|w| w.span.start < span.end && span.start < w.span.end)
            .map(|w| w.region)
            .reduce(Rect::union)
            .unwrap_or(self.region)
    }

    fn is_unsure(&self) -> bool {
        self.confidence < Confidence::UNSURE
    }
}

/// Text to look for: case-insensitive, with runs of whitespace equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextQuery {
    raw: String,
    folded: String,
}

impl TextQuery {
    /// The text as given (trimmed).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl std::str::FromStr for TextQuery {
    type Err = ScreenTextError;
    fn from_str(raw: &str) -> Result<Self, ScreenTextError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ScreenTextError::EmptyQuery);
        }
        Ok(Self {
            raw: raw.to_owned(),
            folded: Folded::new(raw).text,
        })
    }
}

/// A line's text lowercased with whitespace runs collapsed, remembering
/// where each folded byte came from in the original.
struct Folded {
    text: String,
    origin: Vec<usize>,
    original_len: usize,
}

impl Folded {
    fn new(original: &str) -> Self {
        let mut text = String::with_capacity(original.len());
        let mut origin = Vec::with_capacity(original.len());
        for (at, c) in original.char_indices() {
            let pieces: Vec<char> = if c.is_whitespace() {
                if text.ends_with(' ') {
                    continue;
                }
                vec![' ']
            } else {
                c.to_lowercase().collect()
            };
            for piece in pieces {
                text.push(piece);
                origin.resize(text.len(), at);
            }
        }
        Self {
            text,
            origin,
            original_len: original.len(),
        }
    }

    /// The original byte range a folded range came from.
    fn original(&self, folded: Range<usize>) -> Range<usize> {
        let start = self
            .origin
            .get(folded.start)
            .copied()
            .unwrap_or(self.original_len);
        let end = self
            .origin
            .get(folded.end)
            .copied()
            .unwrap_or(self.original_len);
        start..end
    }
}

/// Where a query was found: the matched words' region within a line.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMatch<'a> {
    pub line: &'a TextLine,
    /// The matched text as it reads in the line.
    pub text: &'a str,
    pub region: Rect,
}

/// What recognition read off a display.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenText {
    display: Size,
    lines: Vec<TextLine>,
    recognizer: Recognizer,
}

/// Which of Vision's recognizers read a display's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recognizer {
    /// The accurate one, as asked.
    Accurate,
    /// The fast one, because the accurate one failed: it misreads small and
    /// dense text more often.
    Fast(AccurateFailed),
}

/// Why Vision's accurate recognizer failed. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccurateFailed(String);

impl AccurateFailed {
    /// `None` for an empty reason, which says nothing failed.
    #[must_use]
    pub fn new(why: impl Into<String>) -> Option<Self> {
        let why = why.into();
        (!why.is_empty()).then_some(Self(why))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ScreenText {
    /// Lines rendered at most; past this the rest is counted, not listed.
    pub const RENDERED_LINES: usize = 300;

    /// Orders the lines for reading: rows top to bottom, each left to right.
    #[must_use]
    pub fn new(display: Size, mut lines: Vec<TextLine>) -> Self {
        lines.sort_by(|a, b| middle(a.region).total_cmp(&middle(b.region)));
        let mut rows: Vec<Vec<TextLine>> = Vec::new();
        for line in lines {
            match rows.last_mut() {
                Some(row) if shares_row(row[0].region, line.region) => row.push(line),
                Some(_) | None => rows.push(vec![line]),
            }
        }
        let lines = rows
            .into_iter()
            .flat_map(|mut row| {
                row.sort_by(|a, b| a.region.left().total_cmp(&b.region.left()));
                row
            })
            .collect();
        Self {
            display,
            lines,
            recognizer: Recognizer::Accurate,
        }
    }

    /// The same text, as read by `recognizer`.
    #[must_use]
    pub fn read_by(self, recognizer: Recognizer) -> Self {
        Self { recognizer, ..self }
    }

    #[must_use]
    pub const fn recognizer(&self) -> &Recognizer {
        &self.recognizer
    }

    #[must_use]
    pub const fn display(&self) -> Size {
        self.display
    }

    #[must_use]
    pub fn lines(&self) -> &[TextLine] {
        &self.lines
    }

    /// Every occurrence of the query, in reading order.
    #[must_use]
    pub fn find(&self, query: &TextQuery) -> Vec<TextMatch<'_>> {
        self.lines
            .iter()
            .flat_map(|line| {
                let folded = Folded::new(&line.text);
                folded
                    .text
                    .match_indices(query.folded.as_str())
                    .map(|(at, found)| folded.original(at..at + found.len()))
                    .map(|span| TextMatch {
                        line,
                        text: line.text.get(span.clone()).unwrap_or(&line.text),
                        region: line.region_of(&span),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The text a model reads: each line (or match) with the point to click.
    #[must_use]
    pub fn render(&self, query: Option<&TextQuery>) -> String {
        let total = self.lines.len();
        let mut out = String::new();
        let lines = |n: usize| format!("{n} recognized line{}", if n == 1 { "" } else { "s" });
        // (what was read, the line it's in, where to click)
        let shown: Vec<(&str, &TextLine, Rect)> = match query {
            None => {
                let _ = writeln!(
                    out,
                    "# Text on screen · {} · {total} line{}",
                    self.display,
                    if total == 1 { "" } else { "s" }
                );
                self.lines
                    .iter()
                    .map(|line| (line.text.as_str(), line, line.region))
                    .collect()
            }
            Some(query) => {
                let matches = self.find(query);
                let _ = match matches.len() {
                    0 => writeln!(out, "# {:?} not found in {}", query.raw, lines(total)),
                    n => writeln!(
                        out,
                        "# {:?} · {n} match{} in {}",
                        query.raw,
                        if n == 1 { "" } else { "es" },
                        lines(total)
                    ),
                };
                matches
                    .into_iter()
                    .map(|m| (m.text, m.line, m.region))
                    .collect()
            }
        };
        match &self.recognizer {
            Recognizer::Accurate => {}
            Recognizer::Fast(why) => {
                let _ = writeln!(
                    out,
                    "Read by the fast recognizer, as the accurate one failed ({}): small or dense text may be misread.",
                    why.as_str()
                );
            }
        }
        if !shown.is_empty() {
            out.push_str("Points are display points: pass them to click as x, y.\n");
        }
        for &(text, line, region) in shown.iter().take(Self::RENDERED_LINES) {
            let at = region.center();
            let (x, y) = (at.x(), at.y());
            let _ = write!(out, "- {text:?} at {x:.0}, {y:.0}");
            if text != line.text {
                let _ = write!(out, " in {:?}", line.text);
            }
            if line.is_unsure() {
                out.push_str(" (unsure)");
            }
            out.push('\n');
        }
        if let Some(more) = shown
            .len()
            .checked_sub(Self::RENDERED_LINES)
            .filter(|n| *n > 0)
        {
            let _ = writeln!(out, "… {more} more not shown; pass text to narrow it down.");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xy(at: crate::geometry::Point) -> (f64, f64) {
        (at.x(), at.y())
    }

    fn region(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect::new(x, y, width, height).unwrap()
    }

    /// A line whose words are laid out left to right, 10 pt per character.
    fn line(text: &str, x: f64, y: f64, confidence: f32) -> TextLine {
        let mut words = Vec::new();
        let mut at = 0;
        for word in text.split_whitespace() {
            let start = at + text[at..].find(word).unwrap();
            words.push((
                word.to_owned(),
                region(x + 10.0 * start as f64, y, 10.0 * word.len() as f64, 20.0),
            ));
            at = start + word.len();
        }
        TextLine::new(
            text.to_owned(),
            region(x, y, 10.0 * text.len() as f64, 20.0),
            Confidence::new(confidence).unwrap(),
            words,
        )
        .unwrap()
    }

    fn screen(lines: Vec<TextLine>) -> ScreenText {
        ScreenText::new(Size::new(1280.0, 800.0).unwrap(), lines)
    }

    fn query(raw: &str) -> TextQuery {
        raw.parse().unwrap()
    }

    #[test]
    fn an_empty_reason_is_no_failure() {
        assert_eq!(AccurateFailed::new(""), None);
        assert_eq!(
            AccurateFailed::new("e5rtError 13").unwrap().as_str(),
            "e5rtError 13"
        );
    }

    #[test]
    fn text_from_the_fast_recognizer_says_so() {
        let text = screen(vec![line("Save", 100.0, 500.0, 0.9)]).read_by(Recognizer::Fast(
            AccurateFailed::new("e5rtError 13").unwrap(),
        ));
        insta::assert_snapshot!(text.render(None), @r#"
        # Text on screen · 1280×800 pt · 1 line
        Read by the fast recognizer, as the accurate one failed (e5rtError 13): small or dense text may be misread.
        Points are display points: pass them to click as x, y.
        - "Save" at 120, 510
        "#);
    }

    #[test]
    fn a_match_points_at_its_words_not_the_line() {
        // "Save" starts at character 6: x 160..200, so its center is 180.
        let text = screen(vec![line("Don't Save Cancel", 100.0, 500.0, 0.9)]);
        let found = text.find(&query("save"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "Save");
        assert_eq!(xy(found[0].region.center()), (180.0, 510.0));
        assert_eq!(found[0].line.text(), "Don't Save Cancel");
    }

    #[test]
    fn a_match_across_words_covers_them_all() {
        let text = screen(vec![line("Cancel Save Changes", 0.0, 0.0, 0.9)]);
        let found = text.find(&query("SAVE   changes"));
        // "Save" starts at 7 (x 70), "Changes" ends at 19 (x 190).
        assert_eq!(found[0].region, region(70.0, 0.0, 120.0, 20.0));
    }

    #[test]
    fn every_occurrence_is_found_in_reading_order() {
        let text = screen(vec![
            line("Save", 300.0, 400.0, 0.9),
            line("Save as… Save", 0.0, 100.0, 0.9),
        ]);
        let points: Vec<_> = text
            .find(&query("save"))
            .iter()
            .map(|m| xy(m.region.center()))
            .collect();
        // The second "Save" is at byte 11: "…" takes three.
        assert_eq!(points, vec![(20.0, 110.0), (130.0, 110.0), (320.0, 410.0)]);
    }

    #[test]
    fn a_row_reads_left_to_right_though_its_boxes_differ_slightly_in_height() {
        let text = screen(vec![
            line("Window", 500.0, 5.6, 0.9),
            line("below", 0.0, 40.0, 0.9),
            line("Chrome", 60.0, 5.2, 0.9),
            line("Edit", 160.0, 6.4, 0.9),
            line("File", 110.0, 5.0, 0.9),
        ]);
        let order: Vec<_> = text.lines().iter().map(TextLine::text).collect();
        assert_eq!(order, ["Chrome", "File", "Edit", "Window", "below"]);
    }

    #[test]
    fn folding_keeps_offsets_for_text_that_changes_length() {
        // "İ" lowercases to two chars; the match after it must still map back.
        let text = screen(vec![line("İstanbul Ok", 0.0, 0.0, 0.9)]);
        let found = text.find(&query("ok"));
        let ok = text.lines()[0].text().find("Ok").unwrap();
        assert_eq!(found[0].region.center().x(), 10.0 * ok as f64 + 10.0);
    }

    #[test]
    fn words_must_occur_in_their_line_in_order() {
        let r = region(0.0, 0.0, 1.0, 1.0);
        let c = Confidence::new(1.0).unwrap();
        assert!(TextLine::new("a b".into(), r, c, vec![("b".into(), r), ("a".into(), r)]).is_err());
        assert!(TextLine::new("a b".into(), r, c, vec![("x".into(), r)]).is_err());
        // A repeated word takes the next occurrence, not the first again.
        let twice = TextLine::new(
            "go go".into(),
            r,
            c,
            vec![("go".into(), r), ("go".into(), r)],
        )
        .unwrap();
        assert_eq!(twice.words[1].span, 3..5);
    }

    #[test]
    fn values_are_parsed() {
        assert_eq!("  ".parse::<TextQuery>(), Err(ScreenTextError::EmptyQuery));
        assert!(Confidence::new(1.5).is_err());
        assert!(Rect::new(0.0, 0.0, -1.0, 1.0).is_err());
        assert!(Rect::new(f64::NAN, 0.0, 1.0, 1.0).is_err());
        assert!(Rect::new(-2.0, 0.0, 1.0, 1.0).is_ok(), "boxes may overhang");
        assert_eq!(xy(region(-4.0, -4.0, 4.0, 4.0).center()), (0.0, 0.0));
    }

    #[test]
    fn renders_lines_with_points_and_marks_guesses() {
        let text = screen(vec![
            line("Untitled", 600.0, 4.0, 0.95),
            line("lorem ipsvm", 40.0, 200.0, 0.3),
        ]);
        insta::assert_snapshot!(text.render(None), @r#"
        # Text on screen · 1280×800 pt · 2 lines
        Points are display points: pass them to click as x, y.
        - "Untitled" at 640, 14
        - "lorem ipsvm" at 95, 210 (unsure)
        "#);
    }

    #[test]
    fn renders_matches_and_misses() {
        let text = screen(vec![line("Don't Save Cancel", 100.0, 500.0, 0.9)]);
        insta::assert_snapshot!(text.render(Some(&query("save"))), @r#"
        # "save" · 1 match in 1 recognized line
        Points are display points: pass them to click as x, y.
        - "Save" at 180, 510 in "Don't Save Cancel"
        "#);
        insta::assert_snapshot!(text.render(Some(&query("Open"))), @r#"# "Open" not found in 1 recognized line"#);
    }

    #[test]
    fn long_screens_are_cut_and_counted() {
        let lines = (0..ScreenText::RENDERED_LINES + 2)
            .map(|i| line("x", 0.0, i as f64, 0.9))
            .collect();
        let out = screen(lines).render(None);
        assert_eq!(
            out.lines().filter(|l| l.starts_with("- ")).count(),
            ScreenText::RENDERED_LINES
        );
        assert!(
            out.ends_with("… 2 more not shown; pass text to narrow it down.\n"),
            "{out}"
        );
    }
}
