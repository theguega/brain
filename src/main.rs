use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use paperdb::date::Day;
use paperdb::discover::{self, Inbox};
use paperdb::index::{Index, Query};
use paperdb::ingest::{self, Route};
use paperdb::library::SyncOutcome;
use paperdb::paper::CARD_TEMPLATE;
use paperdb::{Card, Error, Library, Paper, PaperId, Result, Slug, Source, Tag};

/// `println!` that exits quietly when stdout is closed (`paperdb search | head`)
/// instead of panicking on the broken pipe.
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        if writeln!(std::io::stdout(), $($t)*).is_err() {
            std::process::exit(0);
        }
    }};
}

macro_rules! out_raw {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        if write!(std::io::stdout(), $($t)*).is_err() {
            std::process::exit(0);
        }
    }};
}

const SKILL: &str = include_str!("../skill/SKILL.md");

const HELP: &str = "\
paperdb - personal paper library

Library: $PAPERDB_LIBRARY or ~/papers (a git repo of plain files).
Run with no arguments for the library's status.
Exit codes: 0 ok, 1 error, 2 bad usage, 3 paper not in library, 4 network.

  init [<git-url>]                 create a library, or clone yours on a new machine
  add <ref>... [--name N] [--tag T]... [--note TEXT] [--link URL]...
                                   arXiv id/URL (or HF paper URL): metadata + full text
  add <url> --title T [--slug S] [--pdf URL]
                                   blog post, report, or paper not on arXiv
  rm <id>...
  search [text] [--where SQL] [--tag T]... [--limit N] [--json]
  tags [--json]                    tags in use, with paper counts
  show <id> [--text] [--json]
  tag <id> [+]tag... -tag...       add / remove tags
  name <id> <short-name>
  note <id> <text | ->             replace the note ('-' reads stdin; '' clears)
  card <id>                        print the card
  card set <id>                    card JSON on stdin (see `card schema`)
  card todo [--json]               papers with text but no card
  card schema
  text <id> [--pdf | -]            re-fetch full text (--pdf keeps tables;
                                   - reads markdown from stdin)
  discover [--limit N] [--json]    new papers like your library -> inbox
  inbox [--json]                   untriaged candidates
  skip <id>... | skip --all        dismiss candidates for good
  sync                             commit, pull, push
  index                            force an index rebuild
  skill [install [<dir>]]          print, or install the agent skill
                                   (default ~/.claude/skills/paperdb)
  path                             print the library path
";

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match run(&raw) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("paperdb: {e}");
            if let Error::NotFound(r) = &e {
                eprintln!("try `paperdb search {r}`");
            }
            ExitCode::from(e.exit_code())
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Human,
    Json,
}

fn run(raw: &[String]) -> Result<()> {
    let root = Library::default_root()?;
    let Some((cmd, rest)) = raw.split_first() else {
        return status(&root);
    };
    let lib = || Library::open(&root);
    match cmd.as_str() {
        "help" | "-h" | "--help" => out_raw!("{HELP}"),
        "-V" | "--version" | "version" => out!("paperdb {}", env!("CARGO_PKG_VERSION")),
        "path" => out!("{}", root.display()),
        "init" => init(&root, &Args::parse(rest, &[], &[])?)?,
        "add" => add(
            &lib()?,
            &Args::parse(
                rest,
                &[
                    "--name", "--tag", "--note", "--link", "--slug", "--title", "--pdf",
                ],
                &[],
            )?,
        )?,
        "rm" => rm(&lib()?, &Args::parse(rest, &[], &[])?)?,
        "search" | "s" => search(
            &lib()?,
            &Args::parse(rest, &["--where", "--tag", "--limit"], &["--json"])?,
        )?,
        "tags" => tags(&lib()?, &Args::parse(rest, &[], &["--json"])?)?,
        "show" | "open" => show(&lib()?, &Args::parse(rest, &[], &["--text", "--json"])?)?,
        "tag" => tag(&lib()?, &Args::parse(rest, &[], &[])?)?,
        "name" => name(&lib()?, &Args::parse(rest, &[], &[])?)?,
        "note" => note(&lib()?, &Args::parse(rest, &[], &[])?)?,
        "card" => card(&lib()?, &Args::parse(rest, &[], &["--json"])?)?,
        "text" => text(&lib()?, &Args::parse(rest, &[], &["--pdf"])?)?,
        "discover" => run_discover(&lib()?, &Args::parse(rest, &["--limit"], &["--json"])?)?,
        "inbox" => inbox(&lib()?, &Args::parse(rest, &[], &["--json"])?)?,
        "skip" => skip(&lib()?, &Args::parse(rest, &[], &["--all"])?)?,
        "sync" => sync(&lib()?)?,
        "index" => out!("indexed {} papers", Index::rebuild(&lib()?)?),
        "skill" => skill(&Args::parse(rest, &[], &[])?)?,
        other => {
            return Err(usage(&format!(
                "unknown command {other:?}; see `paperdb help`"
            )));
        }
    }
    Ok(())
}

