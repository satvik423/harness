//! Drawing the [`App`]: header, transcript, prompt panel, input box, status
//! bar and overlays (Tech spec 3.16 layout).

use harness_ports::TaskRecord;
use harness_protocol::TaskState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::{App, Item, Level, Overlay, Prompt, ToolCard};
use crate::backend::ChangedFile;
use crate::diff::{DiffLine, counts, unified};
use crate::ledger::{money, tokens};
use crate::markdown;
use crate::slash::COMMANDS;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DIM: Style = Style::new().fg(Color::DarkGray);
const ACCENT: Style = Style::new().fg(Color::Cyan);

fn state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::AwaitingApproval => "waiting for you",
        TaskState::Paused => "paused",
        TaskState::Recovering => "recovering",
        TaskState::Completed => "done",
        TaskState::Failed => "failed",
        TaskState::BudgetExhausted => "out of budget",
        TaskState::Cancelled => "cancelled",
    }
}

fn diff_line(line: &DiffLine) -> Line<'static> {
    match line {
        DiffLine::Header(t) => Line::styled(format!("    {t}"), Style::new().fg(Color::Magenta)),
        DiffLine::Same(t) => Line::styled(format!("      {t}"), DIM),
        DiffLine::Removed(t) => Line::styled(format!("    - {t}"), Style::new().fg(Color::Red)),
        DiffLine::Added(t) => Line::styled(format!("    + {t}"), Style::new().fg(Color::Green)),
        DiffLine::Note(t) => Line::styled(format!("    {t}"), DIM),
    }
}

fn tool_lines(card: &ToolCard, tick: u64, out: &mut Vec<Line<'static>>) {
    let status = match card.ok {
        None => Span::styled(
            SPINNER[usize::try_from(tick % 10).unwrap_or(0)],
            Style::new().fg(Color::Yellow),
        ),
        Some(true) => Span::styled("✓", Style::new().fg(Color::Green)),
        Some(false) => Span::styled("✗", Style::new().fg(Color::Red)),
    };
    let mut head = vec![
        Span::styled("▸ ", ACCENT),
        Span::styled(card.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ];
    if card.todo.is_empty() {
        head.push(Span::raw(card.summary.clone()));
        head.push(Span::raw(" "));
    }
    head.push(status);
    let (added, removed) = counts(&card.preview);
    if added + removed > 0 {
        head.push(Span::styled(format!(" (+{added} −{removed})"), DIM));
    }
    out.push(Line::from(head));
    for (status, text) in &card.todo {
        let (mark, style) = match status.as_str() {
            "completed" => ("[x]", Style::new().fg(Color::Green)),
            "in_progress" => ("[~]", Style::new().fg(Color::Yellow)),
            _ => ("[ ]", Style::default()),
        };
        out.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(mark, style),
            Span::raw(format!(" {text}")),
        ]));
    }
    // Diff previews while the change is pending; the result line after.
    if card.ok.is_none() {
        out.extend(card.preview.iter().take(12).map(diff_line));
        for line in card
            .live
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            out.push(Line::styled(format!("    {line}"), DIM));
        }
    } else if !card.result.is_empty() && card.todo.is_empty() {
        let style = if card.ok == Some(false) {
            Style::new().fg(Color::Red)
        } else {
            DIM
        };
        out.push(Line::styled(format!("    {}", card.result), style));
    }
}

