# brain — design

brain is the knowledge layer: a vault of plain files, an index rebuilt from those files, one CLI, and one skill. It calls no model. The vault is the truth, and everything else can be rebuilt from it.

The repo is paperdb renamed (D2). It holds the Rust crate, `feed/` (Python, the paper and news feed), `planner/` (prompt files), and `contracts/` (JSON Schemas for C1–C16).

Status: design, round 3. Requirements B1–B16, F1–F10, P1–P7.

## Layers

| Layer | Owns | Language |
|---|---|---|
| vault | `~/brain`: docs, pages, log, chats, labels, profile | files |
| index | `~/.cache/brain/index.db`: chunks, FTS5, vectors, links | Rust (rusqlite) |
| CLI + skill | the agent contract (C4, C5) | Rust |
| feed | candidate pool, ranker, daily selection (C6, C7) | Python (uv) |
| planner | prompt files and deterministic checks, run through drover | files + Rust |
| contracts | `contracts/*.schema.json`, golden tests | JSON Schema |

## Vault

```text
~/brain/                      local git, no remote (Q15)
  docs/<id>/meta.json         C1: kind, source, title, required metadata per kind
  docs/<id>/text.md           full text (artifact: index.html; text extracted at index time)
  pages/<slug>.md             C2: people, projects, decisions; frontmatter + [[wikilinks]]
  log/YYYY-MM-DD.md           C3: journal, plan, shutdown, chat summaries
  chats/<id>.jsonl            raw chat transcripts (normalized C9 events)
  labels.jsonl                C7: feed verdicts
  profile.md                  C5: regenerated nightly, ≤400 tokens
  repos.toml                  C13: roots, include/exclude globs
  eval/questions.jsonl        B8: the 50 real questions
  private/                    personal material (indexed like the rest)
```

Rules:
- One writer per file type. `brain` writes docs, pages, the log and the profile. `feed` writes `labels.jsonl` only through `brain label`.
- A write goes to a temp file and is then renamed (`write_atomic`, kept from paperdb), and is committed to local git in the background, debounced to one commit per minute.
- Nothing in the vault holds an absolute path or a hostname (B16).

## Kinds

Each kind is a JSON Schema in `contracts/kinds/<kind>.schema.json`. Adding a kind adds a file and needs no code change (C1). `brain lint` validates every doc against its schema.

| Kind | Required metadata | Filled by |
|---|---|---|
| paper | title, authors, date, venue?, `links{code, project, hf, openreview, video}` | arXiv, HF Hub (`githubRepo`, `projectPage`), alphaXiv |
| book · course | title, authors, year, `links{home}` | you, or the PDF's metadata |
| blog · news | title, url, site, date | page metadata (Jina Reader) |
| talk | title, speakers, date, `links{video, slides}` | you |
| note | title | you |
| artifact | title, why, source, captured_at | `brain add page.html --kind artifact --why` |
| meeting | started_at, tz, place, attendees[] → pages, project? | you, by paste or file |

Missing optional fields are fine. A missing required field is a `lint` error, and `add` reports it on the spot.

## Ids

