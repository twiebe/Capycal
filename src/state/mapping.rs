use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkRecord {
    pub sink_uid: String,
    pub sink_href: String,
    pub source_href: String,
    pub source_etag: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UidMapping {
    entries: HashMap<String, SinkRecord>,
}

impl UidMapping {
    pub fn get(&self, source_uid: &str) -> Option<&SinkRecord> {
        self.entries.get(source_uid)
    }

    pub fn insert(&mut self, source_uid: String, record: SinkRecord) {
        self.entries.insert(source_uid, record);
    }

    pub fn remove(&mut self, source_uid: &str) -> Option<SinkRecord> {
        self.entries.remove(source_uid)
    }

    /// Find the source UID for a given source href.
    pub fn find_by_source_href(&self, source_href: &str) -> Option<(&str, &SinkRecord)> {
        self.entries
            .iter()
            .find(|(_, record)| record.source_href == source_href)
            .map(|(k, v)| (k.as_str(), v))
    }

    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SinkRecord)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Known source etags by source href, the baseline for change detection.
    pub fn source_etags(&self) -> HashMap<&str, &str> {
        self.entries
            .values()
            .map(|record| (record.source_href.as_str(), record.source_etag.as_str()))
            .collect()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(suffix: &str) -> SinkRecord {
        SinkRecord {
            sink_uid: format!("sink-{suffix}"),
            sink_href: format!("/cal/sink-{suffix}.ics"),
            source_href: format!("/cal/src-{suffix}.ics"),
            source_etag: format!("\"etag-{suffix}\""),
        }
    }

    #[test]
    fn test_insert_and_get() {
        let mut m = UidMapping::default();
        m.insert("uid-1".to_string(), sample_record("1"));
        assert!(m.get("uid-1").is_some());
        assert_eq!(m.get("uid-1").unwrap().sink_uid, "sink-1");
        assert!(m.get("uid-2").is_none());
    }

    #[test]
    fn test_remove() {
        let mut m = UidMapping::default();
        m.insert("uid-1".to_string(), sample_record("1"));
        let removed = m.remove("uid-1");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().sink_uid, "sink-1");
        assert!(m.get("uid-1").is_none());
    }

    #[test]
    fn test_remove_nonexistent() {
        let mut m = UidMapping::default();
        assert!(m.remove("nope").is_none());
    }

    #[test]
    fn test_find_by_source_href() {
        let mut m = UidMapping::default();
        m.insert("uid-1".to_string(), sample_record("1"));
        m.insert("uid-2".to_string(), sample_record("2"));

        let found = m.find_by_source_href("/cal/src-2.ics");
        assert!(found.is_some());
        let (uid, record) = found.unwrap();
        assert_eq!(uid, "uid-2");
        assert_eq!(record.sink_uid, "sink-2");
    }

    #[test]
    fn test_find_by_source_href_not_found() {
        let mut m = UidMapping::default();
        m.insert("uid-1".to_string(), sample_record("1"));
        assert!(m.find_by_source_href("/cal/nonexistent.ics").is_none());
    }

    #[test]
    fn test_source_etags() {
        let mut m = UidMapping::default();
        m.insert("uid-1".to_string(), sample_record("1"));
        m.insert("uid-2".to_string(), sample_record("2"));

        let etags = m.source_etags();
        assert_eq!(etags.len(), 2);
        assert_eq!(etags.get("/cal/src-1.ics"), Some(&"\"etag-1\""));
        assert_eq!(etags.get("/cal/src-2.ics"), Some(&"\"etag-2\""));
    }

    #[test]
    fn test_insert_overwrites() {
        let mut m = UidMapping::default();
        m.insert("uid-1".to_string(), sample_record("1"));
        assert_eq!(m.get("uid-1").unwrap().source_etag, "\"etag-1\"");

        m.insert(
            "uid-1".to_string(),
            SinkRecord {
                source_etag: "\"etag-updated\"".to_string(),
                ..sample_record("1")
            },
        );
        assert_eq!(m.get("uid-1").unwrap().source_etag, "\"etag-updated\"");
        assert_eq!(m.len(), 1);
    }
}
