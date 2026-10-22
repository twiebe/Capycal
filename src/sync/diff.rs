use std::collections::{HashMap, HashSet};

/// Etag-based change detection for sources without a server-side change log.
///
/// Fed one current (href, etag) at a time, so adapters can decide per event
/// whether to fetch or convert it (EventKit objects only live inside their
/// autorelease pool). `removed` yields the known hrefs never observed.
pub struct EtagDiff<'a> {
    known: HashMap<&'a str, &'a str>,
    current: HashSet<String>,
}

impl<'a> EtagDiff<'a> {
    /// `known` maps source href -> last synced source etag.
    pub fn new(known: HashMap<&'a str, &'a str>) -> Self {
        Self {
            known,
            current: HashSet::new(),
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn contains(&self, href: &str) -> bool {
        self.current.contains(href)
    }

    /// Record a current href. Returns true if it is new or its etag differs
    /// from the known one; false if unchanged or already observed.
    pub fn observe(&mut self, href: &str, etag: &str) -> bool {
        if !self.current.insert(href.to_string()) {
            return false;
        }
        self.known.get(href) != Some(&etag)
    }

    /// Number of distinct hrefs observed.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn observed(&self) -> usize {
        self.current.len()
    }

    /// Known hrefs that were not observed.
    pub fn removed(&self) -> Vec<String> {
        self.known
            .keys()
            .filter(|&&href| !self.current.contains(href))
            .map(|&href| href.to_string())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> HashMap<&'static str, &'static str> {
        [("/a", "1"), ("/b", "2"), ("/c", "3")].into()
    }

    #[test]
    fn test_new_href_is_changed() {
        let mut diff = EtagDiff::new(known());
        assert!(diff.observe("/new", "1"));
    }

    #[test]
    fn test_changed_and_unchanged_etag() {
        let mut diff = EtagDiff::new(known());
        assert!(diff.observe("/a", "1b"));
        assert!(!diff.observe("/b", "2"));
    }

    #[test]
    fn test_removed_are_unobserved_known() {
        let mut diff = EtagDiff::new(known());
        diff.observe("/a", "1");
        diff.observe("/new", "x");
        let mut removed = diff.removed();
        removed.sort();
        assert_eq!(removed, vec!["/b", "/c"]);
        assert_eq!(diff.observed(), 2);
    }

    #[test]
    fn test_duplicate_observation_is_not_changed_again() {
        let mut diff = EtagDiff::new(known());
        assert!(diff.observe("/new", "x"));
        assert!(diff.contains("/new"));
        assert!(!diff.observe("/new", "x"));
        assert_eq!(diff.observed(), 1);
    }

    #[test]
    fn test_empty() {
        let diff = EtagDiff::new(HashMap::new());
        assert!(diff.removed().is_empty());
        assert_eq!(diff.observed(), 0);

        let mut diff = EtagDiff::new(HashMap::new());
        assert!(diff.observe("/a", "1"));
        assert!(diff.removed().is_empty());

        let diff = EtagDiff::new(known());
        assert_eq!(diff.removed().len(), 3);
    }
}
