# Second brain: requirements

Settled in round 2 (2026-10-19). Every requirement has an acceptance check. MUST means it's in the first build; SHOULD means it's built if cheap, otherwise deferred. The contracts (C1–C16) are in `ARCHITECTURE.md`.

## S · Systems & layout

The ground every other part runs on. It goes first because the brain migration needs the new layout.

- **Done when:** `systemctl --user list-timers` shows every job, a failed job reaches your phone, and the restore from backup is tested once.
- **Non-goals:** No orchestrator, no containers, no HA. If meerkat is down, the files in git are the recovery path.

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| S1 | MUST | Layout: `~/Developer` holds only code, `~/brain` the vault, `~/.local/state/<tool>` runtime state, `~/.config/<tool>` config and credentials, `~/.cache/<tool>` anything rebuildable. | No runtime file inside any git checkout (`git status --ignored` is clean of state). |
| S2 | MUST | Each binary is pinned to a release tag (mise), not built from `branch:main`. | `mise ls` shows tags only. |
| S3 | MUST | Every scheduled job is a systemd user timer. Jobs set in local time (feed 06:00, plan 07:30, shutdown 18:30) follow your current timezone (C15): one 15-minute tick runs whatever is due, once per local day. Each run appends a C10 line to `jobs.jsonl`, and a failure triggers `OnFailure=` → push. | Fly from SF to Paris, and the next plan arrives at 07:30 Paris time, exactly once. |
| S4 | MUST | Backup: nightly restic of `~/brain` and `~/.local/state` to a disk you own. The vault has no git remote: Opalin data, ClickUp and repos can be re-fetched. | A restore into a fresh directory followed by `brain reindex` gives identical `brain find` results. |
| S5 | MUST | One secrets pattern: age-encrypted in dotfiles → `~/.config/<tool>/credentials` at mode 600, loaded with systemd `LoadCredential=`. | `grep -r` finds no token in any repo or `.env`. |
| S6 | MUST | GPU jobs run with `Nice=19` and skip their run when training holds more than 2 GB of GPU memory. | The embedding job logs `skipped: gpu busy` during a training run. |
| S8 | SHOULD | One-time cleanup: prunable worktrees, stale `DASHBOARD_*` keys, the ~56 empty session dirs, the stray patch in `~`. | Checklist closed. |

## B · brain

The knowledge layer: vault files, a rebuildable index, one CLI and one skill. Built from paperdb (D2).

