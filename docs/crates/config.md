# harness-config

Layered configuration (SDD 2.10, Tech spec 3.17). See also the
[configuration guide](../guides/configuration.md).

## Public API

| Item | Purpose |
|---|---|
| `Config` and section structs | Typed schema with defaults; unknown keys are rejected |
| `Config::validate` | Range checks the type system cannot express |
| `ConfigLoader` | Merges defaults → user → trusted project → persona → env → CLI |
| `LoadedConfig`, `Layer` | Result plus which layers applied or were skipped |
| `TrustStore` | Trusted project list, canonical paths, atomic saves |
| `SecretRef` | `keyring:NAME` or `env:VAR`; plain secrets are rejected |
| `env_layer` | `HARNESS_SECTION__KEY=value` → TOML layer |
| `deep_merge` | Table-wise merge; other values (including arrays) replace |
| `Paths` | `~/.config/harness`, `~/.local/share/harness` and derived files |

## Example

```rust
use harness_config::{ConfigLoader, Paths};

let paths = Paths::discover()?;
let loaded = ConfigLoader::new(paths)
    .project_dir(std::env::current_dir()?)
    .env(std::env::vars())
    .load()?;
println!("persona: {}", loaded.config.default_persona);
```

## Design notes

- Each layer is checked against the schema on its own before merging, so an
  error names its source (`/home/me/.config/harness/config.toml`, `environment`, `cli`).
- An untrusted project's `.harness/config.toml` is never read; it only appears
  as `Layer::ProjectSkipped` so the user can see why it had no effect.
- Errors for bad secret references never echo the input, in case it is a pasted key.
- Secret *resolution* (reading the keyring or environment) lives in the
  provider layer, starting in Phase 1.
