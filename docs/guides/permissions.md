# Permissions: modes, approvals and rules

harness checks every tool call before it runs. A call is **allowed**, **asked about**,
or **denied** with a reason the model can read. This guide covers how that is decided
and how to change it. (Design: Tech spec 3.6, ADR-029.)

## Modes

Every session runs in a **mode** that decides what runs without asking. You can switch
it at any time (ADR-042):

- in the TUI: **Shift+Tab** cycles read-only → auto → plan, or type `/mode plan`
  (`/mode` alone lists them);
- at startup: `harness --mode plan`, `harness run --mode read-only "…"`;
- by default: `policy.profile` in your config.

| Mode | Runs without asking | Asks before | Refuses |
|---|---|---|---|
| `read-only` | reads | edits, commands, network | protected paths |
| `auto` (default) | reads, edits inside the project | commands (until the Phase 13 sandbox), edits outside the project, network | protected paths |
| `plan` | reads | commands, network | **every edit**, until you approve a plan |
| `full` | everything | nothing | protected paths only; needs `--dangerously-full-access` |

The header shows the current mode. `plan` is drawn in magenta and `full` in red. A
resumed session (`harness -c`) keeps its mode; `--mode` switches it.

### Plan mode

In plan mode the agent explores (reading files, and running commands you approve), then
calls `propose_plan`. The plan appears in the transcript, and you choose:

1. **Yes, and auto-accept edits**: the plan is approved and the mode becomes `auto`.
2. **Yes, but ask before each edit**: the plan is approved and the mode becomes `read-only`.
3. **No, keep planning**: plan mode stays on. Or type what to change; the agent gets your
   words and proposes again.

Edits in plan mode are refused even if a rule or an earlier "always" allows them. In
`harness run --mode plan`, nobody can approve, so the agent ends with the plan.

## Profiles

The config's `policy.profile` picks the starting mode, plus how approvals work:

```toml
[policy]
profile = "auto"   # read-only | auto | plan | ci | full
```

| Profile | Starts in | Never asks? |
|---|---|---|
| `read-only` | `read-only` | no |
| `auto` (default) | `auto` | no |
| `plan` | `plan` | no |
| `ci` | `auto` | yes: what would need approval is denied, with a reason |
| `full` | `full` | yes; needs `--dangerously-full-access` |

`--approval never` makes any profile deny instead of asking (scripts and CI);
`harness run` then exits with code 4 if something was denied that way.

## Answering an approval prompt

```
▸ shell: cargo test
  Run the unit tests
Allow run `cargo test` (commands run without a sandbox)? [y]es once / [s]ession / [a]lways / [p]roject always / [n]o (n <why>):
```

The first line under the call is the agent's own one-line description of what the
command does and why (ADR-043). It helps you decide, but it is the agent's claim, so the
exact command is always shown under it. Judge the command. Rules and "always" answers
match the command, never the description.

| Answer | Effect |
|---|---|
| `y` | Run it this once |
| `s` | Run it, and allow exactly this command again for the rest of the session |
| `a` | Run it, and save a rule in `~/.config/harness/rules.toml` allowing exactly this command |
| `p` | Same, saved in the project's `.harness/rules.toml` (only if you trusted the project) |
| `n`, or Enter | Deny it |
| `n <why>` or any other text | Deny it, and tell the model why (for example `n run the unit tests only`) |

Remembered commands match **exactly**: allowing `rm -rf build` does not allow
`rm -rf build ~`. Commands with `$(…)`, backticks or `$VARIABLES` can only be allowed once.

## Rules files

Rules live in `~/.config/harness/rules.toml` (yours) and `<project>/.harness/rules.toml`
(the project's; used only when you trust the project, see the configuration guide).

```toml
[[rule]]
tool = "shell"
match = "prefix:cargo test"      # allow cargo test, with any extra arguments
decision = "allow"

[[rule]]
tool = "shell"
match = "prefix:git push"
decision = "ask"                 # always ask, even after "session" or "always"
reason = "pushing is visible to others"

[[rule]]
tool = "edit_file|write_file|apply_patch|read_file"
path = "glob:**/secrets/**"
decision = "deny"
reason = "Secrets are off limits"

[[rule]]
tool = "mcp__github__*"
match = "glob:get_*"
decision = "allow"
```

| Field | Meaning |
|---|---|
| `tool` | Tool names, `|` between alternatives, `*` as a wildcard |
| `match` | For `shell`: each simple command. For MCP tools: the tool's own name. `prefix:` compares whole words (`prefix:cargo test` matches `cargo test --all`, not `cargo testing`); `glob:` and `regex:` compare the text |
| `path` | A path the call touches: `glob:` (`**` spans folders, `~/` is your home), `prefix:` or `regex:`. Relative patterns match paths inside the project |
| `decision` | `allow`, `ask` or `deny` |
| `reason` | Shown to you and to the model |

How rules combine:

- All rules from all places are checked; **deny beats ask beats allow**.
- A compound command (`a && b`, `a | b`, `a; b`) is judged part by part; every part
  must be allowed for the whole to run without asking.
- `$(…)`, backticks, subshells, `eval`, `source`, `if`/`for`/`while`, and redirects
  outside the project always ask.
- An `allow` rule with a `path` must cover every file the call touches.
- An invalid rules file stops harness at start with the file name and rule number.

## Protected paths

These are denied to every tool, including shell arguments, whatever the rules say:

```toml
[policy]
default_deny_paths = ["**/.env*", "**/*.pem", "~/.ssh/**", "**/.harness/**"]
```

Command parsing is a convenience, not a wall: a determined command can always be
written another way. The sandbox (Phase 13) is the wall.
