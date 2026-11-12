//! medrec: a household's medical records in a plain Drive folder
//! (piekstra-cli/1). The command tree and output live here; the mechanisms
//! live in the library.

use std::io::Read;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::AuthStatus;
use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{confirm, output, CliError, CommonArgs};
use pk_cli_selfupdate::{SelfUpdateArgs, Updater};
use serde::Serialize;
use serde_json::{json, Value};

use medrec::config::{Config, KEYS};
use medrec::folder::{Access, Folder};
use medrec::intake::{self, Given};
use medrec::store::{Location, BIN};
use medrec::views::Range;
use medrec::{imaging, import, maint, notes, people, review, store, view, views};

const REPO: &str = "piekstra/medrec-cli";

/// A household's medical records in a plain Drive folder: people, intake of
/// scans and files with local OCR, imaging studies, provider and insurer
/// imports, timelines, search and notes.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Set up a records folder (or adopt one) and make it this machine's.
    Init(InitArgs),
    /// The people whose records these are.
    #[command(subcommand)]
    People(PeopleCmd),
    /// File documents: scans, PDFs and images, from files, the inbox or a
    /// Drive folder.
    Intake(IntakeArgs),
    /// What intake could not decide.
    #[command(subcommand)]
    Review(ReviewCmd),
    /// Import records from a provider or insurer CLI (records-stream/v1).
    Import(ImportArgs),
    /// Provider CLIs medrec can import from.
    #[command(subcommand)]
    Providers(ProvidersCmd),
    /// Everything dated, newest first: visits, results, documents, studies,
    /// notes.
    Timeline(TimelineArgs),
    /// Find documents, records, studies and notes by their words.
    Search(SearchArgs),
    /// Lab results and reports.
    Results(RecordArgs),
    /// Medications.
    Meds(RecordArgs),
    /// Conditions and problems.
    Conditions(RecordArgs),
    /// Allergies and intolerances.
    Allergies(RecordArgs),
    /// Immunizations.
    Immunizations(RecordArgs),
    /// Filed documents.
    #[command(subcommand)]
    Documents(DocumentsCmd),
    /// Dated notes, kept in each person's notes.md.
    #[command(subcommand)]
    Note(NoteCmd),
    /// Imaging studies (DICOM).
    #[command(subcommand)]
    Images(ImagesCmd),
    /// Check this machine and the records folder.
    Doctor,
    /// Check every indexed file is in the folder with the hash the index
    /// recorded.
    Verify(VerifyArgs),
    /// Copy the whole records folder to another rclone remote or folder,
    /// then compare every file.
    Backup(BackupArgs),
    /// Non-secret settings.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// medrec holds no credentials (auth-status/v1).
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
    /// This machine's copies of records-folder files: the read cache and
    /// opened documents and studies
    #[command(subcommand)]
    Cache(CacheCmd),
}

#[derive(Args, Debug)]
struct InitArgs {
    /// `<rclone remote>:<path>` (canonical) or a local directory.
    location: String,
    /// Set up medrec in a folder that already holds other files.
    #[arg(long)]
    allow_other_files: bool,
}

#[derive(Subcommand, Debug)]
enum PeopleCmd {
    /// Add a person.
    Add(PersonAddArgs),
    /// List the people.
    List {
        #[command(flatten)]
        list: ListArgs,
    },
    /// One person, with counts of what is filed for them.
    Show { person: String },
    /// Change a person (their folder keeps its name).
    Edit(PersonEditArgs),
}

#[derive(Args, Debug)]
struct PersonAddArgs {
    /// Their name as it appears on records.
    name: String,
    /// YYYY-MM-DD.
    #[arg(long)]
    birth_date: Option<String>,
    /// An adult (default: from the birth date).
    #[arg(long, conflicts_with = "child")]
    adult: bool,
    /// A child.
    #[arg(long)]
    child: bool,
    /// Who looks after a child's records (a person, repeatable).
    #[arg(long = "guardian")]
    guardians: Vec<String>,
    /// Another name records may use (repeatable).
    #[arg(long = "alias")]
    aliases: Vec<String>,
    /// Their Huishouden Health person id (a link only; nothing is read).
    #[arg(long)]
    huishouden_health_person: Option<String>,
    /// Their Huishouden Baby profile id (a link only).
    #[arg(long)]
    baby_profile: Option<String>,
}

