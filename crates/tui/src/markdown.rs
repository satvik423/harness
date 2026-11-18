//! Markdown to styled terminal lines (P4-T7): `pulldown-cmark` for structure,
//! `syntect` for code blocks.

use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Syntax definitions and the code theme, loaded once (they take a few ms).
fn highlighting() -> &'static (SyntaxSet, Theme) {
    static ASSETS: OnceLock<(SyntaxSet, Theme)> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let mut themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .remove("base16-ocean.dark")
            .unwrap_or_default();
        (syntaxes, theme)
    })
}

const CODE: Style = Style::new().fg(Color::Yellow);
const DIM: Style = Style::new().fg(Color::DarkGray);

/// Builds lines while walking the Markdown events.
struct Builder {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    quote: usize,
    code: Option<(String, String)>,
    link: Option<String>,
}

impl Builder {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::default(), |acc, s| acc.patch(*s))
    }

    fn prefix(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        if self.quote > 0 {
            spans.push(Span::styled("│ ".repeat(self.quote), DIM));
        }
        spans
    }

    fn text(&mut self, text: &str) {
        if self.current.is_empty() {
            self.current = self.prefix();
        }
        let style = self.style();
        self.current.push(Span::styled(text.to_owned(), style));
    }

    fn end_line(&mut self) {
        let spans = std::mem::take(&mut self.current);
        self.lines.push(Line::from(spans));
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.end_line();
        }
    }

    fn blank(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn code_block(&mut self, lang: &str, body: &str) {
        self.flush();
        let (syntaxes, theme) = highlighting();
        let syntax = syntaxes
            .find_syntax_by_token(lang)
            .filter(|_| !lang.is_empty());
        let mut highlighter = syntax.map(|s| HighlightLines::new(s, theme));
        for raw in body.lines() {
            let mut spans = self.prefix();
            spans.push(Span::styled("  ", DIM));
            let line = format!("{raw}\n");
            let highlighted = highlighter
                .as_mut()
                .and_then(|h| h.highlight_line(&line, syntaxes).ok());
            match highlighted {
                Some(regions) => {
                    for (style, piece) in regions {
                        let fg = style.foreground;
                        spans.push(Span::styled(
                            piece.trim_end_matches('\n').to_owned(),
                            Style::new().fg(Color::Rgb(fg.r, fg.g, fg.b)),
                        ));
                    }
                }
                None => spans.push(Span::styled(raw.to_owned(), CODE)),
            }
            self.lines.push(Line::from(spans));
        }
        self.lines.push(Line::default());
    }
}