Ids are derived from the source, so an id can never disagree with its content (as in paperdb's `id.rs`):
- `arxiv:2410.24164` (the version is dropped)
- `web:<slug>`
- `file:<sha256[:8]>`
- `meeting:<date>-<slug>`
- `artifact:<slug>`

The directory name is the id with `:` replaced by `--`.

## Time

Every timestamp is ISO 8601 with its offset, plus the IANA timezone and a place, read from C15 (`~/.local/state/context.json`) at capture time (B15). Displays use the stored zone, not the viewer's. Day boundaries (`log/YYYY-MM-DD.md`, "today") use your current timezone.

## Index

```sql
docs(id PK, kind, title, date, meta JSON, hash, taint)          -- taint: own | external
chunks(id PK, doc, ord, heading, text)
chunks_fts USING fts5(heading, text, tokenize='porter unicode61')
vectors(chunk PK, dim, v BLOB)                                   -- f16, L2-normalized
links(src, dst, rel)                                             -- from [[wikilinks]] and links{}
repos(name PK, path, remote, head, indexed_at)
PRAGMA user_version                                              -- a mismatch means drop and reindex
```

- **Freshness:** each file's hash is compared with `docs.hash`, and only changed docs are re-chunked and re-embedded. `brain reindex --full` rebuilds from nothing, and the golden test checks that it gives identical `find` results (B1).
- **Chunking:** split on headings, aiming for about 400 tokens with an overlap of 1 paragraph. Tables are kept whole in a chunk but stripped from snippets.
- **Vectors:** from a local OpenAI-compatible `/v1/embeddings` server (default Qwen3-Embedding-0.6B, 1024-d, on CPU). They're stored as f16 BLOBs and searched by brute-force cosine in Rust: 10⁵ × 1024 is about 0.1 GFLOP, a few ms. This means no sqlite-vec, which would need `unsafe` to load (the crate has `unsafe_code = "forbid"`). If the embedder is down, search runs BM25-only and says `degraded: no vectors` (B12).
- **Ranking:** BM25 (column weights inherited from paperdb) and cosine, fused with RRF: `score(d) = Σ_r 1/(60 + rank_r(d))`. Results are collapsed to the best chunk per doc. A title phrase match gives a boost, and an exact match on a page alias goes straight to that page. No reranker until the eval asks for one.
- **Taint:** anything from outside (a paper, the web, news, an abstract) is `external`. `find` and `get` print its text inside a fence, `<<<external … >>>`, so an injected instruction is never a bare line (B5).

## Repos (C13)

`repos.toml` lists the roots (default `~/Developer`) and include/exclude globs. Discovery finds every git checkout under those roots. For each repo the index holds:
- its markdown files (README, AGENTS.md, DESIGN.md, `docs/**`) as `kind: note` chunks, `taint: own`
- its commit messages since the last indexed `head`, as log-like chunks
- a repo card: remote, languages, last activity, pages and papers linking to it

Code is never copied. `brain code <q> [--repo]` runs `rg --json` across the registered repos and prints `repo:path:line | text`, capped at 20 hits.

## CLI (C4)

| Command | Does | Output |
|---|---|---|
| `brain` | prints the profile (C5) | the profile text |
| `brain find <q> [--kind --since --repo --limit 8]` | hybrid search | `results[n/total]{id,kind,date,title,snippet}` |
| `brain get <id>[#span] [--full]` | a doc, page or chunk, capped at 1000 chars | `key: value` plus a text block |
| `brain code <q> [--repo]` | live rg across repos | `hits[n/total]{repo,path,line,text}` |
| `brain add <arxiv\|url\|file> [--kind --why --tag]` | ingest, idempotent | `id`, `missing`, `help` |
| `brain log <text> [--source]` | appends a C3 block | `at`, `file` |
| `brain label <id> like\|skip\|later` | appends C7 (a like also runs `add`) | `id`, `verdict` |
| `brain link <a> <b> <rel>` | adds a link to a page's frontmatter | `noop:` if it already exists |
| `brain lint` | validates against the schemas | errors, exit 1 |
| `brain reindex [--full]` | incremental or full rebuild | counts |
| `brain eval` | recall@8 and tokens per answer | two numbers, plus a diff against the last run |
| `brain migrate` | moves the system to a new host (B16) | a checklist |

Output rules:
- A list prints as a table with a `name[shown/total]` header and the id first. A single value prints as `key: value`. Long text is capped with `--full`.
- `--json` prints compact JSONL.
- Errors print as `error:` followed by `next:`. Exit 0 ok, 1 error, 2 usage.
- `--help` is the only place flags are documented. Setup text never appears in help shown at run time.

## Implementation

- The CLI is built with clap derive, plus `clap_complete` for shell completions. The command tree above is the whole surface, and every flag is documented only in `--help`. The goal is a CLI a person enjoys using and an agent can drive from `--help` alone.
- The Rust follows the `rust` skill (stable 1.99, edition 2024): types that make invalid states impossible, no panics in the library, let chains, `thiserror` errors. paperdb's lint set stays (`unwrap_used`, `expect_used`, `panic`, `indexing_slicing` denied, `unsafe_code` forbidden).
- The paperdb modules are reviewed against that skill before being renamed. The review and the rename are two separate diffs.
- Dependencies stay as few as possible: rusqlite, serde, serde_json, thiserror, clap, plus a JSON Schema validator for `lint`.

## Profile (C5)

`brain profile` runs nightly and on demand, with no LLM. It assembles:
- the 3 lines of focus from `pages/me.md`
- active projects: pages with `status: active` that were touched in the last 14 days
- today's committed plan items
- items that need you: drover `needs` events and an overdue `later`
- one line: "ask brain before guessing: `brain find`"

It's capped at 400 tokens, counted with a tokenizer approximation, and the cap is tested.

## Eval (B8)

`eval/questions.jsonl` holds `{q, expect: [id…]}`. `brain eval` runs `find` on each question and reports recall@8 and the mean output tokens per hit. Every real `find` is appended to `~/.local/state/brain/queries.jsonl`, and the eval set is grown from that file by hand. The results are logged per commit, and a retrieval change ships only if recall doesn't drop.

## feed (Python, `feed/`)

A uv project with one entry point, `feed run`, started by the 15-minute tick when it's due (S3).

**Pool (C6):** `~/.local/state/feed/pool.db`.

```sql
candidates(id PK, kind, title, url, abstract, sources JSON, first_seen, score, bucket, shown_at, expires_at)
cursors(source PK, cursor, ok_at)
```

**Sources** (each one switchable in `feed.toml`). Each source has a cursor, and a failed run leaves its cursor unchanged, so the next run backfills the gap (F1):
- arXiv listings for cs.RO, cs.LG, cs.CV and cs.AI, by `submittedDate` range
- HF `daily_papers?date=`
- S2 recommendations: 100 positives rotated per call, skips as negatives
- alphaXiv For You and Hot, over MCP with an API key
- lab RSS
- F10 news: web search through a backend interface (SearXNG by default), HN Algolia, Reddit JSON, GitHub trending. Queries are generated nightly from `profile.md` and active project pages.

**Dedupe:** by arXiv id without its version, then by canonical URL, then by normalized title (Jaccard ≥ 0.9). Anything already in the library or already in `labels.jsonl` is dropped.

**Ranker.**
- *Phase A* (14 days): the score is S2 rank plus `log(1 + hf_upvotes)`.
- *Phase B*: logistic regression `g(x) = σ(wᵀφ(x) + b)` on frozen embeddings of title and abstract.
  - Labels: positives are library papers and likes; explicit skips are negatives with weight 1; unlabeled pool items are background with weight 0.2.
  - Calibration (Elkan & Noto 2008): `c = E[g(x) | y = 1]`, estimated on held-out positives, and `p(y = 1 | x) = g(x) / c`.
  - Retrained nightly. Time-split AUC and like-rate go to the weekly review.
- News uses the same model on title and snippet, with its own slot.

**Selection:** from the unshown, unexpired pool, ranked by `score × decay(age)`:
- the top 15 papers
- 3 papers with `|p − 0.5|` smallest
- 2 trending papers from outside your cluster
- 5 news items

Each item records its `bucket`. Items not shown carry over. Items expire after 21 days, and a `later` comes back after 7.

**Summaries:** HF `ai_summary` or the S2 tldr when present. Otherwise the free-tier LLM writes the 5-field card for the selected items only, and only for kinds paper, rss and news (F6). A failure is recorded as `summary: failed`.

**Output:** the day's selection is written to `pool.db`, then one Web Push goes out with the count (F9).

## planner (`planner/`)

These are prompt files, not code (P7):
- `plan.md` (07:30): reads `brain`, both calendars, `ocu tasks --jsonl`, `gh` issues and PRs, `drover ls`, and yesterday's log.
- `shutdown` (18:30): deterministic, written in Rust, as `brain shutdown`. It reads commits and PRs across repos, task changes and drover runs, and writes them to `log/`.
- `review.md` (Sunday): estimated vs actual time, deep-work hours, rollovers, like-rate, AUC.

`brain plan checks` computes the challenge checks deterministically: free calendar time vs estimates, WIP above 3, items rolled over 3 or more times, no deep-work block. It prints them as `key: value` lines, which are pasted into the prompt.

A plan run is `drover run --headless --profile reader --prompt-file planner/plan.md`. Its output is a plan block written by `brain log --source plan`, and committing it (`brain plan commit`) marks the items.

## Migration from paperdb (B10)

1. `brain migrate-paperdb ~/papers` turns each `papers/<id>/paper.json` into `docs/<id>/meta.json` with `kind: paper`.
   - The card fields are kept under `meta.card` (still validated by the paper schema).
   - `links[]` is classified into `links{code, project, …}` by host.
   - `starred` becomes a tag.
2. `paper.md` becomes `text.md`, and the PDFs are moved from `.cache/pdf`.
3. The drover journal (`journal.db`, 13 entries) becomes `log/` blocks with their original dates.
4. `readings/` (9 files) becomes `docs` with `kind: book`, and text extraction is reused from paperdb's ingest chain.
5. `inbox.jsonl` is folded into the feed pool, and its skips into `labels.jsonl`.
6. The old `~/papers` stays read-only for 30 days, then is deleted.

Acceptance: 327 papers, 13 entries and 9 books carried over, and `brain lint` is clean.

## Portability (B16)

The whole system lives in `~/brain`, `~/.local/state`, `~/.config/brain` and the dotfiles. `brain migrate` prints and checks the procedure:
1. restic restore
2. `dot`
3. `brain reindex --full`
4. `systemctl --user enable --now brain-tick.timer`
5. `brain eval`, compared with the last score

## Storage

| Path | Holds | Backup |
|---|---|---|
| `~/brain` | the vault | restic nightly, local git for history |
| `~/.cache/brain/index.db` | the index | none, rebuildable |
| `~/.local/state/feed/pool.db` | candidates, cursors | restic |
| `~/.local/state/brain/queries.jsonl` | real queries for the eval | restic |
| `~/.local/state/context.json` | C15 current timezone and place | none |
| `~/.config/brain/` | `feed.toml`, credentials (mode 600) | dotfiles (age) |

## Non-goals

- No model calls inside the `brain` binary.
- No MCP server.
- No claims table, open loops, reranker, contextual-retrieval prefixes or code embeddings until `brain eval` shows a failure that one of them fixes.
- No storing of skipped candidates beyond their label.
- No git remote for the vault.

## Decided

- brain is paperdb renamed, with the ingest chain kept (D2).
- Rust for the store and CLI, Python for the feed (D3).
- Brute-force vectors in BLOBs, no sqlite-vec.
- Files win: any disagreement between a file and the index is resolved by reindexing.
- The feed is a persistent pool with cursors per source, never a daily rebuild.

## Open (design level)

- Chunk size, 400 vs 800 tokens. Settled by the eval.
- Embedder default: Qwen3-Embedding-0.6B on CPU vs 4B on an idle GPU. Settled by the eval.