#[derive(Args, Debug)]
struct PersonEditArgs {
    person: String,
    #[arg(long)]
    name: Option<String>,
    /// YYYY-MM-DD.
    #[arg(long)]
    birth_date: Option<String>,
    #[arg(long, conflicts_with = "child")]
    adult: bool,
    #[arg(long)]
    child: bool,
    #[arg(long = "add-guardian")]
    add_guardians: Vec<String>,
    #[arg(long = "remove-guardian")]
    remove_guardians: Vec<String>,
    #[arg(long = "add-alias")]
    add_aliases: Vec<String>,
    #[arg(long = "remove-alias")]
    remove_aliases: Vec<String>,
    /// `source=value`, e.g. `uhc:member=...` (repeatable).
    #[arg(long = "add-identifier")]
    add_identifiers: Vec<String>,
    /// A source, or `source=value` (repeatable).
    #[arg(long = "remove-identifier")]
    remove_identifiers: Vec<String>,
    #[arg(long)]
    huishouden_health_person: Option<String>,
    #[arg(long)]
    baby_profile: Option<String>,
    /// Remove both Huishouden links.
    #[arg(long)]
    clear_links: bool,
}

#[derive(Args, Debug, Clone)]
struct GivenArgs {
    /// Whose document it is.
    #[arg(long)]
    person: Option<String>,
    /// Document type (`medrec intake --help` lists them).
    #[arg(long = "type", value_name = "TYPE")]
    doc_type: Option<String>,
    /// Date of service, YYYY-MM-DD.
    #[arg(long)]
    date: Option<String>,
    /// The organization it is from.
    #[arg(long)]
    org: Option<String>,
}

#[derive(Args, Debug)]
#[command(
    after_help = "Document types: visit_summary, lab_report, imaging_report, immunization_record, \
discharge_summary, referral, prescription, letter, bill, eob, insurance_card, other.\n\n\
Files from the inbox and ~/Downloads are moved (removed once filed and read back); \
other files and Drive folders are copied. --move moves named files too; --keep copies everything."
)]
struct IntakeArgs {
    /// Files to file.
    files: Vec<PathBuf>,
    /// Drain the inbox (config `inbox`, default `inbox/` in the records
    /// folder).
    #[arg(long)]
    inbox: bool,
    /// File everything in a Drive folder or directory (copied, not moved).
    #[arg(long, value_name = "REMOTE:PATH|DIR")]
    drive: Option<String>,
    #[command(flatten)]
    given: GivenArgs,
    /// Who classified the given values (`organize-scans`): medrec checks
    /// them against its own reading and holds a disagreement for review.
    #[arg(long)]
    classified_by: Option<String>,
    /// Move named files (remove each once filed and read back).
    #[arg(long = "move", conflicts_with = "keep")]
    move_files: bool,
    /// Copy everything, the inbox included.
    #[arg(long)]
    keep: bool,
    /// Show what would happen; change nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand, Debug)]
enum ReviewCmd {
    /// What waits for review.
    List {
        #[command(flatten)]
        list: ListArgs,
    },
    /// One item: what medrec read and why it is held.
    Show {
        id: String,
        /// All of the extracted text.
        #[arg(long)]
        text: bool,
    },
    /// File an item, with the values it was unsure of (or be asked).
    File {
        id: String,
        #[command(flatten)]
        given: GivenArgs,
    },
    /// Discard an item and its file.
    Drop {
        id: String,
        /// Do not ask.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args, Debug)]
struct ImportArgs {
    /// The provider CLI (`mychart`, `kp`, `uhc`, ...).
    #[arg(long)]
    from: String,
    /// The patient (or member) id at that provider.
    #[arg(long)]
    patient: String,
    /// Whose records they are (remembered for the next import).
    #[arg(long)]
    person: Option<String>,
    /// The provider's organization, for a CLI that serves several.
    #[arg(long)]
    org: Option<String>,
    /// Only what changed since, YYYY-MM-DD (passed to the provider).
    #[arg(long)]
    since: Option<String>,
    /// Import even when the source's birth date differs from the person's.
    #[arg(long)]
    allow_birth_date_mismatch: bool,
}

#[derive(Subcommand, Debug)]
enum ProvidersCmd {
    /// Each known or configured provider CLI and whether it can export.
    List,
}

#[derive(Args, Debug, Clone)]
struct RangeArgs {
    /// From this date, YYYY-MM-DD.
    #[arg(long, value_parser = iso_date)]
    since: Option<String>,
    /// Up to this date, YYYY-MM-DD.
    #[arg(long, value_parser = iso_date)]
    until: Option<String>,
}

/// A `YYYY-MM-DD` flag, checked as clap parses it: a bad date is a usage
/// error before anything is read.
fn iso_date(s: &str) -> Result<String, String> {
    pk_cli_core::dates::parse_iso(s)
        .map(|_| s.to_string())
        .map_err(|e| e.to_string())
}

impl RangeArgs {
    fn range(&self) -> Range {
        Range {
            since: self.since.clone(),
            until: self.until.clone(),
        }
    }
}

/// `--limit N` (DESIGN.md §1.3): every list command takes it. Unset, a
/// list is whole; `search` alone defaults to 50 hits.
#[derive(Args, Debug, Clone)]
struct ListArgs {
    /// At most this many items (1 or more; default: all).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
}

impl ListArgs {
    fn take<'a, T>(&self, rows: &'a [T]) -> &'a [T] {
        match self.limit {
            Some(n) => &rows[..rows.len().min(n as usize)],
            None => rows,
        }
    }
}

#[derive(Args, Debug)]
struct TimelineArgs {
    #[arg(long)]
    person: Option<String>,
    #[command(flatten)]
    range: RangeArgs,
    /// Only these kinds (document, study, note, encounter, result, report,
    /// condition, medication, allergy, immunization, procedure, coverage,
    /// claim); repeatable.
    #[arg(long = "kind", value_parser = clap::builder::PossibleValuesParser::new(views::TIMELINE_KINDS))]
    kinds: Vec<String>,
    #[command(flatten)]
    list: ListArgs,
}

#[derive(Args, Debug)]
struct SearchArgs {
    /// Words to find (all must match, each as a word prefix).
    #[arg(required = true)]
    query: Vec<String>,
    #[arg(long)]
    person: Option<String>,
    #[command(flatten)]
    range: RangeArgs,
    /// At most this many hits (1 or more).
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..))]
    limit: u64,
}