/// Every transcript line, before wrapping.
#[must_use]
pub fn transcript_lines(app: &App) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for item in &app.items {
        match item {
            Item::User { text, attachments } => {
                out.push(Line::default());
                for (i, line) in text.lines().enumerate() {
                    let lead = if i == 0 { "you › " } else { "      " };
                    out.push(Line::from(vec![
                        Span::styled(lead, ACCENT.add_modifier(Modifier::BOLD)),
                        Span::raw(line.to_owned()),
                    ]));
                }
                for path in attachments {
                    out.push(Line::styled(format!("      📎 {path}"), DIM));
                }
            }
            Item::Assistant { text, done } => {
                let mut lines = markdown::render(text);
                if !done && let Some(last) = lines.last_mut() {
                    last.spans.push(Span::styled("▍", ACCENT));
                }
                out.extend(lines);
            }
            Item::Thinking(text) => {
                for line in text.lines() {
                    out.push(Line::styled(
                        line.to_owned(),
                        DIM.add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            Item::Tool(card) => tool_lines(card, app.tick, &mut out),
            Item::Notice { level, text } => {
                let style = match level {
                    Level::Info => DIM,
                    Level::Warning => Style::new().fg(Color::Yellow),
                    Level::Error => Style::new().fg(Color::Red),
                };
                out.push(Line::styled(format!("• {text}"), style));
            }
        }
    }
    out
}

fn header(app: &App) -> Line<'static> {
    let effort = app
        .view
        .effort
        .map(|e| format!(" · {e:?}").to_lowercase())
        .unwrap_or_default();
    Line::from(vec![
        Span::styled(
            " harness ",
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {} · {}{effort} · seg {} · {}",
            app.view.persona, app.view.model, app.segment, app.view.profile
        )),
    ])
}

/// The status bar text: context, cache, cost, task state and key hints.
#[must_use]
pub fn status_line(app: &App) -> Line<'static> {
    let session = app.ledger.session();
    let used = app
        .ledger
        .last()
        .map_or(0, |t| t.usage.total_input() + t.usage.output);
    let cost = if app.view.priced {
        money(session.cost_micros)
    } else {
        "$?".to_owned()
    };
    let state = app.task.map_or("idle", |(_, s)| state_name(s));
    let hint = if app.busy() {
        "Esc interrupt · ^P pause"
    } else {
        "/help · ^D quit"
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cache = (session.cache_ratio() * 100.0).round() as u64;
    Line::from(vec![
        Span::raw(format!(
            " {}/{} ctx · cache {cache}% · {cost} · task {state}",
            tokens(used),
            tokens(app.view.context_limit)
        )),
        Span::styled(format!(" · {hint}"), DIM),
    ])
}

/// A prompt panel: title, content, and key hints kept in the bottom border
/// (so a long, wrapped reason can never push them out of sight).
struct Panel {
    title: String,
    lines: Vec<Line<'static>>,
    keys: Option<Line<'static>>,
}

fn prompt_panel(prompt: &Prompt) -> Panel {
    let (title, lines) = match prompt {
        Prompt::Approval {
            reason, preview, ..
        } => {
            let mut lines = vec![Line::raw(reason.clone())];
            lines.extend(preview.iter().take(10).map(diff_line));
            let keys = Line::from(vec![
                Span::raw(" "),
                Span::styled("[o]", ACCENT),
                Span::raw("nce  "),
                Span::styled("[s]", ACCENT),
                Span::raw("ession  "),
                Span::styled("[a]", ACCENT),
                Span::raw("lways  "),
                Span::styled("[p]", ACCENT),
                Span::raw("roject  "),
                Span::styled("[d]", ACCENT),
                Span::raw("eny "),
            ]);
            return Panel {
                title: "Approve?".into(),
                lines,
                keys: Some(keys),
            };
        }
        Prompt::Reason { text, .. } => (
            "Deny: why? (Enter to send, Esc for no reason)".to_owned(),
            vec![Line::raw(format!("{text}▍"))],
        ),
        Prompt::Question {
            question,
            options,
            multi,
            chosen,
            ..
        } => {
            let mut lines = vec![Line::raw(question.clone())];
            for (i, (option, on)) in options.iter().zip(chosen).enumerate() {
                let mark = if *multi {
                    if *on { "[x] " } else { "[ ] " }
                } else {
                    ""
                };
                lines.push(Line::raw(format!("  {}. {mark}{option}", i + 1)));
            }
            let help = if *multi {
                "numbers to choose, Enter to send, or type your own answer"
            } else {
                "a number to answer, or type your own answer and press Enter"
            };
            lines.push(Line::styled(help, DIM));
            ("Question".to_owned(), lines)
        }
        Prompt::Confirm { question, .. } => {
            ("Confirm".to_owned(), vec![Line::raw(question.clone())])
        }
    };
    Panel {
        title,
        lines,
        keys: None,
    }
}

/// Draws the whole screen.
pub fn render(app: &App, frame: &mut Frame<'_>) {
    let prompt = app.prompt.as_ref().map(prompt_panel);
    // Height of the wrapped content plus the borders, capped so the transcript stays visible.
    let width = frame.area().width.saturating_sub(2).max(1);
    let max_panel = (frame.area().height / 2).max(4);
    let prompt_height = prompt.as_ref().map_or(0, |p| {
        let rows = Paragraph::new(p.lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(width);
        u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(max_panel)
    });
    let input_height = u16::try_from(app.input.lines().len())
        .unwrap_or(1)
        .clamp(1, 6)
        + 2;
    let attach_height = u16::from(!app.attachments.is_empty());
    let [head, body, panel, attach, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(prompt_height),
        Constraint::Length(attach_height),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(Paragraph::new(header(app)), head);

    let transcript = Paragraph::new(transcript_lines(app)).wrap(Wrap { trim: false });
    let total = u16::try_from(transcript.line_count(body.width)).unwrap_or(u16::MAX);
    let max_back = total.saturating_sub(body.height);
    let offset = max_back.saturating_sub(app.scroll_back.min(max_back));
    frame.render_widget(transcript.scroll((offset, 0)), body);

    if let Some(p) = prompt {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(Color::Yellow))
            .title(format!(" {} ", p.title));
        if let Some(keys) = p.keys {
            block = block.title_bottom(keys);
        }
        frame.render_widget(
            Paragraph::new(p.lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            panel,
        );
    }
    if attach_height > 0 {
        let names: Vec<String> = app
            .attachments
            .iter()
            .map(|p| {
                format!(
                    "📎 {}",
                    p.file_name().map_or_else(
                        || p.display().to_string(),
                        |n| n.to_string_lossy().into_owned()
                    )
                )
            })
            .collect();
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {}  (Esc to remove)", names.join("  ")),
                DIM,
            )),
            attach,
        );
    }
    let mut input_box = app.input.clone();
    input_box.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(DIM)
            .title(" › "),
    );
    frame.render_widget(&input_box, input);
    frame.render_widget(
        Paragraph::new(status_line(app)).style(Style::new().bg(Color::Black).fg(Color::Gray)),
        status,
    );
    if let Some(overlay) = &app.overlay {
        render_overlay(overlay, frame);
    }
}

