# Codex history storage

CCCC follows Codex's default history format for new persistent threads instead
of forcing `historyMode: "legacy"`. Codex CLI 0.163.0 defaults to paginated
history and its native terminal can attach to those threads. Existing threads
keep their stored format and identity; CCCC does not migrate them at launch.

The managed adapter resumes with `excludeTurns: true`. New-thread durability is
checked using `thread/read` with `includeTurns: false`, matching the returned
thread id and requiring a persisted path. Neither check requests full history.
The Web runtime terminal uses the stock Codex remote TUI, which owns history
pagination; CCCC does not add another transcript reader or a replacement UI.

See the [official App Server documentation](https://learn.chatgpt.com/docs/app-server)
for `thread/turns/list`, `thread/items/list`, and metadata-only reads. These
interfaces bound history delivery, but do not promise constant-time restoration
of every model-visible context or constant disk usage.

## Verification

Run the adapter's protocol fixtures through the ordinary Rust tests. An optional
Linux real-Codex check exercises native terminal attachment without credentials,
model requests, an existing runtime home, or a running CCCC service:

```sh
CCCC_CODEX_TEST_BINARY=/path/to/codex node scripts/check-codex-paginated-history.mjs
```

This needs Node.js 24+, Python 3, a PTY and local loopback socket permission.
The script creates and removes only its own temporary directory and process
groups. A dummy provider uses an unused loopback endpoint; no turn is submitted.
The legacy migration fixture contains 200 synthetic completed turns by default.
Set `CCCC_CODEX_TEST_HISTORY_TURNS` (4–10000) for a larger retained-history check.
This checks retained answer text and distinct cursor pages as well as startup.

## Existing legacy threads

Codex CLI 0.163.0 exposes an official `migrate-rollouts` command. Inspect the
installed version's help before using it. Without `--apply`, it reports eligible
sessions. Scope inspection and publication to explicit thread IDs with
`--thread`; do not automatically migrate all user history from CCCC.

Before publication, stop every writer using the affected thread, retain a
recoverable copy of the complete relevant Codex home, and budget for migration
I/O and temporary storage. Migrate an isolated copy first. After migration,
verify the same thread id, retained messages, history pages, runtime resume and
native TUI attachment before allowing new model work. Never edit rollout JSONL
or the SQLite history-mode column to simulate a migration. Do not perform a
bulk reverse migration to roll back a CCCC binary; keep verified backups instead.

Changing this default does not rewrite or shrink existing rollout files. The
optional compatibility check uses disposable synthetic history, not production
transcripts or evidence that arbitrary-size sessions have constant resume cost.
