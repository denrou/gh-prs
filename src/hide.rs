//! Local per-PR hide store: keep a PR out of the attention view for good.
//!
//! A snooze acknowledges one state of a PR; a hide is the user's standing
//! decision that a PR is not theirs to act on, typically a review request a
//! teammate is already handling. Nothing on the PR side lifts it, only
//! `gh prs unhide`. It is the one place the tool hides on the user's word
//! alone, so the store records nothing about the PR beyond its URL and when
//! it was hidden.
//!
//! The store is `{url: {"since"}}` at `$XDG_CONFIG_HOME/gh-prs/hidden.json`.
//! Only the attention view (its table and `--count`) consults it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Map, Value, json};

use crate::pr::PullRequest;
use crate::store::{format_timestamp, read_json, store_path, write_json};

/// The store is unreadable or unwritable.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HideError(String);

/// One hidden PR: when it was hidden, informational only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hide {
    pub since: String,
}

impl Hide {
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self {
            since: format_timestamp(now, &TimeZone::UTC),
        }
    }
}

pub type Hidden = BTreeMap<String, Hide>;

#[must_use]
pub fn hide_path() -> PathBuf {
    store_path("hidden.json")
}

/// The stored hides; a missing file is an empty store.
///
/// Anything else that prevents a clean read is an error: fatal for the
/// subcommands (a write must not clobber the file), degradable for the view.
pub fn load_hidden(path: &Path) -> Result<Hidden, HideError> {
    let Some(data) = read_json(path).map_err(HideError)? else {
        return Ok(Hidden::new());
    };
    data.as_object()
        .and_then(|entries| {
            entries
                .iter()
                .map(|(url, entry)| {
                    let since = entry.as_object()?.get("since")?.as_str()?;
                    Some((
                        url.clone(),
                        Hide {
                            since: since.to_owned(),
                        },
                    ))
                })
                .collect()
        })
        .ok_or_else(|| {
            HideError(format!(
                "{} has an unexpected shape (want {{url: {{since}}}})",
                path.display()
            ))
        })
}

pub fn save_hidden(hidden: &Hidden, path: &Path) -> Result<(), HideError> {
    let data: Map<String, Value> = hidden
        .iter()
        .map(|(url, hide)| (url.clone(), json!({ "since": hide.since })))
        .collect();
    write_json(path, &Value::Object(data)).map_err(HideError)
}

/// Partition PRs into (visible, withheld), preserving order. A PR is
/// withheld when its URL is in the store, whatever its head or reasons.
#[must_use]
pub fn split_hidden(
    prs: Vec<PullRequest>,
    hidden: &Hidden,
) -> (Vec<PullRequest>, Vec<PullRequest>) {
    prs.into_iter()
        .partition(|pr| !hidden.contains_key(&pr.url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::Reason;
    use crate::pr::testing::{oid, pr, reasons};

    const URL: &str = "https://github.com/acme/widgets/pull/42";

    fn now() -> Timestamp {
        "2026-10-06T12:00:00Z".parse().unwrap()
    }

    fn load_raw(raw: &[u8]) -> Result<Hidden, HideError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hidden.json");
        std::fs::write(&path, raw).unwrap();
        load_hidden(&path)
    }

    #[test]
    fn missing_file_is_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_hidden(&dir.path().join("hidden.json")),
            Ok(Hidden::new())
        );
    }

    #[test]
    fn save_load_roundtrip_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("hidden.json");
        save_hidden(&Hidden::from([(URL.to_owned(), Hide::new(now()))]), &path).unwrap();
        let since = "2026-10-06T12:00:00+00:00".to_owned();
        assert_eq!(
            load_hidden(&path),
            Ok(Hidden::from([(URL.to_owned(), Hide { since })]))
        );
    }

    #[test]
    fn invalid_content_raises() {
        assert!(
            load_raw(b"{not json")
                .unwrap_err()
                .to_string()
                .contains("not valid JSON")
        );
        assert!(
            load_raw(b"\xff\xfe{\"a\": 1}")
                .unwrap_err()
                .to_string()
                .contains("not valid UTF-8")
        );
    }

    #[test]
    fn wrong_shape_raises() {
        for raw in [
            "[]",
            r#"{"url": "2026-10-06"}"#,
            r#"{"url": {}}"#,
            r#"{"url": {"since": 1}}"#,
        ] {
            let error = load_raw(raw.as_bytes()).unwrap_err().to_string();
            assert!(
                error.ends_with(" has an unexpected shape (want {url: {since}})"),
                "{raw}: {error}"
            );
        }
    }

    #[test]
    fn hidden_url_is_withheld_whatever_its_state() {
        // A hide is a standing decision: nothing about the PR can lift it.
        let pr = PullRequest {
            number: 42,
            url: URL.to_owned(),
            head: oid("beef"),
            attention: reasons(&[Reason::Review, Reason::NewCommits]),
            ..pr()
        };
        let (visible, withheld) = split_hidden(
            vec![pr],
            &Hidden::from([(URL.to_owned(), Hide::new(now()))]),
        );
        assert!(visible.is_empty());
        assert_eq!(withheld.len(), 1);
    }

    #[test]
    fn other_prs_stay_visible() {
        let other = PullRequest {
            url: "https://github.com/acme/widgets/pull/7".to_owned(),
            ..pr()
        };
        let (visible, withheld) = split_hidden(
            vec![other],
            &Hidden::from([(URL.to_owned(), Hide::new(now()))]),
        );
        assert_eq!((visible.len(), withheld.len()), (1, 0));
    }

    #[test]
    fn empty_store_withholds_nothing() {
        let (visible, withheld) = split_hidden(vec![pr()], &Hidden::new());
        assert_eq!((visible.len(), withheld.len()), (1, 0));
    }
}
