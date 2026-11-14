<div align="center">
  <img src="assets/lennon.svg" width="220" alt="Lennon, a shaggy black podenco with tall ears and a melancholy expression" />
  <h1>lennon</h1>
  <p><strong>A loyal companion for unfinished work.</strong></p>
  <p>Claude Code → OpenAI Codex → Gemini CLI.<br />One workspace. One continuing Lennon session.</p>
</div>

Lennon is a small, dependency-free CLI harness for the coding agents you already use. When Claude Code reports a usage limit, Lennon saves the observable conversation and tool history, then continues the task with Codex, followed by Gemini. It uses each CLI's existing login and native tools. No API key is required by Lennon.

**Status: working alpha.** Automatic failover, native resume, durable history, streaming output, provider cooldowns, and an interactive conversation loop are implemented. This is an independent harness, not a fork or a reimplementation of every Claude Code feature. CLI protocols can change. Hidden reasoning, provider-side state, interactive permission dialogs, and vendor-specific plugins cannot be transferred identically between vendors.

## Start

Requires **Node.js 22+** and at least one authenticated provider CLI.

```sh
git clone https://github.com/pballada/lennon.git
cd lennon
npm install --global .

# Install any missing upstream CLIs using their official instructions.
# Login once in each CLI you intend to use:
claude
codex login
gemini

lennon doctor
cd /path/to/your/project
lennon --write "Fix the failing tests and verify the changes"
```

Lennon is not yet published to npm. Install from the repository. Claude Code, Codex, and Gemini CLI must be separately installed; Lennon neither bundles nor modifies them. Account entitlements, model availability, and usage limits belong to the respective providers.

```sh
lennon                                  # conversation loop, read-only tools
lennon --write                          # conversation loop, edits permitted
lennon -p "Explain this repository"      # one task
git diff | lennon -p "Review this diff"  # add piped context
lennon --continue -p "Now add tests"     # latest session in this directory
lennon --resume SESSION_ID --write       # retry unfinished work
lennon --order codex,gemini -p "Review the build"
lennon --provider claude --model claude=sonnet -p "Explain this module"
lennon --json -p "Explain the project"   # JSONL for scripts
lennon sessions
lennon config
```

Interactive commands: `/status`, `/retry` (clear cooldowns and retry pending work), `/help`, `/exit`. To clear cooldowns in a one-shot run, pass `--retry`. `--resume` without a new prompt retries the pending task; it does not invent a new task after a completed turn. Every run prints the Lennon session ID.

## How continuity works

1. Save the user request in a private append-only event journal.
2. Start Claude Code with streaming JSON and the current project directory.
3. Persist native session IDs, responses, tool calls, and results as they arrive.
4. On an explicit quota/usage-limit error or temporary service failure, try the next configured CLI. Missing executables are skipped.
5. Give the next agent the original request, latest request, portable transcript, and the same files. Tell it to inspect existing changes before repeating interrupted actions.
6. Resume the original native session on later turns when its cooldown expires, sending the intervening history it has not seen.

There is no repeated API polling and no SDK dependency tree. Prompts travel over stdin instead of shell arguments. Explicit streamed quota errors terminate upstream retry loops promptly. The primary provider is retried on future turns after its cooldown; Lennon does not switch away from an actively successful provider mid-turn.

If all providers are unavailable, Lennon exits with an unfinished session ready to resume. It cannot keep running without an available authenticated account. Authentication failures, permission errors, invalid options, unknown failures, explicit timeouts, and cancellation stop the run. They do not silently broaden access or switch providers.

### Context and side effects

Lennon retains the complete raw event journal. It passes observable messages and tool activity to other providers, excluding thinking blocks from the portable transcript. Long handoffs include the recent history inline plus a private file containing the full portable handoff. The agent is instructed to read that file before continuing. If its permissions prevent reading it, continuity requires resolving that access restriction.

This is context reconstruction, not an identical vendor-native session. Context windows, compaction, model interpretations, and native tools differ. Interrupting an agent cannot roll back a command already executed. The next agent is instructed to inspect files and tool results before proceeding; exactly-once execution of external side effects is not guaranteed. A stalled provider that emits no error cannot be detected as quota-limited. `--timeout MS` can stop it while keeping the task pending.

## Permissions and credentials

The default uses each provider's read-only/plan policy. `--write` selects Claude `acceptEdits`, Codex `workspace-write`, and Gemini `auto_edit`. Lennon never adds permission-bypass or yolo flags. These modes are not identical security boundaries. Upstream configured hooks, MCP tools, sandbox policies, project instructions, and trust settings still apply. Run only in projects you trust.

Headless CLIs cannot all present their usual interactive approval dialogs. Commands that need approval may be denied. For Claude, grant specific tools through your native settings or `providers.claude.allowedTools` (for example `"Read,Edit,Bash(npm test)"`). Lennon does not translate permissions, hooks, MCP servers, or plugins across providers.

Lennon does not read, copy, or exchange login tokens. It launches the CLIs using their own credentials. By default it removes common API-key environment variables from child processes so an inherited key does not take precedence over subscription login. Stored upstream authentication and billing choices remain the responsibility of each CLI. Set `"authentication": "environment"` only when you deliberately want to pass through API-key environment variables.

