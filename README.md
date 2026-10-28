# brain

A personal paper library: one Rust binary, a git-synced folder of plain files,
and an agent skill to fill it.

- **Your selection only.** You find something worth reading (paper, blog post,
  report), your agent runs `brain add`. `brain discover` suggests new papers
  like the ones you keep; nothing enters without you.
- **Plain files.** `papers/<id>/paper.json` (metadata, tags, note, card) and
  `papers/<id>/paper.md` (full text). Diffable, greppable, and readable without
  the tool.
- **Every machine.** The library is a git repo; `brain sync` pulls and pushes.
  The search index is a cache, rebuilt automatically when files change.

## Install

```bash
cargo install --git https://github.com/theguega/paperdb
brain init                      # first machine: new library at ~/papers
brain init <library-git-url>    # other machines: clone it
brain skill install             # agent skill -> ~/.claude/skills/paperdb
```

Runtime needs `git` and `curl`. Papers the Hugging Face Hub can't serve as
markdown (mostly pre-2022) are converted from PDF with `uvx` (pymupdf4llm) or,
failing that, poppler's `pdftotext`. Set `PAPERDB_LIBRARY` to put the library
somewhere other than `~/papers`.

## Use

```bash
brain add 2410.24164 --name pi0 --tag vla     # arXiv id, arXiv/HF URL
brain add https://www.pi.website/blog/pi05 --title "π0.5" --name pi05-blog --tag blog
brain search "flow matching" --tag vla
brain search "" --where "family = 'vla' AND open_weights = 1"
brain show pi0 --text
brain discover                                # new papers like your library
brain sync
```

`brain help` lists everything. In practice your agent does most of this
through the skill (`skill/SKILL.md`, compiled into the binary): adding papers
you mention, writing cards from the full text, triaging the inbox, and
answering from the papers.

## Library layout

```text
~/papers/
  papers/<id>/paper.json   record: source, title, name, authors, tags, note, card
  papers/<id>/paper.md     full text
  inbox.jsonl              papers discover listed (new / skipped)
  .cache/                  index.db + PDFs, gitignored
```

## Where data comes from

| What | Source |
|---|---|
| Metadata | arXiv API; Hugging Face Hub for AI summary, keywords and links, and for ids arXiv misses |
| Full text | Hub markdown (`huggingface.co/papers/<id>.md`), else the PDF; web pages via [Jina Reader](https://jina.ai/reader), else your agent pipes the text in |
| `discover` | [Semantic Scholar recommendations](https://api.semanticscholar.org/api-docs/recommendations): starred + recent papers as positives, skips as negatives |

The Hub flattens tables; `brain text <id> --pdf` re-extracts from the PDF when
the numbers matter.

## Development

Library code follows strict lints (no `unwrap`/`expect`/`panic!`, no slice
indexing, no `unsafe`); see `[lints]` in `Cargo.toml`. Dependencies are kept
to `rusqlite`, `serde`, `serde_json` and `thiserror`; HTTP and git go through
the system `curl` and `git`.

```bash
cargo clippy --all-targets && cargo test
```
