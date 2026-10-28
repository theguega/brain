# brain — design

brain is the knowledge layer: a vault of plain files, an index rebuilt from those files, one CLI, and one skill. It calls no model. The vault is the truth, and everything else can be rebuilt from it.

The repo is paperdb renamed (D2). It holds the Rust crate, `feed/` (Python, the paper and news feed), `planner/` (prompt files), and `contracts/` (JSON Schemas for C1–C16).

Status: design, round 3. Requirements B1–B20, F1–F10, P1–P10.

## Layers

| Layer | Owns | Language |
|---|---|---|
| vault | `~/brain`: docs, pages, log, chats, labels, profile | files |
| index | `~/.cache/brain/index.db`: chunks, FTS5, vectors, links | Rust (rusqlite) |
| CLI + skill | the agent contract (C4, C5) | Rust |
| feed | candidate pool, ranker, daily selection (C6, C7) | Python (uv) |
| planner | prompt files and deterministic checks, run through drover | files + Rust |
| contracts | `contracts/*.schema.json`, golden tests | JSON Schema |

## Jobs: cron, lazy, event (B20)

The brain serves three goals, and every job exists for at least one of them:
- **Learn:** keep learning and see your progress. What you read becomes cited wiki pages, and your corrections make it better.
- **Find:** everything you know is searchable from one CLI.
- **Plan:** the day is proposed, captured and reviewed.

Jobs come in three kinds, so work happens when it's useful and nothing runs just because it's time:
- **cron:** a fixed local time (C15), for things you expect at a moment of the day.
- **lazy:** checked by `brain tick` every 15 minutes with a cheap `due` test (a file hash, a queue length, a label count). It runs only when there is work, and does nothing otherwise.
- **event:** runs right away, inside the command that caused it.

| Job | Kind | When | Does | Model | Serves |
|---|---|---|---|---|---|
| feed | cron | 06:00 | fetch, dedupe, rank, select (F1–F10) | free tier, summaries only | Learn |
| plan | cron | 07:30 weekdays | plan proposal (P1) | reader run | Plan |
| shutdown | cron | 18:30 | commits, PRs, tasks, runs → `log/` (P5) | none | Plan, Learn |
| profile | cron | 03:00 | regenerate `profile.md` (C5) | none | Find, Plan |
| review | cron | Sunday 18:00 | the week: time, focus, progress (P6) | reader run | Plan, Learn |
| learn | cron | Sunday, before review | corrections → proposals and eval questions (P10) | reader run | Learn, Find |
| wiki-lint | cron | Sunday, before review | semantic wiki check (P9) | reader run | Find |
| backup | cron | 02:00 | restic of the vault and state (S4) | none | — |
| reindex | lazy | the vault's HEAD moved since the last index | incremental reindex (B1) | none | Find |
| embed | lazy | chunks without vectors, and the GPU is free (S6) | fill vectors (B12) | local embedder | Find |
| repos | lazy | a registered repo's HEAD moved (C13) | index its markdown, commits and card | none | Find, Plan |
| compile | lazy | `wiki queue` not empty, 22:00–06:00, ≤5 a night | doc → wiki proposal (P8) | reader run | Learn, Find |
| ranker | lazy | ≥20 new labels since the last training, once a day at most | retrain and calibrate (F4) | none | Learn |
| news queries | lazy | `profile.md` changed | regenerate the F10 queries | free tier | Learn |
| eval | lazy | the retrieval code or `eval/questions.jsonl` changed | recall@8 and tokens (B8) | none | Find |
| like | event | `brain label <id> like` | `add`, then queue for compile | none | Learn |
| merge | event | `brain wiki merge` | reindex the changed pages, log, add eval questions | none | Learn, Find |
| run done | event | `drover done` | journal entry → `brain log` | none | Plan, Learn |

Rules:
- `brain tick` is the only scheduler: one systemd timer runs it every 15 minutes. It decides what's due and starts reader runs through drover. It calls no model itself.
- Night and GPU windows keep the model jobs off your working hours and off training runs.
- Every job that runs appends a C10 line. The tick also appends one line per tick, so a dead tick shows as stale even when no lazy job had work.