fn render_overlay(overlay: &Overlay, frame: &mut Frame<'_>) {
    let area = frame.area();
    let w = area.width.saturating_mul(9) / 10;
    let h = area.height.saturating_mul(8) / 10;
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ACCENT)
        .title(format!(" {} ", overlay.title))
        .title_bottom(Line::styled(" Esc to close · ↑↓ PgUp PgDn to scroll ", DIM));
    frame.render_widget(
        Paragraph::new(overlay.lines.clone())
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((overlay.scroll, 0)),
        rect,
    );
}

fn overlay(title: &str, lines: Vec<Line<'static>>) -> Overlay {
    Overlay {
        title: title.into(),
        lines,
        scroll: 0,
    }
}

/// `/help`.
#[must_use]
pub fn help_overlay() -> Overlay {
    let mut lines: Vec<Line<'static>> = COMMANDS
        .iter()
        .map(|(cmd, what)| {
            Line::from(vec![
                Span::styled(format!("{cmd:<38}"), ACCENT),
                Span::raw(*what),
            ])
        })
        .collect();
    lines.push(Line::default());
    for (key, what) in [
        ("Enter", "send"),
        ("Alt+Enter, Ctrl+J", "new line"),
        ("Esc, Ctrl+C", "interrupt the running task"),
        ("Ctrl+P", "pause at the next safe point"),
        ("PgUp / PgDn, Shift+↑↓", "scroll the transcript"),
        ("Ctrl+D (empty input)", "quit"),
        ("paste or drop files", "attach them"),
        (
            "approvals",
            "o once · s session · a always · p project · d deny (with a reason)",
        ),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("{key:<38}"), ACCENT),
            Span::raw(what),
        ]));
    }
    overlay("Help", lines)
}

/// `/cost`: per turn, task and session (FR-PRV-06).
#[must_use]
pub fn cost_overlay(app: &App) -> Overlay {
    let price = |micros| {
        if app.view.priced {
            money(micros)
        } else {
            "$?".to_owned()
        }
    };
    let mut lines = Vec::new();
    let s = app.ledger.session();
    lines.push(Line::styled(
        "Session",
        Style::new().add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(format!(
        "  {} turns · input {} · output {} · cache read {} · cache write {} · {}",
        s.turns,
        tokens(s.input),
        tokens(s.output),
        tokens(s.cache_read),
        tokens(s.cache_write),
        price(s.cost_micros)
    )));
    lines.push(Line::default());
    lines.push(Line::styled(
        "Tasks",
        Style::new().add_modifier(Modifier::BOLD),
    ));
    for (task, t) in app.ledger.by_task() {
        lines.push(Line::raw(format!(
            "  {task}  {} turns · in {} · out {} · {}",
            t.turns,
            tokens(t.input + t.cache_read + t.cache_write),
            tokens(t.output),
            price(t.cost_micros)
        )));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "Turns",
        Style::new().add_modifier(Modifier::BOLD),
    ));
    for (n, t) in app.ledger.turns().iter().enumerate() {
        lines.push(Line::raw(format!(
            "  {:>3}. in {} (cache {}) · out {} · {}",
            n + 1,
            tokens(t.usage.total_input()),
            tokens(t.usage.cache_read),
            tokens(t.usage.output),
            price(t.cost_micros)
        )));
    }
    if !app.view.priced {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("No price for {} in the config: add [pricing.\"{}\"] with input/output per million tokens.", app.view.model, app.view.model),
            Style::new().fg(Color::Yellow),
        ));
    }
    overlay("Cost", lines)
}

