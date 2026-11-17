//! Shell command analysis with tree-sitter-bash (Tech spec 3.6, step 2).
//!
//! A command line is split on `&&`, `||`, `;` and `|` into simple commands
//! ([`Part`]s), and each part is judged on its own. Constructs whose effect
//! cannot be read from the text force the call to ask the user: subshells,
//! `$(…)` and backticks, process substitution, `eval`-like builtins, control
//! flow, and redirections to a computed target. Parse errors force ask too.
//!
//! This is a convenience layer, not a wall (guardrail 11): the sandbox is the
//! wall. The analysis only needs to be conservative, never clever.

use tree_sitter::{Node, Parser};

/// Builtins that run text as code, so their arguments cannot be judged.
const EVAL_LIKE: &[&str] = &["eval", "exec", "source", "."];

/// One simple command of a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The command's words after quote removal, starting with any leading
    /// `NAME=value` assignments. `None` marks a word whose value is only known
    /// at run time (it contains `$VAR` or `${…}`).
    pub words: Vec<Option<String>>,
    /// The part's source text, used for `glob:` and `regex:` rules.
    pub text: String,
    /// Files written by redirections (`>`, `>>`, `&>`, …), as written.
    pub writes: Vec<String>,
    /// Files read by redirections (`<`), as written.
    pub reads: Vec<String>,
}

impl Part {
    /// The literal words, in order, stopping at the first run-time word.
    #[must_use]
    pub fn literal_prefix(&self) -> Vec<&str> {
        self.words.iter().map_while(|w| w.as_deref()).collect()
    }

    /// `true` if every word is known before the command runs.
    #[must_use]
    pub fn is_static(&self) -> bool {
        self.words.iter().all(Option::is_some)
    }
}

/// The result of analysing a command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Analysis {
    /// The simple commands, in source order.
    pub parts: Vec<Part>,
    /// Why the command must be shown to the user regardless of rules (empty
    /// if nothing forces it).
    pub force_ask: Vec<String>,
}

/// Splits `command` into parts and notes anything that forces ask.
///
/// # Examples
/// ```
/// use harness_policy::analyze;
/// let a = analyze("cargo test && git push");
/// assert_eq!(a.parts.len(), 2);
/// assert!(a.force_ask.is_empty());
/// assert!(!analyze("echo $(whoami)").force_ask.is_empty());
/// ```
#[must_use]
pub fn analyze(command: &str) -> Analysis {
    let mut analysis = Analysis::default();
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        analysis
            .force_ask
            .push("the shell parser is unavailable".into());
        return analysis;
    }
    let Some(tree) = parser.parse(command, None) else {
        analysis
            .force_ask
            .push("the command could not be parsed".into());
        return analysis;
    };
    let root = tree.root_node();
    if root.has_error() {
        analysis
            .force_ask
            .push("the command could not be parsed".into());
    }
    let mut walker = Walker {
        src: command.as_bytes(),
        analysis: &mut analysis,
    };
    walker.statement_list(root);
    if analysis.parts.is_empty() && analysis.force_ask.is_empty() {
        analysis.force_ask.push("the command is empty".into());
    }
    analysis
}

struct Walker<'a> {
    src: &'a [u8],
    analysis: &'a mut Analysis,
}

