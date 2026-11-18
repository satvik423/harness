//! Telling the user when the terminal is in the background (P4-T3): a bell and
//! an OSC 9 desktop notification, which most terminals show (and the rest ignore).

/// The bytes that ring the bell and raise a notification with `message`.
///
/// # Examples
/// ```
/// assert_eq!(harness_tui::notify::bytes("done"), "\x1b]9;harness: done\x07\x07");
/// ```
#[must_use]
pub fn bytes(message: &str) -> String {
    // Control characters would end the escape sequence early.
    let clean: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    format!("\x1b]9;harness: {clean}\x07\x07")
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_characters_cannot_break_out_of_the_sequence() {
        let b = super::bytes("a\x07b\x1b]c\n");
        assert_eq!(b, "\x1b]9;harness: ab]c\x07\x07");
    }
}