#[derive(Args, Debug)]
struct RecordArgs {
    #[arg(long)]
    person: Option<String>,
    #[command(flatten)]
    range: RangeArgs,
    /// Only current ones (not completed, stopped, resolved or inactive).
    #[arg(long)]
    active: bool,
    #[command(flatten)]
    list: ListArgs,
    /// One test or item over time: its code (LOINC, ICD-10, RxNorm, CVX)
    /// or words of its name.
    #[arg(long, value_name = "CODE|NAME")]
    code: Option<String>,
}

#[derive(Subcommand, Debug)]
enum DocumentsCmd {
    /// Filed documents, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[arg(long = "type", value_name = "TYPE")]
        doc_type: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Open a document from a private local copy.
    Open {
        id: String,
        /// Make the copy and print its path; open nothing.
        #[arg(long)]
        no_launch: bool,
    },
}

#[derive(Subcommand, Debug)]
enum NoteCmd {
    /// Add a note (`-` reads the text from stdin).
    Add {
        person: String,
        text: String,
        /// YYYY-MM-DD (default today).
        #[arg(long)]
        date: Option<String>,
        /// A tag (repeatable, or comma-separated).
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// A document, record or study the note is about (repeatable).
        #[arg(long = "about")]
        about: Vec<String>,
    },
    /// Notes, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
        #[command(flatten)]
        list: ListArgs,
    },
}

#[derive(Subcommand, Debug)]
enum ImagesCmd {
    /// File the studies on a disc, in a folder, a DICOMDIR, a .zip or an
    /// .iso; a study already filed gains only its new images.
    Intake {
        path: PathBuf,
        #[arg(long)]
        person: Option<String>,
        /// File even when the study's birth date differs from the person's.
        #[arg(long)]
        allow_birth_date_mismatch: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Filed studies, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
    /// One study: its series and report.
    Show { study: String },
    /// Open a study in the viewer from a private local copy.
    Open {
        study: String,
        /// Make the copy and print its path; open nothing.
        #[arg(long)]
        no_launch: bool,
    },
    /// Copy a study's files to a new or empty folder.
    Export {
        study: String,
        #[arg(short = 'o', long = "out", value_name = "DIR")]
        out: PathBuf,
    },
}

#[derive(Args, Debug)]
struct VerifyArgs {
    /// Download every document and compare its SHA-256 too.
    #[arg(long)]
    deep: bool,
}

#[derive(Args, Debug)]
struct BackupArgs {
    /// `<rclone remote>:<path>` or a local directory.
    #[arg(long)]
    to: String,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Print the settings.
    Show,
    /// Set a setting.
    Set { key: String, value: String },
    /// Remove a setting.
    Unset { key: String },
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Report credential state: none is needed.
    Status,
}

#[derive(Subcommand, Debug)]
enum CacheCmd {
    /// Remove every copy medrec keeps on this machine (the records folder
    /// itself is untouched). The next command reads from Drive again.
    Clear,
    /// The background refresh a stale cached read starts.
    #[command(hide = true)]
    Sync {
        #[arg(long, value_name = "REL", conflicts_with = "revalidate_listing")]
        revalidate_file: Option<String>,
        #[arg(long, value_name = "REL")]
        revalidate_listing: Option<String>,
        #[arg(long, hide = true)]
        remote_spec: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let json = cli.common.json;
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, json));
    }
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("medrec's DTOs serialize to JSON")
}