/// Bare `paperdb`: what is in the library and what needs doing, without network calls.
fn status(root: &std::path::Path) -> Result<()> {
    let lib = match Library::open(root) {
        Ok(lib) => lib,
        Err(Error::NoLibrary(_)) => {
            out!("no library at {}", root.display());
            out!("next: `paperdb init` (or `paperdb init <git-url>`); `paperdb help` for commands");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let st = Index::open(&lib)?.stats()?;
    out!(
        "library  {}  {} papers · {} without card · {} without text",
        root.display(),
        st.papers,
        st.no_card,
        st.no_text
    );
    let pending = Inbox::load(&lib)?.pending().len();
    out!(
        "inbox    {pending} to triage{}",
        if pending > 0 {
            " (`paperdb inbox`)"
        } else {
            ""
        }
    );
    match lib.unpushed() {
        None => out!("git      no remote"),
        Some(0) => out!("git      up to date with last fetch"),
        Some(n) => out!("git      {n} unpushed commits (`paperdb sync`)"),
    }
    if st.no_card > 0 {
        out!("next: `paperdb card todo` lists papers to card");
    }
    out!("`paperdb help` for commands");
    Ok(())
}

fn tags(lib: &Library, a: &Args) -> Result<()> {
    let tags = Index::open(lib)?.tags()?;
    if a.output() == Output::Json {
        return print_json(&tags.into_iter().collect::<BTreeMap<_, _>>());
    }
    for (t, n) in &tags {
        out!("{n:4} {t}");
    }
    out!(
        "{} tags; filter with `paperdb search --tag <t>`",
        tags.len()
    );
    Ok(())
}

fn init(root: &std::path::Path, a: &Args) -> Result<()> {
    let lib = match a.pos.first() {
        Some(remote) => Library::clone_from(remote, root)?,
        None => Library::create(root)?,
    };
    out!("library ready at {}", lib.root().display());
    out!("next: `paperdb skill install`, then `paperdb add <arxiv-id>`");
    Ok(())
}

fn add(lib: &Library, a: &Args) -> Result<()> {
    if a.pos.is_empty() {
        return Err(usage("add <arxiv-id or URL>..."));
    }
    let single = ["--name", "--slug", "--title", "--pdf"];
    if a.pos.len() > 1 && single.iter().any(|k| a.one(k).is_some()) {
        return Err(usage(
            "--name, --slug, --title and --pdf apply to a single paper",
        ));
    }
    let tags = a
        .all("--tag")
        .iter()
        .map(|t| Tag::parse(t))
        .collect::<Result<Vec<_>>>()?;
    let mut inbox = Inbox::load(lib)?;
    let (mut added, mut failed) = (Vec::new(), 0);
    for (i, r) in a.pos.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_secs(3)); // arXiv's requested pace
        }
        // One bad reference must not lose the others: report it and keep going.
        match add_one(lib, r, a, &tags) {
            Ok((id, fresh)) => {
                if let PaperId::Arxiv(aid) = &id {
                    inbox.take(aid);
                }
                if fresh {
                    added.push(id);
                }
            }
            Err(e) => {
                eprintln!("paperdb: {r}: {e}");
                failed += 1;
            }
        }
    }
    inbox.save(lib)?;
    lib.commit(&format!("add {}", join_ids(&added)))?;
    if failed > 0 {
        return Err(Error::Partial {
            failed,
            total: a.pos.len(),
        });
    }
    if let [id] = added.as_slice() {
        out!("next: `paperdb card schema`, then `paperdb card set {id}`");
    }
    Ok(())
}

