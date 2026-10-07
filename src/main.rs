use std::{
    fmt::Write as _,
    fs::File,
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use chrono::Local;
use clap::{Parser, Subcommand};
use optmem::{
    Block, CompressionRequest, ConfigKey, Error, OptMem, Result, SummaryStatus, Wake,
    settings::AppSettings, storage::FileStorage,
};

#[derive(Parser)]
#[command(
    name = "memo",
    version,
    about = "Permanent, append-only memory for AI agents"
)]
struct Cli {
    /// User config file; defaults to $XDG_CONFIG_HOME/optmem/config.toml
    #[arg(long = "config", global = true, env = "OPTMEM_CONFIG")]
    config_file: Option<PathBuf>,
    /// Override storage_dir from the user config for this command
    #[arg(long, global = true, env = "MEMORY_DIR")]
    memory_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the memory explicitly and print the agent setup block
    Init,
    /// Read memory, oldest first; continue pages using the printed snapshot
    Wake {
        #[arg(default_value_t = 1)]
        part: usize,
        snapshot: Option<u64>,
    },
    /// Record one UTF-8 line, at most ENTRY_CHARS bytes
    Note { text: String },
    /// Show the next compression, or submit a summary for it
    Nap {
        #[arg(requires = "text")]
        block: Option<Block>,
        #[arg(requires = "block")]
        text: Option<String>,
    },
    /// Search the full raw log with a case-insensitive regex
    Recall {
        #[arg(allow_hyphen_values = true)]
        regex: String,
    },
    /// Open an aligned lo-hi block into its two halves
    Zoom { block: Block },
    /// Drop a summary and its dependent suffixes; never edits raw memories
    Forget { block: Block },
    /// Show/edit user settings; storage_dir=PATH or NAME=VALUE; NAME= resets
    Config { changes: Vec<String> },
    /// Bulk append dated lines: YYYY-MM-DD followed by text
    Import { file: PathBuf },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut output = String::new();
    let result = run(cli, &mut output);
    // Emit acknowledged writes even if generating the following nap prompt fails.
    let mut stdout = io::stdout().lock();
    if let Err(e) = stdout
        .write_all(output.as_bytes())
        .and_then(|_| stdout.flush())
    {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return ExitCode::SUCCESS;
        }
        eprintln!("stdout: {e}");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-:\\".contains(&b))
    {
        return value.into_owned();
    }
    #[cfg(windows)]
    {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn run(cli: Cli, out: &mut String) -> Result<bool> {
    let settings = AppSettings::load(cli.config_file.clone())?;
    if let Command::Config { changes } = &cli.command {
        let settings = if changes.is_empty() {
            settings
        } else {
            settings.update(changes, cli.memory_dir.as_deref())?
        };
        let dir = settings.storage_dir(cli.memory_dir.as_deref())?;
        let config = settings.memory_config(&dir)?;
        writeln!(
            out,
            "config_file  {}\nstorage_dir  {}",
            settings.path().display(),
            dir.display()
        )
        .unwrap();
        for (key, value) in [
            (ConfigKey::WakeLines, config.wake_lines),
            (ConfigKey::EntryChars, config.entry_chars),
            (ConfigKey::PartChars, config.part_chars),
            (ConfigKey::PartLines, config.part_lines),
        ] {
            writeln!(
                out,
                "{:<12} {value:<7} {}",
                key.to_string().to_ascii_lowercase(),
                key.description()
            )
            .unwrap();
        }
        return Ok(true);
    }
    let dir = settings.storage_dir(cli.memory_dir.as_deref())?;
    let mut me = String::from("memo");
    if cli.config_file.is_some() {
        write!(me, " --config {}", shell_quote(settings.path())).unwrap();
    }
    if cli.memory_dir.is_some() {
        write!(me, " --memory-dir {}", shell_quote(&dir)).unwrap();
    }
    let fresh = !dir.is_dir();
    let storage = if matches!(cli.command, Command::Init) {
        FileStorage::initialize(&dir)?
    } else {
        FileStorage::open(&dir).map_err(|e| match e {
            Error::MissingStore(_) => Error::Invalid(format!("No memory at {}.\nTo create one, run: {me} init\nTo use an existing one, point MEMORY_DIR at it.", dir.display())),
            other => other,
        })?
    };
    let config = settings.memory_config(&dir)?;
    let settings = if matches!(cli.command, Command::Init) {
        settings.initialize(&dir, &config)?
    } else {
        settings
    };
    let memory = OptMem::with_config(storage, config)?;
    match cli.command {
        Command::Init => {
            if fresh {
                writeln!(out, "Created {}: this machine's memory.", dir.display()).unwrap();
            } else {
                writeln!(
                    out,
                    "Found {}: {}.",
                    dir.display(),
                    plural(memory.len()?, "memory")
                )
                .unwrap();
            }
            writeln!(
                out,
                "Settings live in {}. Inspect them with memo config.",
                settings.path().display()
            )
            .unwrap();
            if cli.config_file.is_some() {
                writeln!(
                    out,
                    "For this config profile, set OPTMEM_CONFIG={} in your agent's environment.",
                    shell_quote(settings.path())
                )
                .unwrap();
            }
            out.push_str("\nPaste this at the top of your agent's AGENTS.md (or CLAUDE.md):\n\n");
            out.push_str(include_str!("setup.md"));
        }
        Command::Note { text } => {
            let entry = memory.note(Local::now().date_naive(), &text)?;
            writeln!(out, "Saved as #{}.", entry.id).unwrap();
            if let Some(request) = memory
                .next_compression(Some(entry.id + 1))
                .map_err(|e| recovery(e, &me))?
            {
                out.push('\n');
                prompt(out, &request, &me);
            }
        }
        Command::Nap { block, text } => {
            if let (Some(block), Some(text)) = (block, text) {
                match memory
                    .submit_summary(block, &text)
                    .map_err(|e| recovery(e, &me))?
                {
                    SummaryStatus::Saved => writeln!(out, "{block} saved.").unwrap(),
                    SummaryStatus::AlreadySettled => {
                        writeln!(out, "{block} is already settled.").unwrap()
                    }
                    SummaryStatus::NothingPending => {
                        out.push_str("Nothing left to compress.\n");
                        return Ok(true);
                    }
                }
                out.push('\n');
            }
            if let Some(request) = memory
                .next_compression(None)
                .map_err(|e| recovery(e, &me))?
            {
                prompt(out, &request, &me);
            } else {
                out.push_str("Nothing left to compress.\n");
            }
        }
        Command::Wake { part, snapshot } => {
            match memory.wake(part, snapshot).map_err(|e| recovery(e, &me))? {
                Wake::NeedsCompression { missing, request } => {
                    writeln!(out, "Cannot wake: the memory context needs #{missing}, which is not compressed yet.\nDo the {} below, then run {me} wake again.\n", plural(request.remaining + 1, "compression")).unwrap();
                    prompt(out, &request, &me);
                    return Ok(false);
                }
                Wake::Ready(page) => {
                    if page.snapshot == 0 {
                        writeln!(
                            out,
                            "No memories yet. Record the first with: {me} note \"<one line>\""
                        )
                        .unwrap();
                    }
                    if page.total_parts > 1 {
                        writeln!(
                            out,
                            "Your memory, part {} of {}, oldest first ({}).",
                            page.part,
                            page.total_parts,
                            plural(page.snapshot, "memory")
                        )
                        .unwrap();
                    }
                    for entry in &page.entries {
                        writeln!(out, "{entry}").unwrap();
                    }
                    if page.part < page.total_parts {
                        writeln!(
                            out,
                            "Not awake yet. Run: {me} wake {} {}",
                            page.part + 1,
                            page.snapshot
                        )
                        .unwrap();
                    } else {
                        out.push_str("You are awake.\n");
                        if let Some(request) = page.next_compression {
                            out.push('\n');
                            prompt(out, &request, &me);
                        }
                    }
                }
            }
        }
        Command::Recall { regex } => {
            let result = memory.recall(&regex)?;
            if result.total_matches == 0 {
                out.push_str("No match.\n");
            } else {
                for entry in &result.entries {
                    writeln!(out, "{entry}").unwrap();
                }
                if (result.entries.len() as u64) < result.total_matches {
                    writeln!(
                        out,
                        "Newest {} of {}. Narrow the regex.",
                        result.entries.len(),
                        plural(result.total_matches, "match")
                    )
                    .unwrap();
                } else {
                    writeln!(out, "{}.", plural(result.total_matches, "match")).unwrap();
                }
            }
        }
        Command::Zoom { block } => {
            for entry in memory.zoom(block).map_err(|e| recovery(e, &me))? {
                writeln!(out, "{entry}").unwrap();
            }
        }
        Command::Forget { block } => {
            let gone = memory.forget(block)?;
            writeln!(
                out,
                "Forgot {}, from {} up. Run: {me} nap",
                plural(gone.len() as u64, "summary"),
                gone[0]
            )
            .unwrap();
        }
        Command::Config { .. } => {
            unreachable!("config is handled without opening storage");
        }
        Command::Import { file } => {
            let source = File::open(&file).map_err(|source| Error::Io { path: file, source })?;
            let result = memory.import(BufReader::new(source))?;
            writeln!(
                out,
                "Imported {}, #{} to #{}.",
                plural(result.count, "memory"),
                result.first_id,
                result.first_id + result.count - 1
            )
            .unwrap();
            if result.pending > 0 {
                writeln!(
                    out,
                    "{} pending. Run: {me} nap",
                    plural(result.pending, "compression")
                )
                .unwrap();
            }
        }
    }
    Ok(true)
}

fn recovery(error: Error, me: &str) -> Error {
    match error {
        Error::CorruptSummary { block, reason } => Error::Invalid(format!(
            "The summary of #{block} is {reason}. Run: {me} forget {block}"
        )),
        Error::WrongBlock { requested, next } => Error::Invalid(format!(
            "Wrong block: {requested}. Blocks are built in order; the next is {next}. Run: {me} nap"
        )),
        Error::FutureSnapshot { snapshot, count } => Error::Invalid(format!(
            "T={snapshot}, but the log holds {}. Run: {me} wake",
            plural(count, "memory")
        )),
        other => other,
    }
}

fn prompt(out: &mut String, request: &CompressionRequest, me: &str) {
    writeln!(out, "Compress memories #{} into one line of at most {} bytes.\nKeep what has lasting effect, drop what does not. Invent nothing.\n", request.block, request.max_bytes).unwrap();
    for entry in &request.sources {
        writeln!(out, "  {entry}").unwrap();
    }
    out.push('\n');
    if request.remaining > 0 {
        writeln!(
            out,
            "{} after this one.",
            if request.remaining == 1 {
                "1 compression remains".into()
            } else {
                format!("{} compressions remain", request.remaining)
            }
        )
        .unwrap();
    }
    writeln!(out, "Run: {me} nap {} \"<your line>\"", request.block).unwrap();
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        return format!("1 {word}");
    }
    if let Some(stem) = word.strip_suffix('y') {
        format!("{n} {stem}ies")
    } else if word.ends_with(['s', 'h', 'x']) {
        format!("{n} {word}es")
    } else {
        format!("{n} {word}s")
    }
}