impl Walker<'_> {
    fn text(&self, node: Node<'_>) -> String {
        node.utf8_text(self.src).unwrap_or_default().to_owned()
    }

    fn force(&mut self, why: impl Into<String>) {
        let why = why.into();
        if !self.analysis.force_ask.contains(&why) {
            self.analysis.force_ask.push(why);
        }
    }

    /// Walks the named children of a node that only sequences statements.
    fn statement_list(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.statement(child);
        }
    }

    fn statement(&mut self, node: Node<'_>) {
        match node.kind() {
            "comment" => {}
            "list" | "pipeline" | "program" => self.statement_list(node),
            "negated_command" => self.statement_list(node),
            "command" => self.command(node, Vec::new(), Vec::new()),
            "redirected_statement" => self.redirected(node),
            "variable_assignment" => {
                let text = self.text(node);
                let value = node
                    .child_by_field_name("value")
                    .map(|v| self.word(v))
                    .unwrap_or(Some(String::new()));
                self.analysis.parts.push(Part {
                    words: vec![value.map(|_| text.clone())],
                    text,
                    writes: Vec::new(),
                    reads: Vec::new(),
                });
            }
            "subshell" => self.force("it runs a subshell"),
            "function_definition" => self.force("it defines a shell function"),
            "if_statement"
            | "for_statement"
            | "c_style_for_statement"
            | "while_statement"
            | "case_statement"
            | "compound_statement" => {
                self.force("it uses shell control flow (if, for, while, case or { … })");
            }
            "ERROR" => self.force("the command could not be parsed"),
            other => self.force(format!(
                "it uses shell syntax that is not analysed ({other})"
            )),
        }
    }

    fn redirected(&mut self, node: Node<'_>) {
        let mut writes = Vec::new();
        let mut reads = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "file_redirect" => self.file_redirect(child, &mut writes, &mut reads),
                "heredoc_redirect" | "herestring_redirect"
                    if contains_kind(child, &["command_substitution", "process_substitution"]) =>
                {
                    self.force("it runs a command substitution ($(…) or backticks)");
                }
                _ => {}
            }
        }
        match node.child_by_field_name("body") {
            Some(body) if body.kind() == "command" => self.command(body, writes, reads),
            Some(body) => {
                self.statement(body);
                // Redirections around a compound body: record them on their own.
                if !writes.is_empty() || !reads.is_empty() {
                    self.analysis.parts.push(Part {
                        words: Vec::new(),
                        text: self.text(node),
                        writes,
                        reads,
                    });
                }
            }
            None => {
                // A bare redirection such as `> file` truncates the file.
                self.analysis.parts.push(Part {
                    words: Vec::new(),
                    text: self.text(node),
                    writes,
                    reads,
                });
            }
        }
    }

    /// Records the target of `> f`, `>> f`, `&> f` (written) or `< f` (read).
    fn file_redirect(&mut self, node: Node<'_>, writes: &mut Vec<String>, reads: &mut Vec<String>) {
        let op = self.redirect_operator(node);
        let Some(dest) = node.child_by_field_name("destination") else {
            return;
        };
        match self.word(dest) {
            Some(target) if op.starts_with('<') && !op.starts_with("<>") => reads.push(target),
            // `2>&1` and friends duplicate a descriptor; nothing is written.
            Some(target)
                if op.ends_with('&') && target.chars().all(|c| c.is_ascii_digit() || c == '-') => {}
            Some(target) => writes.push(target),
            None => self.force("it redirects to a file named at run time"),
        }
    }

    fn redirect_operator(&self, node: Node<'_>) -> String {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|c| !c.is_named())
            .map(|c| self.text(c))
            .unwrap_or_default()
    }

    fn command(&mut self, node: Node<'_>, mut writes: Vec<String>, mut reads: Vec<String>) {
        let mut words = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "variable_assignment" => {
                    let value = child.child_by_field_name("value").map(|v| self.word(v));
                    let known = value.is_none_or(|v| v.is_some());
                    words.push(known.then(|| self.text(child)));
                }
                "command_name" => {
                    let name = child.named_child(0).and_then(|n| self.word(n));
                    match &name {
                        None => self.force("the command name is computed at run time"),
                        Some(n) if EVAL_LIKE.contains(&n.as_str()) => {
                            self.force(format!("it runs text as code ({n})"));
                        }
                        Some(_) => {}
                    }
                    words.push(name);
                }
                // Redirections written before or between the arguments.
                "file_redirect" => self.file_redirect(child, &mut writes, &mut reads),
                "herestring_redirect" => {
                    if contains_kind(child, &["command_substitution", "process_substitution"]) {
                        self.force("it runs a command substitution ($(…) or backticks)");
                    }
                }
                "comment" => {}
                _ => words.push(self.word(child)),
            }
        }
        self.analysis.parts.push(Part {
            words,
            text: self.text(node),
            writes,
            reads,
        });
    }

    /// The value of a word after quote removal, or `None` if it depends on
    /// run-time expansion. Substitutions force ask.
    fn word(&mut self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            "word" | "number" => Some(unescape(&self.text(node))),
            "raw_string" => {
                let t = self.text(node);
                Some(t.trim_start_matches('\'').trim_end_matches('\'').to_owned())
            }
            "ansi_c_string" => None,
            "string" => {
                let mut out = String::new();
                let mut cursor = node.walk();
                let mut known = true;
                for part in node.named_children(&mut cursor) {
                    match part.kind() {
                        "string_content" => out.push_str(&self.text(part)),
                        _ => {
                            if let Some(v) = self.word(part) {
                                out.push_str(&v);
                            } else {
                                known = false;
                            }
                        }
                    }
                }
                known.then_some(out)
            }
            "concatenation" => {
                let mut out = String::new();
                let mut known = true;
                let mut cursor = node.walk();
                for part in node.named_children(&mut cursor) {
                    match self.word(part) {
                        Some(v) => out.push_str(&v),
                        None => known = false,
                    }
                }
                known.then_some(out)
            }
            "command_substitution" => {
                self.force("it runs a command substitution ($(…) or backticks)");
                None
            }
            "process_substitution" => {
                self.force("it uses process substitution (<(…) or >(…))");
                None
            }
            "arithmetic_expansion" => {
                if contains_kind(node, &["command_substitution"]) {
                    self.force("it runs a command substitution ($(…) or backticks)");
                }
                None
            }
            _ => {
                if contains_kind(node, &["command_substitution", "process_substitution"]) {
                    self.force("it runs a command substitution ($(…) or backticks)");
                }
                None
            }
        }
    }
}

