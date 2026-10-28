use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory as _, Parser, Subcommand};
use serde_json::{Value, json};

use brain::contract::Kind;
use brain::doc::Tag;
use brain::ingest::{self, AddOpts, Added};
use brain::label::{self, Label, Verdict};
use brain::log::{self, Entry};
use brain::{Rel, Result, Slug, Vault, lint, page, time, vault};

/// brain: the knowledge layer. A vault of plain files in a local git repo, and one CLI.
///
/// Bare `brain` prints the profile. Lists print as `name[shown/total]{columns}:` with the
/// id first; single values as `key: value`. Errors print `error:` then `next:`.
/// Exit codes: 0 ok, 1 error, 2 usage.
#[derive(Parser)]
#[command(name = "brain", version)]
struct Cli {
    /// Vault directory [default: ~/brain]
    #[arg(long, global = true, env = "BRAIN_VAULT", value_name = "DIR")]
    vault: Option<PathBuf>,
    /// Print compact JSONL, one object per row or value
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest an arXiv id or URL, a web URL, or a file; adding twice changes nothing
    ///
    /// Prints `id`, `missing` (required metadata the kind's schema still wants) and `help`.
    /// A file's kind defaults to artifact for HTML and note otherwise; markdown
    /// frontmatter (title, started_at, attendees, project...) fills the metadata.
    Add {
        /// arXiv id or URL, web URL, or file path
        source: String,
        /// Doc kind: one per schema in contracts/kinds/
        #[arg(long, value_parser = kind_parser())]
        kind: Option<Kind>,
        /// Why it is worth keeping
        #[arg(long)]
        why: Option<String>,
        /// Tag (repeatable): lowercase letters, digits, '-'
        #[arg(long = "tag", value_parser = Tag::parse)]
        tags: Vec<Tag>,
    },
    /// Append a block to today's log: the first line is the title, the rest the body
    Log {
        text: String,
        /// Who wrote it: cli, plan, shutdown, compile...
        #[arg(long, default_value = "cli", value_parser = Slug::parse)]
        source: Slug,
    },
    /// Record a feed verdict in labels.jsonl; like also adds the doc
    Label {
        /// Doc id, or an arXiv id or URL
        id: String,
        verdict: Verdict,
    },
    /// Add `rel: b` to page a's frontmatter links
    Link {
        /// Page slug: pages/<a>.md
        #[arg(value_parser = Slug::parse)]
        a: Slug,
        /// Doc id or page slug
        b: String,
        /// Relation, e.g. builds_on, related, part_of
        #[arg(value_parser = Rel::parse)]
        rel: Rel,
    },
    /// Validate every doc and page against its schema; exit 1 on any error
    Lint,
}

fn kind_parser() -> impl clap::builder::TypedValueParser<Value = Kind> {
    use clap::builder::TypedValueParser as _;
    clap::builder::PossibleValuesParser::new(brain::contract::kinds())
        .try_map(|s: String| Kind::parse(&s))
}