fn items<T: Serialize>(rows: &[T]) -> Value {
    json!({ "items": to_value(&rows) })
}

fn table(v: &Value, columns: &[&str]) {
    output::table(&output::table_view(&output::rows_of(v, "items"), columns));
}

fn note(cli: &Cli, msg: &str) {
    if !cli.common.quiet && !cli.common.json {
        eprintln!("{msg}");
    }
}

fn load(store: &ConfigStore) -> Result<Config, CliError> {
    Config::load(store)
}

fn run(cli: &Cli) -> Result<(), CliError> {
    let json = cli.common.json;
    let store = ConfigStore::new(BIN);
    match &cli.command {
        Command::Init(a) => {
            let mut cfg = load(&store)?;
            let loc = Location::parse(&a.location);
            let budget = cfg.rclone_budget();
            let r = maint::init(&mut cfg, &loc, a.allow_other_files, budget)?;
            store.save(&cfg)?;
            output::emit(json, "init", to_value(&r), |v| {
                println!(
                    "{} records folder {}",
                    v["status"].as_str().unwrap_or(""),
                    v["folder"].as_str().unwrap_or("")
                );
                if r.people == 0 {
                    println!("Next: medrec people add \"<name>\" --birth-date YYYY-MM-DD");
                }
            });
            Ok(())
        }
        Command::People(cmd) => people_cmd(cli, cmd, &store),
        Command::Intake(a) => intake_cmd(cli, a, &store),
        Command::Review(cmd) => review_cmd(cli, cmd, &store),
        Command::Import(a) => import_cmd(cli, a, &store),
        Command::Providers(ProvidersCmd::List) => {
            let cfg = load(&store)?;
            let rows = import::providers(&cfg);
            let v = items(&rows);
            output::emit(json, "provider-list", v, |v| {
                table(v, &["cli", "status", "version", "note"])
            });
            Ok(())
        }
        Command::Timeline(a) => {
            let (f, h) = read(&store)?;
            let rows = views::timeline(&f, &h, a.person.as_deref(), &a.range.range(), &a.kinds)?;
            output::emit(json, "timeline", items(a.list.take(&rows)), |v| {
                table(
                    v,
                    &["date", "person_name", "kind", "title", "detail", "source"],
                )
            });
            Ok(())
        }
        Command::Search(a) => {
            let (f, h) = read(&store)?;
            let q = a.query.join(" ");
            let rows = views::search(
                &f,
                &h,
                a.person.as_deref(),
                &q,
                &a.range.range(),
                a.limit as usize,
            )?;
            output::emit(json, "search", items(&rows), |v| {
                table(
                    v,
                    &["date", "person_name", "kind", "id", "title", "snippet"],
                )
            });
            Ok(())
        }
        Command::Results(a) => records_cmd(cli, &store, a, &["result", "report"], "result"),
        Command::Meds(a) => records_cmd(cli, &store, a, &["medication"], "medication"),
        Command::Conditions(a) => records_cmd(cli, &store, a, &["condition"], "condition"),
        Command::Allergies(a) => records_cmd(cli, &store, a, &["allergy"], "allergy"),
        Command::Immunizations(a) => records_cmd(cli, &store, a, &["immunization"], "immunization"),
        Command::Documents(cmd) => documents_cmd(cli, cmd, &store),
        Command::Note(cmd) => note_cmd(cli, cmd, &store),
        Command::Images(cmd) => images_cmd(cli, cmd, &store),
        Command::Doctor => {
            let cfg = load(&store)?;
            let checks = maint::doctor(&cfg);
            let failed = checks.iter().filter(|c| c.status == "fail").count();
            output::emit(
                json,
                "doctor",
                json!({ "ok": failed == 0, "checks": to_value(&checks) }),
                |v| {
                    output::table(&output::table_view(
                        &output::rows_of(v, "checks"),
                        &["check", "status", "detail"],
                    ))
                },
            );
            if failed > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Verify(a) => {
            let cfg = load(&store)?;
            let f = Folder::open(&cfg, Access::Read)?;
            let r = maint::verify(&f, a.deep)?;
            let clean = r.clean();
            output::emit(json, "verify", to_value(&r), |v| {
                println!(
                    "checked {} documents, {} study files, {} raw records, {} review items: {} ok",
                    r.documents, r.study_files, r.raw_records, r.review_items, r.ok
                );
                for (label, key) in [
                    ("missing", "missing"),
                    ("changed", "changed"),
                    ("not indexed", "unindexed"),
                ] {
                    for p in v[key].as_array().into_iter().flatten() {
                        println!("{label}: {}", p.as_str().unwrap_or(""));
                    }
                }
            });
            if !clean {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Backup(a) => {
            let cfg = load(&store)?;
            let f = Folder::open(&cfg, Access::Read)?;
            let r = maint::backup(&f, &Location::parse(&a.to), cfg.rclone_budget())?;
            let clean = r.mismatched.is_empty();
            output::emit(json, "backup", to_value(&r), |_| {
                println!(
                    "copied {} files ({} bytes) to {}: {} verified",
                    r.files, r.bytes, r.to, r.verified
                );
                for m in &r.mismatched {
                    println!("mismatched: {m}");
                }
            });
            if !clean {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Config(cmd) => config_cmd(cli, cmd, &store),
        Command::Auth(AuthCmd::Status) => {
            AuthStatus::not_required().emit(json);
            Ok(())
        }
        Command::SelfUpdate(args) => Updater {
            repo: REPO.into(),
            binary: BIN.into(),
            target: env!("BUILD_TARGET").into(),
            current: env!("CARGO_PKG_VERSION").into(),
        }
        .run(args, json, cli.common.quiet),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            Ok(())
        }
        Command::Info => {
            let info = CliInfo::new(
                BIN,
                env!("CARGO_PKG_VERSION"),
                &format!("https://github.com/{REPO}"),
                AuthInfo {
                    required: false,
                    method: "none".into(),
                    login_hint: None,
                },
                &[],
            );
            output::json(&to_value(&info));
            Ok(())
        }
        Command::Cache(CacheCmd::Clear) => {
            let r = maint::clear_local_copies()?;
            output::emit(json, "cache-clear", to_value(&r), |v| {
                println!(
                    "removed {} files ({} bytes) from {}",
                    v["files"],
                    v["bytes"],
                    v["path"].as_str().unwrap_or("")
                )
            });
            Ok(())
        }
        Command::Cache(CacheCmd::Sync {
            revalidate_file,
            revalidate_listing,
            remote_spec,
        }) => {
            let cfg = load(&store)?;
            let spec = match (remote_spec, cfg.folder_location()) {
                (Some(s), _) => s.clone(),
                (None, Ok(Location::Remote(s))) => s,
                _ => {
                    return Err(CliError::Usage(
                        "the records folder is not an rclone remote, so it has no read cache"
                            .into(),
                    ))
                }
            };
            if let Some((listing, rel)) = revalidate_file
                .as_ref()
                .map(|r| (false, r))
                .or(revalidate_listing.as_ref().map(|r| (true, r)))
            {
                let outcome = store::revalidate(&spec, listing, rel, cfg.rclone_budget())?;
                output::emit(
                    json,
                    "cache-sync",
                    json!({ "rel": rel, "outcome": outcome }),
                    |_| {},
                );
                return Ok(());
            }
            Err(CliError::Usage(
                "nothing to do: pass --revalidate-file or --revalidate-listing".into(),
            ))
        }
    }
}

fn read(store: &ConfigStore) -> Result<(Folder, medrec::model::Household), CliError> {
    let cfg = load(store)?;
    let f = Folder::open(&cfg, Access::Read)?;
    let h = f.household()?;
    Ok((f, h))
}

fn people_cmd(cli: &Cli, cmd: &PeopleCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        PeopleCmd::Add(a) => {
            let cfg = load(store)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let p = people::add(
                &f,
                people::NewPerson {
                    name: a.name.clone(),
                    birth_date: a.birth_date.clone(),
                    adult: if a.adult {
                        Some(true)
                    } else if a.child {
                        Some(false)
                    } else {
                        None
                    },
                    guardians: a.guardians.clone(),
                    aliases: a.aliases.clone(),
                    huishouden_health_person: a.huishouden_health_person.clone(),
                    baby_profile: a.baby_profile.clone(),
                },
            )?;
            output::emit(json, "person", to_value(&p), |_| {
                println!("added {} ({}), folder `{}`", p.name, p.id, p.folder)
            });
            Ok(())
        }
        PeopleCmd::List { list } => {
            let (_, h) = read(store)?;
            output::emit(json, "person-list", items(list.take(&h.people)), |v| {
                table(v, &["id", "name", "birth_date", "adult", "folder"])
            });
            Ok(())
        }
        PeopleCmd::Show { person } => {
            let cfg = load(store)?;
            let f = Folder::open(&cfg, Access::Read)?;
            let d = people::show(&f, person)?;
            output::emit_one(json, "person", to_value(&d));
            Ok(())
        }
        PeopleCmd::Edit(a) => {
            let cfg = load(store)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let p = people::edit(
                &f,
                &a.person,
                people::Edit {
                    name: a.name.clone(),
                    birth_date: a.birth_date.clone(),
                    adult: if a.adult {
                        Some(true)
                    } else if a.child {
                        Some(false)
                    } else {
                        None
                    },
                    add_guardians: a.add_guardians.clone(),
                    remove_guardians: a.remove_guardians.clone(),
                    add_aliases: a.add_aliases.clone(),
                    remove_aliases: a.remove_aliases.clone(),
                    add_identifiers: a.add_identifiers.clone(),
                    remove_identifiers: a.remove_identifiers.clone(),
                    huishouden_health_person: a.huishouden_health_person.clone(),
                    baby_profile: a.baby_profile.clone(),
                    clear_links: a.clear_links,
                },
            )?;
            output::emit_one(json, "person", to_value(&p));
            Ok(())
        }
    }
}

fn given(g: &GivenArgs, classified_by: Option<String>) -> Given {
    Given {
        person: g.person.clone(),
        doc_type: g.doc_type.clone(),
        date: g.date.clone(),
        organization: g.org.clone(),
        classified_by,
    }
}

fn intake_cmd(cli: &Cli, a: &IntakeArgs, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    if a.files.is_empty() && !a.inbox && a.drive.is_none() {
        return Err(CliError::Usage(
            "name files, or pass --inbox or --drive <folder>".into(),
        ));
    }
    // Arguments first: a bad --type or --date never costs a folder read.
    let given = given(&a.given, a.classified_by.clone());
    intake::validate_given(&given)?;
    let cfg = load(store)?;
    let inbox = if a.inbox {
        Some(cfg.inbox_location()?)
    } else {
        None
    };
    let f = Folder::open(
        &cfg,
        if a.dry_run {
            Access::Read
        } else {
            Access::Write
        },
    )?;
    let req = intake::Request {
        files: a.files.clone(),
        inbox,
        drive: a.drive.as_deref().map(Location::parse),
        given,
        keep: a.keep,
        move_files: a.move_files,
        dry_run: a.dry_run,
        interactive: cli.common.interactive(),
    };
    let (report, ok) = intake::run(&f, &cfg, &req)?;
    let v = to_value(&report);
    output::emit(json, "intake", v, |v| {
        output::table(&output::table_view(
            &output::rows_of(v, "items"),
            &[
                "status",
                "source",
                "person_name",
                "doc_type",
                "date",
                "path",
                "reasons",
                "error",
            ],
        ));
        let counts: Vec<String> = report
            .counts
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect();
        if !counts.is_empty() {
            println!("{}", counts.join(", "));
        }
    });
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

fn review_cmd(cli: &Cli, cmd: &ReviewCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let cfg = load(store)?;
    match cmd {
        ReviewCmd::List { list } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let rows = review::list(&f)?;
            output::emit(json, "review-list", items(list.take(&rows)), |v| {
                table(v, &["id", "received_at", "original_name", "reasons"])
            });
        }
        ReviewCmd::Show { id, text } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let d = review::show(&f, id, *text)?;
            output::emit_one(json, "review-item", to_value(&d));
        }
        ReviewCmd::File { id, given: g } => {
            let given = given(g, None);
            intake::validate_given(&given)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let r = review::file(
                &f,
                &review::FileRequest {
                    id: id.clone(),
                    given,
                    interactive: cli.common.interactive(),
                },
            )?;
            output::emit(json, "review-filed", to_value(&r), |_| {
                println!(
                    "{} {} as {} for {}: {}",
                    r.status, r.review, r.doc_type, r.person_name, r.path
                )
            });
        }
        ReviewCmd::Drop { id, force } => {
            confirm::gate(
                &cli.common,
                *force,
                &format!("Discard review item {id} and its file?"),
            )?;
            let f = Folder::open(&cfg, Access::Write)?;
            let r = review::drop(&f, id)?;
            output::emit(json, "review-dropped", to_value(&r), |_| {
                println!("dropped {}", r.review)
            });
        }
    }
    Ok(())
}

fn import_cmd(cli: &Cli, a: &ImportArgs, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    if let Some(s) = &a.since {
        pk_cli_core::dates::parse_iso(s)?;
    }
    let cfg = load(store)?;
    let provider = import::probe(&a.from);
    if !provider.usable() {
        return Err(CliError::Usage(format!(
            "`{}` cannot export records ({}{})",
            a.from,
            provider.status,
            provider
                .note
                .as_deref()
                .map(|n| format!(": {n}"))
                .unwrap_or_default()
        )));
    }
    let f = Folder::open(&cfg, Access::Write)?;
    note(cli, &format!("importing from {} ...", a.from));
    let r = import::run(
        &f,
        &cfg,
        &provider,
        &import::Request {
            from: a.from.clone(),
            patient: a.patient.clone(),
            person: a.person.clone(),
            since: a.since.clone(),
            org: a.org.clone(),
            allow_birth_date_mismatch: a.allow_birth_date_mismatch,
        },
    )?;
    output::emit(json, "import", to_value(&r), |_| {
        println!(
            "{} resources from {} for {}: {} records added, {} updated, {} unchanged; {} documents added",
            r.resources, r.organization, r.person_name, r.records_added, r.records_updated, r.records_unchanged, r.documents_added
        );
        match r.skipped {
            0 => {}
            1 => println!("1 resource or search was skipped by the provider or as too large"),
            n => println!("{n} resources or searches were skipped by the provider or as too large"),
        }
    });
    Ok(())
}

fn records_cmd(
    cli: &Cli,
    store: &ConfigStore,
    a: &RecordArgs,
    kinds: &[&str],
    record: &str,
) -> Result<(), CliError> {
    let (f, h) = read(store)?;
    let rows = views::records(
        &f,
        &h,
        a.person.as_deref(),
        kinds,
        &a.range.range(),
        a.active,
        a.code.as_deref(),
    )?;
    output::emit(
        cli.common.json,
        &format!("{record}-list"),
        items(a.list.take(&rows)),
        |v| {
            table(
                v,
                &[
                    "date",
                    "person_name",
                    "display",
                    "detail",
                    "status",
                    "sources",
                ],
            )
        },
    );
    Ok(())
}

/// Copy one file into a fresh private open folder and return its path.
fn private_copy(f: &Folder, rel: &str, label: &str) -> Result<PathBuf, CliError> {
    let dir = view::new_open_dir(label)?;
    let name = rel.rsplit('/').next().unwrap_or("document");
    let to = dir.join(name);
    let bytes = f.store.read_bytes(rel)?;
    medrec::write_private(&to, &bytes)?;
    Ok(to)
}

fn documents_cmd(cli: &Cli, cmd: &DocumentsCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        DocumentsCmd::List {
            person,
            doc_type,
            range,
            list,
        } => {
            let (f, h) = read(store)?;
            let rows = views::documents(
                &f,
                &h,
                person.as_deref(),
                doc_type.as_deref(),
                &range.range(),
            )?;
            output::emit(json, "document-list", items(list.take(&rows)), |v| {
                table(
                    v,
                    &[
                        "id",
                        "date",
                        "person_name",
                        "doc_type",
                        "organization",
                        "title",
                    ],
                )
            });
        }
        DocumentsCmd::Open { id, no_launch } => {
            let (f, h) = read(store)?;
            let (_, d) = views::find_document(&f, &h, id)?;
            let path = private_copy(&f, &d.path, &d.id)?;
            let launched = !*no_launch && launch(&view::Viewer::for_documents(None), &path)?;
            output::emit(
                json,
                "document-open",
                json!({ "document": d.id, "path": path.display().to_string(), "launched": launched }),
                |v| println!("{}", v["path"].as_str().unwrap_or("")),
            );
        }
    }
    Ok(())
}

fn launch(viewer: &view::Viewer, path: &std::path::Path) -> Result<bool, CliError> {
    match viewer.command(path) {
        Some(cmd) => view::launch(&cmd).map(|_| true),
        None => Ok(false),
    }
}

fn note_cmd(cli: &Cli, cmd: &NoteCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let cfg = load(store)?;
    match cmd {
        NoteCmd::Add {
            person,
            text,
            date,
            tags,
            about,
        } => {
            let text = if text == "-" {
                let mut s = String::new();
                std::io::stdin()
                    .take(1024 * 1024)
                    .read_to_string(&mut s)
                    .map_err(|e| CliError::Usage(format!("reading the note from stdin: {e}")))?;
                s
            } else {
                text.clone()
            };
            let f = Folder::open(&cfg, Access::Write)?;
            let h = f.household()?;
            let p = people::resolve(&h, person)?.clone();
            let author = cfg
                .me
                .as_deref()
                .and_then(|m| h.person(m))
                .map(|m| m.name.clone());
            let n = notes::add(
                &f,
                &p,
                &text,
                date.as_deref(),
                tags.clone(),
                about.clone(),
                author,
            )?;
            output::emit(
                json,
                "note",
                json!({ "person": p.id, "person_name": p.name, "note": to_value(&n) }),
                |_| println!("added note {} for {} on {}", n.id, p.name, n.date),
            );
        }
        NoteCmd::List {
            person,
            tag,
            range,
            list,
        } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let rows = views::notes(&f, &h, person.as_deref(), tag.as_deref(), &range.range())?;
            output::emit(json, "note-list", items(list.take(&rows)), |v| {
                table(v, &["date", "person_name", "tags", "text"])
            });
        }
    }
    Ok(())
}

