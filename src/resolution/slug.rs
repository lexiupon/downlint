use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Slug(String);

impl Slug {
    pub fn from_heading_text(input: &str) -> Self {
        let mut out = String::new();
        let mut last_was_dash = false;

        for ch in input.nfkc().flat_map(char::to_lowercase) {
            if ch.is_alphanumeric() || is_non_ascii_letter_or_number(ch) || ch == '-' || ch == '_' {
                out.push(ch);
                last_was_dash = false;
            } else if ch.is_whitespace() && !last_was_dash && !out.is_empty() {
                out.push('-');
                last_was_dash = true;
            }
        }

        while out.ends_with('-') {
            out.pop();
        }

        Self(out)
    }

    pub fn with_suffix(base: &Self, suffix: usize) -> Self {
        Self(format!("{}-{suffix}", base.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_subsequence(haystack: &str, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let mut chars = needle.chars().map(|ch| ch.to_ascii_lowercase());
        let mut current = chars.next();
        for ch in haystack.chars().map(|ch| ch.to_ascii_lowercase()) {
            if Some(ch) == current {
                current = chars.next();
                if current.is_none() {
                    return true;
                }
            }
        }
        false
    }

    pub fn is_substring(haystack: &str, needle: &str) -> bool {
        haystack
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    }

    /// A normalized form of the slug with consecutive `-` collapsed to a single
    /// `-`. Used for tolerant anchor matching: a link with a literal `--` (often
    /// from copy-pasting an em-dash-separated heading) can still resolve to a
    /// heading whose slug has `-` from the dropped em dash.
    pub fn folded(&self) -> Self {
        let mut out = String::with_capacity(self.0.len());
        let mut prev_dash = false;
        for ch in self.0.chars() {
            if ch == '-' {
                if !prev_dash {
                    out.push(ch);
                    prev_dash = true;
                }
            } else {
                out.push(ch);
                prev_dash = false;
            }
        }
        Self(out)
    }
}

impl From<String> for Slug {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Slug {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

fn is_non_ascii_letter_or_number(ch: char) -> bool {
    !ch.is_ascii() && (ch.is_alphanumeric() || ch.is_numeric())
}

#[cfg(test)]
mod tests {
    use super::Slug;

    #[test]
    fn slug_preserves_cjk_and_strips_punctuation() {
        assert_eq!(Slug::from_heading_text("What's Up?").as_str(), "whats-up");
        assert_eq!(Slug::from_heading_text("中文 标题").as_str(), "中文-标题");
    }

    #[test]
    fn folded_collapses_consecutive_dashes() {
        // Single dash unchanged.
        assert_eq!(Slug::from("foo-bar-baz").folded().as_str(), "foo-bar-baz");
        // Doubled dash collapsed to a single dash.
        assert_eq!(Slug::from("foo--bar").folded().as_str(), "foo-bar");
        // Tripled dash collapsed to a single dash.
        assert_eq!(Slug::from("foo---bar").folded().as_str(), "foo-bar");
        // Trailing doubled dash collapsed.
        assert_eq!(Slug::from("foo--").folded().as_str(), "foo-");
        // Leading doubled dash collapsed.
        assert_eq!(Slug::from("--foo").folded().as_str(), "-foo");
        // Slug with no dashes unchanged.
        assert_eq!(Slug::from("foo").folded().as_str(), "foo");
        // Empty slug unchanged.
        assert_eq!(Slug::from("").folded().as_str(), "");
        // Tolerant: an em-dash heading slug (`2024-closest`) and a literal
        // double-dash anchor (`2024--closest`) become equal after folding.
        let heading_slug = Slug::from("2024-closest").folded();
        let anchor_slug = Slug::from("2024--closest").folded();
        assert_eq!(heading_slug.as_str(), anchor_slug.as_str());
    }
}
