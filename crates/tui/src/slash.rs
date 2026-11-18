//! Slash commands typed in the input box (P4-T4).

/// A parsed slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slash {
    /// Show the commands and keys.
    Help,
    /// Show models, or continue the session with another one (ADR-046).
    Model(Option<String>),
    /// Show the effort, or continue the session at another one (ADR-046).
    Effort(Option<String>),
    /// Show the permission modes, or switch to one (ADR-042).
    Mode(Option<String>),
    /// Compact the session now, optionally with a focus.
    Compact(Option<String>),
    /// List the anchors, or attach recalled events to the next message.
    Recall(Option<String>),
    /// Fork the session after an event (the last one by default).
    Fork(Option<String>),
    /// Pause the running task at its next safe point.
    Pause,
    /// Resume the paused task.
    Resume,
    /// List the session's tasks.
    Tasks,
    /// Show the files this session changed.
    Diff,
    /// Restore the files changed by the last turn.
    Undo,
    /// Start a new session.
    Clear,
    /// List this project's sessions and switch to one.
    Sessions,
    /// Token and cost totals.
    Cost,
    /// Context window usage.
    Context,
    /// Leave harness.
    Quit,
}

/// Every command with a one-line description, for `/help`.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "commands and keys"),
    (
        "/model [provider/model]",
        "show models, or continue this session with another one",
    ),
    (
        "/effort [low|medium|high|xhigh|max]",
        "show the effort, or continue this session at another one",
    ),
    (
        "/mode [read-only|auto|plan|full]",
        "show or switch the permission mode (also Shift+Tab)",
    ),
    (
        "/compact [focus]",
        "compact the conversation into a summary now (keeps every detail recallable)",
    ),
    (
        "/recall [aN | words]",
        "list anchors, or attach an anchor or search result to your next message",
    ),
    (
        "/fork [event]",
        "a new session sharing this one's history up to an event (default: now)",
    ),
    ("/pause", "pause the task at its next safe point"),
    ("/resume", "resume the paused task"),
    ("/tasks", "this session's tasks"),
    ("/diff", "files this session changed"),
    ("/undo", "restore the files changed by the last turn"),
    ("/clear", "start a new session (this one stays saved)"),
    (
        "/sessions",
        "this project's sessions; pick one to switch to it",
    ),
    ("/cost", "tokens and cost per turn, task and session"),
    ("/context", "context window usage"),
    ("/quit", "leave harness (also Ctrl-D)"),
];

/// Parses `input` if it is a slash command. `None` means it is ordinary text.
///
/// # Errors
/// Inside the `Some`: a message for an unknown command.
///
/// # Examples
/// ```
/// use harness_tui::slash::{parse, Slash};
/// assert_eq!(parse("/model openai/gpt-5.5"), Some(Ok(Slash::Model(Some("openai/gpt-5.5".into())))));
/// assert_eq!(parse("hello"), None);
/// ```
#[must_use]
pub fn parse(input: &str) -> Option<Result<Slash, String>> {
    let input = input.trim();
    let rest = input.strip_prefix('/')?;
    // "/ path/like/this" or "/usr/bin" typed as text are not commands.
    let (name, arg) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, None), |(n, a)| {
            (n, Some(a.trim().to_owned()).filter(|a| !a.is_empty()))
        });
    if name.contains('/') || name.is_empty() {
        return None;
    }
    Some(match name.to_ascii_lowercase().as_str() {
        "help" | "?" => Ok(Slash::Help),
        "model" => Ok(Slash::Model(arg)),
        "effort" => Ok(Slash::Effort(arg)),
        "mode" => Ok(Slash::Mode(arg)),
        "compact" => Ok(Slash::Compact(arg)),
        "recall" => Ok(Slash::Recall(arg)),
        "fork" => Ok(Slash::Fork(arg)),
        "pause" => Ok(Slash::Pause),
        "resume" => Ok(Slash::Resume),
        "tasks" => Ok(Slash::Tasks),
        "diff" => Ok(Slash::Diff),
        "undo" => Ok(Slash::Undo),
        "clear" | "new" => Ok(Slash::Clear),
        "sessions" | "switch" => Ok(Slash::Sessions),
        "cost" => Ok(Slash::Cost),
        "context" => Ok(Slash::Context),
        "quit" | "exit" => Ok(Slash::Quit),
        other => Err(format!("unknown command /{other}; type /help")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_arguments_parse() {
        assert_eq!(parse("/help"), Some(Ok(Slash::Help)));
        assert_eq!(parse("  /UNDO  "), Some(Ok(Slash::Undo)));
        assert_eq!(
            parse("/effort high"),
            Some(Ok(Slash::Effort(Some("high".into()))))
        );
        assert_eq!(parse("/model"), Some(Ok(Slash::Model(None))));
        assert_eq!(
            parse("/mode plan"),
            Some(Ok(Slash::Mode(Some("plan".into()))))
        );
        assert_eq!(parse("/exit"), Some(Ok(Slash::Quit)));
        assert!(matches!(parse("/frobnicate"), Some(Err(m)) if m.contains("/help")));
    }

    #[test]
    fn paths_and_text_are_not_commands() {
        assert_eq!(parse("explain /usr/bin please"), None);
        assert_eq!(parse("/usr/bin/env is weird"), None);
        assert_eq!(parse("/"), None);
    }

    #[test]
    fn help_lists_every_command() {
        for name in [
            "help", "model", "effort", "mode", "compact", "recall", "fork", "pause", "resume",
            "tasks", "diff", "undo", "clear", "sessions", "cost", "context", "quit",
        ] {
            assert!(
                COMMANDS
                    .iter()
                    .any(|(c, _)| c.starts_with(&format!("/{name}"))),
                "{name}"
            );
        }
    }
}
