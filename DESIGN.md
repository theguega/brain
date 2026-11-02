# Design

brain is a knowledge layer for one person and their agents: a vault of plain files, an
index rebuilt from those files, one CLI, and one agent skill. It calls no language model.
The vault is the truth; everything else can be rebuilt from it.

## Principles

1. **Files are the truth.** Docs, wiki pages, the log and labels are markdown and JSON in a
   local git repo. SQLite is only an index, and `brain reindex --full` rebuilds it.
2. **Only what you mark.** Nothing enters the vault unless you add it. A rejected
   candidate is stored only as a label.
3. **Stores don't think.** brain fetches, parses, indexes and checks. Judgment (writing
   wiki pages, planning) happens in agent runs outside the binary, and their output comes
   back through brain's checks.
4. **Token budget is a spec.** The profile a session loads is capped at 400 tokens, the
   skill at 1,500, and long output is cut behind `--full`. The caps are tested.
5. **Untrusted text is data.** Anything from outside is stored as `taint: external` and
   printed inside a fence, so an injected instruction is never a bare line.
6. **Evidence before features.** Rerankers, claim tables and code summaries wait until
   `brain eval` shows a failure one of them fixes.

## Layers

| Layer | Owns | Where |
|---|---|---|
| vault | docs, pages, log, labels, profile, eval questions | `~/brain` (git, no remote) |
| index | chunks, FTS5, vectors, links, repos | `~/.cache/brain/index.db` |
| CLI + skill | the agent interface | `brain`, `skill/SKILL.md` |
| contracts | the file and output formats | `contracts/*.schema.json` |
| state | job and eval history, queries | `~/.local/state/brain/` |

## Contracts

Each format other programs read is a JSON Schema in `contracts/`, with a `version` field.
Changes only add; a breaking change bumps the version. The code names them by id.

| Id | Contract | Shape |
|---|---|---|
| C1 | Doc | `docs/<dir>/<dir>.md`: YAML frontmatter validated against `contracts/kinds/<kind>.schema.json`, the full text as body |
| C2 | Page | `pages/<slug>.md`: frontmatter `{kind, title, summary, status, sources[], updated, superseded_by?, aliases?, tags?}` and a markdown body |
| C3 | Log entry | `log/YYYY-MM-DD.md`, one `## <ISO time> · <IANA tz> · <place> · <source> · <title>` block per entry |
| C4 | CLI output | lists, `key: value`, `--json` JSONL, `error:`/`next:`, exit 0/1/2 (see CLI) |
| C5 | Profile | `profile.md`, at most 400 tokens |
| C7 | Label | `labels.jsonl` `{id, verdict: like\|skip\|later, ts, surface}` |
| C10 | Job status | `jobs.jsonl` `{job, started, ended, ok, err}` |
| C13 | Repo | `{name, path, remote, project}` from `repos.toml` |
| C15 | Context | `~/.local/state/context.json` `{tz, place?, updated_at, source}` |

## Vault

```text
~/brain/                      local git, no remote
  docs/<dir>/<dir>.md         C1: one note per doc
  docs/<dir>/source.<ext>     an original that can't be re-fetched (a book's PDF or epub)
  docs/<dir>/index.html       an artifact's page
  pages/<slug>.md             C2: the wiki
  log/YYYY-MM-DD.md           C3: decisions, results, journal, compile records
  labels.jsonl                C7: verdicts
  profile.md                  C5: regenerated nightly
  repos.toml                  C13: roots and include/exclude globs (optional)
  eval/questions.jsonl        the eval set
```

- brain is the only writer of docs, the log and the profile. Pages are written by you, or
  arrive on a `wiki/*` branch that reaches `main` only through `brain wiki merge`.
- A write goes to a temp file and is renamed into place, under a vault lock, then committed
  to local git debounced to one commit a minute (synchronously when run by systemd).
- Nothing in the vault holds an absolute path or a hostname, so it moves between machines
  as is.

### Obsidian-native

The vault opens as an Obsidian vault, and its graph view is brain's graph.

- **One note per doc.** `docs/arxiv--2410.24164/arxiv--2410.24164.md`: the directory name
  is the id with `:` replaced by `--`, so `[[arxiv--2410.24164]]` resolves by file name.
  `aliases` holds the title and short handles (`π0`).
- **One link syntax.** A wikilink to a doc is a citation, to a page a cross-reference. A span
  is a heading: `[[arxiv--2410.24164#Method]]`, the same span `brain get` takes.
  `[[dir|label]]` works everywhere.
- **Typed relations are frontmatter properties** with quoted wikilink values:
  `builds_on: ["[[arxiv--2504.16054]]"]`, `superseded_by`, `attendees`, `project`, or any
  name `brain link` writes. The index reads the property name as the relation.

## Kinds

A kind is a schema file; adding one needs no code change. `brain lint` validates every doc.

