pub mod cli;
pub mod completion;
pub mod config;
pub mod diagnostics;
pub mod lsp;
pub mod parser;
pub mod rename;
pub mod resolution;
pub mod utils;
pub mod version;

pub use completion::{CompletionItem, CompletionParams, complete_at};
pub use config::{Config, ConfigError};
pub use diagnostics::{Diagnostic, DiagnosticConfig, check_diagnostics};
pub use parser::{ParseOptions, Structure, parse_document};
pub use resolution::{ConnectionGraph, ResolveInput, resolve_links};
pub use resolution::query::{
    match_document_kinds, resolve_target, MatchKind, ResolveStatus, TargetDestination,
    TargetResolution,
};
