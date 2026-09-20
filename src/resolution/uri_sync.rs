//! Batched + cached execution of `warm_cmd` entries declared under
//! `[uri.mappings]` in `.downlint.toml`.
//!
//! Sync is **per-file** (`{path}` placeholder receives one resolved absolute
//! file path per invocation). The runner never spawns more processes than the
//! configured batch size; results are cached by `(mapping_index, absolute_path)`
//! so repeated lookups within a run (and across LSP requests — see
//! `super::mod::ServerState`) do not re-fork.
//!
//! All subprocess execution is gated by the `--allow-uri-sync` CLI flag. When
//! that flag is absent, the runner reports a single `SyncDecision::Skipped`
//! per mapping so the diagnostics layer can render a one-time info hint.
//!
//! Sync uses `std::process::Command` so that `resolve_links` can stay
//! synchronous (the CLI/LSP layers already run inside a tokio runtime).
//! We use a per-call blocking wait with a watchdog thread for the timeout;
//! shell strings are never built.

use crate::resolution::uri::UriResolver;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Per-path outcome of running sync. Distinct from "is the file present on
/// disk" — the runner always follows up with a stat check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathStatus {
    Present,
    Missing,
    SyncFailed,
    SyncTimedOut,
}

/// Sync gating outcome for a mapping. The diagnostics layer renders a one-time
/// info hint per `Skipped` mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncDecision {
    Ran,
    /// Mapping has a `warm_cmd` but the run did not have `--allow-uri-sync`.
    Skipped,
    /// Mapping has no `warm_cmd` at all — nothing to skip.
    NotApplicable,
}

/// Cached sync result for one `(mapping_index, absolute_path)` key.
#[derive(Clone, Debug)]
pub struct CachedPathResult {
    pub decision: SyncDecision,
    pub status: PathStatus,
    /// True when the configured `verify_cmd` (or auto-detection heuristics)
    /// marked the file as a cloud placeholder. Even if `warm_cmd` succeeded,
    /// a placeholder makes the link unresolved. Lets the diagnostics layer
    /// render a more specific message.
    pub verify_was_placeholder: bool,
}

/// Bounded, shared cache of sync results. Sharing lets the LSP layer (task #8)
/// avoid re-running sync on every keystroke; sharing is `Arc<Mutex<...>>` so
/// the cache can be passed across handlers cheaply.
#[derive(Clone, Debug, Default)]
pub struct UriSyncCache {
    inner: Arc<Mutex<BTreeMap<(usize, PathBuf), CachedPathResult>>>,
}

impl UriSyncCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a previously cached result. Returns `None` if unseen.
    pub fn get(&self, key: &(usize, PathBuf)) -> Option<CachedPathResult> {
        self.inner.lock().expect("uri sync cache poisoned").get(key).cloned()
    }

    /// Insert or replace a cached result.
    pub fn insert(&self, key: (usize, PathBuf), value: CachedPathResult) {
        self.inner
            .lock()
            .expect("uri sync cache poisoned")
            .insert(key, value);
    }

    /// Wipe the cache. Used by the LSP layer when `.downlint.toml` changes.
    pub fn clear(&self) {
        self.inner
            .lock()
            .expect("uri sync cache poisoned")
            .clear();
    }

    /// Number of cached entries (for diagnostics + tests).
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("uri sync cache poisoned")
            .len()
    }

    /// Whether the cache has zero entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Owns the resolver reference, gating flag, batch size, and shared cache.
#[derive(Clone, Debug)]
pub struct SyncRunner<'a> {
    resolver: &'a UriResolver,
    allow_sync: bool,
    /// Reserved for Phase 2 (fanning out multiple `{path}` placeholders per
    /// single subprocess invocation). Currently the runner invokes `warm_cmd`
    /// once per file, which is a strict subset of the eventual behavior.
    /// Field is kept (with `#[allow(dead_code)]`) so callers can already
    /// plumb the value through.
    #[allow(dead_code)]
    batch_size: usize,
    cache: UriSyncCache,
}

impl<'a> SyncRunner<'a> {
    /// Construct a runner with a fresh private cache. Equivalent to
    /// `with_cache(..., UriSyncCache::new())`. Prefer `with_cache` when the
    /// caller already has a shared cache (e.g. LSP `ServerState`).
    pub fn new(
        resolver: &'a UriResolver,
        allow_sync: bool,
        batch_size: usize,
    ) -> Self {
        Self::with_cache(resolver, allow_sync, batch_size, UriSyncCache::new())
    }

