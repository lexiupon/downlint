pub mod check;

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
    path: Option<PathBuf>,
}

#[derive(Args, Clone, Debug, Default)]
struct ServerArgs {
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    #[arg(long)]
    wait_for_debugger: bool,
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
            crate::lsp::run_server(args.verbose, args.wait_for_debugger).await
        }
        Some(Command::Check(args)) => {
            init_tracing(args.verbose);
            check::run_check(map_check_args(args, cli.quiet)).await
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