/// `true` if `node` or a descendant has one of `kinds`.
fn contains_kind(node: Node<'_>, kinds: &[&str]) -> bool {
    if kinds.contains(&node.kind()) {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| contains_kind(c, kinds))
}

/// Removes backslash escapes from an unquoted word.
fn unescape(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(a: &Analysis) -> Vec<Vec<Option<String>>> {
        a.parts.iter().map(|p| p.words.clone()).collect()
    }

    fn lit(ws: &[&str]) -> Vec<Option<String>> {
        ws.iter().map(|w| Some((*w).to_owned())).collect()
    }

    #[test]
    fn simple_command_is_one_static_part() {
        let a = analyze("cargo test --workspace");
        assert_eq!(words(&a), vec![lit(&["cargo", "test", "--workspace"])]);
        assert!(a.force_ask.is_empty());
        assert!(a.parts[0].is_static());
    }

    #[test]
    fn lists_and_pipelines_are_split() {
        let a = analyze("cargo fmt && cargo test || echo failed; ls | wc -l");
        assert_eq!(
            words(&a),
            vec![
                lit(&["cargo", "fmt"]),
                lit(&["cargo", "test"]),
                lit(&["echo", "failed"]),
                lit(&["ls"]),
                lit(&["wc", "-l"]),
            ]
        );
        assert!(a.force_ask.is_empty(), "{:?}", a.force_ask);
    }

    #[test]
    fn quotes_and_escapes_are_removed() {
        let a = analyze(r#"git commit -m "fix bug" 'it''s' a\ b"#);
        assert_eq!(
            words(&a),
            vec![lit(&["git", "commit", "-m", "fix bug", "its", "a b"])]
        );
    }

    #[test]
    fn variables_make_words_dynamic_without_forcing_ask() {
        let a = analyze("rm -rf $TARGET/build");
        assert_eq!(
            a.parts[0].words,
            vec![Some("rm".into()), Some("-rf".into()), None]
        );
        assert_eq!(a.parts[0].literal_prefix(), ["rm", "-rf"]);
        assert!(!a.parts[0].is_static());
        assert!(a.force_ask.is_empty());
    }

    #[test]
    fn substitutions_subshells_and_eval_force_ask() {
        for cmd in [
            "echo $(whoami)",
            "echo `whoami`",
            "(cd /tmp && rm -rf x)",
            "diff <(ls a) <(ls b)",
            "eval \"$CMD\"",
            "source ./env.sh",
            ". ./env.sh",
            "if true; then rm -rf /; fi",
            "for f in *; do rm $f; done",
            "f() { rm -rf /; }; f",
            "{ rm -rf /; }",
            "$CMD --flag",
            "echo \"$(id)\"",
        ] {
            assert!(!analyze(cmd).force_ask.is_empty(), "{cmd} should force ask");
        }
    }

    #[test]
    fn parse_errors_force_ask() {
        let a = analyze("echo 'unterminated");
        assert!(a.force_ask.iter().any(|w| w.contains("parsed")), "{a:?}");
        assert!(!analyze("").force_ask.is_empty());
    }

    #[test]
    fn redirections_record_written_and_read_files() {
        let a = analyze("cargo test > out.log 2>&1 < input.txt");
        assert_eq!(words(&a), vec![lit(&["cargo", "test"])]);
        assert_eq!(a.parts[0].writes, ["out.log"]);
        assert_eq!(a.parts[0].reads, ["input.txt"]);
        assert!(a.force_ask.is_empty(), "{:?}", a.force_ask);
        let a = analyze("echo hi >> /etc/hosts");
        assert_eq!(a.parts[0].writes, ["/etc/hosts"]);
    }

    #[test]
    fn computed_redirect_targets_force_ask() {
        assert!(!analyze("echo hi > $OUT").force_ask.is_empty());
    }

    #[test]
    fn assignments_are_kept_as_leading_words() {
        let a = analyze("RUST_LOG=debug cargo run");
        assert_eq!(words(&a), vec![lit(&["RUST_LOG=debug", "cargo", "run"])]);
    }

    #[test]
    fn negation_and_comments_are_transparent() {
        let a = analyze("! grep -q x file # check");
        assert_eq!(words(&a), vec![lit(&["grep", "-q", "x", "file"])]);
        assert!(a.force_ask.is_empty());
    }
}
