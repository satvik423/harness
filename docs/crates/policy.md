# harness-policy

The policy engine (P2-T5, Tech spec 3.6). An adapter crate implementing the
`harness_ports::Policy` port; the CLI builds it from the config.

## Public API

| Item | Purpose |
|---|---|
| `PolicyEngine::new(PolicySettings)` | Loads built-in deny paths, the user's and the trusted project's `rules.toml`; implements `Policy` |
| `PolicySettings` | Starting profile, effective approval policy, sandboxed flag, default deny paths, home, rules file paths, `full_access` |
| `Profile` | `ReadOnly`, `Auto` (default), `Plan`, `Ci`, `Full`; `default_approval()`, `mode()` (the starting mode) |
| `PLAN_MODE_REFUSAL` | Why plan mode refuses an edit (the model reads it) |
| `ApprovalPolicy` | `OnRequest` or `Never` |
| `analyze(command)` | Splits a shell command into `Part`s (words after quote removal, redirections) and lists what forces ask |
| `RulesFile`, `RawRule`, `Decision`, `Origin`, `RuleError` | The rules file format and its errors |

## Evaluation

1. A shell command becomes one unit per simple command (split on `&&`, `||`, `;`,
   `|`); any other call is one unit.
2. The call is judged in its `mode` (ADR-042). `full` counts only with
   `full_access`, else it is judged as `auto`. In `plan` mode a write is denied at
   once, before any rule, so no allow rule or grant overrides it.
3. Each unit is matched against every rule in every layer: built-in deny paths,
   user rules, project rules (trusted projects only), session approvals. The
   strongest match wins (deny > ask > allow); with no match, the mode's default applies:

   | Risk | Default |
   |---|---|
   | ReadOnly | allow |
   | Write inside the workspace | allow (`read-only` mode: ask; `plan` mode: denied in step 2) |
   | Write outside the workspace | ask |
   | Exec | allow when sandboxed and on-request; otherwise ask |
   | Network, External | ask |
   | anything, `full` mode | allow |

4. Subshells, `$(…)`, backticks, process substitution, `eval`/`exec`/`source`,
   control flow, computed redirect targets, writes outside the workspace and parse
   errors force at least ask.
5. Under approval policy `never`, ask becomes deny with a reason for the model, and
   `approval_required: true` so `harness run` exits with code 4.

## Example

```rust
use harness_policy::{PolicyEngine, PolicySettings, Profile};
use harness_ports::{Policy, PolicyCall, Verdict};
use harness_types::{Mode, Risk};

let engine = PolicyEngine::new(PolicySettings {
    profile: Profile::Auto,
    approval: Profile::Auto.default_approval(),
    sandboxed: false,
    default_deny_paths: vec!["**/.env*".into()],
    home: None,
    user_rules: None,
    project_rules: None,
    full_access: false,
})?;
let call = PolicyCall {
    tool: "read_file".into(),
    input: serde_json::json!({"path": ".env"}),
    risk: Risk::ReadOnly { paths: vec!["/proj/.env".into()] },
    cwd: "/proj".into(),
    writable_roots: vec!["/proj".into()],
    mode: Mode::Auto,
};
assert!(matches!(engine.evaluate(&call), Verdict::Deny { .. }));
```

## Design notes (ADR-029)

- Remembered shell approvals match the exact command (`regex:^…$`), so approving
  `rm -rf build` never allows `rm -rf build ~`. Commands with substitutions or
  `$VARS` cannot be remembered.
- An `allow` rule with a `path` must cover every path of the call; deny and ask
  rules need only one.
- Grants never override `ask` rules.
- Built-in deny paths apply to all tools; for `shell` they are checked against
  argument words and redirect targets. This is a convenience, not the wall: the
  sandbox (Phase 13) is the wall (guardrail 11).
- Rule files are loaded once at start; "always" appends to the file and updates
  the loaded rules.