- **Done when:** The 327 papers, the drover journal and readings/ are migrated. `brain eval` reports recall@8 and tokens per answer on 50 real questions.
- **Non-goals:** No LLM calls inside brain, no MCP server, no claims table, no reranker, no code embeddings (Q12).

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| B1 | MUST | `brain reindex` rebuilds `index.db` from `~/brain` alone. Files win over the database. | Golden test: delete the db, reindex, and `find` returns identical results. |
| B2 | MUST | `brain add <arxiv\|url\|pdf\|file> [--kind]` is idempotent and fills the C1 required metadata from arXiv, HF Hub (`githubRepo`, `projectPage`) and alphaXiv, plus paper lineage (`builds_on` frontmatter wikilinks) and `tldr` from Papers with Code. | Adding the same arXiv id twice changes nothing; π0 gets its code and project links, and a `builds_on` link to π0.5. |
| B3 | MUST | `brain find <q>` is hybrid search: BM25 plus vectors fused with RRF at k=60. Default 8 rows, each ≤160 chars of clean snippet. Filters: `--kind --since --repo`. | Output follows C4. No highlight markup or broken tables in snippets. |
| B4 | MUST | `brain get <id>[#span]` returns at most 1000 chars by default, with `--full` for everything. | A span by heading or chunk number returns only that section. |
| B5 | MUST | Text from external sources is stored with `taint: external`, and `find`/`get` print it fenced as data. | An injected instruction in a stored abstract appears inside the fence, never as a bare line. |
| B6 | MUST | `brain log` appends a C3 block. `brain profile` regenerates `profile.md` (≤400 tokens) deterministically, with no LLM. | SessionStart hook output ≤400 tokens (counted). |
| B7 | MUST | Repo source (C13): roots and include/exclude globs are configurable. brain indexes each repo's markdown, commit messages and repo card. `brain code <q>` runs rg live across repos. | A hit in drover and a hit in opalin both come back from one `brain code` call. |
| B8 | MUST | `brain eval` scores `eval/questions.jsonl` on recall@8 and tokens per correct answer. Every real `find` query is captured as an eval candidate, and every merged correction (P10) adds a question automatically. | Running it prints both numbers, and they're tracked across commits. |
| B9 | MUST | `brain lint` validates every doc and page against its schema and lists missing required metadata. | Exit 1 on any invalid file, run in CI. |
| B10 | MUST | Migration: paperdb records become `docs/` (the card moves into the paper schema), `journal.db` becomes `log/`, and `readings/` become `docs` with `kind=book`. | Counts match: 327 papers, 13 journal entries, 9 books. |
| B11 | MUST | The vault is a local git repo with no remote; git gives the history that supersession relies on. | `git -C ~/brain remote` prints nothing. |
| B13 | MUST | Artifacts: `kind: artifact` keeps the HTML pages you find worth keeping, with `brain add page.html --kind artifact --why ...`. Stored as `docs/<id>/index.html` plus metadata (title, source, tags, why, captured_at). The text is extracted for search, and the Library renders the page in a sandbox. | This review page, once added, is found by `brain find contracts` and opens rendered. |
| B14 | MUST | Meetings: `kind: meeting`, only the ones you mark. Metadata: `{started_at, tz, place, attendees → people pages, project, source}`. Added with `brain add notes.md --kind meeting`, or pasted in the dashboard. | A meeting links to its attendees' pages and its project. |
| B15 | MUST | Time follows you: every timestamp is stored as UTC plus the IANA timezone and place at capture time (C15). The current context is set by the dashboard when opened (browser timezone, optional coarse location) and by the SessionStart hook on your laptop (system timezone). The last writer wins. | A log entry written in Tokyo reads in Tokyo local time, even when viewed later from SF. |
| B16 | MUST | Portable host: the whole system lives in `~/brain`, `~/.local/state`, `~/.config` and the dotfiles. No absolute paths, no hostnames in data, and machine names only in config. One documented `migrate` procedure covers restic restore, `dot`, `brain reindex` and enabling the timers. | Move to a second machine in under 30 minutes, and `brain eval` gives the same scores. |
| B17 | MUST | Wiki pages (C2) of kind concept and synthesis cite a doc for every claim (`[[doc-id#heading]]`), list their citations in `sources[]`, and carry a one-line `summary`. `brain pages [--kind]` prints the catalog. | `brain pages --kind concept` lists every concept page with its summary. |
| B18 | MUST | `brain lint` adds wiki checks. Errors: dead links or citations, uncited paragraphs, citations missing from `sources[]`, a missing summary, a page over 1500 tokens. Warnings: orphans, contradictions open more than 14 days, active pages untouched for 90 days. | A paragraph without a citation in a concept page exits 1 and names the line. |
| B19 | MUST | `brain wiki queue` lists marked docs not yet compiled; `brain wiki ls` and `diff <branch>` show the open proposals. `brain wiki merge <branch>` merges an agent branch into the vault, reindexes the changed pages and logs a `source: compile` block; a conflict drops the branch and re-queues the doc. `brain wiki drop <branch>` discards it. | Merging two branches that edit the same page: the first lands, the second's doc is back in the queue. |
| B20 | MUST | `brain tick`, run every 15 minutes by one systemd timer, is the only scheduler. Cron jobs run at their local time (C15), lazy jobs only when their `due` test finds work (vault or repo HEAD moved, queue not empty, enough new labels), event jobs inside the command that causes them. Model jobs keep to the night window and a free GPU. Every run and every tick appends a C10 line. | With nothing new, a tick runs no job but still writes its own line; `brain tick --dry-run` lists what is due and why. |
| B21 | MUST | Obsidian-native vault: one note per doc (`docs/<id>/<id>.md`, metadata in frontmatter, no `meta.json`), citations and cross-references as `[[wikilinks]]`, typed relations as wikilink-valued frontmatter properties. The index's `links` table is built from exactly those. | Opening `~/brain` in Obsidian: the graph shows π0's page linked to π0.5 by `builds_on`, and a concept page linked to every doc it cites. |
| B12 | SHOULD | Embeddings come from a local OpenAI-compatible `/v1/embeddings` server with a configurable model. If it's down, `find` runs BM25-only and prints `degraded: no vectors`. | Stop the embedder and `find` still answers, flagged as degraded. |

