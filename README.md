# Nash AI Engine

A provider-agnostic AI engine and coding agent: a Rust core, a scriptable CLI, and a
Tauri v2 + React desktop app.

One engine speaks to many providers through capability-based adapters, keeps credentials
in the OS keychain, persists conversations in SQLite, and exposes an approval-gated coding
agent — all behind a UI-independent core.

## Highlights

- **Many protocols, one engine** — OpenAI Chat Completions, OpenAI Responses, Anthropic
  Messages, Google Gemini, Ollama, an OpenAI-compatible template endpoint, and a generic
  non-LLM REST client.
- **Capability-based adapters** — providers advertise what they support (tools,
  streaming, vision, …); the engine adapts instead of special-casing vendors.
- **Secrets never touch config** — credentials live in the OS keyring and are referenced
  from configuration by name (`secret_ref`). Nothing sensitive is written to JSON.
- **Streaming with cancellation** — SSE and NDJSON are parsed incrementally and can be
  cancelled mid-flight.
- **Persistent history** — sessions, messages, and usage are stored in SQLite with
  versioned migrations.
- **Coding agent** — file, search, and shell tools with risk levels and an approval gate
  (auto / ask / deny).
- **Diagnostics** — per-provider connectivity and generation checks surfaced in both the
  CLI and the desktop UI.

## Workspace layout

| Crate | Path | Responsibility |
| --- | --- | --- |
| `nash-core` | `crates/nash-core` | Engine, adapters, HTTP, streaming, routing, security, config. UI-independent. |
| `nash-store` | `crates/nash-store` | SQLite persistence (sessions/usage) and OS-keyring secret storage. |
| `nash-agent` | `crates/nash-agent` | The coding agent: tools, approval policy, and the run loop. |
| `nash-cli` | `crates/nash-cli` | The `nash` command line interface. |
| `nash-desktop` | `desktop/src-tauri` | Tauri v2 desktop shell over `nash-core`. |
| — | `desktop/src` | React + TypeScript UI. |

## Requirements

- Rust **1.80+** and Cargo.
- Node **20+** and npm (desktop app only).
- An OS keyring backend: Windows Credential Manager, macOS Keychain, or Linux
  Secret Service. The desktop bundle is produced for Windows.

## Build and test

```bash
cargo build --workspace

# Library, CLI and agent tests (the desktop crate needs the frontend built first)
cargo test --workspace --exclude nash-desktop
cargo clippy --workspace --exclude nash-desktop --all-targets -- -D warnings
```

## CLI quick start

```bash
# Write a starter config (seeds Ollama, OpenAI, Anthropic, Gemini, OpenCode Zen, OpenRouter)
cargo run -p nash-cli -- config init

# Store a credential in the OS keyring (prompted, so it stays out of shell history)
cargo run -p nash-cli -- secret set openai_api_key

# Inspect providers and run live diagnostics
cargo run -p nash-cli -- providers
cargo run -p nash-cli -- test all

# Chat (streams by default)
cargo run -p nash-cli -- chat "Explain Rust's borrow checker" --provider openai

# Drive the coding agent in the current directory
cargo run -p nash-cli -- agent "Add a unit test for the parser" --approve ask
```

## Desktop app

```bash
cd desktop
npm install
npm run tauri dev     # development
npm run tauri build   # production bundle
```

The UI has pages for Providers, Chat, Agent, Secrets, Sessions, Diagnostics, and a raw
Settings editor for the configuration JSON.

## Providers, keys, and free models

`nash-cli config init` seeds these providers, each referencing a keyring secret by name —
no credentials are embedded:

| Provider id | Protocol | Secret reference |
| --- | --- | --- |
| `ollama` | `ollama` | *(none — local)* |
| `openai` | `openai_chat` | `openai_api_key` |
| `anthropic` | `anthropic_messages` | `anthropic_api_key` |
| `gemini` | `google_gemini` | `google_api_key` |
| `opencode` | `openai_chat` | `opencode_api_key` |
| `opencode-muse` | `openai_responses` | `opencode_api_key` |
| `openrouter` | `openai_chat` | `openrouter_api_key` |

Store a key with the matching name (`nash secret set <name>`, or the **Providers** page's
key field in the desktop app), then run `nash test <provider>`. Each provider seeds a list
of currently-free models (OpenCode Zen and OpenRouter) so a fresh install is usable at no
cost; use the in-app **Models** button or `nash models <provider>` to refresh the list from
the provider's live `/models` endpoint.

> **Note:** keyring support requires the platform backend. `nash-store` enables
> `windows-native`, `apple-native`, and `linux-native-sync-persistent` accordingly.

## Configuration

Configuration is a single JSON document. It references secrets by name and never contains
values:

```json
{
  "schema_version": 1,
  "default_provider": "openai",
  "providers": [
    {
      "id": "openai",
      "protocol": "openai_chat",
      "base_url": "https://api.openai.com/v1",
      "auth": "bearer",
      "secret_ref": "openai_api_key",
      "model": "gpt-4o-mini"
    }
  ]
}
```

`nash config show` prints the effective configuration; `nash config paths` prints where the
config and database live.

## License

MIT — see [LICENSE](LICENSE).