/// `/context`: how full the context is.
#[must_use]
pub fn context_overlay(app: &App) -> Overlay {
    let last = app.ledger.last();
    let used = last.map_or(0, |t| t.usage.total_input() + t.usage.output);
    #[allow(clippy::cast_precision_loss)]
    let pct = if app.view.context_limit == 0 {
        0.0
    } else {
        used as f64 * 100.0 / app.view.context_limit as f64
    };
    let lines = vec![
        Line::raw(format!("Model          {}", app.view.model)),
        Line::raw(format!("Segment        {}", app.segment)),
        Line::raw(format!(
            "Last request   {} input + {} output tokens",
            tokens(last.map_or(0, |t| t.usage.total_input())),
            tokens(last.map_or(0, |t| t.usage.output))
        )),
        Line::raw(format!(
            "Cached         {} of the input",
            tokens(last.map_or(0, |t| t.usage.cache_read))
        )),
        Line::raw(format!(
            "Limit          {} ({pct:.1}% used)",
            tokens(app.view.context_limit)
        )),
        Line::default(),
        Line::styled(
            "Compaction into a new segment arrives in Phase 5; until then long sessions keep growing.",
            DIM,
        ),
    ];
    overlay("Context", lines)
}

/// `/tasks`.
#[must_use]
pub fn tasks_overlay(tasks: &[TaskRecord]) -> Overlay {
    let mut lines: Vec<Line<'static>> = tasks
        .iter()
        .map(|t| {
            Line::raw(format!(
                "{}  {:<15}  {}",
                t.id,
                state_name(t.state),
                t.reason.clone().unwrap_or_default()
            ))
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::raw("No tasks yet."));
    }
    overlay("Tasks", lines)
}

