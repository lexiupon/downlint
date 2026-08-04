//! Batched + cached execution of `sync_cmd` entries declared under
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
    /// Mapping has a `sync_cmd` but the run did not have `--allow-uri-sync`.
    Skipped,
    /// Mapping has no `sync_cmd` at all — nothing to skip.
    NotApplicable,
}

/// Cached sync result for one `(mapping_index, absolute_path)` key.
#[derive(Clone, Debug)]
pub struct CachedPathResult {
    pub decision: SyncDecision,
    pub status: PathStatus,
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
    /// single subprocess invocation). Currently the runner invokes `sync_cmd`
    /// once per file, which is a strict subset of the eventual behavior.
    #[allow(dead_code)]
    batch_size: usize,
    cache: UriSyncCache,
}

impl<'a> SyncRunner<'a> {
    pub fn new(
        resolver: &'a UriResolver,
        allow_sync: bool,
        batch_size: usize,
    ) -> Self {
        Self {
            resolver,
            allow_sync,
            batch_size: batch_size.max(1),
            cache: UriSyncCache::new(),
        }
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

    /// Returns a clone of the inner cache for external use (e.g. storing in
    /// `ServerState`).
    pub fn cache(&self) -> UriSyncCache {
        self.cache.clone()
    }

    /// Run sync for one resolved path, consulting and populating the cache.
    /// Returns the cached decision + status.
    pub fn run_for(&self, mapping_index: usize, resolved_path: PathBuf) -> CachedPathResult {
        let key = (mapping_index, resolved_path.clone());
        if let Some(cached) = self.cache.get(&key) {
            return cached;
        }

        let decision = self.decision_for_mapping(mapping_index);
        let status = match decision {
            SyncDecision::Ran => self.run_sync(mapping_index, &resolved_path),
            SyncDecision::Skipped | SyncDecision::NotApplicable => {
                if resolved_path.exists() {
                    PathStatus::Present
                } else {
                    PathStatus::Missing
                }
            }
        };

        let result = CachedPathResult { decision, status };
        self.cache.insert(key, result.clone());
        result
    }

    /// Returns `Skipped` if the mapping has a configured `sync_cmd` but the
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

    /// Spawn the configured `sync_cmd` once, substituting `{path}` with the
    /// resolved absolute path. Honors `sync_timeout`. Returns `SyncFailed`
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

/// Number of subprocess invocations performed across the most recent call to
/// `run_for` from a known set of paths. Not currently exposed outside the
/// module but kept here as a placeholder for future metric collection.
#[allow(dead_code)]
pub fn invocations(_batch_size: usize, _paths: usize) -> usize {
    _paths
}

/// Internal: blocking wait with timeout. Implemented via `wait_timeout` from
/// `std::os::unix::process::ExitStatusExt` -- BUT to keep this portable to
/// Windows, we use a simple spawn-and-wait loop with a watchdog thread.
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
    use crate::config::{Config, UriConfig, finalize_uri};
    use crate::resolution::uri::UriResolver;
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// Build a config with one optional mapping whose `sync_cmd` is `["true"]`
    /// (POSIX no-op) and `sync_required = required`.
    fn resolver_with(
        tmp: &TempDir,
        prefix: &str,
        root: &str,
        sync_cmd: Option<Vec<String>>,
        sync_required: bool,
        sync_timeout: u32,
    ) -> UriResolver {
        let uri = finalize_uri(PartialUriConfig {
            mappings: Some(vec![PartialUriMapping {
                prefix: Some(prefix.to_string()),
                root: Some(root.to_string()),
                sync_cmd,
                sync_required: Some(sync_required),
                sync_timeout: Some(sync_timeout),
            }]),
        })
        .unwrap();
        UriResolver::new(&uri, tmp.path()).unwrap()
    }

    fn empty_resolver() -> UriResolver {
        UriResolver::empty()
    }

    #[test]
    fn decision_for_mapping_without_sync_cmd_is_not_applicable() {
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
    fn decision_for_mapping_with_sync_cmd_but_no_flag_is_skipped() {
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
    fn decision_for_mapping_with_sync_cmd_and_flag_is_ran() {
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
    fn run_for_with_running_sync_cmd_marks_present_after_run() {
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
    fn run_for_with_failing_sync_cmd_returns_sync_failed() {
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

    // Smoke test: every SyncDecision value's PartialEq impl is exercised above.
    #[allow(dead_code)]
    fn _decision_is_partial_eq(_x: &HashMap<String, SyncDecision>) {}
    #[allow(dead_code)]
    fn _status_is_partial_eq(_x: &HashMap<String, PathStatus>) {}
    #[allow(dead_code)]
    fn _config_compat(_c: &Config, _u: &UriConfig) {}
}