| Kind | Required metadata | Filled by |
|---|---|---|
| paper | title, authors, date, `links{code, project, hf, openreview, video}` | arXiv, Hugging Face, alphaXiv, Papers with Code |
| book, course | title, authors, year, `links{home}` | the file's metadata, then you |
| blog, news | title, url, site, date | page metadata |
| talk | title, speakers, date, `links{video, slides}` | you |
| note | title | the file |
| artifact | title, why, source, captured_at | `brain add page.html --why` |
| meeting | started_at, tz, place, attendees → pages, project? | the file's frontmatter |

`taint` follows from the kind: `note` and `meeting` are `own`, everything else `external`.

**Lineage.** For papers, `add` reads Papers with Code and stores predecessors as the
paper's own `builds_on`; a successor already in the vault gets the edge on its note. So
`builds_on` always points from the later paper to the earlier one. If the service is
down, the next `reindex` retries.

## Ids

Ids are derived from the source, so an id never disagrees with its content:
`arxiv:2410.24164` (version dropped), `web:<slug>`, `file:<sha256[:8]>`,
`meeting:<date>-<slug>`, `artifact:<slug>`.

A `web:` slug comes from the canonical URL: `https`, lowercase host without `www.`, path and
query without tracking parameters (`utm_*`, `fbclid`, `gclid`, `mc_cid`, `mc_eid`, `ref`,
`ref_src`), no fragment, no trailing `/`. A slug over 64 characters, or one another source
already holds, ends in `-<sha256[:6]>` of the canonical URL (web) or the HTML bytes
(artifact). Adding the same source again finds it and changes nothing.

## Time

Every timestamp is ISO 8601 with its offset, plus the IANA zone and a place, read from C15
at capture time, falling back to the system zone. Day boundaries use your current zone.

## Index

```sql
docs(id PK, kind, title, date, meta JSON, hash, taint)
chunks(id PK, doc, ord, heading, text)
chunks_fts USING fts5(heading, text, tokenize='porter unicode61')
vectors(chunk PK, model, dim, v BLOB)       -- f16, L2-normalized
links(src, dst, rel)                        -- indexed on src and dst
repos(name PK, path, remote, head, indexed_at)
PRAGMA user_version                         -- a mismatch drops and rebuilds
```

- **Freshness.** Each file's hash is compared with `docs.hash`; only changed docs are
  re-chunked and re-embedded. Deleting `index.db` and reindexing gives identical `find`
  output, and a golden test checks it.
- **Chunking.** Split on headings, about 400 tokens with one paragraph of overlap. Tables
  stay whole.
- **Vectors.** From any OpenAI-compatible `/v1/embeddings` server (default
  `qwen3-embedding:0.6b` on Ollama), stored as f16 and searched by brute-force cosine:
  10⁵ chunks × 1024 dims is a few milliseconds. No sqlite-vec, which would need `unsafe`.
  Without an embedder, search runs on BM25 alone and says `degraded: no vectors`.
- **Ranking.** BM25 (title weighted) and cosine, fused with reciprocal rank fusion,
  `score(d) = Σ_r 1/(60 + rank_r(d))`, then collapsed to the best chunk per doc. A title
  phrase match gets a boost; an exact page alias goes straight to that page.
- **Links.** Built only from frontmatter relations and from body wikilinks in own-taint
  notes (rel `cites` for a doc, `mentions` for a page). External text adds no edges.

## Repos

`repos.toml` lists roots (default `~/Developer`) and include/exclude globs; every git
checkout under a root is a repo. The index holds each repo's markdown, its commit messages
since the last indexed head, and a repo card. Code is never copied: `brain code` runs
`rg` live across the repos, capped at 20 hits and 10 seconds.

## CLI

| Command | Does |
|---|---|
| `brain` | prints the profile |
| `brain find <q>` | hybrid search over docs, pages, the log and repos |
| `brain get <id>[#span]` | a doc, page, log block or repo file, capped at 1,000 chars |
| `brain code <regex>` | live ripgrep across repos |
| `brain add <arxiv\|url\|file>` | ingest, idempotent; reports missing metadata |
| `brain log <text>` | appends a C3 block: first line title, rest body |
| `brain label <id> like\|skip\|later` | appends C7; `like` also adds |
| `brain link <page> <target> <rel>` | adds a typed relation to a page |
| `brain pages` | the wiki catalog |
| `brain wiki queue\|ls\|diff\|propose\|merge\|drop` | the wiki review loop |
| `brain lint` | schemas plus wiki checks; exit 1 on errors |
| `brain reindex [--full]` | brings the index in line with the vault |
| `brain eval` | recall@8 and tokens per hit |
| `brain profile` | regenerates the profile |
| `brain tick [--dry-run]` | runs every due job once |
| `brain migrate` | checks the steps for moving to a new host |