    /// Use a pre-existing cache (e.g. one stored in LSP `ServerState`).
    pub fn with_cache(
        resolver: &'a UriResolver,
        allow_sync: bool,
        batch_size: usize,
        cache: UriSyncCache,
    ) -> Self {
        Self {
            resolver,
            allow_sync,
            batch_size: batch_size.max(1),
            cache,
        }
    }

    /// Run sync for one resolved path, consulting and populating the cache.
    /// Returns the cached decision + status.
    pub fn run_for(&self, mapping_index: usize, resolved_path: PathBuf) -> CachedPathResult {
        let key = (mapping_index, resolved_path.clone());
        if let Some(cached) = self.cache.get(&key) {
            return cached;
        }

        let decision = self.decision_for_mapping(mapping_index);
        let (status, verify_was_placeholder) = match decision {
            SyncDecision::Ran => self.run_sync_and_verify(mapping_index, &resolved_path),
            SyncDecision::Skipped | SyncDecision::NotApplicable => {
                if resolved_path.exists() {
                    (PathStatus::Present, false)
                } else {
                    (PathStatus::Missing, false)
                }
            }
        };

        let result = CachedPathResult {
            decision,
            status,
            verify_was_placeholder,
        };
        self.cache.insert(key, result.clone());
        result
    }

    /// Returns `Skipped` if the mapping has a configured `warm_cmd` but the
    /// CLI flag is off; `NotApplicable` if there's nothing to run; `Ran` if
    /// sync will execute.
    pub fn decision_for_mapping(&self, mapping_index: usize) -> SyncDecision {
        let Some(sync) = self.resolver.sync_config(mapping_index) else {
            return SyncDecision::NotApplicable;
        };
        if sync.cmd.is_none() {
            return SyncDecision::NotApplicable;
        }
        if self.allow_sync {
            SyncDecision::Ran
        } else {
            SyncDecision::Skipped
        }
    }

    /// Spawn the configured `warm_cmd` once, substituting `{path}` with the
    /// resolved absolute path. Honors `warm_timeout`. Returns `SyncFailed`
    /// for non-zero exits, `SyncTimedOut` on deadline expiry.
    fn run_sync(&self, mapping_index: usize, path: &PathBuf) -> PathStatus {
        let Some(sync) = self.resolver.sync_config(mapping_index) else {
            return PathStatus::SyncFailed;
        };
        let Some(cmd) = sync.cmd else {
            return PathStatus::SyncFailed;
        };
        if cmd.is_empty() {
            return PathStatus::SyncFailed;
        }

        let binary = &cmd[0];
        let args: Vec<String> = cmd[1..]
            .iter()
            .map(|arg| substitute_path(arg, path))
            .collect();
        let timeout = Duration::from_secs(sync.timeout as u64);

        let mut command = Command::new(binary);
        command.args(&args);

        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return PathStatus::SyncFailed,
        };

