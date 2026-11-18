//! The TUI's event loop: terminal input, session events and a redraw tick.

use std::io::{Stdout, Write};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, Event,
    EventStream,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use harness_protocol::{Envelope, InputItem, Op};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::broadcast;

use crate::app::{Action, App, Effect, Level};
use crate::backend::{Attached, Backend};
use crate::slash::Slash;
use crate::{notify, view};

/// How the TUI starts.
#[derive(Debug, Clone, Default)]
pub struct TuiOptions {
    /// A prompt to send right away (`harness "…"`).
    pub first_prompt: Option<String>,
    /// Show reasoning summaries (`ui.show_thinking`).
    pub show_thinking: bool,
}

/// Puts the terminal back as it was, also when the TUI panics.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableFocusChange,
        LeaveAlternateScreen
    );
}

/// Runs the TUI until the user quits.
///
/// # Errors
/// Terminal I/O errors (for example when stdout is not a terminal).
pub async fn run(
    backend: Arc<dyn Backend>,
    attached: Attached,
    options: TuiOptions,
) -> std::io::Result<()> {
    enable_raw_mode()?;
    let restore = Restore;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let result = event_loop(&mut terminal, backend, attached, options).await;
    drop(restore);
    let _ = std::panic::take_hook();
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    backend: Arc<dyn Backend>,
    attached: Attached,
    options: TuiOptions,
) -> std::io::Result<()> {
    let mut app = App::new(&attached, options.show_thinking);
    let mut events = attached.events;
    let mut events_open = true;
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    let mut pending: Vec<Action> = Vec::new();
    if let Some(prompt) = options.first_prompt {
        pending.push(Action::Send(Op::Submit {
            session: app.view.id,
            input: vec![InputItem::Text { text: prompt }],
        }));
    }
    loop {
        for action in std::mem::take(&mut pending) {
            perform(
                &mut app,
                backend.as_ref(),
                &mut events,
                &mut events_open,
                action,
            )
            .await;
        }
        if app.quit {
            return Ok(());
        }
        terminal.draw(|frame| view::render(&app, frame))?;
        tokio::select! {
            event = input.next() => match event {
                Some(Ok(Event::Key(key))) => pending = app.key(key),
                Some(Ok(Event::Paste(text))) => app.paste(&text),
                Some(Ok(Event::FocusGained)) => app.focused = true,
                Some(Ok(Event::FocusLost)) => app.focused = false,
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e),
                None => return Ok(()),
            },
            received = events.recv(), if events_open => {
                let mut batch = vec![received];
                // Drain whatever else is waiting, then draw once.
                while let Ok(more) = events.try_recv() {
                    batch.push(Ok(more));
                }
                for received in batch {
                    handle_event(&mut app, received, &mut events_open)?;
                }
            }
            _ = tick.tick() => app.tick = app.tick.wrapping_add(1),
        }
    }
}

fn handle_event(
    app: &mut App,
    received: Result<Envelope, broadcast::error::RecvError>,
    events_open: &mut bool,
) -> std::io::Result<()> {
    match received {
        Ok(envelope) => {
            for effect in app.apply(&envelope) {
                let Effect::Notify(message) = effect;
                if !app.focused {
                    let mut out = std::io::stdout();
                    out.write_all(notify::bytes(&message).as_bytes())?;
                    out.flush()?;
                }
            }
        }
        Err(broadcast::error::RecvError::Lagged(n)) => app.items.push(crate::app::Item::Notice {
            level: Level::Warning,
            text: format!("the display fell behind; {n} events were skipped"),
        }),
        Err(broadcast::error::RecvError::Closed) => {
            *events_open = false;
            app.items.push(crate::app::Item::Notice {
                level: Level::Error,
                text: "the session stopped".into(),
            });
        }
    }
    Ok(())
}

/// Carries out one action, reporting failures in the transcript.
async fn perform(
    app: &mut App,
    backend: &dyn Backend,
    events: &mut broadcast::Receiver<Envelope>,
    events_open: &mut bool,
    action: Action,
) {
    let failed = |app: &mut App, e: String| {
        app.items.push(crate::app::Item::Notice {
            level: Level::Error,
            text: e,
        });
    };
    match action {
        Action::Send(op) => {
            if let Err(e) = backend.send(op).await {
                failed(app, e);
            }
        }
        Action::Quit => app.quit = true,
        Action::NewSession { model, effort } => {
            app.items.push(crate::app::Item::Notice {
                level: Level::Info,
                text: format!("starting a new session with {model}…"),
            });
            match backend.new_session(model, effort).await {
                Ok(attached) => {
                    app.attach(&attached);
                    *events = attached.events;
                    *events_open = true;
                }
                Err(e) => failed(app, format!("could not start a new session: {e}")),
            }
        }
        Action::Command(Slash::Tasks) => match backend.tasks().await {
            Ok(tasks) => app.overlay = Some(view::tasks_overlay(&tasks)),
            Err(e) => failed(app, e),
        },
        Action::Command(Slash::Diff) => match backend.changes().await {
            Ok(changes) => app.overlay = Some(view::diff_overlay(&changes)),
            Err(e) => failed(app, e),
        },
        Action::Command(Slash::Model(None)) => {
            app.overlay = Some(view::models_overlay(app, &backend.models()));
        }
        Action::Command(Slash::Effort(None)) => {
            let current = app
                .view
                .effort
                .map_or_else(|| "default".to_owned(), |e| format!("{e:?}").to_lowercase());
            let next = app
                .next
                .effort
                .map_or_else(|| "default".to_owned(), |e| format!("{e:?}").to_lowercase());
            app.items.push(crate::app::Item::Notice {
                level: Level::Info,
                text: format!("effort: {current} (next session: {next}); pick with /effort low|medium|high|xhigh|max"),
            });
        }
        Action::Command(other) => {
            let more = app.command(other);
            for action in more {
                Box::pin(perform(app, backend, events, events_open, action)).await;
            }
        }
    }
}