/// The paper's id, and whether it is new: adding a paper that is already in the
/// library is not an error, so a repeated `add` is safe.
fn add_one(lib: &Library, r: &str, a: &Args, tags: &[Tag]) -> Result<(PaperId, bool)> {
    let mut paper = match PaperId::parse(r) {
        Ok(PaperId::Arxiv(aid)) if a.one("--title").is_none() => {
            let id = PaperId::Arxiv(aid.clone());
            if lib.contains(&id) {
                return Ok(already(lib, id));
            }
            ingest::describe(&aid, Day::today())?
        }
        _ if r.contains("://") => web_paper(r, a)?,
        Ok(_) | Err(_) => return Err(usage(&format!("{r}: want an arXiv id or a URL"))),
    };
    let id = paper.id();
    if lib.contains(&id) {
        return Ok(already(lib, id));
    }
    if let Some(n) = a.one("--name") {
        n.clone_into(&mut paper.name);
    }
    if let Some(n) = a.one("--note") {
        n.clone_into(&mut paper.note);
    }
    for t in tags {
        paper.tag(t.clone());
    }
    paper.links.extend(a.all("--link").iter().cloned());
    lib.save(&paper)?;
    let via = match ingest::fetch_text(lib, &paper, Route::Auto) {
        Ok(via) => format!("text via {via:?}").to_lowercase(),
        Err(e) => format!("NO TEXT ({e})"),
    };
    out!("added {id}  {}  [{via}]", paper.label());
    Ok((id, true))
}

fn already(lib: &Library, id: PaperId) -> (PaperId, bool) {
    let label = lib
        .load(&id)
        .map(|p| p.label().to_owned())
        .unwrap_or_default();
    out!("have  {id}  {label}  [already in library; edit with tag/name/note]");
    (id, false)
}

fn web_paper(url: &str, a: &Args) -> Result<Paper> {
    let title = a
        .one("--title")
        .ok_or_else(|| usage(&format!("{url} is not on arXiv; give it a --title")))?;
    let slug = match a.one("--slug") {
        Some(s) => Slug::parse(s)?,
        None => Tag::slugify(a.one("--name").unwrap_or(title))
            .and_then(|t| Slug::parse(t.as_str()).ok())
            .ok_or_else(|| usage("can't make a slug from that title; pass --slug"))?,
    };
    let pdf = a
        .one("--pdf")
        .map(str::to_owned)
        .or_else(|| url.ends_with(".pdf").then(|| url.to_owned()));
    let source = Source::Web {
        slug,
        url: url.to_owned(),
        pdf,
    };
    Ok(Paper::new(source, title.to_owned(), Day::today()))
}

fn rm(lib: &Library, a: &Args) -> Result<()> {
    let ids = a
        .pos
        .iter()
        .map(|r| lib.find(r))
        .collect::<Result<Vec<_>>>()?;
    for id in &ids {
        lib.remove(id)?;
        out!("removed {id}");
    }
    lib.commit(&format!("rm {}", join_ids(&ids)))
}

