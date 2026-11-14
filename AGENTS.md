# Working on Lennon

Lennon is a native Rust CLI for macOS and Linux. Keep startup fast, keep the
production executable independent of Node.js, and preserve subscription-first
provider authentication. Minimum supported Rust is declared in Cargo.toml.

- Never import proprietary or leaked Claude Code source.
- Launch provider commands with argument arrays; never use a shell wrapper.
- Never read native login tokens or add permission-bypass flags.
- The version-1 JSONL journal is authoritative and compatible with the original
  JavaScript harness. Preserve its schema and provider route identities.
- Persist observable tool activity before handoff. Keep full-sync-per-event
  durability unless a user explicitly asks for a different durability policy.
- Do not claim identical hidden context or exactly-once side effects.
- Keep native IDs separate from Lennon IDs; invalidate native history when a
  configured route's adapter, model, or command changes.
- Run cargo fmt --all -- --check, cargo clippy --all-targets --all-features --
  -D warnings, and cargo test --all-features.
- The test-fixtures feature enables native mock providers, not production tools.
  Install/build only --bin lennon for production.
- Live model tests use account quota; use small read-only prompts when needed.

- The interactive UI (src/tui.rs, src/view.rs) should look and behave like
  Claude Code: same layout, glyphs, colours, slash commands, shortcuts, and
  options. Observe the installed `claude` for reference; never copy its
  source. The one deliberate difference is Lennon's mascot (view::mascot).
- Never add bypassPermissions or dangerously-skip-permissions modes.

Production source lives in src/*.rs, native integration tests in tests/.
benchmarks/reference-js is a frozen benchmark/compatibility reference, never a
production fallback. Model/CLI startup latency is separate from harness overhead.
