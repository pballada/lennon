<div align="center">
  <img src="assets/lennon.svg" width="220" alt="Lennon, a wiry black podenco with one upright ear, one folded ear, silver eyebrows and a gentle, melancholy expression" />
  <h1>lennon</h1>
  <p><strong>A loyal companion for unfinished work.</strong></p>
  <p>Claude Code → OpenAI Codex → Gemini CLI.<br />One workspace. One continuing session.</p>
</div>

Lennon is **Claude Code, in Rust, that keeps working when you hit a limit.**
It looks and behaves like Claude Code: the same prompt box, `⏺`/`⎿` transcript,
spinner, slash commands, permission modes, shortcuts, and command-line options.
The only visual difference is the icon: Lennon instead of Claude's mascot.

```
 ▄▄    ▄█   Lennon v0.3.0
 ▀▄▀▀█▀▀█   claude → codex → gemini
  ▀▀▀▀▀▀▀   ~/projects/app
```

Under the hood Lennon drives the coding agents you already use. When Claude Code
reports a usage limit, Lennon saves the observable conversation and tool
activity and continues the same task with the next configured provider (Codex,
then Gemini by default), in the same session and workspace. It uses each CLI's
existing login, tools, and native sessions. **No Node.js, JavaScript runtime, or
API key is required by Lennon.** Upstream CLIs retain their own requirements.