## Vault

```text
~/brain/                      local git, no remote (Q15)
  docs/<id>/meta.json         C1: kind, source, title, required metadata per kind
  docs/<id>/text.md           full text (artifact: index.html; text extracted at index time)
  pages/<slug>.md             C2: the wiki: people, projects, decisions, concepts, syntheses
  log/YYYY-MM-DD.md           C3: journal, plan, shutdown, chat summaries
  chats/<id>.jsonl            raw chat transcripts (normalized C9 events)
  labels.jsonl                C7: feed verdicts
  profile.md                  C5: regenerated nightly, ≤400 tokens
  repos.toml                  C13: roots, include/exclude globs
  eval/questions.jsonl        B8: the 50 real questions
  private/                    personal material (indexed like the rest)
```

Rules:
- One writer per file type. `brain` writes docs, the log and the profile. `feed` writes `labels.jsonl` only through `brain label`. Pages are written by you, or by `brain wiki propose` on a branch that reaches `main` only through `brain wiki merge` (see Wiki).
- A write goes to a temp file and is then renamed (`write_atomic`, kept from paperdb), and is committed to local git in the background, debounced to one commit per minute.
- Nothing in the vault holds an absolute path or a hostname (B16).

## Kinds

Each kind is a JSON Schema in `contracts/kinds/<kind>.schema.json`. Adding a kind adds a file and needs no code change (C1). `brain lint` validates every doc against its schema.

| Kind | Required metadata | Filled by |
|---|---|---|
| paper | title, authors, date, venue?, `links{code, project, hf, openreview, video}` | arXiv, HF Hub (`githubRepo`, `projectPage`), alphaXiv, Papers with Code (`tldr`, lineage) |
| book · course | title, authors, year, `links{home}` | you, or the PDF's metadata |
| blog · news | title, url, site, date | page metadata (Jina Reader) |
| talk | title, speakers, date, `links{video, slides}` | you |
| note | title | you |
| artifact | title, why, source, captured_at | `brain add page.html --kind artifact --why` |
| meeting | started_at, tz, place, attendees[] → pages, project? | you, by paste or file |

**Lineage.** For papers, `add` also reads Papers with Code (`paperswithcode.co/api/v1/papers/<arxiv>/`) and stores its `predecessor_papers` and `successor_papers` as links with `rel: builds_on`, whether or not the other paper is in the library. Compile uses them to order a concept page's history (π0 → π0.5) instead of guessing it. PwC's code links, tasks and methods are not used: on π0 they are empty, too coarse, or wrong. If PwC is down, the paper just has no lineage yet, and the next `reindex` retries.

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
| `brain pages [--kind]` | the wiki catalog: one line per page | `pages[n/total]{slug,kind,updated,summary}` |
| `brain wiki queue` | marked docs no compile has covered yet | `queue[n/total]{id,kind,added}` |
| `brain wiki ls` · `diff <branch>` | open `wiki/*` branches; one branch's page diff | `branches[n/total]{branch,doc,pages,created}` · the diff |
| `brain wiki propose <doc-id\|lint>` | reads a run's page blocks on stdin, lints them, commits them to `wiki/<doc-id>` | `branch`, `pages`, or lint errors, exit 1 |
| `brain wiki merge <branch>` · `drop <branch>` | lands or discards a proposed page edit | `merged`, `pages`, `conflict?` |
| `brain lint [--vault <dir>]` | schemas, plus the wiki checks | errors and warnings, exit 1 on errors |
| `brain reindex [--full]` | incremental or full rebuild | counts |
| `brain eval` | recall@8 and tokens per answer | two numbers, plus a diff against the last run |
| `brain tick [--dry-run]` | runs every due job once (see Jobs) | `ran[n]{job,ok,took}`; `--dry-run` prints what is due |
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

## Wiki (C2, B17–B19)