## F · feed

Daily paper and news candidates you like or skip. Replaces weekly-papers and `paperdb discover`.

- **Done when:** Two weeks of the S2 baseline logged, then the ranker beats its like-rate. A simulated 3-day outage loses no paper.
- **Non-goals:** No keyword scoring, no hand-maintained query list, no HTML digest, no LLM ranking, and no storing of candidates you skipped (only the label).

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| F1 | MUST | A persistent candidate pool (C6) in `~/.local/state/feed/pool.db` with a cursor per source. Re-runs are idempotent and gaps are backfilled. | Disable the timer for 3 days, re-enable it, and every paper from those days enters the pool. |
| F2 | MUST | Sources are configurable: arXiv cs.RO/LG/CV/AI by date range, HF `daily_papers?date=`, S2 recommendations seeded with your library and skips, and alphaXiv For You and Hot via an MCP API key. Lab RSS goes in an unranked news section. | Each source can be switched off in config without code changes. |
| F3 | MUST | Dedupe on the arXiv id without its version, then on the normalized title. Papers already in the library or already labeled are excluded. | A paper on both HF and arXiv appears once. |
| F4 | MUST | Phase A (2 weeks): rank by S2 order plus HF upvotes and log the like-rate. Phase B: PU logistic regression on local embeddings (positives: library; negatives: skips; background: unlabeled), retrained nightly, with time-split AUC logged. | The like-rate of each phase is visible in the weekly review. |
| F5 | MUST | Daily selection from the unshown pool: top 15 + 3 nearest the decision boundary + 2 trending. Counts are configurable. | Each item carries its `bucket` (top, explore or trending). |
| F6 | MUST | Summaries: HF `ai_summary`, the Papers with Code `tldr` or the S2 tldr first; a free-tier LLM writes 5-field cards for the selected items only. Failures are shown, never masked. The free tier is used only for kind ∈ {paper, rss}. | A forced LLM failure shows `summary: failed`, not the abstract. |
| F7 | MUST | Labels via C7 from the dashboard or `feed like\|skip\|later <id>`. A like runs `brain add`. | A like shows up in `brain find` within a minute. |
| F8 | MUST | Unshown items carry over and expire after 21 days with a decaying score. `later` re-surfaces an item after 7 days. | A high-scoring paper from a missed day appears the next day. |
| F9 | SHOULD | One push per day with the count, never one per paper. | — |
| F10 | MUST | Field news via web search, as a `kind: news` source in the same pool. Queries are generated nightly from the profile and active project pages. The search backend sits behind the source interface (SearXNG self-hosted by default; Brave, Exa or Gemini grounding swappable). Also HN Algolia, Reddit r/robotics and r/MachineLearning, and GitHub trending. Dedupe on canonical URL plus title similarity. Ranked by the same model on title and snippet, with its own ~5-item daily slot. A like saves it via `brain add` as `kind: blog`. | Changing project focus changes the next day's queries; the same story from 5 outlets shows once. |

## D · drover v2

The agent runtime: pane and headless runs through native adapters, approvals, events. The terminal workflow is kept (D4).

