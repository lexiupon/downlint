pub mod check;
pub mod sync;

use crate::diagnostics::DiagnosticSeverity;
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "downlint")]
#[command(about = "Markdown checker and language server")]
#[command(version = crate::version::VERSION)]
#[command(propagate_version = true)]
#[command(help_template = "\
{name} {version}
{about-with-newline}\
\n{usage-heading} {usage}\n\n\
{all-args}")]
struct Cli {
    #[command(flatten)]
    check: CheckArgs,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(global = true, short = 'q', long = "quiet", action = ArgAction::SetTrue)]
    quiet: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    Check(CheckArgs),
    Server(ServerArgs),
    Sync(SyncArgs),
}

#[derive(Args, Clone, Debug, Default)]
struct CheckArgs {
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long, default_value = "text")]
    format: FormatArg,
    #[arg(long = "min-severity", default_value = "warning")]
    min_severity: SeverityArg,
    #[arg(long, default_value = "auto")]
    color: String,
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    #[arg(long)]
    fix: bool,
    #[arg(long, short = 'w')]
    watch: bool,
    #[arg(long)]
    stdin: bool,
    /// Permit subprocess execution of `sync_cmd` entries defined under
    /// `[uri.mappings]`. Without this flag, sync is skipped and a one-time
    /// info diagnostic is emitted per configured mapping. Note: enabling this
    /// flag means `.downlint.toml` controls which commands run.
    #[arg(long = "allow-uri-sync", action = ArgAction::SetTrue)]
    allow_uri_sync: bool,
    /// Suppress the "no URI mapping found" hint diagnostic. The broken-link
    /// diagnostic itself is still emitted.
    #[arg(long = "no-uri-hints", action = ArgAction::SetTrue)]
    no_uri_hints: bool,
    /// Batch size for per-file `sync_cmd` invocations across external
    /// mappings. Controls how many `{path}` placeholders are fanned out per
    /// spawned subprocess. Lower values reduce memory; higher values reduce
    /// fork overhead.
    #[arg(long = "uri-sync-batch-size", default_value_t = 50)]
    uri_sync_batch_size: usize,
    path: Option<PathBuf>,
}

#[derive(Args, Clone, Debug, Default)]
struct ServerArgs {
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    #[arg(long)]
    wait_for_debugger: bool,
    /// Permit subprocess execution of `sync_cmd` entries defined under
    /// `[uri.mappings]`. Without this flag, sync is skipped and a one-time
    /// info diagnostic is emitted per configured mapping. Note: enabling this
    /// flag means `.downlint.toml` controls which commands run.
    #[arg(long = "allow-uri-sync", action = ArgAction::SetTrue)]
    allow_uri_sync: bool,
    /// Suppress the "no URI mapping found" hint diagnostic. The broken-link
    /// diagnostic itself is still emitted.
    #[arg(long = "no-uri-hints", action = ArgAction::SetTrue)]
    no_uri_hints: bool,
    /// Batch size for per-file `sync_cmd` invocations across external
    /// mappings.
    #[arg(long = "uri-sync-batch-size", default_value_t = 50)]
    uri_sync_batch_size: usize,
}

#[derive(Args, Clone, Debug, Default)]
struct SyncArgs {
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    /// REQUIRED for sync. Without this flag, the subcommand exits 2 with a
    /// clear error message. Same semantics as `downlint check --allow-uri-sync`.
    #[arg(long = "allow-uri-sync", action = ArgAction::SetTrue)]
    allow_uri_sync: bool,
    /// Batch size for per-file `sync_cmd` invocations across external mappings.
    #[arg(long = "uri-sync-batch-size", default_value_t = 50)]
    uri_sync_batch_size: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum FormatArg {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum SeverityArg {
    Info,
    #[default]
    Warning,
    Error,
}

pub async fn run() -> i32 {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Server(args)) => {
            init_tracing(args.verbose);
            let uri_opts = crate::resolution::UriOptions {
                allow_sync: args.allow_uri_sync,
                no_hints: args.no_uri_hints,
                batch_size: args.uri_sync_batch_size.max(1),
            };
            crate::lsp::run_server(args.verbose, args.wait_for_debugger, uri_opts).await
        }
        Some(Command::Check(args)) => {
            init_tracing(args.verbose);
            check::run_check(map_check_args(args, cli.quiet)).await
        }
        Some(Command::Sync(args)) => {
            init_tracing(args.verbose);
            sync::run_sync(map_sync_args(args, cli.quiet)).await
        }
        None => {
            init_tracing(cli.check.verbose);
            check::run_check(map_check_args(cli.check, cli.quiet)).await
        }
    }
}

fn map_check_args(args: CheckArgs, quiet: bool) -> check::CheckOptions {
    check::CheckOptions {
        root: args.root,
        format: match args.format {
            FormatArg::Text => check::OutputFormat::Text,
            FormatArg::Json => check::OutputFormat::Json,
        },
        min_severity: match args.min_severity {
            SeverityArg::Info => DiagnosticSeverity::Info,
            SeverityArg::Warning => DiagnosticSeverity::Warning,
            SeverityArg::Error => DiagnosticSeverity::Error,
        },
        color: args.color,
        verbose: args.verbose,
        fix: args.fix,
        watch: args.watch,
        stdin: args.stdin,
        quiet,
        path: args.path,
        allow_uri_sync: args.allow_uri_sync,
        no_uri_hints: args.no_uri_hints,
        uri_sync_batch_size: args.uri_sync_batch_size.max(1),
    }
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "error",
        1 => "warn",
        2 => "info",
        3 => "debug",
        _ => "trace",
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(level)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Map clap's `SyncArgs` into the runtime `sync::SyncOptions`.
fn map_sync_args(args: SyncArgs, quiet: bool) -> sync::SyncOptions {
    sync::SyncOptions {
        root: args.root,
        verbose: args.verbose,
        quiet,
        allow_uri_sync: args.allow_uri_sync,
        uri_sync_batch_size: args.uri_sync_batch_size.max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Smoke test: `downlint server --allow-uri-sync` parses cleanly. The
    /// flag is plumbed into `run_server` via `UriOptions`, which would panic
    /// in production if the type didn't match — so we just assert parsing.
    #[test]
    fn server_args_parse_with_uri_flags() {
        let cli = Cli::try_parse_from([
            "downlint",
            "server",
            "--allow-uri-sync",
            "--no-uri-hints",
            "--uri-sync-batch-size",
            "10",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Server(args)) => {
                assert!(args.allow_uri_sync);
                assert!(args.no_uri_hints);
                assert_eq!(args.uri_sync_batch_size, 10);
            }
            other => panic!("expected Server subcommand, got {other:?}"),
        }
    }

    /// Default behavior: no `--allow-uri-sync` flag → sync disabled.
    #[test]
    fn server_args_default_to_sync_disabled() {
        let cli = Cli::try_parse_from(["downlint", "server"]).unwrap();
        match cli.command {
            Some(Command::Server(args)) => {
                assert!(!args.allow_uri_sync);
                assert!(!args.no_uri_hints);
                assert_eq!(args.uri_sync_batch_size, 50);
            }
            other => panic!("expected Server subcommand, got {other:?}"),
        }
    }
}