Karpathy's LLM Wiki pattern (gist 442a6bf5, 2026): instead of re-deriving answers from raw docs at every query, an agent compiles what you mark into pages that hold the current synthesis, so knowledge accumulates. Docs are the immutable sources; pages are the wiki; the compile and lint prompts are its schema. brain stays model-free: it stores, checks and merges pages, and the writing happens in drover runs.

**Pages.** Frontmatter `{kind, title, summary, status, sources[], updated, superseded_by?}`. `summary` is one line of at most 120 chars, and is what `brain pages` prints. Kinds:

| Kind | Holds | Citations |
|---|---|---|
| person · project · decision | as before | optional |
| concept | a method, idea or result across papers: "diffusion policy", "JEPA", "action chunking" | required |
| synthesis | an answer worth keeping: a comparison, a chat answer, a reading of several docs | required |

**Citations.** A claim cites its source inline as `[@<doc-id>]` or `[@<doc-id>#<span>]`, the same span syntax as `brain get`. In a concept or synthesis page, every paragraph and list item outside `## Open questions` carries at least one citation, and every cited id is in `sources[]`. This is the guard against drift: a summary of a summary is caught, because each claim must point back to a doc you can open.

**Contradictions.** When a new doc disagrees with a page, compile keeps both claims and adds a `> [!contradiction]` callout citing both. You resolve it by editing the page; open callouts are counted in the Sunday review.

**Compile.** A like or `brain add` puts the doc in `brain wiki queue` (marked docs minus those listed on the `docs:` line of a `source: compile` log block, and minus those with an open `wiki/<doc>` branch, so dropping or conflicting a branch re-queues its doc). The tick starts one ordinary `reader` run per queued doc, at most 5 a night, with `planner/compile.md`. Each run:
1. reads the doc and `brain pages`, and `brain find`s related pages
2. replies with the full new text of each page it creates or changes, one `=== page: <slug>` block per page; it writes no file

The tick pipes that reply into `brain wiki propose <doc-id>`, which lints the pages and commits them to branch `wiki/<doc-id>` through a temporary worktree under `~/.cache/brain/`, leaving the vault's checkout untouched. On lint errors the tick retries the run once with the errors appended to the prompt, then gives up and logs it. So the agent never writes to the vault, brain stays its only writer, and drover needs no wiki-specific profile.

Marking a doc is your approval to compile it. Merging is your approval to publish it. `brain wiki merge` does a git merge into the vault's `main`, reindexes the changed pages, and writes one C3 block `source: compile` listing the doc ids and pages. A conflict aborts the merge, drops the branch and re-queues the doc, so the next run starts from the merged state.

**Filing answers.** From a chat thread, "file as page" starts a compile run with the thread as input and a `synthesis` page as the target. The same review applies.

**Learning from corrections.** This follows Meta's "organizational second brain" (Meta Engineering, 2026-09-02), scaled down to one person. When you correct an agent ("no, that's wrong", or redirecting a run), the correction is a training signal that today is lost in a transcript. `planner/learn.md` is a weekly reader run over the week's normal chat threads (`chats/`) and drover run transcripts, in two separate steps:
1. **Extract** each substantive correction, with the brain ids the agent had loaded (its `find` and `get` calls).
2. **Attribute** it with one test: could the agent have reached the right answer from what it loaded?
   - No, the knowledge was missing or wrong: a page edit, through `brain wiki propose learn`.
   - Yes, the procedure was wrong: a suggested edit to a prompt, skill or AGENTS.md, listed in the review for you to apply.
   - The sources disagree, or it's a judgment call: listed in the review as ambiguous, with no proposal.

A reader can't call drover, so the tick gathers its input first: the week's `chats/` threads, plus each run's text and reply events (C9) exported from drover, written into the run's cwd.

Classifying by the wording of the correction fails, because one remark can be any of the three; Meta found the same. Incognito threads are never read.