**Status: working alpha, macOS and Linux.** Lennon is an independent
implementation, not a Claude Code fork. Hidden reasoning, provider-side state,
plugins, and interactive permission dialogs cannot transfer identically between
vendors; see [Permissions](#permissions-and-credentials).

## Install

The `lennon` command is installed locally on the development machine. To build
from this private repository elsewhere, install [Rust](https://rust-lang.org/tools/install/)
1.85 or later and run:

```sh
git clone https://github.com/pballada/lennon.git
cd lennon
cargo install --path . --locked --bin lennon
```

The crate is not published to crates.io. The application is one native executable
with no JavaScript runtime dependency. Separately install and authenticate at
least one provider CLI using its official instructions:

```sh
claude
codex login
gemini
lennon doctor
```

## Use

Use it like `claude`:

```sh
cd /path/to/your/project
lennon                                   # interactive session
lennon "fix the failing tests"           # interactive, starting with a prompt
lennon -p "explain this repository"      # print the answer and exit
cat error.log | lennon -p "what went wrong?"
lennon -c                                # continue the latest conversation here
lennon -r                                # pick a conversation to resume
lennon -r SESSION_ID "now add tests"     # resume a specific conversation
lennon --permission-mode acceptEdits     # allow file edits
lennon -p --output-format json "..."     # one JSON result (or stream-json)
lennon --model opus                      # model for the primary provider
```

Supported options mirror Claude Code where they apply: `-p/--print`,
`-c/--continue`, `-r/--resume [id]`, `--model`, `--permission-mode`,
`--output-format text|json|stream-json`, `--add-dir`, `--allowedTools`,
`--disallowedTools`, `--verbose`, `-v/--version`, `-h/--help`. Lennon adds
`--order`, `--provider`, `--config`, `--cwd`, `--timeout MS`, and `--retry`.
Lennon 0.2 spellings still work (`--write`, `--json`, `--prompt`).

### In the session

| Input | Action |
| --- | --- |
| `/` | Slash command menu: `/help`, `/clear`, `/resume`, `/model`, `/status`, `/cost`, `/config`, `/doctor`, `/init`, `/memory`, `/exit`, plus Lennon's `/providers` and `/retry`. Your `.claude/commands` and `.claude/skills` appear too and are passed to the provider. |
| `@` | File path completion |
| `!` | Shell mode: run a command; its output joins the next message |
| `?` | Shortcuts |
| shift+tab | Cycle permission modes: manual → accept edits → plan → auto |
| esc | Interrupt the provider (the session is saved), close menus; twice clears input |
| enter while working | Queue a message for when the provider finishes (up arrow edits it) |
| shift+enter, `\`+enter, ctrl+j | Newline |
| ↑ / ↓ | Prompt history for this project |
| ctrl+o | Verbose tool output · ctrl+g edit prompt in `$EDITOR` · ctrl+l redraw · ctrl+z suspend |
| ctrl+c | Interrupt or clear input; twice to exit |

When a provider hits its limit, the transcript says so and work continues:

```
⏺ claude hit its usage limit · continuing with codex
```

If every provider is unavailable, the session is saved; run `/retry` (or
`lennon -c` later). `--timeout MS` stops a stalled provider while preserving the
task; timeouts do not cause automatic failover. Ctrl-C/Esc and SIGTERM stop the
provider process group and save an unfinished session. Uncooperative children
receive SIGKILL after a 1.5-second grace period.

Other commands: `lennon sessions`, `lennon providers`, `lennon doctor`,
`lennon config`.

## Configure providers, models, and fallback order

The default chain is `claude,codex,gemini`. Each CLI chooses its own default model
unless a model is configured. Config lives at `~/.config/lennon/config.json`,
respecting `XDG_CONFIG_HOME`.

```sh
lennon config init                              # create; never overwrite
lennon config set order codex,claude,gemini
lennon config set providers.claude.model sonnet
lennon config set providers.gemini.enabled false
lennon config unset providers.claude.model      # restore native model default
lennon config                                  # effective settings and locations
lennon providers                               # entries, models, chain positions
```

Multiple entries can use different models on the same backend:

```json
{
  "order": ["quick", "deep", "codex", "gemini"],
  "authentication": "subscription",
  "cooldownMs": 300000,
  "handoffChars": 120000,
  "providers": {
    "quick": { "adapter": "claude", "model": "sonnet" },
    "deep": { "adapter": "claude", "model": "opus" },
    "codex": { "adapter": "codex" },
    "gemini": { "adapter": "gemini", "enabled": true }
  }
}
```

```sh
lennon config set providers.quick '{"adapter":"claude","model":"sonnet"}'
lennon config set order quick,codex,gemini
lennon --order codex,claude --model claude=sonnet -p "Review this change"
lennon --provider quick -p "Explain the project"
lennon --config /path/to/config.json providers
```

Entries accept `adapter` (`claude`, `codex`, `gemini`), `model`, `command`, and
`enabled`. Built-in entry names inherit their matching adapter. Claude also
accepts `allowedTools` in acceptEdits and auto modes. `command` is a single executable path,
never a shell command. Models are passed unchanged to the upstream CLI and must
be available to your account. Disabled entries are skipped. At least one ordered
entry must be enabled. Duplicate IDs, unknown settings, and invalid assignments
fail before starting any provider.

A bare `--model NAME` sets the primary (first enabled) entry, like Claude Code;
repeat `--model ID=MODEL` for other entries. `/model` changes models in a session. `--provider` selects one entry;
it cannot combine with `--order`. Precedence: CLI flags → selected config →
defaults. `--config` overrides `LENNON_CONFIG`, which overrides the user config
path. An explicitly selected missing file is an error. Per-run flags never
modify the saved configuration. Resumed sessions use the current configuration.
Changing a route's adapter, command, or model starts a fresh native session with
the portable context. Each entry keeps separate native IDs and cooldowns; model
entries do not increase shared account quotas.

## How continuity works

1. Journal the user request in private local state.
2. Start the first eligible provider with JSON streaming, the selected model,
   the project directory, and the task on stdin.
3. Persist native session IDs, observable messages, tool calls, and results.
4. On explicit quota errors or temporary service failures, try the next entry.
   Missing executables are skipped; authentication and permission failures stop.
5. Pass the original/latest requests, observable history, and the same workspace
   to the fallback. It must inspect existing changes before repeating actions.
6. Resume native sessions on later turns, including intervening context they have
   not seen. Retry the primary after its session-local cooldown expires.

Explicit streamed quota errors terminate native retry loops promptly. No repeated
API polling is used. If every provider is unavailable, Lennon exits with a saved
pending session; it cannot continue without an available authenticated account.
Unknown errors, invalid options, timeouts, and cancellation also stop the run.

### Existing JavaScript sessions

The Rust rewrite retains the original **version-1 JSON/JSONL formats**, config
locations, session IDs, native IDs, and routing settings. No migration command
or data deletion is needed. JavaScript → Rust → JavaScript native-session resume
is tested against the archived predecessor in the compatibility benchmark.

State defaults to `~/.local/state/lennon/sessions/SESSION_ID/`, respecting
`XDG_STATE_HOME`; override with `LENNON_STATE_DIR`. The append-only journal
recovers native IDs, context cursors, and pending state after interrupted metadata
writes. Only a torn final record is repaired; other corruption stops recovery.
Exclusive session locks prevent simultaneous writers. Native provider session
files must remain available for native resume.

### Context and durability

Raw events are journaled; the portable transcript excludes private thinking
blocks. Long handoffs include recent context inline and a private file with the
full portable history. The agent is instructed to read that file before
continuing and to report permission problems instead of guessing missing context.

The Rust journal retains a file descriptor and calls `sync_all` after **every
event**, preserving the original Node implementation's full-sync policy on
macOS. Handoff files and atomic metadata writes are synced too. The rewrite
speeds up launch and parsing; it does not hide disk costs by weakening durability.
Actual power-loss guarantees still depend on the filesystem and hardware.

This is context reconstruction, not an identical vendor-native session.
Compaction, model interpretation, and tools differ. Commands already executed
cannot be rolled back by interrupting a provider. Exactly-once execution of
external side effects is not guaranteed. A provider that hangs without emitting
an error requires cancellation or an explicit timeout.

## Permissions and credentials

Lennon uses Claude Code's permission modes and maps them onto each provider:

| Mode | Claude Code | Codex | Gemini |
| --- | --- | --- | --- |
| manual (default) | `default` | read-only sandbox | `default` |
| acceptEdits | `acceptEdits` | workspace-write sandbox | `auto_edit` |
| plan | `plan`, read-only tools | read-only sandbox | `plan` |
| auto | `auto` | workspace-write sandbox | `auto_edit` |

Lennon never enables `bypassPermissions`, `--dangerously-skip-permissions`, or
yolo mode; those options are rejected. Providers run headless, so they cannot
show interactive approval dialogs. A tool that would prompt is denied, and the
transcript shows the denial. Allow specific tools with your native settings,
`--allowedTools "Bash(npm test)"`, or a configured `allowedTools`, or switch modes
with shift+tab. Native hooks, MCP tools, trust settings, and sandbox policies
still apply.

Lennon does not read or exchange login tokens. Subscription mode removes common
API-key environment variables from child processes to avoid taking precedence
over native subscription login. Native stored authentication and billing remain
under each CLI's control. Set `authentication` to `environment` only when you want
to pass API-key environment variables through.

Session files may contain code and tool output. New files/directories use private
POSIX permissions. **Automatic failover sends recorded task history to the next
configured provider.** Include only providers you intend to receive it. Lennon
has no telemetry or remote session store.

## Development and performance

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo build --locked --release --bin lennon
```

The `test-fixtures` feature enables native mock executables used by integration
tests. Production installation/build selects only `--bin lennon`. Tests need no
Node/Python runtime, accounts, or model spend. CI covers Linux/macOS and Rust 1.85.

The archived JavaScript source in `benchmarks/reference-js` exists only for
comparative measurements and compatibility tests; it is never invoked by the
native application. Python/Node are optional benchmark tools, not runtime
requirements. See [native port results](benchmarks/NATIVE_PORT.md),
[benchmark methodology](benchmarks/README.md), and [validation](VALIDATION.md).

## Provenance

Inspired by [Claude Code's workflow](https://github.com/anthropics/claude-code).
The core CLI is not provided there under an open-source license; Anthropic's
[license](https://github.com/anthropics/claude-code/blob/main/LICENSE.md) reserves
its rights. Lennon contains independently written harness code, not proprietary
or leaked Claude Code source. It operates supported command-line interfaces:
[Claude Code](https://code.claude.com/docs/en/headless),
[Codex](https://developers.openai.com/codex/noninteractive), and
[Gemini CLI](https://geminicli.com/docs/cli/headless/).

Lennon is named after a wiry black podenco with a gentle, sad expression.
One ear up. One folded. Silver eyebrows. Stays with the work.

MIT © 2026 pballada. Upstream CLIs and services retain their own terms.