/// What a command prints, before the text or JSON rendering.
enum Reply {
    Fields(Vec<(&'static str, Value)>),
    Table {
        name: &'static str,
        cols: &'static [&'static str],
        rows: Vec<Vec<Value>>,
    },
    Text(String),
}

fn main() -> ExitCode {
    if let Some(root) = std::env::var_os(vault::COMMITTER_ENV) {
        return match vault::run_committer(Path::new(&root)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();
    let cli = Cli::parse();
    match run(&cli) {
        Ok((reply, code)) => {
            print(&render(&reply, cli.json));
            code
        }
        Err(e) => {
            eprintln!("error: {e}\nnext: {}", e.next());
            ExitCode::from(e.exit_code())
        }
    }
}

/// Exits quietly when stdout is closed (`brain lint | head`) instead of panicking.
fn print(s: &str) {
    if std::io::stdout().lock().write_all(s.as_bytes()).is_err() {
        std::process::exit(0);
    }
}

fn run(cli: &Cli) -> Result<(Reply, ExitCode)> {
    let root = match &cli.vault {
        Some(v) => v.clone(),
        None => Vault::default_root()?,
    };
    let Some(cmd) = &cli.cmd else {
        let vault = Vault::open_or_create(&root)?;
        let profile = vault.read(Path::new("profile.md"))?;
        let reply = match (profile, cli.json) {
            (p, true) => Reply::Fields(vec![("profile", json!(p.unwrap_or_default()))]),
            (Some(p), false) => Reply::Text(p),
            (None, false) => Reply::Fields(vec![("profile", json!("none"))]),
        };
        return Ok((reply, ExitCode::SUCCESS));
    };
    if let Cmd::Lint = cmd {
        return run_lint(&Vault::open(&root)?);
    }
    let vault = Vault::open_or_create(&root)?;
    let reply = write(&vault, cmd);
    // Also after a failure: whatever was written before it still gets committed.
    if let Err(e) = vault.commit_later() {
        eprintln!("warning: the vault will not autocommit: {e}");
    }
    reply
}

fn write(vault: &Vault, cmd: &Cmd) -> Result<(Reply, ExitCode)> {
    let reply = match cmd {
        Cmd::Add {
            source,
            kind,
            why,
            tags,
        } => {
            let opts = AddOpts {
                kind: *kind,
                why: why.clone(),
                tags: tags.clone(),
            };
            added(ingest::add(vault, source, &opts)?)
        }
        Cmd::Log { text, source } => {
            let entry = Entry::new(time::now()?, source.clone(), text)?;
            let file = log::append(vault, &entry)?;
            Reply::Fields(vec![("at", json!(entry.at)), ("file", json!(file))])
        }
        Cmd::Label { id, verdict } => {
            let id = match verdict {
                Verdict::Like => warn(ingest::add(vault, id, &AddOpts::default())?).id,
                Verdict::Skip | Verdict::Later => ingest::resolve(id)?,
            };
            let ts = time::now()?.at;
            label::append(
                vault,
                &Label {
                    id: id.clone(),
                    verdict: *verdict,
                    ts,
                    surface: "cli".to_owned(),
                },
            )?;
            Reply::Fields(vec![("id", json!(id)), ("verdict", json!(verdict))])
        }
        Cmd::Link { a, b, rel } => {
            let dst = ingest::resolve(b)
                .map(|d| d.to_string())
                .or_else(|_| Slug::parse(b).map(|s| s.to_string()))?;
            let what = format!("{a} {rel} {dst}");
            if page::link(vault, a, &dst, rel)? {
                Reply::Fields(vec![("link", json!(what))])
            } else {
                Reply::Fields(vec![("noop", json!(format!("{what} already there")))])
            }
        }
        Cmd::Lint => return run_lint(vault),
    };
    Ok((reply, ExitCode::SUCCESS))
}

fn warn(a: Added) -> Added {
    for w in &a.warnings {
        eprintln!("warning: {w}");
    }
    a
}

fn added(a: Added) -> Reply {
    let a = warn(a);
    let missing: Vec<String> = a
        .missing
        .iter()
        .map(|p| match p {
            brain::contract::Problem::Missing(f) => f.clone(),
            other => other.to_string(),
        })
        .collect();
    let help = if missing.is_empty() {
        format!("brain get {}", a.id)
    } else {
        format!("fill them in {}, then run brain lint", a.meta.display())
    };
    let mut fields = vec![("id", json!(a.id))];
    if a.text_added {
        fields.push(("text", json!("added")));
    } else if !a.created {
        fields.push(("noop", json!("already in the vault")));
    }
    fields.push(("missing", json!(missing)));
    fields.push(("help", json!(help)));
    Reply::Fields(fields)
}

fn run_lint(vault: &Vault) -> Result<(Reply, ExitCode)> {
    let findings = lint::lint(vault)?;
    let code = if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "error: {} schema errors\nnext: fix each file, then run brain lint",
            findings.len()
        );
        ExitCode::FAILURE
    };
    let rows = findings
        .into_iter()
        .map(|f| vec![json!(f.file), json!(f.error)])
        .collect();
    Ok((
        Reply::Table {
            name: "errors",
            cols: &["file", "error"],
            rows,
        },
        code,
    ))
}

fn render(reply: &Reply, json: bool) -> String {
    let mut out = String::new();
    match (reply, json) {
        (Reply::Text(t), _) => {
            out.push_str(t);
            if !t.ends_with('\n') {
                out.push('\n');
            }
        }
        (Reply::Fields(fields), true) => {
            let obj: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect();
            out.push_str(&Value::Object(obj).to_string());
            out.push('\n');
        }
        (Reply::Fields(fields), false) => {
            for (k, v) in fields {
                out.push_str(&format!("{k}: {}\n", plain(v)));
            }
        }
        (Reply::Table { cols, rows, .. }, true) => {
            for row in rows {
                let obj: serde_json::Map<String, Value> = cols
                    .iter()
                    .zip(row)
                    .map(|(c, v)| ((*c).to_owned(), v.clone()))
                    .collect();
                out.push_str(&Value::Object(obj).to_string());
                out.push('\n');
            }
        }
        (Reply::Table { name, cols, rows }, false) => {
            out.push_str(&format!(
                "{name}[{n}/{n}]{{{}}}:\n",
                cols.join(","),
                n = rows.len()
            ));
            for row in rows {
                let cells: Vec<String> = row.iter().map(cell).collect();
                out.push_str(&format!("  {}\n", cells.join(",")));
            }
        }
    }
    out
}

/// A `key: value` value: strings bare, lists comma-separated, `none` when empty.
fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) if a.is_empty() => "none".to_owned(),
        Value::Array(a) => a.iter().map(plain).collect::<Vec<_>>().join(", "),
        Value::Null => "none".to_owned(),
        other => other.to_string(),
    }
}

/// A table cell: bare unless it holds a delimiter or quote, then a JSON string.
fn cell(v: &Value) -> String {
    let s = plain(v);
    if s.contains([',', '"', '\n']) || s.trim() != s {
        Value::String(s).to_string()
    } else {
        s
    }
}
