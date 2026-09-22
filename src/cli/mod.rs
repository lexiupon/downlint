pub mod check;
pub mod graph;
pub mod info;
pub mod init;
pub mod rename;
pub mod resolve;

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
    #[command(name = "init", about = "Create a .downlint.toml config file in the workspace root")]
    Init(InitArgs),
    Server(ServerArgs),
    /// Move a markdown file or attachment on disk and rewrite every link
    /// that points at it. Kind-class (markdown vs attachment) is inferred
    /// from the source file's extension.
    #[command(name = "rename-file", about = "Rename a markdown file or attachment and rewrite references")]
    RenameFile(RenameFileArgs),
    /// Rewrite a logical link identifier across the workspace. No disk move.
    #[command(name = "rename-link", about = "Rewrite a link-target identifier across the workspace")]
    RenameLink(RenameLinkArgs),
    /// Show what a link target resolves to: documents, attachments, folders,
    /// and URI-scheme mappings.
    #[command(name = "resolve", about = "Show what a link target resolves to (documents, attachments, folders, URI mappings)")]
    Resolve(ResolveArgs),
    /// Read-only link-graph queries: backlinks, links, orphans, deadends,
    /// unresolved.
    #[command(
        name = "graph",
        about = "Query the link graph (backlinks, links, orphans, deadends, unresolved)"
    )]
    Graph(GraphArgs),
    /// Show what downlint sees: the resolved workspace (mounts, schemas,
    /// document counts, conflicts).
    #[command(
        name = "info",
        about = "Show the resolved workspace (mounts, schemas, documents, conflicts)"
    )]
    Info(InfoArgs),
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
    /// Permit subprocess execution of a `[[schemas]]` `verify_cmd`. Without
    /// this flag, `verify_cmd` is skipped. Note: enabling this flag means
    /// `.downlint.toml` controls which commands run.
    #[arg(long = "allow-uri-sync", action = ArgAction::SetTrue)]
    allow_uri_sync: bool,
    /// Suppress the "no URI mapping found" hint diagnostic. The broken-link
    /// diagnostic itself is still emitted.
    #[arg(long = "no-uri-hints", action = ArgAction::SetTrue)]
    no_uri_hints: bool,
    path: Option<PathBuf>,
}

#[derive(Args, Clone, Debug, Default)]
struct ServerArgs {
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    #[arg(long)]
    wait_for_debugger: bool,
    /// Permit subprocess execution of a `[[schemas]]` `verify_cmd`. Without
    /// this flag, `verify_cmd` is skipped. Note: enabling this flag means
    /// `.downlint.toml` controls which commands run.
    #[arg(long = "allow-uri-sync", action = ArgAction::SetTrue)]
    allow_uri_sync: bool,
    /// Suppress the "no URI mapping found" hint diagnostic. The broken-link
    /// diagnostic itself is still emitted.
    #[arg(long = "no-uri-hints", action = ArgAction::SetTrue)]
    no_uri_hints: bool,
    /// Start the server in the background and exit immediately. Used by
    /// agent workflows that perform many renames in sequence. The server
    /// runs as a planning service — CLI `rename-file` / `rename-link` with
    /// `--server` connect to it via TCP.
    #[arg(long = "detach", action = ArgAction::SetTrue)]
    detach: bool,
    /// Stop a running detached server for the current project. Exits 0 if
    /// the server was stopped, 3 if no server was running.
    #[arg(long = "stop", action = ArgAction::SetTrue)]
    stop: bool,
    /// TCP port for the detached server. Use `--port 0` to let the OS
    /// pick an available port (the chosen port is reported to stdout and
    /// written to `.downlint/.server.pid`).
    #[arg(long = "port", default_value_t = 0)]
    port: u16,
}

#[derive(Args, Clone, Debug, Default)]
struct InitArgs {
    /// Workspace root in which to create .downlint.toml (default: current directory).
    #[arg(long)]
    root: Option<PathBuf>,
    /// Overwrite an existing .downlint.toml.
    #[arg(long)]
    force: bool,
}

#[derive(Args, Clone, Debug, Default)]
struct RenameFileArgs {
    #[arg(long)]
    root: Option<PathBuf>,
    /// Workspace-relative path to the source file.
    #[arg(long)]
    from: PathBuf,
    /// Workspace-relative path to the destination.
    #[arg(long)]
    to: PathBuf,
    /// Print the planned edits without applying them.
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    #[arg(long, short = 'q', action = ArgAction::SetTrue)]
    quiet: bool,
    /// Connect to a detached server for planning (avoids re-indexing).
    /// Falls back to in-process indexing if no server is detected.
    #[arg(long, action = ArgAction::SetTrue)]
    server: bool,
}

#[derive(Args, Clone, Debug, Default)]
struct RenameLinkArgs {
    #[arg(long)]
    root: Option<PathBuf>,
    /// Source identifier (no `/`, `#`, `|`, etc.).
    #[arg(long)]
    from: String,
    /// Destination identifier (no `/`, `#`, `|`, etc.).
    #[arg(long)]
    to: String,
    /// Print the planned edits without applying them.
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    #[arg(long, short = 'q', action = ArgAction::SetTrue)]
    quiet: bool,
    /// Connect to a detached server for planning.
    #[arg(long, action = ArgAction::SetTrue)]
    server: bool,
}