fn search(lib: &Library, a: &Args) -> Result<()> {
    let limit = match a.one("--limit") {
        Some(n) => n.parse().map_err(|_| usage("--limit takes a number"))?,
        None => 20,
    };
    let text = a.pos.join(" ");
    let hits = Index::open(lib)?.search(&Query {
        text: &text,
        filter: a.one("--where"),
        tags: a.all("--tag"),
        limit,
    })?;
    if a.output() == Output::Json {
        return print_json(&hits);
    }
    for h in &hits {
        let label = if h.name.is_empty() { &h.title } else { &h.name };
        out!(
            "{:12} {:10} {:16} {}",
            h.id,
            h.published,
            h.family.as_deref().unwrap_or("-"),
            clip(label, 70)
        );
        if !h.snippet.is_empty() {
            out!(
                "{:12} {}",
                "",
                clip(
                    &h.snippet.split_whitespace().collect::<Vec<_>>().join(" "),
                    110
                )
            );
        }
    }
    if hits.is_empty() {
        out!("no matches");
    }
    Ok(())
}

fn show(lib: &Library, a: &Args) -> Result<()> {
    let id = one_id(lib, a)?;
    let p = lib.load(&id)?;
    if a.has("--text") {
        let body = lib
            .text(&id)?
            .ok_or_else(|| usage(&format!("no text for {id}; run `paperdb text {id}`")))?;
        out_raw!("{body}");
        return Ok(());
    }
    if a.output() == Output::Json {
        let mut v = serde_json::to_value(&p).map_err(|source| Error::Decode {
            what: "paper",
            source,
        })?;
        if let Some(o) = v.as_object_mut() {
            o.insert("id".into(), id.to_string().into());
            o.insert("url".into(), p.url().into());
            o.insert(
                "text_path".into(),
                lib.text_path(&id).to_string_lossy().into_owned().into(),
            );
        }
        return print_json(&v);
    }
    out!("{}", p.title);
    if !p.name.is_empty() {
        out!("name:      {}", p.name);
    }
    out!("id:        {id}    {}", p.url());
    let more = if p.authors.len() > 6 { " et al." } else { "" };
    out!(
        "authors:   {}{more}",
        p.authors
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    out!("published: {}    added: {}", p.published, p.added);
    if !p.tags.is_empty() {
        out!(
            "tags:      {}",
            p.tags.iter().map(Tag::as_str).collect::<Vec<_>>().join(" ")
        );
    }
    for l in &p.links {
        out!("link:      {l}");
    }
    let text = lib.text_path(&id);
    out!(
        "text:      {}",
        if text.is_file() {
            text.display().to_string()
        } else {
            "missing".into()
        }
    );
    if !p.note.is_empty() {
        out!("\nnote: {}", p.note);
    }
    out!(
        "\n{}",
        if p.summary.is_empty() {
            &p.abstract_
        } else {
            &p.summary
        }
    );
    if let Some(c) = &p.card {
        out!();
        print_json(c)?;
    }
    Ok(())
}

fn tag(lib: &Library, a: &Args) -> Result<()> {
    let (id, ops) = id_and_rest(lib, a)?;
    let mut p = lib.load(&id)?;
    for op in ops {
        match op.strip_prefix('-') {
            Some(t) => p.untag(&Tag::parse(t)?),
            None => p.tag(Tag::parse(op.strip_prefix('+').unwrap_or(op))?),
        }
    }
    lib.save(&p)?;
    out!(
        "{id}: {}",
        p.tags.iter().map(Tag::as_str).collect::<Vec<_>>().join(" ")
    );
    lib.commit(&format!("tag {id}"))
}

fn name(lib: &Library, a: &Args) -> Result<()> {
    let (id, rest) = id_and_rest(lib, a)?;
    let mut p = lib.load(&id)?;
    p.name = rest.join(" ");
    lib.save(&p)?;
    lib.commit(&format!("name {id}"))
}

fn note(lib: &Library, a: &Args) -> Result<()> {
    let (id, rest) = id_and_rest(lib, a)?;
    let mut p = lib.load(&id)?;
    p.note = match rest {
        [dash] if dash == "-" => stdin()?.trim().to_owned(),
        words => words.join(" "),
    };
    lib.save(&p)?;
    lib.commit(&format!("note {id}"))
}

fn card(lib: &Library, a: &Args) -> Result<()> {
    match a.pos.as_slice() {
        [s] if s == "schema" => out!("{CARD_TEMPLATE}"),
        [s] if s == "todo" => {
            let mut todo = Vec::new();
            for p in lib.papers()? {
                if p.card.is_none() && lib.text_path(&p.id()).is_file() {
                    todo.push((p.id().to_string(), p.label().to_owned()));
                }
            }
            if a.output() == Output::Json {
                return print_json(&todo.iter().map(|(id, _)| id).collect::<Vec<_>>());
            }
            for (id, label) in &todo {
                out!("{id:12} {}", clip(label, 80));
            }
            out!("{} papers without a card", todo.len());
        }
        [s, r] if s == "set" => {
            let id = lib.find(r)?;
            let mut p = lib.load(&id)?;
            let c: Card = serde_json::from_str(&stdin()?).map_err(|source| Error::Decode {
                what: "card on stdin (see `paperdb card schema`)",
                source,
            })?;
            p.card = Some(c);
            lib.save(&p)?;
            out!("card saved for {id}");
            lib.commit(&format!("card {id}"))?;
        }
        [r] => {
            let id = lib.find(r)?;
            match lib.load(&id)?.card {
                Some(c) => print_json(&c)?,
                None => out!("no card for {id}; see `paperdb card schema`"),
            }
        }
        _ => return Err(usage("card <id> | card set <id> | card todo | card schema")),
    }
    Ok(())
}

fn text(lib: &Library, a: &Args) -> Result<()> {
    let (id, rest) = id_and_rest(lib, a)?;
    let p = lib.load(&id)?;
    let via = match rest {
        [dash] if dash == "-" => {
            lib.save_text(&id, &stdin()?)?;
            "stdin".to_owned()
        }
        [] => {
            let route = if a.has("--pdf") {
                Route::Pdf
            } else {
                Route::Auto
            };
            format!("{:?}", ingest::fetch_text(lib, &p, route)?).to_lowercase()
        }
        _ => return Err(usage("text <id> [--pdf | -]")),
    };
    out!("text for {id} via {via}");
    lib.commit(&format!("text {id}"))
}

fn run_discover(lib: &Library, a: &Args) -> Result<()> {
    let limit = match a.one("--limit") {
        Some(n) => n.parse().map_err(|_| usage("--limit takes a number"))?,
        None => 20,
    };
    let fresh = discover::discover(lib, limit)?;
    lib.commit("discover")?;
    if a.output() == Output::Json {
        return print_json(&fresh);
    }
    print_candidates(&fresh.iter().collect::<Vec<_>>());
    out!(
        "{} new; add what you like, `paperdb skip --all` for the rest",
        fresh.len()
    );
    Ok(())
}

fn inbox(lib: &Library, a: &Args) -> Result<()> {
    let inbox = Inbox::load(lib)?;
    let pending = inbox.pending();
    if a.output() == Output::Json {
        return print_json(&pending);
    }
    print_candidates(&pending);
    out!(
        "{} to triage: `paperdb add <id>` or `paperdb skip <id>`",
        pending.len()
    );
    Ok(())
}

fn skip(lib: &Library, a: &Args) -> Result<()> {
    let mut inbox = Inbox::load(lib)?;
    let ids: Vec<paperdb::ArxivId> = if a.has("--all") {
        inbox.pending().into_iter().map(|c| c.id.clone()).collect()
    } else {
        a.pos
            .iter()
            .map(|s| paperdb::ArxivId::parse(s))
            .collect::<Result<_>>()?
    };
    // Skipping is idempotent; an id the inbox never listed is reported, not fatal,
    // so one stale id does not lose the others.
    let mut skipped = 0;
    for id in &ids {
        if inbox.skip(id) {
            skipped += 1;
        } else {
            eprintln!("paperdb: {id} is not in the inbox; ignored");
        }
    }
    inbox.save(lib)?;
    out!(
        "skipped {skipped}; {} left to triage",
        inbox.pending().len()
    );
    lib.commit("skip")
}

fn sync(lib: &Library) -> Result<()> {
    match lib.sync()? {
        SyncOutcome::LocalOnly => out!(
            "committed (no remote; add one with `git -C {} remote add origin <url>`)",
            lib.root().display()
        ),
        SyncOutcome::Remote {
            pulled: 0,
            pushed: 0,
        } => out!("in sync"),
        SyncOutcome::Remote { pulled, pushed } => {
            out!("pulled {pulled}, pushed {pushed} commits");
            if pulled > 0 {
                out!("indexed {} papers", Index::rebuild(lib)?);
            }
        }
        _ => {}
    }
    Ok(())
}

fn skill(a: &Args) -> Result<()> {
    match a.pos.as_slice() {
        [] => out_raw!("{SKILL}"),
        [s, rest @ ..] if s == "install" => {
            let dir = match rest {
                [d] => PathBuf::from(d),
                _ => {
                    PathBuf::from(std::env::var_os("HOME").ok_or_else(|| usage("HOME is not set"))?)
                        .join(".claude/skills/paperdb")
                }
            };
            let path = dir.join("SKILL.md");
            std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::write(&path, SKILL))
                .map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
            out!("installed {}", path.display());
        }
        _ => return Err(usage("skill [install [<dir>]]")),
    }
    Ok(())
}

