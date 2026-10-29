//! The links and bold text of a message in the reader. The text stays in a
//! read-only editor, so that it can still be selected and copied (a code
//! sent by mail); the marks are laid over it as a highlighter, and a click
//! on a link opens it.

use crate::theme;
use avero_sync::parse::Line;
use iced::advanced::text::highlighter::{self, Format};
use iced::{Font, Theme};
use std::ops::Range;
use std::sync::Arc;

/// How a piece of text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Link,
    Strong,
}

/// A link of the message: where it goes, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub label: String,
}

/// One marked piece of a line: its byte range, and where it goes if it
/// is a link.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marked {
    range: Range<usize>,
    mark: Mark,
    link: Option<Link>,
}

/// The marks of a whole message, line by line. Shared, and compared by
/// identity: the reader hands them to its editor at every frame.
#[derive(Debug, Clone, Default)]
pub struct Marks(Arc<Vec<Vec<Marked>>>);

impl PartialEq for Marks {
    fn eq(&self, other: &Marks) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Marks {
    /// The link under column `column` (a byte index) of line `line`.
    pub fn link_at(&self, line: usize, column: usize) -> Option<&Link> {
        self.0
            .get(line)?
            .iter()
            .find(|marked| marked.range.start <= column && column < marked.range.end)?
            .link
            .as_ref()
    }
}

/// The text the reader shows for `lines`, and its marks.
pub fn layout(lines: &[Line]) -> (String, Marks) {
    let mut text = String::new();
    let mut marks = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        let mut marked = Vec::new();
        let mut column = 0;
        for span in &line.spans {
            let range = column..column + span.text.len();
            let mark = if span.link.is_some() {
                Some(Mark::Link)
            } else if span.strong || line.heading {
                Some(Mark::Strong)
            } else {
                None
            };
            if let Some(mark) = mark {
                marked.push(Marked {
                    range: range.clone(),
                    mark,
                    link: span.link.clone().map(|url| Link {
                        url,
                        label: span.text.clone(),
                    }),
                });
            }
            text.push_str(&span.text);
            column = range.end;
        }
        marks.push(marked);
    }
    (text, Marks(Arc::new(marks)))
}

/// Feeds the marks to the editor, line after line.
pub struct Highlighter {
    marks: Marks,
    line: usize,
}

impl highlighter::Highlighter for Highlighter {
    type Settings = Marks;
    type Highlight = Mark;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Mark)>;

    fn new(marks: &Marks) -> Highlighter {
        Highlighter {
            marks: marks.clone(),
            line: 0,
        }
    }

    fn update(&mut self, marks: &Marks) {
        self.marks = marks.clone();
        self.line = 0;
    }

    fn change_line(&mut self, line: usize) {
        self.line = self.line.min(line);
    }

    fn highlight_line(&mut self, _line: &str) -> Self::Iterator<'_> {
        let marks: Vec<_> = self
            .marks
            .0
            .get(self.line)
            .map(|line| {
                line.iter()
                    .map(|marked| (marked.range.clone(), marked.mark))
                    .collect()
            })
            .unwrap_or_default();
        self.line += 1;
        marks.into_iter()
    }

    fn current_line(&self) -> usize {
        self.line
    }
}

/// How the editor draws a mark.
pub fn format(mark: &Mark, theme: &Theme) -> Format<Font> {
    match mark {
        Mark::Link => Format {
            color: Some(theme::tokens(theme).accent),
            font: None,
        },
        Mark::Strong => Format {
            color: None,
            font: Some(theme::SEMIBOLD),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avero_sync::parse::Span;

    fn span(text: &str, link: Option<&str>, strong: bool) -> Span {
        Span {
            text: text.into(),
            link: link.map(str::to_string),
            strong,
        }
    }

    #[test]
    fn links_are_found_where_they_are_drawn() {
        let lines = vec![
            Line {
                spans: vec![span("Titre", None, false)],
                heading: true,
            },
            Line::default(),
            Line {
                spans: vec![
                    span("Voir ", None, false),
                    span("ici", Some("https://ex.com"), false),
                    span(" et là", None, true),
                ],
                heading: false,
            },
        ];
        let (text, marks) = layout(&lines);
        assert_eq!(text, "Titre\n\nVoir ici et là");
        assert_eq!(marks.link_at(2, 4), None);
        let link = Link {
            url: "https://ex.com".into(),
            label: "ici".into(),
        };
        assert_eq!(marks.link_at(2, 5), Some(&link));
        assert_eq!(marks.link_at(2, 7), Some(&link));
        assert_eq!(marks.link_at(2, 8), None);
        assert_eq!(marks.link_at(0, 0), None);
        assert_eq!(marks.link_at(9, 0), None);

        let mut highlighter = <Highlighter as highlighter::Highlighter>::new(&marks);
        let first: Vec<_> =
            highlighter::Highlighter::highlight_line(&mut highlighter, "").collect();
        assert_eq!(first, vec![(0..5, Mark::Strong)]);
        highlighter::Highlighter::highlight_line(&mut highlighter, "");
        let third: Vec<_> =
            highlighter::Highlighter::highlight_line(&mut highlighter, "").collect();
        assert_eq!(third, vec![(5..8, Mark::Link), (8..15, Mark::Strong)]);
    }
}
