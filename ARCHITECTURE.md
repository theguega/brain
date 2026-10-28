# Second brain: architecture and contracts

A personal second brain for one research engineer. Small backends each own their data behind a contract, and a separate dashboard sits on top as the harness. This document is the reference shared by the three repos:

- **brain**: knowledge, the paper and news feed, planner prompts, and the contracts themselves (`contracts/*.schema.json`)
- **drover**: the agent runtime
- **dashboard**: the human harness, a BFF plus a PWA

Settled in design rounds 1–3 (2026-10-18/19). Requirements are in `REQUIREMENTS.md`. Each repo has its own `DESIGN.md`.

## Principles

1. **Contracts over tools.** Parts talk only through versioned file formats, CLI output and event streams. Replacing ClickUp, the agent harness or the dashboard means honoring one contract.
2. **Files are the truth.** Curated knowledge is markdown or JSON in `~/brain`, a local git repo with no remote. SQLite is only an index, and `brain reindex` rebuilds it.
3. **Query live, don't mirror.** Tasks and calendars are read from their source when needed. The brain stores pointers and decisions.
4. **Only what you mark.** Nothing enters the brain unless you mark it. A skip is stored only as a label (id, verdict, ts).
5. **Stores don't think.** Stores call no model. Deterministic jobs fetch and score. Judgment is made by an agent that runs on a timer.
6. **Token budget is a spec.** At most 400 tokens load with every session. A skill body is at most 600 tokens (brain's own skill: 1,500, since agents otherwise pay for several `--help` calls per task), there are at most 8 skills, and long output is capped behind `--full`.
7. **Readers can't act.** The agent that reads untrusted text has no shell. Actions go through drover, with a prompt you have approved.
8. **Nothing runs without your approval.** An actor run starts only when you start it or approve it (`autostart = "never"`).
9. **Evidence before code.** Claims tables, open loops, rerankers and code summaries all wait until `brain eval` or real use shows they're needed.

## Layers

```text
SOURCES                       CORE (on meerkat)                          SURFACES
research: arXiv, HF, S2,  ─▶  feed (Python, timer)  ──likes──▶ ┐
  alphaXiv, RSS, web news                                       │
git repos (live rg)       ─────────────docs, commits───────────▶ brain (vault + index)  ──CLI + skill──▶ agents
ClickUp, GitHub, calendar ─live─▶ planner (prompts on timers) ─▶ │                       ──HTTP──▶ dashboard (PWA)
                                  drover v2 (runs, events) ──journal──▶ ┘                ◀─SSE── drover
```

- What you mark is compiled into wiki pages (Karpathy's LLM Wiki): a reader run drafts the pages, `brain wiki propose` lints them onto a vault branch, and they land only when you merge them.
- Agents read the brain through the CLI and one skill. The dashboard reads it through its BFF over HTTP, with drover events arriving over SSE.
- drover runs agents either in herdr panes (terminal pair-coding) or headless (the dashboard and other agents). Both modes emit the same events.
- ocu stays as the ClickUp client. Mail management is postponed: native Gmail filters cover it for now.

## Contracts

Each contract is a JSON Schema in `brain/contracts/` with a version field. Changes may only add to a contract. A breaking change bumps the version and ships a migration. Rust types (`typify`) and TypeScript types (`json-schema-to-typescript`) are generated from these schemas.

| ID | Contract | Producer → consumer | Shape | Swap test |
|---|---|---|---|---|
| C1 | Doc (file) | brain add, feed like → index, Obsidian, rg | `docs/<id>/<id>.md`: YAML frontmatter with the metadata, the text as body; originals beside it (`source.<ext>`, artifacts `index.html`). Relations are wikilink-valued properties (`builds_on`, …). `kind` is one of paper, book, course, blog, news, talk, note, artifact, meeting. Each kind has a JSON Schema with its required metadata; a paper's includes `links{code, project, hf, openreview, video}` | A new kind is a schema file, with no code change |
| C2 | Page (file) | you, reader runs via `brain wiki propose` and `merge` → index | `pages/<slug>.md`, frontmatter `{kind, title, summary, status, sources[], updated, superseded_by?}` + `[[wikilinks]]`. `kind` is one of person, project, decision, concept, synthesis. Typed relations are wikilink-valued properties. In concept and synthesis pages every claim cites `[[<doc-id>#heading]]` | Editable in any markdown editor |
| C3 | Log entry (file) | drover done, planner, shutdown, chat → index, planner | `log/YYYY-MM-DD.md`, one `## <ISO time with offset> · <IANA tz> · <place> · source · title` block per entry. Chat transcripts are in `chats/<id>.jsonl` | Any runtime writes the same block |
| C4 | brain CLI (agent) | brain → any agent | `find · get · code · add · log · label · link · pages · wiki · lint · reindex · eval`. A list prints as a table with the id first and a count; a single value prints as `key: value`; `--json` gives compact JSONL; errors print `error:` then `next:`; exit codes are 0, 1 or 2 | A reimplementation passes the same golden tests |
| C5 | Profile (agent) | nightly job → SessionStart hook | `profile.md`, at most 400 tokens: your focus, active projects, today's commitments, items that need you | Works with any harness that has a session-start hook |
| C6 | Candidate pool (stream) | feed → dashboard, push | `{id, kind, first_seen, sources[], title, url, abstract, score, bucket, shown_at?, verdict?}`. Each source has its own cursor, gaps are backfilled, unshown items carry over, and items expire after 21 days | A week of downtime loses no paper |
| C7 | Label (stream) | dashboard, CLI → feed ranker | `labels.jsonl {id, verdict: like\|skip\|later, ts, surface}` | The ranker can be retrained from this file alone |
| C8 | Task (stream) | ocu, gh+jq, any tracker → planner, dashboard | `{id, source, title, state: open\|doing\|blocked\|done, status_raw, due, project, url, updated_at}`. Verbs: list, get, create, update | Switching ClickUp to Linear means one new emitter |
| C9 | Run event (stream) | drover → agents (`wait`), dashboard (SSE) | `{run, mode: pane\|headless, role: task\|mate\|crew\|chat, parent?, at, kind: started\|text\|tool_start\|tool_end\|needs\|reply\|done\|failed, data, usage}` | Switching harness changes no consumer |
| C10 | Job status (stream) | every timer → dashboard, push | `jobs.jsonl {job, started, ended, ok, err}`. A source counts as stale once it's older than 2× its interval | Any scheduler writes the same line |
| C11 | Dashboard API | BFF → SPA | Thin HTTP over C1–C10, C15 and C16, plus SSE for C9. Login is Google OAuth via oauth2-proxy | A TUI could use the same API |
| C12 | Harness adapter (trait) | drover → agent CLIs | A native adapter per harness, each with five verbs (`start · send · respond · abort · events`) and declared capabilities. Claude and Cursor on day one | A new harness is one adapter file |
| C13 | Repo (file) | `repos.toml` → index, `brain code` | `{name, path, remote, project}`. Roots and include/exclude globs are configurable. Markdown, commit messages and a repo card are indexed; code is read live | Moving a repo is one path change |
| C14 | Mail action | deferred | Reserved in case mail volume grows | — |
| C15 | Context (file) | dashboard, laptop hook → every timestamp, local-time jobs | `~/.local/state/context.json {tz, place?, updated_at, source}`. The last writer wins | Travel needs no config change |
| C16 | Goal graph (data) | mate → drover → dashboard monitor | `{mate, goal, nodes[{id, check, status, round, crew[]}], edges[{after}]}` | A mate on another harness draws the same graph |

## Trust boundary

- **Untrusted text** (papers, web pages, news, issues) is stored with `taint: external` and printed inside a fence.
- **Free-tier models** only ever receive `kind ∈ {paper, rss, news}`.
- **The reader profile** (chat, planner, wiki compile) has only brain read commands and Read. It cannot run a shell or `ssh`.
- **Wiki compile** is a reader run: its reply is text that brain lints and commits to a branch. Nothing reaches the vault's `main` without your merge, and lint rejects uncited claims.
- **The actor profile** runs only with a prompt you started or approved, and every tool call outside the allowlist waits for your answer.
- **Claude** approvals use the `PermissionRequest` http hook. **Cursor** uses a `beforeShellExecution` / `beforeMCPExecution` hook that calls drover.
- **The dashboard** is reachable on the tailnet only, behind oauth2-proxy with Google OIDC and an email allowlist. The BFF is bound to localhost and never invokes a shell.
- **Personal data on meerkat** is an accepted risk (Q10). Tokens are stored with mode 600, and the vault has no remote.

## Decisions

| ID | Decision |
|---|---|
| D1 | Runs on meerkat. Opalin and personal accounts are both in scope. |
| D2 | Repos: brain (paperdb renamed, with `feed/`, `planner/`, `contracts/`), drover, and dashboard. |
| D3 | The brain store and CLI are Rust; the feed ranker is Python. |
| D4 | drover has pane and headless modes, both first-class. The terminal workflow stays. |
| D5 | Discord is removed once the PWA and Web Push exist. |
| D6 | Chat runs one warm reader process per thread, through drover's adapter, so it isn't tied to one harness. Each harness runs on its own login or an API key, never Claude OAuth outside Claude Code. |
| D7 | The feed starts with a Semantic Scholar baseline, then moves to a PU-logistic ranker (Elkan & Noto calibration). Laya and Jev are not used for now. |
| D8 | brain is the only memory. The drover journal moves into `brain log`, and supermemory goes. |
| D9 | The dashboard is designed in Claude Design and built at step 5. |
| D10 | The wiki follows Karpathy's LLM Wiki (compile, query, lint), plus cited claims and branch-per-edit review. Omnigraph was considered and rejected: git already gives branches, merge and history over plain files, and the only graph need (C16) fits in SQLite. |
| D11 | Jobs are cron, lazy or event, all started by `brain tick`, and each serves learn, find or plan. Corrections are learned weekly, after Meta's organizational second brain, and each merged correction becomes an eval question. |
| Q11 | Tailnet plus Google OAuth. |
| Q12 | Code knowledge is docs, commits, repo cards and live rg. Summaries only if the eval shows code questions failing. |
| Q13 | A native adapter per harness. |
| Q14 | Mail management is postponed in favour of native Gmail filters. |
| Q15 | The vault has no remote; restic is the backup. |
| Q16 | Artifacts and meetings are their own doc kinds. |
| Q17 | Time follows you (C15). |
| — | The BFF is Rust (axum); the UI is React. |
| — | Approvals go through a `PermissionRequest` http hook in both modes. |

## Build order

0. Systems and layout: the XDG layout, pinned binaries, timers, restic, cleanup.
1. brain: review paperdb against the Rust skill, rename it, migrate the data, build the index and CLI, write the 50-question eval.
2. feed: the persistent pool, sources, the S2 baseline, then the ranker. Retire weekly-papers.
3. drover v2: the Claude and Cursor adapters (after the hook spike), events, approvals, the goal graph.
4. planner: prompts, timers, shutdown capture, then compile and wiki lint.
5. dashboard: the Rust BFF, then the React PWA from the Claude Design session.
6. Cleanup: remove Discord, prune skills, drop supermemory, backfill the last month.