/// Renders Markdown as terminal lines: headings, emphasis, lists, quotes,
/// links, tables and highlighted code blocks.
///
/// # Examples
/// ```
/// let lines = harness_tui::markdown::render("# Title\n\nSome **bold** text.");
/// assert_eq!(lines[0].to_string(), "Title");
/// assert_eq!(lines[2].to_string(), "Some bold text.");
/// ```
#[must_use]
pub fn render(markdown: &str) -> Vec<Line<'static>> {
    let mut b = Builder {
        lines: Vec::new(),
        current: Vec::new(),
        styles: Vec::new(),
        lists: Vec::new(),
        quote: 0,
        code: None,
        link: None,
    };
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        if let Some((_, body)) = &mut b.code {
            match event {
                Event::Text(t) => body.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    if let Some((lang, body)) = b.code.take() {
                        b.code_block(&lang, &body);
                    }
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                b.blank();
                let color = if level == HeadingLevel::H1 {
                    Color::Cyan
                } else {
                    Color::Blue
                };
                b.styles
                    .push(Style::new().fg(color).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                b.styles.pop();
                b.flush();
                b.lines.push(Line::default());
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                b.flush();
                if b.lists.is_empty() {
                    b.lines.push(Line::default());
                }
            }
            Event::Start(Tag::Emphasis) => {
                b.styles.push(Style::new().add_modifier(Modifier::ITALIC))
            }
            Event::Start(Tag::Strong) => b.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Event::Start(Tag::Strikethrough) => {
                b.styles
                    .push(Style::new().add_modifier(Modifier::CROSSED_OUT));
            }
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough) => {
                b.styles.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                b.flush();
                b.quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                b.flush();
                b.quote = b.quote.saturating_sub(1);
            }
            Event::Start(Tag::List(start)) => {
                b.flush();
                b.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                b.flush();
                b.lists.pop();
                if b.lists.is_empty() {
                    b.lines.push(Line::default());
                }
            }
            Event::Start(Tag::Item) => {
                b.flush();
                let depth = b.lists.len().saturating_sub(1);
                let bullet = match b.lists.last_mut() {
                    Some(Some(n)) => {
                        let s = format!("{n}. ");
                        *n += 1;
                        s
                    }
                    _ => "• ".to_owned(),
                };
                b.current = b.prefix();
                b.current
                    .push(Span::raw(format!("{}{bullet}", "  ".repeat(depth))));
            }
            Event::End(TagEnd::Item) => b.flush(),
            Event::TaskListMarker(done) => b.text(if done { "[x] " } else { "[ ] " }),
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                b.code = Some((lang, String::new()));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                b.link = Some(dest_url.to_string());
                b.styles.push(
                    Style::new()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Event::End(TagEnd::Link) => {
                b.styles.pop();
                if let Some(url) = b.link.take() {
                    b.current.push(Span::styled(format!(" ({url})"), DIM));
                }
            }
            Event::Start(Tag::TableCell) => {
                if !b.current.is_empty() {
                    b.current.push(Span::styled(" │ ", DIM));
                }
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => b.end_line(),
            Event::End(TagEnd::Table) => b.lines.push(Line::default()),
            Event::Text(t) => b.text(&t),
            Event::Code(t) => {
                if b.current.is_empty() {
                    b.current = b.prefix();
                }
                b.current.push(Span::styled(t.to_string(), CODE));
            }
            Event::SoftBreak => b.text(" "),
            Event::HardBreak => b.end_line(),
            Event::Rule => {
                b.flush();
                b.lines.push(Line::styled("─".repeat(40), DIM));
            }
            Event::Html(t) | Event::InlineHtml(t) => b.text(&t),
            _ => {}
        }
    }
    b.flush();
    while b.lines.last().is_some_and(|l| l.spans.is_empty()) {
        b.lines.pop();
    }
    b.lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn paragraphs_lists_and_quotes() {
        let lines = render("Intro line\nwrapped.\n\n- one\n- two\n  1. nested\n\n> quoted");
        assert_eq!(
            text(&lines),
            [
                "Intro line wrapped.",
                "",
                "• one",
                "• two",
                "  1. nested",
                "",
                "│ quoted"
            ]
        );
    }

    #[test]
    fn emphasis_and_inline_code_are_styled() {
        let lines = render("a **b** *c* `d`");
        let spans = &lines[0].spans;
        assert!(
            spans
                .iter()
                .any(|s| s.content == "b" && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            spans
                .iter()
                .any(|s| s.content == "c" && s.style.add_modifier.contains(Modifier::ITALIC))
        );
        assert!(spans.iter().any(|s| s.content == "d" && s.style == CODE));
    }

    #[test]
    fn code_blocks_are_highlighted_by_language() {
        let lines = render("```rust\nfn main() {}\n```");
        assert_eq!(lines[0].to_string(), "  fn main() {}");
        let colours: Vec<_> = lines[0].spans.iter().filter_map(|s| s.style.fg).collect();
        assert!(
            colours.iter().any(|c| matches!(c, Color::Rgb(..))),
            "{colours:?}"
        );
        let plain = render("```\nno lang\n```");
        assert_eq!(plain[0].spans[1].style, CODE);
    }

    #[test]
    fn links_tables_and_tasks() {
        assert_eq!(
            render("[docs](https://docs.rs)")[0].to_string(),
            "docs (https://docs.rs)"
        );
        let table = render("| a | b |\n|---|---|\n| 1 | 2 |");
        assert_eq!(text(&table)[..2], ["a │ b", "1 │ 2"]);
        assert_eq!(render("- [x] done")[0].to_string(), "• [x] done");
    }

    #[test]
    fn headings_are_bold_and_spaced() {
        let lines = render("## Steps\nDo it.");
        assert_eq!(text(&lines), ["Steps", "", "Do it."]);
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }
}
