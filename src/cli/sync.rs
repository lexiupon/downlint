use crate::config::{ConfigError, UriConfig};
use crate::resolution::uri::UriResolver;
use crate::resolution::uri_sync::{self, SyncRunner};
use crate::resolution::{ResolveInput, UriOptions, resolve_links};
use crate::utils::{WorkspaceInput, discover_workspace};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct SyncOptions {
    pub root: Option<PathBuf>,
    pub verbose: u8,
    pub quiet: bool,
    pub allow_uri_sync: bool,
    pub uri_sync_batch_size: usize,
}

/// Per-mapping summary printed at the end of a successful run.
#[derive(Clone, Debug, Default)]
struct MappingSummary {
    total: usize,
    synced: usize,
    missing: usize,
    failed: usize,
    timed_out: usize,
}

pub async fn run_sync(options: SyncOptions) -> i32 {
    match run_sync_once(&options) {
        Ok(outcome) => {
            if !options.quiet {
                print_summary(&outcome.summaries, outcome.total_elapsed);
            }
            if outcome.any_failed() {
                1
            } else {
                0
            }
        }
        Err(error) => {
            eprintln!("downlint: error: {error}");
            2
        }
    }
}

struct SyncOutcome {
    summaries: BTreeMap<String, MappingSummary>,
    total_elapsed: std::time::Duration,
}

impl SyncOutcome {
    fn any_failed(&self) -> bool {
        self.summaries
            .values()
            .any(|s| s.failed > 0 || s.timed_out > 0 || s.missing > 0)
    }
}

fn run_sync_once(options: &SyncOptions) -> Result<SyncOutcome, ConfigError> {
    if !options.allow_uri_sync {
        return Err(ConfigError::Validation(
            "'warm-uri-mappings' requires --allow-uri-sync (safety gate). \
             Without this flag, .downlint.toml could execute configured \
             warm_cmd without explicit user consent."
                .into(),
        ));
    }

    let workspace = build_workspace(options)?;
    let mut input = ResolveInput::from_workspace(&workspace);
    if let Some(error) = input.uri_error.take() {
        return Err(ConfigError::Validation(error));
    }
    input.uri_opts = UriOptions {
        allow_sync: true,
        no_hints: input.uri_opts.no_hints,
        batch_size: options.uri_sync_batch_size,
    };

    let started = Instant::now();
    let graph = resolve_links(input);

    let resolver = build_resolver(&workspace.config.uri, &workspace.folder.root)?;
    let runner = SyncRunner::with_cache(
        &resolver,
        true,
        options.uri_sync_batch_size,
        crate::resolution::UriSyncCache::new(),
    );

    // Bucket URI targets by mapping index. We pull from BOTH resolved and
    // unresolved references because a broken URI link in `check` output is
    // exactly the case the `sync` subcommand exists to fix.
    let mut by_mapping: BTreeMap<usize, Vec<PathBuf>> = BTreeMap::new();
    for resolved in &graph.resolved_references {
        let Some(destination) = resolved.destinations.first() else {
            continue;
        };
        let Some(mapping_index) = mapping_index_for(&resolver, &destination.name) else {
            continue;
        };
        by_mapping
            .entry(mapping_index)
            .or_default()
            .push(destination.path.clone());
    }
    for unresolved in &graph.unresolved_references {
        let Some(mapping_index) = mapping_index_for(&resolver, &unresolved.target) else {
            continue;
        };
        // Unresolved references only carry `target` (the raw URI); we need
        // to resolve to a path via the resolver.
        if let crate::resolution::uri::UriOutcome::Resolved { resolved_path, .. } =
            resolver.resolve(&unresolved.target)
        {
            by_mapping
                .entry(mapping_index)
                .or_default()
                .push(resolved_path);
        }
    }

    let mut summaries: BTreeMap<String, MappingSummary> = BTreeMap::new();
    for (mapping_index, paths) in by_mapping {
        let prefix = format!("mapping[{mapping_index}]");
        let mut summary = MappingSummary {
            total: paths.len(),
            ..Default::default()
        };
        let batch_outcome = uri_sync::run_for_many(&runner, mapping_index, paths);
        for status in batch_outcome.statuses {
            match status {
                uri_sync::PathStatus::Present => summary.synced += 1,
                uri_sync::PathStatus::Missing => summary.missing += 1,
                uri_sync::PathStatus::SyncFailed => summary.failed += 1,
                uri_sync::PathStatus::SyncTimedOut => summary.timed_out += 1,
            }
        }
        summaries.insert(prefix, summary);
    }

    Ok(SyncOutcome {
        summaries,
        total_elapsed: started.elapsed(),
    })
}

fn build_workspace(options: &SyncOptions) -> Result<crate::utils::Workspace, ConfigError> {
    let input = WorkspaceInput::Path(options.root.clone().unwrap_or_else(|| PathBuf::from(".")));
    discover_workspace(input, options.root.as_deref())
}

fn build_resolver(config: &UriConfig, root: &std::path::Path) -> Result<UriResolver, ConfigError> {
    UriResolver::new(config, root)
        .map_err(|error| ConfigError::Validation(format!("uri.mappings: {error}")))
}

fn mapping_index_for(resolver: &UriResolver, target: &str) -> Option<usize> {
    use crate::resolution::uri::UriOutcome;
    match resolver.resolve(target) {
        UriOutcome::Resolved { mapping_index, .. } => Some(mapping_index),
        _ => None,
    }
}

fn print_summary(summaries: &BTreeMap<String, MappingSummary>, elapsed: std::time::Duration) {
    println!();
    println!("Sync summary ({}):", format_duration(elapsed));
    for (prefix, summary) in summaries {
        println!(
            "  {prefix}: total={total} synced={synced} failed={failed} missing={missing} timed_out={timed_out}",
            total = summary.total,
            synced = summary.synced,
            failed = summary.failed,
            missing = summary.missing,
            timed_out = summary.timed_out,
        );
    }
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{:.0}ms", d.as_millis())
    } else {
        format!("{:.2}s", secs)
    }
}