        match wait_with_timeout(child, timeout) {
            Ok(status) if status.success() => {
                if path.exists() {
                    PathStatus::Present
                } else {
                    PathStatus::Missing
                }
            }
            Ok(_) => PathStatus::SyncFailed,
            Err(WaitError::TimedOut) => PathStatus::SyncTimedOut,
            Err(WaitError::Io) => PathStatus::SyncFailed,
        }
    }

    /// Run `warm_cmd` then `verify_cmd` (and auto-detection heuristics) to
    /// decide between `Present` and `Missing`. The tuple return carries the
    /// final status plus whether verification specifically flagged the path
    /// as a placeholder (so diagnostics can attribute the failure).
    fn run_sync_and_verify(
        &self,
        mapping_index: usize,
        path: &PathBuf,
    ) -> (PathStatus, bool) {
        let status = self.run_sync(mapping_index, path);
        if !matches!(status, PathStatus::Present) {
            return (status, false);
        }
        match self.run_verify(mapping_index, path) {
            VerifyOutcome::Passed => (PathStatus::Present, false),
            VerifyOutcome::Failed => (PathStatus::Missing, true),
            VerifyOutcome::NotConfigured => (PathStatus::Present, false),
        }
    }

    /// Run the configured `verify_cmd` after `warm_cmd` succeeds, falling
    /// back to auto-detection heuristics when no `verify_cmd` is set.
    pub fn run_verify(&self, mapping_index: usize, path: &PathBuf) -> VerifyOutcome {
        let Some(sync) = self.resolver.sync_config(mapping_index) else {
            return VerifyOutcome::NotConfigured;
        };
        if let Some(cmd) = sync.verify_cmd {
            return self.run_verify_cmd(mapping_index, path, cmd, sync.timeout);
        }
        // No verify_cmd → fall back to auto-detection heuristics. Returns
        // Failed when a heuristic classifies the path as a placeholder,
        // Passed when heuristics say real, and NotConfigured only when the
        // resolver has no mode info (which doesn't actually happen).
        let mode = self.resolver.auto_verify_mode();
        match super::auto_verify::classify(path, mode) {
            super::auto_verify::AutoVerifyOutcome::Placeholder => VerifyOutcome::Failed,
            super::auto_verify::AutoVerifyOutcome::Real => VerifyOutcome::Passed,
            super::auto_verify::AutoVerifyOutcome::Unknown => VerifyOutcome::Passed,
        }
    }

    fn run_verify_cmd(
        &self,
        _mapping_index: usize,
        path: &PathBuf,
        cmd: &[String],
        timeout_secs: u32,
    ) -> VerifyOutcome {
        if cmd.is_empty() {
            return VerifyOutcome::NotConfigured;
        }
        let binary = &cmd[0];
        let args: Vec<String> = cmd[1..]
            .iter()
            .map(|arg| substitute_path(arg, path))
            .collect();
        let timeout = Duration::from_secs(timeout_secs as u64);
        let mut command = Command::new(binary);
        command.args(&args);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return VerifyOutcome::Failed,
        };
        match wait_with_timeout(child, timeout) {
            Ok(status) if status.success() => VerifyOutcome::Passed,
            Ok(_) => VerifyOutcome::Failed,
            Err(_) => VerifyOutcome::Failed,
        }
    }
}

/// Outcome of running `verify_cmd` (or auto-detection) on a synced path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyOutcome {
    /// File is real, not a placeholder.
    Passed,
    /// File is a placeholder or otherwise unreadable.
    Failed,
    /// No `verify_cmd` and auto-detection returned Unknown (effectively a
    /// no-op; surfaces as `Present` in the caller).
    NotConfigured,
}

/// Replace every occurrence of `{path}` in `arg` with the resolved absolute
/// file path. Used both at sync invocation time and by tests.
fn substitute_path(arg: &str, path: &PathBuf) -> String {
    if arg.contains("{path}") {
        arg.replace("{path}", &path.to_string_lossy())
    } else {
        arg.to_string()
    }
}

/// Maximum UTF-8 byte length of a constructed argv list before the batch
/// fan-out falls back to per-file invocation. Chosen conservatively below
/// typical ARG_MAX (Linux/macOS) and Windows' CreateProcess limit.
pub const MAX_BATCH_BYTES: usize = 128 * 1024;

/// Per-batch outcome returned by `run_for_many`. One batch = one subprocess
/// invocation covering up to `batch_size` paths for one mapping.
#[derive(Clone, Debug)]
pub struct BatchOutcome {
    /// Per-path status, in the same order as the input paths. Failed spawn
    /// or timeout produces `SyncFailed` / `SyncTimedOut` for every entry.
    pub statuses: Vec<PathStatus>,
    /// True when the constructed argv exceeded `MAX_BATCH_BYTES` and the
    /// runner fell back to per-file invocation. The caller emits a one-time
    /// uri/batch-clamped info diagnostic per mapping.
    pub fell_back_to_per_file: bool,
    /// True when the batch was empty (no work). Not an error, just a no-op.
    pub empty: bool,
}

impl BatchOutcome {
    fn failed_all(status: PathStatus, count: usize) -> Self {
        Self {
            statuses: vec![status; count],
            fell_back_to_per_file: false,
            empty: count == 0,
        }
    }
}