fn print_candidates(cs: &[&discover::Candidate]) {
    for c in cs {
        out!(
            "{:12} {:10} {}\n{:23} {}",
            c.id,
            c.published,
            clip(&c.title, 80),
            "",
            c.url()
        );
    }
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(v).map_err(|source| Error::Decode {
        what: "output",
        source,
    })?;
    out!("{s}");
    Ok(())
}

fn stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|source| Error::Io {
            path: "<stdin>".into(),
            source,
        })?;
    Ok(s)
}

fn one_id(lib: &Library, a: &Args) -> Result<PaperId> {
    match a.pos.as_slice() {
        [r] => lib.find(r),
        _ => Err(usage("expected exactly one paper (id or name)")),
    }
}

fn id_and_rest<'a>(lib: &Library, a: &'a Args) -> Result<(PaperId, &'a [String])> {
    let (r, rest) = a
        .pos
        .split_first()
        .ok_or_else(|| usage("expected a paper (id or name)"))?;
    Ok((lib.find(r)?, rest))
}

fn join_ids(ids: &[PaperId]) -> String {
    ids.iter()
        .map(PaperId::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

fn usage(msg: &str) -> Error {
    Error::Usage(msg.to_owned())
}

/// Positionals, `--key value` options (repeatable) and `--switch`es. Anything else
/// starting with `--` is an error, so a typo never silently becomes a positional.
struct Args {
    pos: Vec<String>,
    opts: BTreeMap<String, Vec<String>>,
}

impl Args {
    fn parse(raw: &[String], valued: &[&str], switches: &[&str]) -> Result<Self> {
        let mut pos = Vec::new();
        let mut opts: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut it = raw.iter();
        while let Some(arg) = it.next() {
            let (key, inline) = match arg.split_once('=') {
                Some((k, v)) if k.starts_with("--") => (k, Some(v.to_owned())),
                _ => (arg.as_str(), None),
            };
            if valued.contains(&key) {
                let v = inline
                    .or_else(|| it.next().cloned())
                    .ok_or_else(|| usage(&format!("{key} needs a value")))?;
                opts.entry(key.to_owned()).or_default().push(v);
            } else if switches.contains(&key) {
                opts.entry(key.to_owned()).or_default();
            } else if arg.starts_with("--") {
                return Err(usage(&format!("unknown option {arg}")));
            } else {
                pos.push(arg.clone());
            }
        }
        Ok(Self { pos, opts })
    }

    fn one(&self, key: &str) -> Option<&str> {
        self.opts
            .get(key)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    fn all(&self, key: &str) -> &[String] {
        self.opts.get(key).map_or(&[], Vec::as_slice)
    }

    fn has(&self, key: &str) -> bool {
        self.opts.contains_key(key)
    }

    fn output(&self) -> Output {
        if self.has("--json") {
            Output::Json
        } else {
            Output::Human
        }
    }
}
