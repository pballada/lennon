# Working on Lennon

Lennon is an independent, dependency-free Node.js CLI harness. Keep startup fast,
use Node 22+ standard APIs, and preserve subscription-first authentication.

- Never import proprietary or leaked Claude Code source.
- Launch provider commands with argument arrays and `shell: false`.
- Never log credentials, read native token stores, or add permission-bypass flags.
- The event journal is authoritative. Persist observable tool activity before handoff.
- Do not claim identical hidden context or exactly-once execution across vendors.
- Keep native session IDs separate from Lennon session IDs.
- Add protocol fixtures when modifying provider adapters. Run `npm test`.
- `npm run bench` measures harness startup, not upstream or model latency.
- Live model tests use account quota; use small read-only prompts when needed.

Code lives in `src/`, the executable in `bin/`, and subprocess tests in `test/`.
There is no build step and no runtime dependency installation.