/// `/diff`: what the session changed, file by file.
#[must_use]
pub fn diff_overlay(changes: &[ChangedFile]) -> Overlay {
    let mut lines = Vec::new();
    for change in changes {
        let label = match (&change.before, &change.after) {
            (None, Some(_)) => "created",
            (Some(_), None) => "deleted",
            _ => "changed",
        };
        lines.push(Line::styled(
            format!("{} ({label})", change.path.display()),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        let diff = unified(
            change.before.as_deref().unwrap_or(""),
            change.after.as_deref().unwrap_or(""),
            400,
        );
        if diff.is_empty() {
            lines.push(Line::styled("    (back to how it was)", DIM));
        }
        lines.extend(diff.iter().map(diff_line));
        lines.push(Line::default());
    }
    if lines.is_empty() {
        lines.push(Line::raw("This session has not changed any files."));
    }
    overlay("Changes in this session", lines)
}

/// `/model` without a name: the current and offered models.
#[must_use]
pub fn models_overlay(app: &App, models: &[harness_types::ModelRef]) -> Overlay {
    let mut lines = vec![
        Line::raw(format!(
            "This session: {} (a session keeps its model)",
            app.view.model
        )),
        Line::raw(format!("Next session: {}", app.next.model)),
        Line::default(),
        Line::styled("Pick one with /model provider/model:", DIM),
    ];
    lines.extend(models.iter().map(|m| Line::raw(format!("  {m}"))));
    overlay("Models", lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ToolCard;
    use crate::backend::{Attached, SessionView};
    use harness_ports::Effort;
    use harness_types::{SessionId, TaskId, Usage};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app() -> App {
        let (_tx, rx) = tokio::sync::broadcast::channel(4);
        let attached = Attached {
            view: SessionView {
                id: SessionId::new(),
                persona: "default".into(),
                model: "openai/gpt-6-luna".parse().unwrap(),
                effort: Some(Effort::High),
                profile: "auto".into(),
                context_limit: 250_000,
                priced: true,
                cwd: "/proj".into(),
            },
            events: rx,
            history: Vec::new(),
        };
        App::new(&attached, false)
    }

    fn screen(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(app, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .chunks(usize::from(w))
            .map(|row| {
                row.iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample() -> App {
        let mut app = app();
        let task = TaskId::new();
        app.task = Some((task, TaskState::AwaitingApproval));
        app.items = vec![
            Item::User {
                text: "fix the login test".into(),
                attachments: vec!["spec.md".into()],
            },
            Item::Assistant {
                text: "The check used `<` instead of `<=`.".into(),
                done: true,
            },
            Item::Tool(ToolCard {
                id: "c1".into(),
                name: "edit_file".into(),
                summary: "src/auth.rs".into(),
                live: String::new(),
                ok: None,
                result: String::new(),
                preview: vec![
                    DiffLine::Removed("x < y".into()),
                    DiffLine::Added("x <= y".into()),
                ],
                todo: vec![],
            }),
        ];
        app.prompt = Some(Prompt::Approval {
            id: harness_types::ApprovalId::new(),
            reason: "write src/auth.rs".into(),
            preview: vec![
                DiffLine::Removed("x < y".into()),
                DiffLine::Added("x <= y".into()),
            ],
        });
        app.ledger.record(
            task,
            Usage {
                input: 1_200,
                output: 300,
                cache_read: 40_000,
                cache_write: 0,
            },
            420_000,
        );
        app.segment = 0;
        app
    }

    #[test]
    fn the_main_screen_shows_transcript_approval_input_and_status() {
        insta::assert_snapshot!(screen(&sample(), 80, 20));
    }

    #[test]
    fn the_status_bar_reports_context_cache_cost_and_state() {
        let app = sample();
        assert_eq!(
            status_line(&app).to_string(),
            " 41.5k/250.0k ctx · cache 97% · $0.42 · task waiting for you · Esc interrupt · ^P pause"
        );
        let mut unpriced = app;
        unpriced.view.priced = false;
        assert!(status_line(&unpriced).to_string().contains("$?"));
    }

    #[test]
    fn approval_keys_stay_visible_when_the_reason_wraps() {
        let mut app = sample();
        app.prompt = Some(Prompt::Approval {
            id: harness_types::ApprovalId::new(),
            reason: format!(
                "write /{} (the read-only profile asks before writes)",
                "very-long-directory-name/".repeat(6)
            ),
            preview: (0..20)
                .map(|i| DiffLine::Added(format!("line {i}")))
                .collect(),
        });
        let s = screen(&app, 60, 24);
        assert!(s.contains("[o]nce") && s.contains("[d]eny"), "{s}");
    }

    #[test]
    fn overlays_render_over_the_transcript() {
        let mut app = sample();
        app.prompt = None;
        app.overlay = Some(help_overlay());
        let s = screen(&app, 90, 30);
        assert!(
            s.contains("Help") && s.contains("/undo") && s.contains("Esc to close"),
            "{s}"
        );
    }

    #[test]
    fn diff_and_task_overlays_list_their_content() {
        let d = diff_overlay(&[ChangedFile {
            path: "/p/a.txt".into(),
            before: None,
            after: Some("hi\n".into()),
        }]);
        let text: Vec<String> = d.lines.iter().map(ToString::to_string).collect();
        assert_eq!(text[0], "/p/a.txt (created)");
        assert!(text.iter().any(|l| l.contains("+ hi")));
        assert_eq!(
            diff_overlay(&[]).lines[0].to_string(),
            "This session has not changed any files."
        );
        assert_eq!(tasks_overlay(&[]).lines[0].to_string(), "No tasks yet.");
    }

    #[test]
    fn cost_overlay_explains_missing_prices() {
        let mut app = sample();
        app.view.priced = false;
        let text: Vec<String> = cost_overlay(&app)
            .lines
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            text.iter()
                .any(|l| l.contains("[pricing.\"openai/gpt-6-luna\"]")),
            "{text:?}"
        );
        assert!(text.iter().any(|l| l.contains("1 turns")));
    }

    #[test]
    fn long_transcripts_follow_the_bottom_unless_scrolled_back() {
        let mut app = app();
        app.items = (0..50)
            .map(|i| Item::Notice {
                level: Level::Info,
                text: format!("line {i}"),
            })
            .collect();
        let s = screen(&app, 60, 12);
        assert!(s.contains("line 49") && !s.contains("line 10"), "{s}");
        app.scroll_back = 35;
        let s = screen(&app, 60, 12);
        assert!(s.contains("line 10") && !s.contains("line 49"), "{s}");
    }
}
