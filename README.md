# brain

The personal knowledge layer: a vault of plain files in a local git repo, an index
rebuilt from those files, one CLI, and one agent skill. brain calls no model.

- **Your selection only.** Nothing enters the vault unless you mark it: `brain add`,
  or a `like` on the feed.
- **Files are the truth.** Docs, wiki pages, the log and labels are markdown and JSON in
  `~/brain`. The SQLite index (`~/.cache/brain/index.db`) is a cache: `brain reindex --full`
  rebuilds it.
- **Obsidian-native.** `~/brain` opens as an Obsidian vault. A doc is one note,
  `docs/<id>/<id>.md` (`arxiv--2410.24164`), its metadata in the frontmatter. Citations are
  `[[arxiv--2410.24164#Method]]` wikilinks and typed relations are wikilink-valued
  properties (`builds_on: ["[[arxiv--2504.16054]]"]`), so the graph view shows the edges
  agents query.
- **One front door.** `brain find` searches docs, pages, the log and your repos (BM25 and
  vectors, fused). Bare `brain` prints the 400-token profile a session starts with.
- **One scheduler.** `brain tick` runs every job that is due, once. A systemd timer calls it
  every 15 minutes.

The design is in `DESIGN.md`, the contracts in `ARCHITECTURE.md` and `contracts/`, the
requirements in `REQUIREMENTS.md`. Every command and flag is documented in `brain --help`.

## Install

```bash
cargo install --path .
mkdir -p ~/.claude/skills/brain && brain --skill >~/.claude/skills/brain/SKILL.md
```

Other machines run brain on the host over ssh: link `bin/brain-ssh` as `brain` on PATH and
set `BRAIN_HOST` to the host's ssh name. `--skill` is answered locally; every other command
runs on the host.

Runtime needs `git` and `curl`, `rg` for `brain code`, and an OpenAI-compatible
embedding server (default `http://localhost:11434/v1/embeddings`, model
`qwen3-embedding:0.6b`; `BRAIN_EMBED_URL` and `BRAIN_EMBED_MODEL` change them). Without
it, `find` runs on BM25 alone and says `degraded: no vectors`.

The vault is `~/brain` (`--vault` or `BRAIN_VAULT` change it). Machine state, which
never goes in the vault, is in `~/.local/state/brain/`: `jobs.jsonl` (one line per job run
and per tick), `eval.jsonl` (scores per build), `queries.jsonl`, `tick.json`.

## Run on a timer

`systemd/` holds the unit files. brain never installs them:

```bash
cp systemd/brain-tick.* ~/.config/systemd/user/
systemctl --user enable --now brain-tick.timer
```

`brain tick --dry-run` prints what is due and why, and runs nothing. `brain migrate` checks
the steps for a new host and changes nothing.

## Development

Library code follows strict lints (no `unwrap`/`expect`/`panic!`, no slice indexing, no
`unsafe`); see `[lints]` in `Cargo.toml`. Dependencies stay few: clap, rusqlite, serde,
serde_json, serde-saphyr (frontmatter YAML), thiserror and a JSON Schema validator; HTTP
and git go through the system `curl` and `git`.

```bash
cargo clippy --all-targets -- -D warnings && cargo test
```
