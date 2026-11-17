//! The output capper (P2-T3, FR-TOOL-04): decides, once, what part of a tool's
//! output the model sees.
//!
//! The capped form is built when the output is first recorded and never changes
//! afterwards (guardrail 03: trimming later would edit history). The full output
//! stays in the `ToolFinished` event; the footer tells the model how to get it back.

use harness_types::{ContentBlock, Seq};

/// Share of the cap kept from the start of the output; the rest comes from the end.
const HEAD_SHARE: f64 = 0.6;

/// Rough characters per token, used to estimate sizes without a tokenizer.
pub const CHARS_PER_TOKEN: u64 = 4;

/// Estimated token count of `text` (about [`CHARS_PER_TOKEN`] characters per token).
///
/// # Examples
/// ```
/// use harness_context::estimate_tokens;
/// assert_eq!(estimate_tokens(""), 0);
/// assert_eq!(estimate_tokens("abcd"), 1);
/// assert_eq!(estimate_tokens("abcde"), 2);
/// ```
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(CHARS_PER_TOKEN)
}

/// Returns the content to send to the model for a tool result.
///
/// If the text blocks together are estimated at no more than `cap_tokens`, the
/// content is returned unchanged. Otherwise the text is replaced by one block
/// holding the head (60% of the cap), the tail (40%) and a footer such as
/// `[output trimmed: 18,240 tokens total · full: recall {"seq_from":812,"seq_to":812}]`,
/// where `seq` is the sequence number of the event that holds the full output.
/// Non-text blocks (images, documents) are kept in front of it.
///
/// # Examples
/// ```
/// use harness_context::cap_output;
/// use harness_types::{ContentBlock, Seq};
///
/// let small = vec![ContentBlock::text("ok")];
/// assert_eq!(cap_output(&small, 2_000, Seq(7)), small);
///
/// let big = vec![ContentBlock::text("x".repeat(100_000))];
/// let ContentBlock::Text { text } = &cap_output(&big, 100, Seq(7))[0] else { panic!() };
/// assert!(text.ends_with(r#"full: recall {"seq_from":7,"seq_to":7}]"#));
/// ```
#[must_use]
pub fn cap_output(content: &[ContentBlock], cap_tokens: u64, seq: Seq) -> Vec<ContentBlock> {
    let text = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let total = estimate_tokens(&text);
    if total <= cap_tokens {
        return content.to_vec();
    }
    let budget = cap_tokens.saturating_mul(CHARS_PER_TOKEN);
    // The share is a fixed 60/40 split of a small integer; rounding is harmless.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let head_chars = (budget as f64 * HEAD_SHARE) as usize;
    let tail_chars = usize::try_from(budget).unwrap_or(usize::MAX) - head_chars;
    let head = head_of(&text, head_chars);
    let tail = tail_of(&text, tail_chars);
    // A cut inside one long line omits no whole line; report characters then.
    let lines = text
        .lines()
        .count()
        .saturating_sub(head.lines().count() + tail.lines().count());
    let omitted = if lines > 0 {
        format!("{} lines", group_digits(lines as u64))
    } else {
        let chars = text.chars().count() - head.chars().count() - tail.chars().count();
        format!("{} characters", group_digits(chars as u64))
    };
    let trimmed = format!(
        "{head}\n[… {omitted} omitted …]\n{tail}\n[output trimmed: {} tokens total · full: recall {{\"seq_from\":{s},\"seq_to\":{s}}}]",
        group_digits(total),
        s = seq.0,
    );
    let mut out: Vec<ContentBlock> = content
        .iter()
        .filter(|b| !matches!(b, ContentBlock::Text { .. }))
        .cloned()
        .collect();
    out.push(ContentBlock::text(trimmed));
    out
}

/// The first `max_chars` characters, cut back to the last line break when one is
/// in the second half of that range (so lines are not split needlessly).
fn head_of(text: &str, max_chars: usize) -> &str {
    let end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(i, _)| i);
    let cut = &text[..end];
    match cut.rfind('\n') {
        Some(nl) if nl >= end / 2 => &cut[..nl],
        _ => cut,
    }
}