- **Done when:** A Claude run started from the terminal and one started from the dashboard both show in `drover ls` and stream C9. An approval works from either side.
- **Non-goals:** No Discord once the PWA exists, no journal, no usage scraping, no resident mates process, no scouts special case.

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| D1 | MUST | Two run modes, one registry: `pane` (herdr, interactive: `new`, `attach`, `screen`, `keys`) and `headless`. | `drover ls` shows both with their `mode`. |
| D2 | MUST | A native adapter trait (C12) with five verbs. The Claude adapter ships first: stream-json for headless, `http` hooks for pane runs. | Pane runs produce C9 events with no screen scraping. |
| D3 | MUST | C9 events over SSE and `drover watch`. Agents use `drover wait --any`. | `wait` blocks and returns the next event as C4 output. |
| D4 | MUST | Approvals: a `permission_request` becomes a `needs` event, answered via CLI or HTTP. drover enforces the owner inside `send`. | Sending to a pane you don't own is refused. |
| D5 | MUST | State lives in `~/.local/state/drover` (SQLite: runs, events). Nothing in the checkout. | — |
| D6 | MUST | drover-wt worktrees, local and over ssh, keep their current semantics. | `#pr`, prefix, and safe rm still work. |
| D7 | MUST | Profiles: `reader` (tools limited to brain read commands — `find`, `get`, `pages`, `code` — and Read) and `actor`. Dashboard chat is a reader run: a warm process, killed after 15 minutes idle. | A reader run cannot execute `ssh` or `brain wiki merge`. |
| D8 | MUST | `done` writes the journal entry with `brain log`. There is no `journal.db`. | — |
| D9 | MUST | CLI rebuilt on clap with C4 output rules. A mate is any harness session following the mates skill, registered with drover as `role: mate`. There is no resident mates process. | The skill body is ≤600 tokens and contains no command list. |
| D11 | MUST | Every run carries `role` (task, mate, crew or chat) and `parent`. A mate's goal graph (C16: nodes, checks, rounds, crew runs) is stored with its run and streamed in C9. | `drover show <mate>` lists its nodes and its crew runs. |
| D12 | MUST | Token usage: tokens and cost per run come from `turn_end` events. Plan-window usage per machine and account comes from one isolated module, which shows `unavailable` when the upstream endpoint breaks. | When the usage endpoint fails, only that one field changes. |
| D10 | MUST | Discord is removed once the PWA and push exist (D5). Cost comes from `turn_end` usage events. | Discord and serenity are gone from the dependencies. |

## P · planner

The day around you: a proposal in the morning, capture in the evening, a review on Sunday. Prompts are files and the harness is swappable.

- **Done when:** For 10 working days a plan is proposed by 07:35, shutdown writes the log, and you commit or edit every plan.
- **Non-goals:** No automatic calendar blocking, no silent rescheduling, no task mirroring.

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| P1 | MUST | 07:30 weekdays: a reader run reads the profile, both calendars, `ocu` tasks, `gh` issues and PRs, drover runs and yesterday's log, then writes a plan proposal block to `log/`. | The proposal cites the source of every item. |
| P2 | MUST | At most 3 focus items, each with a time block and a cue ("at 9:00, desk, do X"), plus a separate needs-you list and a paper slot. | — |
| P3 | MUST | Challenge checks are computed deterministically and fed to the prompt: over capacity versus free calendar time, WIP above 3, an item rolled over ≥3 times ("kill or split?"), no deep-work block. | Each triggered check appears in the proposal. |
| P4 | MUST | Commit is explicit, from the dashboard or `plan commit`. The system never reschedules itself. | — |
| P5 | MUST | 18:30 shutdown, deterministic: commits and PRs across registered repos, task state changes and drover runs go into `log/`. Uncommitted focus items roll over with a count. | Opalin work appears in the log without any manual journaling. |
| P6 | MUST | Sunday review in two parts. Plan: estimated vs actual time, deep-work hours, rollovers. Progress: docs marked, concept pages created or changed (linked), contradictions opened and resolved, corrections learned, procedure fixes waiting, recall@8 trend, like-rate, AUC. The counts come from `brain review stats`, with no model. | Every number in the review matches `brain review stats` for that week. |
| P7 | MUST | Planner prompts are versioned files in the brain repo, run through drover's adapter (harness-agnostic). | Switching harness changes no prompt file. |
| P8 | MUST | Compile: each queued doc gets one `reader` run (`planner/compile.md`, at most 5 a night) that replies with the full text of the pages it updates or creates, keeping disagreements as `[!contradiction]` callouts citing both sides. `brain wiki propose` lints the reply and commits it to branch `wiki/<doc-id>`; a lint failure is retried once with the errors. Chat's "file as page" runs the same prompt with a thread as input. | Liking π0.5 yields a branch that edits the existing "π0" and "flow matching" concept pages instead of creating duplicates. |
| P9 | SHOULD | Weekly wiki lint: a `reader` run (`planner/wiki-lint.md`) proposes fixes for stale claims, missing concepts and missing cross-links on one `wiki/lint-<date>` branch. | — |
| P10 | MUST | Weekly learn: a `reader` run (`planner/learn.md`) reads the week's normal chat threads and drover transcripts, never incognito ones. It extracts each correction with the brain ids the agent had loaded, then attributes it: a knowledge gap becomes a page proposal (`wiki/learn-<date>`) carrying its question, a procedure problem becomes a suggested prompt or skill edit in the review, and an ambiguous one is listed. Merging appends the question to the eval set. | A chat where you corrected a wrong claim about π0's action expert yields a page edit, and after the merge `brain eval` includes that question. |

