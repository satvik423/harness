# Configuration guide

## Where settings come from

Later layers override earlier ones:

1. Built-in defaults
2. `~/.config/harness/config.toml` (your settings)
3. `<project>/.harness/config.toml`, **only after you trust the project**
4. The active persona (Phase 6)
5. Environment variables `HARNESS_…`
6. Command-line flags

Tables merge key by key. Lists are replaced, so a later layer can shorten a list.
Unknown keys are errors, so typos are caught instead of silently ignored.

## Trusting a project

A cloned repository could ship settings, rules or hooks you did not write, so a
project's `.harness/` folder is ignored until you trust it. The trust list lives
in `~/.local/share/harness/trust.toml`. The `harness trust` command arrives in
Phase 6.

## Environment variables

`HARNESS_SECTION__KEY=value` sets `section.key`. Double underscores separate
sections, names are case-insensitive, and values are parsed as TOML when possible:

```sh
HARNESS_POLICY__PROFILE=read-only
HARNESS_CONTEXT__COMPACT_AT_TOKENS=150000
HARNESS_SANDBOX__NETWORK=true
HARNESS_DEFAULT_PERSONA=coder
```

`HARNESS_LOG` is reserved for the log filter (for example `harness=debug,info`).

## Default model

```toml
default_model = "openai/<model-id>"   # or "anthropic/claude-opus-5-5" (the default)
```

`harness run` uses this unless you pass `--model`. Supported providers: `anthropic`, `openai`.

## Secrets

Config files never contain API keys. Use a reference instead:

```toml
[providers.anthropic]
api_key = "keyring:anthropic"        # OS keyring, service "harness"
# api_key = "env:ANTHROPIC_API_KEY"  # or an environment variable

[providers.openai]
api_key = "keyring:openai"           # falls back to OPENAI_API_KEY
```

A plain key in a config file is rejected with an error that does not repeat the key.

## All keys

See Building Harness, Tech spec 3.17, for every key and its default. The
defaults are defined in `crates/config/src/schema.rs`.

## Interface and prices

```toml
[ui]
show_thinking = false   # show reasoning summaries
mouse = true            # the wheel scrolls the TUI; Shift+drag selects text. false frees the mouse

[pricing."openai/gpt-6-luna"]   # US dollars per million tokens; overrides the built-in table
input = 0.10
output = 0.50
cache_read = 0.01
cache_write = 0.125
```

The built-in prices (Claude 5.5 and OpenAI gpt-6 models) are fixed in harness and change
only with an update; harness never fetches prices.

## Permissions

`[policy] profile` (`read-only`, `auto`, `plan`, `ci` or `full`) picks the mode sessions
start in; `default_deny_paths` lists files no tool may touch. Rules go in separate
`rules.toml` files. You can switch modes while working (Shift+Tab, `/mode`, `--mode`).
See the [permissions guide](permissions.md).

## Long sessions

`[context]` decides when a session compacts into a new segment (see
[long sessions](context.md)):

```toml
[context]
compact_at_ratio = 0.7        # share of the model's context window
compact_at_tokens = 250000    # compact at this many input tokens at the latest
compaction_model = "same"     # or a provider/model on the same provider, e.g. a cheaper one
output_cap_tokens = 2000      # tool output the model sees before "recall" is needed
```

A `compaction_model` on another provider is ignored (with a log line): compaction runs
on the session's provider.
