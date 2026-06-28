# Pastebin Service

A clean, layered pastebin REST API (Axum + SQLite/sqlx) — **not yet started**.

Work begins after the URL shortener is complete, following the same conventions
in [`../docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md). The build order is in
[`../docs/PR_PLAN_pastebin.md`](../docs/PR_PLAN_pastebin.md); its PR #1 creates
the `Cargo.toml`, source tree, and `/health` endpoint, at which point this crate
is added to the root workspace's `members`.