Session data can contain source code and tool output. It is stored outside the repository, with private file/directory modes on POSIX. **Automatic failover sends the recorded task history to the next configured provider.** Configure the order to include only providers you want to receive it. Lennon has no telemetry or remote session store.

## Configuration

User config: `~/.config/lennon/config.json` (respects `XDG_CONFIG_HOME`). No project-local config is executed. Model defaults come from the installed CLIs so Lennon does not pin stale model names.

```json
{
  "order": ["claude", "codex", "gemini"],
  "authentication": "subscription",
  "cooldownMs": 300000,
  "handoffChars": 120000,
  "providers": {
    "claude": { "command": "claude" },
    "codex": { "command": "codex" },
    "gemini": { "command": "gemini" }
  }
}
```

Manage the configuration from the terminal:

```sh
lennon config init                              # create a file; never overwrite one
lennon config set order codex,claude,gemini      # persist fallback order
lennon config set providers.claude.model sonnet
lennon config set providers.gemini.enabled false
lennon config unset providers.claude.model      # restore the native CLI default
lennon config                                  # show effective configuration
lennon providers                               # list entries, models, and positions
```

Provider entries accept `adapter` (`claude`, `codex`, or `gemini`), `model`,
`command`, and `enabled`. The built-in entries inherit the adapter matching
their name. Disabled entries are skipped even if they remain in `order`.
At least one ordered entry must be enabled. Omit `model` to use the native CLI's
configured default. Model names are passed unchanged to that CLI and must be
available to your account. Claude also accepts `allowedTools` in write mode.
`command` is a single executable path, never a shell command.

For multiple models from the same backend, define separate named entries:

```json
{
  "order": ["quick", "deep", "codex", "gemini"],
  "providers": {
    "quick": { "adapter": "claude", "model": "sonnet" },
    "deep": { "adapter": "claude", "model": "opus" },
    "codex": { "adapter": "codex" },
    "gemini": { "adapter": "gemini", "enabled": true }
  }
}
```

You can create an entry without editing JSON manually:

```sh
lennon config set providers.quick '{"adapter":"claude","model":"sonnet"}'
lennon config set order quick,codex,gemini
```

Each entry has its own native session and cooldown. Provider account limits may
be shared between models; adding entries does not increase account quota.
Changing an entry's adapter, command, or model starts a fresh native session
with the recorded context, avoiding incompatible native histories.

Per-invocation overrides:

```sh
lennon --order codex,claude --model claude=sonnet -p "Review this change"
lennon --provider quick -p "Explain the project"  # only one configured entry
lennon --config /path/to/config.json providers
```

Repeat `--model ID=MODEL` for different entries. `--provider` and `--order` are
mutually exclusive. Precedence is CLI flags → selected config file → defaults.
`--config` overrides `LENNON_CONFIG`, which overrides the normal user config path.
Explicitly selected missing files fail immediately. Flags do not modify the
saved file, and resumed sessions use the current config and flags. Existing
config files with `order`, `model`, and `command` remain compatible.

Cooldowns are local to a Lennon session and default to five minutes; they do not
predict provider reset times. Unknown settings, invalid model assignments,
duplicate order entries, and unknown adapter IDs are rejected before execution.

State: `~/.local/state/lennon/sessions/SESSION_ID/` (respects `XDG_STATE_HOME`). Override with `LENNON_STATE_DIR`; override config location with `LENNON_CONFIG`. The journal is authoritative when recovering an interrupted metadata write. Only an incomplete final JSONL record is repaired; other corruption stops recovery. Session locks prevent simultaneous writers. Native session files must also remain available for native resume; deleted native sessions require starting a new Lennon session.

## Development

```sh
npm test
npm run bench
npm pack --dry-run
```

Tests use mock executables, exercise actual subprocesses, and require no accounts or model spend. They cover failover, streamed errors, native resume, context handoff, cooldowns, permissions, cancellation, timeouts, UTF-8 framing, crash recovery, session locking, and the end-to-end CLI. Live provider verification is separate from protocol fixtures.

The next milestones are richer permission interaction, provider protocol compatibility fixtures, and deliberate context compaction with verification. Full Claude Code feature parity is not claimed.

Runtime measurements and reproducible Node/Bun/Rust probes are in
[benchmarks/RESULTS.md](benchmarks/RESULTS.md). Rust led minimal startup and
memory; Bun led the JSON workload. Per-event disk flushing dominated the
equal-durability streaming tests. The default runtime remains Node.

## Provenance

Inspired by the workflow in [anthropics/claude-code](https://github.com/anthropics/claude-code). Its public repository does not provide the core CLI under an open-source license; its [license](https://github.com/anthropics/claude-code/blob/main/LICENSE.md) reserves Anthropic's rights. Lennon contains independently written harness code and does not incorporate Claude Code source or leaked mirrors. It operates the supported command-line interfaces:

- [Claude Code headless mode](https://code.claude.com/docs/en/headless)
- [Codex non-interactive mode](https://developers.openai.com/codex/noninteractive)
- [Gemini CLI headless mode](https://geminicli.com/docs/cli/headless/)
- [Gemini session management](https://geminicli.com/docs/cli/session-management/)

Lennon is named after a shaggy black podenco with a sad expression. Long ears. Soft eyes. Stays with the work.

MIT © 2026 pballada. Upstream CLIs and services retain their own licenses and terms.