#[derive(Args, Clone, Debug, Default)]
struct ResolveArgs {
    /// The link target to resolve (wiki-link target grammar: title/stem,
    /// explicit path, folder target, optional #anchor, or scheme://…).
    target: String,
    #[arg(long)]
    root: Option<PathBuf>,
    /// Resolve source-relative targets (`./…`/`../…`) as if the link were in
    /// this document. Bare wiki `path/file` and `/…` targets are root-relative
    /// and ignore this flag (RFC 0013).
    #[arg(long)]
    from: Option<PathBuf>,
    #[arg(long, default_value = "text")]
    format: FormatArg,
    /// Also list prefix candidates when wiki.obsidian_prefix is off (advisory).
    #[arg(long = "include-prefix", action = ArgAction::SetTrue)]
    include_prefix: bool,
    /// Permit subprocess execution of a `[[schemas]]` `verify_cmd`. Without
    /// this flag, `verify_cmd` is skipped. Note: enabling this flag means
    /// `.downlint.toml` controls which commands run.
    #[arg(long = "allow-uri-sync", action = ArgAction::SetTrue)]
    allow_uri_sync: bool,
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
}

#[derive(Args, Clone, Debug)]
struct GraphArgs {
    /// Global so it can follow the subcommand (`graph backlinks <f> --root …`),
    /// matching `resolve`.
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[arg(long, short = 'v', default_value_t = 2, global = true)]
    verbose: u8,
    /// Output format: `text` (one line per result) or `json` (envelope).
    #[arg(long, default_value = "text", global = true)]
    format: FormatArg,
    #[command(subcommand)]
    query: graph::GraphQuery,
}

#[derive(Args, Clone, Debug)]
struct InfoArgs {
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    /// Output format: `text` (default) or `json`.
    #[arg(long, default_value = "text")]
    format: FormatArg,
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
            if args.stop {
                // Stop a detached server. For now, the persistent server
                // is a Phase 5 stretch goal; this branch returns 3 with a
                // clear message until the server module lands.
                eprintln!(
                    "downlint: persistent server not yet implemented (RFC 0009 Phase 5 stretch goal)"
                );
                return 3;
            }
            if args.detach {
                eprintln!(
                    "downlint: persistent server not yet implemented (RFC 0009 Phase 5 stretch goal)"
                );
                return 3;
            }
            let uri_opts = crate::resolution::UriOptions {
                allow_sync: args.allow_uri_sync,
                no_hints: args.no_uri_hints,
            };
            crate::lsp::run_server(args.verbose, args.wait_for_debugger, uri_opts).await
        }
        Some(Command::Check(args)) => {
            init_tracing(args.verbose);
            check::run_check(map_check_args(args, cli.quiet)).await
        }
        Some(Command::Init(args)) => {
            init::run_init(init::InitOptions {
                root: args.root,
                force: args.force,
            })
        }
        Some(Command::RenameFile(args)) => {
            init_tracing(args.verbose);
            rename::run_rename_file(rename::RenameFileOptions {
                root: args.root,
                from: args.from,
                to: args.to,
                dry_run: args.dry_run,
                verbose: args.verbose,
                quiet: args.quiet,
            })
        }
        Some(Command::RenameLink(args)) => {
            init_tracing(args.verbose);
            rename::run_rename_link(rename::RenameLinkOptions {
                root: args.root,
                from: args.from,
                to: args.to,
                dry_run: args.dry_run,
                verbose: args.verbose,
                quiet: args.quiet,
            })
        }
        Some(Command::Resolve(args)) => {
            init_tracing(args.verbose);
            resolve::run_resolve(resolve::ResolveOptions {
                root: args.root,
                from: args.from,
                format: match args.format {
                    FormatArg::Text => check::OutputFormat::Text,
                    FormatArg::Json => check::OutputFormat::Json,
                },
                include_prefix: args.include_prefix,
                allow_uri_sync: args.allow_uri_sync,
                target: args.target,
            })
        }
        Some(Command::Graph(args)) => {
            init_tracing(args.verbose);
            graph::run_graph(graph::GraphOptions {
                root: args.root,
                query: args.query,
                format: match args.format {
                    FormatArg::Text => check::OutputFormat::Text,
                    FormatArg::Json => check::OutputFormat::Json,
                },
            })
        }
        Some(Command::Info(args)) => {
            init_tracing(args.verbose);
            info::run_info(info::InfoOptions {
                root: args.root,
                format: match args.format {
                    FormatArg::Text => check::OutputFormat::Text,
                    FormatArg::Json => check::OutputFormat::Json,
                },
            })
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
        ])
        .unwrap();
        match cli.command {
            Some(Command::Server(args)) => {
                assert!(args.allow_uri_sync);
                assert!(args.no_uri_hints);
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
            }
            other => panic!("expected Server subcommand, got {other:?}"),
        }
    }
}