/// Run sync for up to `batch_size` paths for a single mapping. Folds the
/// constructed argv through `MAX_BATCH_BYTES` and falls back to per-file
/// invocation if the limit would be exceeded. Honors the `{paths}`
/// placeholder for stdin piping.
pub fn run_for_many(
    runner: &SyncRunner<'_>,
    mapping_index: usize,
    paths: Vec<PathBuf>,
) -> BatchOutcome {
    if paths.is_empty() {
        return BatchOutcome {
            statuses: Vec::new(),
            fell_back_to_per_file: false,
            empty: true,
        };
    }
    let Some(sync) = runner.resolver.sync_config(mapping_index) else {
        return BatchOutcome::failed_all(PathStatus::SyncFailed, paths.len());
    };
    let Some(cmd) = sync.cmd else {
        return BatchOutcome::failed_all(PathStatus::SyncFailed, paths.len());
    };
    if cmd.is_empty() {
        return BatchOutcome::failed_all(PathStatus::SyncFailed, paths.len());
    }
    let uses_stdin = cmd.iter().any(|arg| arg.contains("{paths}"));
    let batch_size = runner.batch_size.max(1);
    // Build the batched arg list once to measure against MAX_BATCH_BYTES.
    let preview_args = build_batched_args(cmd, &paths[..paths.len().min(batch_size)]);
    let argv_bytes: usize = preview_args.iter().map(|arg| arg.len() + 1).sum();
    if argv_bytes > MAX_BATCH_BYTES && !uses_stdin {
        return batch_per_file(runner, mapping_index, paths);
    }
    let statuses = if uses_stdin {
        run_batch_with_stdin(sync.timeout, &cmd, &paths, batch_size)
    } else {
        run_batch_positional(sync.timeout, &cmd, &paths, batch_size)
    };
    BatchOutcome {
        statuses,
        fell_back_to_per_file: false,
        empty: false,
    }
}

/// Construct positional argv: repeat args template with each path substituted
/// into `{path}` slots. Returns a flat list of strings.
fn build_batched_args(cmd: &[String], paths: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::with_capacity(1 + cmd.len().saturating_sub(1) * paths.len());
    out.push(cmd[0].clone());
    for path in paths {
        for arg in &cmd[1..] {
            out.push(substitute_path(arg, path));
        }
    }
    out
}

/// Run a positional batch via `Command`. Chunks `paths` into groups of
/// `batch_size` and spawns one subprocess per group.
fn run_batch_positional(
    timeout_secs: u32,
    cmd: &[String],
    paths: &[PathBuf],
    batch_size: usize,
) -> Vec<PathStatus> {
    let mut out = Vec::with_capacity(paths.len());
    let timeout = Duration::from_secs(timeout_secs as u64);
    for chunk in paths.chunks(batch_size.max(1)) {
        let args = build_batched_args(cmd, chunk);
        let mut command = Command::new(&cmd[0]);
        command.args(&args);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                for _ in 0..chunk.len() {
                    out.push(PathStatus::SyncFailed);
                }
                continue;
            }
        };
        let outcome = match wait_with_timeout(child, timeout) {
            Ok(status) if status.success() => {
                let mut v = Vec::with_capacity(chunk.len());
                for p in chunk {
                    v.push(if p.exists() {
                        PathStatus::Present
                    } else {
                        PathStatus::Missing
                    });
                }
                v
            }
            Ok(_) => vec![PathStatus::SyncFailed; chunk.len()],
            Err(WaitError::TimedOut) => vec![PathStatus::SyncTimedOut; chunk.len()],
            Err(WaitError::Io) => vec![PathStatus::SyncFailed; chunk.len()],
        };
        out.extend(outcome);
    }
    out
}

/// Run a `{paths}`-style batch via stdin piping. Each batch invocation gets
/// one line per path on stdin. `{path}` substitutions produce empty strings
/// since paths are in stdin instead.
fn run_batch_with_stdin(
    timeout_secs: u32,
    cmd: &[String],
    paths: &[PathBuf],
    batch_size: usize,
) -> Vec<PathStatus> {
    use std::io::Write;
    use std::process::Stdio;
    let mut out = Vec::with_capacity(paths.len());
    let timeout = Duration::from_secs(timeout_secs as u64);
    for chunk in paths.chunks(batch_size.max(1)) {
        let args: Vec<String> = cmd[1..]
            .iter()
            .map(|arg| {
                if arg.contains("{paths}") {
                    arg.replace("{paths}", "")
                } else if arg.contains("{path}") {
                    // {path} without {paths} is meaningless in stdin mode;
                    // empty-substitute so the user sees a clear argv error.
                    String::new()
                } else {
                    arg.clone()
                }
            })
            .collect();
        let mut command = Command::new(&cmd[0]);
        command.args(&args).stdin(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                for _ in 0..chunk.len() {
                    out.push(PathStatus::SyncFailed);
                }
                continue;
            }
        };
        if let Some(stdin) = child.stdin.as_mut() {
            let body: String = chunk
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\n");
            let _ = stdin.write_all(body.as_bytes());
        }
        // Closing stdin: drop the handle so the child sees EOF.
        drop(child.stdin.take());
        let statuses = match wait_with_timeout(child, timeout) {
            Ok(status) if status.success() => chunk
                .iter()
                .map(|p| {
                    if p.exists() {
                        PathStatus::Present
                    } else {
                        PathStatus::Missing
                    }
                })
                .collect(),
            Ok(_) => vec![PathStatus::SyncFailed; chunk.len()],
            Err(WaitError::TimedOut) => vec![PathStatus::SyncTimedOut; chunk.len()],
            Err(WaitError::Io) => vec![PathStatus::SyncFailed; chunk.len()],
        };
        out.extend(statuses);
    }
    out
}