## M · mail

Postponed: your volume is low. Native Gmail filters and labels in both accounts cover it for now.

- **Done when:** Both accounts have filters and labels that keep the inbox to what needs you.
- **Non-goals:** Deferred: C14 mail actions, triage signals, the inbox-zero campaign, people pages from mail. Revisit if volume grows.

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| M1 | MUST | A one-time, agent-assisted session sets up native Gmail filters and labels in both accounts (bulk mail, notifications, receipts, per-project labels). Nothing runs on our side afterwards. | A week later the inbox shows only mail that needs you. |

## X · dashboard BFF

The thin server in the dashboard repo. It maps contracts to HTTP and owns no data.

- **Done when:** Every endpoint is generated from or typed against C11, and an unauthenticated request never reaches the BFF.
- **Non-goals:** No business logic, no database except UI preferences, no public exposure (Q11).

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| X1 | MUST | oauth2-proxy with Google OIDC and an email allowlist sits in front. The BFF is bound to localhost and reachable on the tailnet only. | A coworker on the tailnet gets the Google login wall. |
| X2 | MUST | CLIs are called with `execFile` and an argv allowlist, never through a shell. Ids are validated. | Fuzzing ids yields no shell execution. |
| X3 | MUST | Endpoints map C4, C6–C10 and C14, plus SSE for C9. | An OpenAPI spec is generated and the UI is typed from it. |
| X4 | MUST | Push via ntfy or Web Push for needs-you items and the daily digest only. | — |

## U · dashboard UI

Only the requirements here. The look comes in round 4, in Claude Design.

- **Done when:** Defined in round 4.
- **Non-goals:** No feature that bypasses a contract, no mock data in production.

| ID | Level | Requirement | Acceptance check |
|---|---|---|---|
| U1 | MUST | Screens: Today (plan, WIP 3, drag to time), Inbox (papers, news, needs-you), Agents, Chat (threads in `chats/`), Library (docs, artifacts, meetings). | — |
| U6 | MUST | Agents is a dedicated drover monitor showing every run on every machine (pane and headless): status, tokens and cost per run, plan-window usage, and running mates with their goal graph. Clicking a mate opens its crew per node, with check status, round and escalations. Live via C9. | A mate dispatching 3 crew shows as one graph with 3 live nodes. |
| U2 | MUST | Keyboard-first: a command palette, one key per triage verb (accept, snooze, agent, drop), drag where space matters. | — |
| U3 | MUST | An installable PWA that works on the phone. | — |
| U4 | MUST | Chat shows citations to brain ids, visible tool calls and approval gates, and can promote a thread to a drover run with a prompt you edit. | — |
| U5 | MUST | Calm: a badge only for things blocked on you. | — |
| U7 | MUST | Wiki review: proposed page edits appear in Inbox as a rendered diff per page, with the source doc beside it. Keys: `a` merge, `x` drop, `g` open the branch for a manual edit. Library shows pages with their citations as chips. | A compile branch is merged from the phone in two taps. |