/// The last `max_chars` characters, starting after a line break when one is in
/// the first half of that range.
fn tail_of(text: &str, max_chars: usize) -> &str {
    let count = text.chars().count();
    let start = text
        .char_indices()
        .nth(count.saturating_sub(max_chars))
        .map_or(text.len(), |(i, _)| i);
    let cut = &text[start..];
    match cut.find('\n') {
        Some(nl) if nl <= cut.len() / 2 => &cut[nl + 1..],
        _ => cut,
    }
}

/// `18240` → `"18,240"`.
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(blocks: &[ContentBlock]) -> &str {
        match blocks.last() {
            Some(ContentBlock::Text { text }) => text,
            other => panic!("expected text, got {other:?}"),
        }
    }

    fn numbered(lines: usize) -> String {
        (1..=lines)
            .map(|i| format!("line {i:05}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn output_within_the_cap_is_unchanged() {
        let content = vec![ContentBlock::text("a".repeat(8_000))];
        assert_eq!(cap_output(&content, 2_000, Seq(1)), content);
    }

    #[test]
    fn large_output_keeps_head_and_tail_and_adds_a_recall_footer() {
        let content = vec![ContentBlock::text(numbered(10_000))];
        let capped = cap_output(&content, 500, Seq(812));
        let t = text(&capped);
        assert!(t.starts_with("line 00001\n"), "{}", &t[..40]);
        assert!(t.contains("line 10000\n[output trimmed:"), "{t}");
        assert!(
            t.ends_with(
                "[output trimmed: 27,500 tokens total · full: recall {\"seq_from\":812,\"seq_to\":812}]"
            ),
            "{}",
            &t[t.len() - 120..]
        );
        assert!(t.contains(" lines omitted …]"));
    }

    #[test]
    fn capped_text_stays_close_to_the_cap() {
        let content = vec![ContentBlock::text(numbered(10_000))];
        let capped = cap_output(&content, 500, Seq(1));
        let tokens = estimate_tokens(text(&capped));
        // Cap plus the two marker lines.
        assert!(tokens <= 540, "{tokens}");
        assert!(tokens >= 450, "{tokens}");
    }

    #[test]
    fn head_gets_about_sixty_percent() {
        let content = vec![ContentBlock::text(numbered(10_000))];
        let t = cap_output(&content, 1_000, Seq(1));
        let t = text(&t);
        let (head, rest) = t.split_once("\n[…").unwrap();
        let (_, tail) = rest.split_once("…]\n").unwrap();
        let tail = tail.split("\n[output trimmed").next().unwrap();
        let ratio = head.len() as f64 / (head.len() + tail.len()) as f64;
        assert!((0.55..0.65).contains(&ratio), "{ratio}");
    }

    #[test]
    fn lines_are_not_split_when_a_break_is_near() {
        let content = vec![ContentBlock::text(numbered(10_000))];
        let t = cap_output(&content, 300, Seq(1));
        for line in text(&t).lines() {
            assert!(
                line.starts_with("line ") && line.len() == 10 || line.starts_with('['),
                "split line {line:?}"
            );
        }
    }

    #[test]
    fn a_single_huge_line_is_cut_on_char_boundaries() {
        let content = vec![ContentBlock::text("é".repeat(50_000))];
        let t = cap_output(&content, 100, Seq(3));
        let t = text(&t);
        assert!(t.starts_with("éé"));
        assert!(t.contains("characters omitted"), "{t}");
    }

    #[test]
    fn non_text_blocks_are_kept_in_front() {
        let image = ContentBlock::Image {
            blob: harness_types::BlobRef::from_bytes([1; 32]),
            media_type: "image/png".into(),
        };
        let content = vec![image.clone(), ContentBlock::text("y".repeat(40_000))];
        let capped = cap_output(&content, 100, Seq(1));
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0], image);
    }

    #[test]
    fn digits_are_grouped_in_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(18_240), "18,240");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }
}
