# brain

[![ci](https://github.com/theguega/brain/actions/workflows/ci.yml/badge.svg)](https://github.com/theguega/brain/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**A second brain your coding agents can query.** Papers, books, notes, meetings, a daily
log and your git repos in one vault of plain markdown, searchable from one CLI, with a
skill that teaches any agent to use it.

```console
$ brain find flow matching action expert
results[8/756]{id,kind,date,title,snippet}:
  arxiv:2506.01844,paper,2025-06-02,SmolVLA: A Vision-Language-Action Model for …,<<<external Regression vs. Flow Matching …>>>
  arxiv:2410.24164,paper,2024-10-31,π0: A Vision-Language-Action Flow Model for General Robot Control,<<<external Action expert. π0 is …>>>
  …
$ brain get "arxiv:2410.24164#Abstract"
$ brain code "fn slugify"
$ brain log "Chose flow matching over diffusion for the arm policy
Faster inference at equal success; see run 4c2f."
```

## Why

Agents forget everything between sessions and guess at what you already know. brain gives
them one place to look: what you read, what you decided, what you shipped, and the code
you wrote. It is built on a few rules:

- **Plain files are the truth.** The vault is markdown in a local git repo. The search
  index is a cache; `brain reindex --full` rebuilds it with identical results.
- **Only what you mark.** Nothing enters the vault unless you add it.
- **No model inside.** brain fetches, parses, indexes and checks. Writing (wiki pages,
  plans) happens in agent runs, and their output comes back through brain's linter and a
  git branch you merge.
- **Obsidian-native.** The vault opens in Obsidian. Citations and typed relations are
  wikilinks, so the graph view shows the same edges agents query.
- **Untrusted text is fenced.** Papers and web pages print inside `<<<external … >>>`, so a
  prompt injection in a PDF is never a bare instruction.

## Features

- **Hybrid search** over docs, wiki pages, the log and your repos: BM25 and vector search
  fused with reciprocal rank fusion. Works without vectors (BM25 only) and says so.
- **Ingest** arXiv ids and URLs, web pages, PDFs, epubs, HTML and markdown. Papers get
  metadata from arXiv, Hugging Face, alphaXiv and Papers with Code, including which paper
  builds on which. Adding twice changes nothing.
- **Live code search** across every git checkout under your project roots, with READMEs,
  design docs and commit messages indexed for `find`.
- **A daily log** of decisions and results, with timezone-aware timestamps.
- **A reviewed wiki** in the style of Karpathy's LLM Wiki: an agent compiles what you mark
  into concept pages where every claim cites its source; brain lints the proposal onto a
  branch, and nothing lands until you merge it.
- **An eval** (`brain eval`): recall@8 and tokens per hit on your own questions, so retrieval
  changes are measured, not guessed.
- **One scheduler** (`brain tick`) for reindexing, embedding, repo scans, the eval and the
  session profile, run by a systemd timer.
- **Agent-ready output**: compact tables, `key: value` fields or JSONL, and every error
  ends in a `next:` command to run.

## Install

brain runs on Linux. You need Rust 1.89+, `git`, `curl`,
[ripgrep](https://github.com/BurntSushi/ripgrep), poppler (`pdftotext`, `pdfinfo`) and
`unzip`.

```bash
cargo install --locked --git https://github.com/theguega/brain
```

For vector search, run any OpenAI-compatible embeddings server. With
[Ollama](https://ollama.com):

```bash
ollama pull qwen3-embedding:0.6b
```

`BRAIN_EMBED_URL` (default `http://localhost:11434/v1/embeddings`) and `BRAIN_EMBED_MODEL`
point elsewhere.

### The agent skill

```bash
mkdir -p ~/.agents/skills/brain && brain --skill > ~/.agents/skills/brain/SKILL.md
```

Claude Code reads `~/.claude/skills`; link it to the shared copy with
`ln -s ~/.agents/skills/brain ~/.claude/skills/brain`. `brain machine add` does this on a client.

The skill teaches an agent when to search, how to read results and citations, and to
write only what you ask. It works with any harness that reads `SKILL.md` files.

## Quick start

```bash
brain add 2410.24164 --why "flow-matching action expert"   # a paper by arXiv id
brain add https://example.com/post                          # a web page
brain add ~/notes/kickoff.md --kind meeting                 # a file
brain reindex                                               # index the vault and your repos
brain find action expert
brain                                                       # the profile agents load first
```

The vault lives in `~/brain` (`--vault` or `BRAIN_VAULT` to change it) and is created on
first use. Write `pages/me.md` with three lines about your focus: they open the profile.

## Run it on a timer

```bash
cp systemd/brain-tick.* ~/.config/systemd/user/
systemctl --user enable --now brain-tick.timer
```

Every 15 minutes `brain tick` runs whatever is due: reindex when the vault changed, embed
new chunks when the GPU is free, scan repos whose HEAD moved, rerun the eval when retrieval
changed, and regenerate the profile nightly. With nothing to do it does nothing.
`brain tick --dry-run` shows what is due and why.

## Use it from other machines

Keep one vault on one host. To add a client, run this on the host, with ssh to the client
working without a prompt:

```bash
brain machine add laptop
```

It copies `brain-ssh` and the skill, both from this build, to the client: `~/.local/bin/brain`
and `~/.agents/skills/brain`. It also links `~/.claude/skills/brain`. Set `BRAIN_HOST` to this
host's ssh name in the client's shell; `--host` sets the name the client checks with. Every
command runs on the host, and rerunning the line updates both.

## Configuration

| | Default | Change with |
|---|---|---|
| Vault | `~/brain` | `--vault`, `BRAIN_VAULT` |
| Repo roots | `~/Developer` | `repos.toml` in the vault: `roots`, `include`, `exclude` |
| Embedder | Ollama on localhost, `qwen3-embedding:0.6b` | `BRAIN_EMBED_URL`, `BRAIN_EMBED_MODEL`, `BRAIN_EMBED_TIMEOUT` |
| GPU sharing | embedding waits while other processes hold >2 GB of GPU memory | `BRAIN_EMBED_DEVICE=cpu` for a CPU-only embedder, which never waits |
| Index | `~/.cache/brain/index.db` | `XDG_CACHE_HOME` |
| State (jobs, eval, queries) | `~/.local/state/brain/` | `XDG_STATE_HOME` |
| Timezone and place | system zone | `~/.local/state/context.json` |

Every command and flag is documented in `brain --help` and `brain <command> --help`.

## How it works

[DESIGN.md](DESIGN.md) covers the vault layout, the doc kinds and their schemas
(`contracts/`), ids, the index and ranking, the wiki review loop, the jobs and the eval.

## Development

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```

The library denies `unwrap`, `expect`, `panic!` and slice indexing, and forbids `unsafe`
(see `[lints]` in `Cargo.toml`). Dependencies stay few: clap, rusqlite, serde, serde_json,
serde-saphyr, thiserror and jsonschema. HTTP and git go through the system `curl` and
`git`.

## License

[MIT](LICENSE)