Output rules (C4): a list prints `name[shown/total]{columns}:` with the id first; a single
value prints `key: value`; `--json` prints JSONL; errors print `error:` then `next:`; exit
codes are 0 ok, 1 error, 2 usage. `--help` is the only place flags are documented.

## Jobs

`brain tick` is the only scheduler; a systemd timer runs it every 15 minutes. Each job is
**cron** (a fixed local time), **lazy** (a cheap `due` test, runs only when there is work),
or **event** (inside the command that caused it). Every run appends a C10 line, and the
tick one per tick, so a dead timer shows as stale.

| Job | Kind | Runs when | Does |
|---|---|---|---|
| reindex | lazy | the vault's HEAD moved | incremental reindex, lineage retries |
| enrich | lazy | a paper lacks its `hub:` or `axiv:` block | backfills 25 papers per tick |
| embed | lazy | chunks lack vectors and the GPU is free | fills vectors, 10 minutes per tick |
| repos | lazy | a repo's HEAD moved | indexes its markdown, commits and card |
| eval | lazy | retrieval code or the eval set changed | recall@8 and tokens, logged per build |
| profile | cron | 03:00 | regenerates `profile.md` |
| merge | event | `brain wiki merge` | reindexes the pages, logs a compile block |

Jobs that need a model (compile, learn, wiki-lint, plan, review) are agent runs started
through an agent runtime; until one is configured, the tick prints them as skipped. GPU
jobs skip while another process holds more than 2 GB of GPU memory (unless
`BRAIN_EMBED_DEVICE=cpu` says the embedder never touches the GPU), and a failed job backs
off for an hour without stopping the others.

## Profile

`brain profile` assembles, with no model: three lines of focus from `pages/me.md`, active
project pages updated in the last 14 days, today's plan items, items that need you, and one
line reminding agents to search first. It is capped at 400 tokens by a tokenizer
approximation.

## Wiki

The wiki follows Karpathy's LLM Wiki: instead of re-deriving answers from raw docs, an agent
compiles what you mark into pages that hold the current synthesis. brain stays model-free:
it queues, checks, stores and merges pages; the writing happens in agent runs.

| Page kind | Holds | Citations |
|---|---|---|
| person, project, decision | what it says | optional |
| concept | a method or result across docs | required |
| synthesis | an answer worth keeping | required |

**Citations.** In concept and synthesis pages, every paragraph and list item outside
`## Open questions` cites a doc as `[[dir]]` or `[[dir#heading]]`, and every cited id is in
`sources[]`. A summary of a summary is caught, because each claim points back to a doc.

**The loop.**
1. `brain wiki queue` lists marked docs no merged compile covers and no open branch holds.
2. An agent run reads the doc and related pages and replies with one `=== page: <slug>`
   block per page, writing no file.
3. `brain wiki propose <doc>` lints the reply against the vault as it would be, and commits
   the pages to `wiki/<doc>` through a temporary worktree. The vault's checkout is never
   touched.
4. You read it with `brain wiki diff` and land it with `brain wiki merge`, which merges only
   `pages/`, re-lints, reindexes and writes a `source: compile` log block. On a conflict
   nothing lands: a doc's branch is dropped and the doc re-queued, a learn or lint branch
   is kept for you to rebase or drop. `brain wiki drop` discards a branch.

A learn run adds `=== question: <slug>` blocks: the question the corrected page answers,
carried as a `Brain-Question:` commit trailer and appended to the eval set on merge. A later
retrieval change that loses the corrected answer then fails `brain eval`.

**Lint.** Errors: dead links or citations, uncited paragraphs, cited ids missing from
`sources[]`, an ambiguous alias, a page over 1,500 tokens. Warnings: orphan pages, open
contradictions older than 14 days, active pages untouched for 90 days, unknown frontmatter
keys. There is no generated `index.md`; `brain pages` is the catalog.

## Eval

`eval/questions.jsonl` holds `{q, expect: [id…]}`. `brain eval` runs `find` on each and
reports recall@8 and mean output tokens per hit, compared with the last run. Every real
`find` is logged to `queries.jsonl`, the pool new questions come from. A retrieval change
ships only if recall holds.

## Portability

The system is `~/brain`, `~/.local/state`, `~/.config/brain` and your dotfiles. `brain
migrate` checks the move: restore the backup, set up the machine, `brain reindex --full`,
enable the timer, and compare `brain eval` with the last score. Other machines run brain on
the host over ssh through `bin/brain-ssh`, which `brain machine add` copies with the skill.

## Non-goals

- No model calls inside the binary.
- No MCP server.
- No git remote for the vault.
- No derived files in the vault.
- No page edit lands without a merge you made.
- No reranker, claim table or code embeddings until the eval asks for one.

## Open questions

- Chunk size, 400 vs 800 tokens.
- Embedder: a small model on CPU vs a larger one on an idle GPU.
- Whether `find` should rank concept pages above the docs they cite.

Each is settled by the eval.