**Regression.** Each merged `learn` proposal carries its question: the reply adds a `=== question: <slug>` block after the page that answers it, and `propose` stores it as a `Brain-Question: {"q", "expect"}` trailer on the branch's commit. The merge appends `{q, expect: [page-id], from: correction}` to `eval/questions.jsonl`. `brain eval` then fails any later retrieval change that loses the corrected answer. The check is a deterministic recall test, with no LLM judge.

**Lint.** `brain lint` adds deterministic wiki checks to the schema checks:
- errors: dead `[[wikilinks]]` or citations, uncited paragraphs in concept or synthesis pages, cited ids missing from `sources[]`, a missing `summary`, a page over 1500 tokens (split it: a page should load whole, which is the progressive-disclosure lesson)
- warnings: orphan pages (no inbound link), open contradictions older than 14 days, `status: active` pages with no update in 90 days

The semantic pass is `planner/wiki-lint.md`, a weekly `reader` run that looks for stale claims, missing concepts and missing cross-links; its reply goes through `brain wiki propose lint` onto a `wiki/lint-<date>` branch.

**Why no `index.md`.** The gist keeps a generated catalog file for the agent to read first. Here that is `brain pages`: a derived file would duplicate the frontmatter in the vault (principle 2), and the CLI scales past the few hundred pages where a flat catalog stops fitting a context window.

## Eval (B8)

`eval/questions.jsonl` holds `{q, expect: [id…]}`. `brain eval` runs `find` on each question and reports recall@8 and the mean output tokens per hit. Every real `find` is appended to `~/.local/state/brain/queries.jsonl`, and the eval set grows two ways: by hand from that file, and automatically from every merged correction (see Wiki, Regression). The results are logged per commit, and a retrieval change ships only if recall doesn't drop.

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

**Summaries:** HF `ai_summary`, the Papers with Code `tldr` or the S2 tldr when present. Otherwise the free-tier LLM writes the 5-field card for the selected items only, and only for kinds paper, rss and news (F6). A failure is recorded as `summary: failed`.

**Output:** the day's selection is written to `pool.db`, then one Web Push goes out with the count (F9).

## planner (`planner/`)

These are prompt files, not code (P7):
- `plan.md` (07:30): reads `brain`, both calendars, `ocu tasks --jsonl`, `gh` issues and PRs, `drover ls`, and yesterday's log.
- `shutdown` (18:30): deterministic, written in Rust, as `brain shutdown`. It reads commits and PRs across repos, task changes and drover runs, and writes them to `log/`.
- `review.md` (Sunday), in two parts:
  - **Plan:** estimated vs actual time, deep-work hours, rollovers.
  - **Progress:** docs marked, concept pages created or changed (linked, so you can reread what you learned), contradictions opened and resolved, corrections learned and the procedure fixes waiting for you, recall@8 trend, like-rate, AUC.
  - Counts come from `brain review stats`, which is deterministic; the prompt only writes them up.
- `compile.md` (per queued doc), `wiki-lint.md` and `learn.md` (weekly): `reader` runs whose replies `brain wiki propose` turns into a branch (see Wiki).

`brain review stats` counts the week from the log, `git log` of the vault, `labels.jsonl` and the eval history, and prints `key: value` lines.

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
- No generated `index.md` or other derived file in the vault; `brain pages` is the catalog.
- No page edit lands on `main` without a merge you made.

## Decided

- brain is paperdb renamed, with the ingest chain kept (D2).
- Rust for the store and CLI, Python for the feed (D3).
- Brute-force vectors in BLOBs, no sqlite-vec.
- Files win: any disagreement between a file and the index is resolved by reindexing.
- The feed is a persistent pool with cursors per source, never a daily rebuild.
- The wiki follows Karpathy's LLM Wiki, with two additions: every claim cites a doc, and every agent edit is a branch you merge.

## Open (design level)

- Chunk size, 400 vs 800 tokens. Settled by the eval.
- Embedder default: Qwen3-Embedding-0.6B on CPU vs 4B on an idle GPU. Settled by the eval.
- Whether `find` should rank concept pages above the docs they cite. Settled by the eval once 20 concept pages exist.