fn images_cmd(cli: &Cli, cmd: &ImagesCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let cfg = load(store)?;
    match cmd {
        ImagesCmd::Intake {
            path,
            person,
            allow_birth_date_mismatch,
            dry_run,
        } => {
            let f = Folder::open(
                &cfg,
                if *dry_run {
                    Access::Read
                } else {
                    Access::Write
                },
            )?;
            let r = imaging::intake(
                &f,
                &imaging::IntakeRequest {
                    path: path.clone(),
                    person: person.clone(),
                    allow_birth_date_mismatch: *allow_birth_date_mismatch,
                    dry_run: *dry_run,
                    interactive: cli.common.interactive(),
                },
            )?;
            output::emit(json, "image-intake", to_value(&r), |v| {
                println!(
                    "{} files: {} DICOM, {} other, {} unreadable",
                    r.files_seen, r.dicom_files, r.other_files, r.unreadable
                );
                output::table(&output::table_view(
                    &output::rows_of(v, "studies"),
                    &[
                        "status",
                        "date",
                        "modalities",
                        "person_name",
                        "instances_new",
                        "instances_existing",
                        "path",
                        "reason",
                    ],
                ));
                for d in &r.documents_found {
                    println!("not filed (a document; use `medrec intake`): {d}");
                }
            });
        }
        ImagesCmd::List { person, list } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let rows = imaging::list(&f, &h, person.as_deref())?;
            output::emit(json, "study-list", items(list.take(&rows)), |v| {
                table(
                    v,
                    &[
                        "id",
                        "date",
                        "person_name",
                        "modalities",
                        "body_part",
                        "facility",
                        "series",
                        "instances",
                    ],
                )
            });
        }
        ImagesCmd::Show { study } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let (p, s) = imaging::find(&f, &h, study)?;
            output::emit_one(
                json,
                "study",
                json!({ "person": p.id, "person_name": p.name, "study": to_value(&s) }),
            );
        }
        ImagesCmd::Open { study, no_launch } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let (_, s) = imaging::find(&f, &h, study)?;
            let dir = view::new_open_dir(&s.id)?;
            f.store.fetch_tree(&s.path, &dir)?;
            let launched =
                !*no_launch && launch(&view::Viewer::for_images(cfg.viewer.as_deref()), &dir)?;
            output::emit(
                json,
                "study-open",
                json!({ "study": s.id, "path": dir.display().to_string(), "launched": launched }),
                |v| println!("{}", v["path"].as_str().unwrap_or("")),
            );
        }
        ImagesCmd::Export { study, out } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let (_, s) = imaging::find(&f, &h, study)?;
            let r = imaging::export(&f, &s, out)?;
            output::emit(json, "study-export", to_value(&r), |_| {
                println!(
                    "exported {} files ({} bytes) to {}",
                    r.files, r.bytes, r.path
                )
            });
        }
    }
    Ok(())
}

fn config_cmd(cli: &Cli, cmd: &ConfigCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        ConfigCmd::Path => {
            let path = store.path()?;
            output::emit(
                json,
                "config-path",
                json!({ "path": path.display().to_string() }),
                |v| println!("{}", v["path"].as_str().unwrap_or("")),
            );
        }
        ConfigCmd::Show => {
            let cfg = load(store)?;
            output::emit_one(json, "config", to_value(&cfg));
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg = load(store)?;
            cfg.set(key, value)?;
            store.save(&cfg)?;
            output::emit_one(json, "config", to_value(&cfg));
        }
        ConfigCmd::Unset { key } => {
            let mut cfg = load(store)?;
            cfg.unset(key)?;
            store.save(&cfg)?;
            output::emit_one(json, "config", to_value(&cfg));
        }
    }
    let _ = KEYS;
    Ok(())
}
