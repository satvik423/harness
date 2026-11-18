//! The [`UserChannel`] port: how a tool asks the user a question (`ask_user`).
//!
//! The runtime implements it by emitting `Event::QuestionAsked` and waiting for
//! `Op::Answer`. Headless sessions answer [`AskError::NoUser`] at once.

use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// A question for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserQuestion {
    /// The question text.
    pub question: String,
    /// Suggested answers; the user may still answer in their own words.
    pub options: Vec<String>,
    /// Whether several options may be chosen.
    pub multi: bool,
}

/// Why a question got no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AskError {
    /// Nobody can answer (a headless run, or approvals are off).
    #[error("no user is available to answer")]
    NoUser,
    /// The task was interrupted while waiting.
    #[error("interrupted while waiting for the user")]
    Cancelled,
}

/// The answer a front-end sends when its input closed before the user answered
/// (a headless run reading from a closed stdin). Tools treat it like
/// [`AskError::NoUser`] where that matters (`propose_plan`).
pub const NO_ANSWER: &str = "(no answer: the input was closed)";

/// Asks the user a question and waits for the answer.
#[async_trait]
pub trait UserChannel: Send + Sync {
    /// Asks `question` and returns the user's answer text.
    ///
    /// # Errors
    /// [`AskError::NoUser`] in headless sessions; [`AskError::Cancelled`] if
    /// `cancel` fires first.
    async fn ask(
        &self,
        question: UserQuestion,
        cancel: &CancellationToken,
    ) -> Result<String, AskError>;
}

/// A [`UserChannel`] with nobody on the other end (tests, headless runs).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoUser;

#[async_trait]
impl UserChannel for NoUser {
    async fn ask(&self, _: UserQuestion, _: &CancellationToken) -> Result<String, AskError> {
        Err(AskError::NoUser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_user_never_answers() {
        let q = UserQuestion {
            question: "?".into(),
            options: vec![],
            multi: false,
        };
        assert_eq!(
            NoUser.ask(q, &CancellationToken::new()).await,
            Err(AskError::NoUser)
        );
    }
}
