use std::collections::HashMap;
use std::path::PathBuf;

/// Case-insensitive prefix index over file stems.
///
/// Built once per `ResolveInput`. For every stem, every leading prefix of that stem is
/// keyed (lowercased) to a list of paths whose stems start with that prefix. Lookups are
/// O(1) HashMap probes.
///
/// Total size is O(sum of stem char counts). For realistic vaults (a few thousand files
/// averaging tens of characters per stem) this is well under a second to build and under
/// a megabyte to hold.
#[derive(Clone, Debug, Default)]
pub struct PrefixIndex {
    map: HashMap<String, Vec<PathBuf>>,
}

impl PrefixIndex {
    /// Build a prefix index from `(stem, path)` pairs (typically produced by
    /// `ResolvedDocument::file_stem` and `path`).
    pub fn from_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, PathBuf)>,
    {
        let mut index = Self::default();
        for (stem, path) in entries {
            index.insert(&stem, path);
        }
        index
    }

    fn insert(&mut self, stem: &str, path: PathBuf) {
        let stem_lower = stem.to_ascii_lowercase();
        if stem_lower.is_empty() {
            return;
        }
        // Insert every non-empty leading prefix of the stem, plus the full stem
        // itself (so that a target equal to a stem resolves the same way as a
        // prefix lookup). char_indices yields valid UTF-8 byte offsets, so
        // `&stem_lower[..idx]` is always on a char boundary.
        for (idx, _) in stem_lower.char_indices() {
            if idx == 0 {
                continue;
            }
            let prefix = &stem_lower[..idx];
            self.map
                .entry(prefix.to_string())
                .or_default()
                .push(path.clone());
        }
        // Full stem as a key (covers the case where the target is exactly a stem).
        self.map
            .entry(stem_lower.clone())
            .or_default()
            .push(path);
    }

    /// Look up all documents whose stem begins with `target`. Returns an empty slice if
    /// `target` is empty (an empty target would match every document, which is never
    /// what we want) or if no stem starts with `target`.
    pub fn matches(&self, target: &str) -> &[PathBuf] {
        if target.is_empty() {
            return &[];
        }
        let key = target.to_ascii_lowercase();
        self.map.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Number of distinct prefix keys in the index. Useful for tests.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns true if the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn empty_index_returns_empty() {
        let idx = PrefixIndex::default();
        assert!(idx.matches("anything").is_empty());
        assert_eq!(idx.len(), 0);
    }

    #[test]
    fn unique_prefix_resolves() {
        let idx = PrefixIndex::from_entries(vec![(
            "20260801-topic-a-sub-x".to_string(),
            p("a/20260801-topic-a-sub-x.md"),
        )]);
        assert_eq!(idx.matches("20260801-topic-a").len(), 1);
        assert_eq!(idx.matches("20260801").len(), 1);
        assert_eq!(idx.matches("20260801-topic-a-sub-x").len(), 1);
    }

    #[test]
    fn ambiguous_prefix_resolves_to_multiple() {
        let idx = PrefixIndex::from_entries(vec![
            ("20260801-topic-a-sub-x".to_string(), p("sub-x.md")),
            ("20260801-topic-a-sub-y".to_string(), p("sub-y.md")),
        ]);
        assert_eq!(idx.matches("20260801-topic-a").len(), 2);
    }

    #[test]
    fn no_partial_match_returns_empty() {
        let idx = PrefixIndex::from_entries(vec![(
            "20260801-topic-a-sub-x".to_string(),
            p("a.md"),
        )]);
        assert!(idx.matches("zzz").is_empty());
    }

    #[test]
    fn empty_target_does_not_match_anything() {
        let idx = PrefixIndex::from_entries(vec![("abc".to_string(), p("a.md"))]);
        assert!(idx.matches("").is_empty());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let idx = PrefixIndex::from_entries(vec![(
            "20260801-TOPIC-A".to_string(),
            p("a.md"),
        )]);
        assert_eq!(idx.matches("20260801-topic-a").len(), 1);
        assert_eq!(idx.matches("20260801-TOPIC-A").len(), 1);
        assert_eq!(idx.matches("20260801-TOPIC").len(), 1);
    }

    #[test]
    fn leading_prefix_only_does_not_match_suffix() {
        let idx = PrefixIndex::from_entries(vec![(
            "xyz-topic".to_string(),
            p("a.md"),
        )]);
        // `topic` is a suffix of `xyz-topic` but NOT a leading prefix.
        assert!(idx.matches("topic").is_empty());
        // `xyz` is a leading prefix.
        assert_eq!(idx.matches("xyz").len(), 1);
    }

    #[test]
    fn cap_does_not_apply_to_index_itself() {
        // The index returns full candidate lists; capping is the diagnostic rule's job.
        let entries: Vec<_> = (0..10)
            .map(|i| (format!("prefix-{i}"), p(format!("p/{i}.md").as_str())))
            .collect();
        let idx = PrefixIndex::from_entries(entries);
        assert_eq!(idx.matches("prefix").len(), 10);
    }
}