/// Fallback path: invoked when the positional argv would exceed the byte
/// cap. Spawns `warm_cmd` once per path (the original Phase-1 behavior).
fn batch_per_file(
    runner: &SyncRunner<'_>,
    mapping_index: usize,
    paths: Vec<PathBuf>,
) -> BatchOutcome {
    let mut statuses = Vec::with_capacity(paths.len());
    for path in paths {
        let result = runner.run_for(mapping_index, path);
        statuses.push(result.status);
    }
    BatchOutcome {
        statuses,
        fell_back_to_per_file: true,
        empty: false,
    }
}

/// Internal: blocking wait with timeout. Uses a poll loop with a short sleep
/// rather than `wait_timeout` (Unix-only) so the runner stays portable to
/// Windows; precision is ~20ms.
#[derive(Debug)]
enum WaitError {
    TimedOut,
    Io,
}

/// Wait for a child process to exit, bounded by `timeout`. If the deadline
/// passes first, kill the child and report `TimedOut`.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, WaitError> {
    use std::thread;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(WaitError::TimedOut);
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return Err(WaitError::Io),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::uri::{PartialUriConfig, PartialUriMapping};
    use crate::config::finalize_uri;
    use crate::resolution::uri::UriResolver;
    use tempfile::TempDir;

    /// Build a config with one optional mapping whose `warm_cmd` is `["true"]`
    /// (POSIX no-op) and `warm_required = required`.
    fn resolver_with(
        tmp: &TempDir,
        prefix: &str,
        root: &str,
        warm_cmd: Option<Vec<String>>,
        warm_required: bool,
        warm_timeout: u32,
    ) -> UriResolver {
        let uri = finalize_uri(PartialUriConfig {
            mappings: Some(vec![PartialUriMapping {
                prefix: Some(prefix.to_string()),
                root: Some(root.to_string()),
                warm_cmd,
                warm_required: Some(warm_required),
                warm_timeout: Some(warm_timeout),
                verify_cmd: None,
            }]),
            auto_verify: None,
        })
        .unwrap();
        UriResolver::new(&uri, tmp.path()).unwrap()
    }

    fn empty_resolver() -> UriResolver {
        UriResolver::empty()
    }

    #[test]
    fn decision_for_mapping_without_warm_cmd_is_not_applicable() {
        let tmp = TempDir::new().unwrap();
        let resolver = resolver_with(
            &tmp,
            "scheme://",
            tmp.path().to_str().unwrap(),
            None,
            false,
            30,
        );
        let runner = SyncRunner::new(&resolver, true, 50);
        assert_eq!(
            runner.decision_for_mapping(0),
            SyncDecision::NotApplicable
        );
    }

    #[test]
    fn decision_for_mapping_with_warm_cmd_but_no_flag_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let resolver = resolver_with(
            &tmp,
            "scheme://",
            tmp.path().to_str().unwrap(),
            Some(vec!["true".to_string()]),
            false,
            30,
        );
        let runner = SyncRunner::new(&resolver, false, 50);
        assert_eq!(runner.decision_for_mapping(0), SyncDecision::Skipped);
    }

    #[test]
    fn decision_for_mapping_with_warm_cmd_and_flag_is_ran() {
        let tmp = TempDir::new().unwrap();
        let resolver = resolver_with(
            &tmp,
            "scheme://",
            tmp.path().to_str().unwrap(),
            Some(vec!["true".to_string()]),
            false,
            30,
        );
        let runner = SyncRunner::new(&resolver, true, 50);
        assert_eq!(runner.decision_for_mapping(0), SyncDecision::Ran);
    }

    #[test]
    fn run_for_present_file_returns_present_without_sync() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("present.txt");
        std::fs::write(&target, "hi").unwrap();
        let resolver = empty_resolver();
        let runner = SyncRunner::new(&resolver, false, 50);
        let result = runner.run_for(0, target.clone());
        assert_eq!(result.status, PathStatus::Present);
    }

    #[test]
    fn run_for_missing_file_returns_missing_without_sync() {
        let tmp = TempDir::new().unwrap();
        let resolver = empty_resolver();
        let runner = SyncRunner::new(&resolver, false, 50);
        let result = runner.run_for(0, tmp.path().join("not-here.txt"));
        assert_eq!(result.status, PathStatus::Missing);
    }

    #[cfg(unix)]
    #[test]
    fn run_for_with_running_warm_cmd_marks_present_after_run() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("synced.txt");
        let script = tmp.path().join("create.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ntouch \"$1\"\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let resolver = resolver_with(
            &tmp,
            "scheme://",
            tmp.path().to_str().unwrap(),
            Some(vec![
                script.to_string_lossy().to_string(),
                "{path}".to_string(),
            ]),
            false,
            10,
        );
        let runner = SyncRunner::new(&resolver, true, 50);
        let result = runner.run_for(0, target.clone());
        assert_eq!(result.decision, SyncDecision::Ran);
        assert_eq!(result.status, PathStatus::Present);
        assert!(target.exists());

        // Cache hit on second call: no extra stat, no extra subprocess.
        let second = runner.run_for(0, target);
        assert_eq!(second.status, PathStatus::Present);
    }

    #[test]
    fn run_for_with_failing_warm_cmd_returns_sync_failed() {
        let tmp = TempDir::new().unwrap();
        let resolver = resolver_with(
            &tmp,
            "scheme://",
            tmp.path().to_str().unwrap(),
            Some(vec!["false".to_string()]),
            false,
            10,
        );
        let runner = SyncRunner::new(&resolver, true, 50);
        let result = runner.run_for(0, tmp.path().join("x"));
        assert_eq!(result.status, PathStatus::SyncFailed);
    }

    #[test]
    fn run_for_with_missing_binary_returns_sync_failed() {
        let tmp = TempDir::new().unwrap();
        let resolver = resolver_with(
            &tmp,
            "scheme://",
            tmp.path().to_str().unwrap(),
            Some(vec!["definitely-not-a-real-binary".to_string()]),
            false,
            10,
        );
        let runner = SyncRunner::new(&resolver, true, 50);
        let result = runner.run_for(0, tmp.path().join("x"));
        assert_eq!(result.status, PathStatus::SyncFailed);
    }

    #[test]
    fn cache_is_shared_across_runners_with_cache_constructor() {
        let tmp = TempDir::new().unwrap();
        let resolver = empty_resolver();
        let cache = UriSyncCache::new();
        let runner = SyncRunner::with_cache(&resolver, false, 50, cache.clone());
        runner.run_for(0, tmp.path().join("a"));
        assert_eq!(cache.len(), 1);

        // A second runner sharing the same cache sees the entry.
        let runner2 = SyncRunner::with_cache(&resolver, false, 50, cache.clone());
        runner2.run_for(0, tmp.path().join("a"));
        assert_eq!(cache.len(), 1);
        runner2.run_for(0, tmp.path().join("b"));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn substitute_path_replaces_every_occurrence() {
        let path = PathBuf::from("/tmp/foo");
        assert_eq!(substitute_path("{path}", &path), "/tmp/foo");
        assert_eq!(substitute_path("--out={path}", &path), "--out=/tmp/foo");
        assert_eq!(substitute_path("literal", &path), "literal");
    }

    #[test]
    fn cache_clear_resets_state() {
        let cache = UriSyncCache::new();
        cache.insert((0, PathBuf::from("/x")), CachedPathResult {
            decision: SyncDecision::Ran,
            status: PathStatus::Present,
            verify_was_placeholder: false,
        });
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn batch_size_zero_becomes_one() {
        let resolver = empty_resolver();
        let runner = SyncRunner::new(&resolver, false, 0);
        assert_eq!(runner.batch_size, 1);
    }
}
