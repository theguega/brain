---
name: paperdb
description: "Personal paper researcher library. Use to (1) search or answer questions from papers he has saved; (2) add a paper he mentions or links (arXiv id/URL, HF paper URL, PDF); (3) look for new papers and triage the inbox; (4) fill structured cards for papers. Triggers: 'paperdb', 'my papers', 'add this paper', 'what does <paper> say', 'find papers on <topic>', 'anything new on <topic>', an arXiv or huggingface.co/papers link."
---

# brain

`brain` is a CLI over a personal paper library: a git repo of plain files at
`$PAPERDB_LIBRARY` (default `~/papers`). Every paper in it was chosen by the user,
so do not add papers on your own initiative; propose, and add what they confirm.
Run `brain` alone for the library's status (paper counts, inbox, unpushed
commits), and `brain help` for every flag. Exit codes: 2 means fix the command,
3 means the paper is not in the library (search for it), 4 means a network failure
(retry later).

## Answer from the library

```bash
brain search "flow matching action chunking"            # ranked, with snippets
brain search "" --where "family = 'vla' AND open_weights = 1 AND control_hz >= 30"
brain search "reward model" --tag rl --limit 50 --json  # --json for parsing
brain show 2410.24164                                   # record + card
brain show 2410.24164 --text                            # full text (markdown)
```

- Search covers name, title, abstract, Hub keywords, tags, note and full text, stemmed.
  Search for the terms a paper would use, not your paraphrase; try 2-3 phrasings.
- `--where` is SQL over: `id name title authors published added tags note url
  has_text has_card family backbone action_head action_space chunk_size control_hz
  embodiment data_hours data_episodes data_source open_weights open_code open_data
  compute limits eval`. Card columns are NULL when the paper has no card, or does not say.
- Answer from the paper text, not the abstract, whenever numbers matter. Read the
  `text_path` from `show --json` with your file tool; papers are long, so grep for
  the section you need before reading it all.
- The Hub's markdown flattens tables. If a table's numbers look ambiguous, run
  `brain text <id> --pdf` to re-extract the text from the PDF, then re-read.
- Cite papers by name and arXiv id.

## Add papers (and blog posts)

The user finds things to read and asks you to add them. Add exactly what they name.

```bash
brain add 2410.24164 --name pi0 --tag vla --note "why it matters"
brain add https://arxiv.org/abs/2503.14734 https://huggingface.co/papers/2506.01844
brain add https://www.pi.website/blog/pi05 --title "π0.5: ..." --name pi05-blog --tag blog
brain add https://example.org/report.pdf --title "Pinocchio: ..." --name pinocchio
```

- `add` fetches metadata and full text, then commits. Adding a paper that is already
  in the library prints `have <id>` and succeeds, so re-running an `add` is safe. Output ends in `[text via hub]`,
  `[text via pymupdf4llm]`, `[text via pdftotext]` or `[text via reader]` (web pages),
  or in `NO TEXT (...)`.
- Given only a title (or a tweet, a talk), find the arXiv id yourself first, e.g.
  `curl -s "https://export.arxiv.org/api/query?search_query=ti:%22<title words>%22&max_results=3"`,
  and confirm with the user if more than one paper fits.
- Anything not on arXiv (blog post, tech report, project page) needs a `--title`; give a
  `--name` too, it becomes the id. Tag blog posts `blog`.
- `NO TEXT` on a web page means the site blocked the reader. Fetch the page with your
  own web tools and pipe its main text in as markdown: `brain text <id> - <<'EOF'`.
- Give a `--name` (the short handle people use: "pi0", "DreamerV3", "OpenVLA").
- Tags: lowercase slugs. Run `brain tags` and reuse the existing tags rather
  than inventing near-duplicates. `starred` marks the
  user's key papers (it also steers `discover`); only set it when they say so.
- Edit later with `brain tag <id> +t -t`, `brain name <id> X`, `brain note <id> "..."`.
- After adding, write a card (below) unless the user is adding many at once and
  says to skip it.

## Cards

A card is structured data extracted from the paper text. Fill one per paper:

1. `brain card schema` shows the fields and allowed values.
2. Read the full text (`show --text`, or `text_path` from `show --json`).
3. `brain card set <id> <<'EOF'` followed by the JSON and `EOF`.

Rules: `null`, `[]` or `{}` means the paper does not say. **Never infer or guess.**
Eval scores are success rates as fractions (0.93), keyed by benchmark or task name.
`limits` are failure modes the paper itself admits. `card set` rejects unknown
fields and values; fix the JSON, don't drop the field silently.
`brain card todo` lists papers that have text but no card.

## Find new papers

```bash
brain discover            # ~20 new papers like the library, best first
brain inbox --json        # what was listed and not yet triaged
brain add <id> ...        # the user wants it
brain skip <id> ...       # not interested; skip --all clears the rest
```

`discover` asks Semantic Scholar for recent papers like the starred and recently
added ones, minus anything already in the library or listed before; skipped papers
are sent as negative examples, so skipping sharpens future lists. Present the list
as a numbered reading list with links, and for the top few read the abstract
(`curl -s https://huggingface.co/api/papers/<id>`) to give one line on why it fits
the library. Then let the user pick: "add 2, 5 and 7" means add those and, if they
say so, skip the rest.

## Sync and setup

- `brain sync` commits, pulls (rebase) and pushes. Run it after a batch of
  changes, and before starting work on a machine that may be behind.
- Each change already commits locally; never hand-edit `.cache/` (the index rebuilds itself).
- New machine: `cargo install --git https://github.com/theguega/paperdb`, then
  `brain init <library-git-url>`, then `brain skill install`.
