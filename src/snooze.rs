//! Local per-PR snooze store: hide a PR from the attention view until it
//! changes.
//!
//! A snooze records the PR's head oid, the attention reasons it had at
//! snooze time, and, when `--for` asked for one, a window. The PR stays
//! hidden only while all hold: the head still matches, the reasons are
//! unchanged, and the window hasn't elapsed. As soon as one breaks (new
//! commits, a review landing that turns a waiting PR into one ready to
//! merge, the clock, or a value that can't be compared) the PR resurfaces and
//! the dead entry is pruned. A snooze may only ever hide the exact
//! acknowledged state.
//!
//! An open-ended snooze has nothing but the head and the reasons to lift it,
//! so it must carry reasons: [`Snooze::OpenEnded`] can't be built without
//! them, and loading refuses a stored one that lacks them.
//!
//! The store is `{url: {"oid", "until", "reasons"?}}` at
//! `$XDG_CONFIG_HOME/gh-prs/snooze.json`. Only the attention view (its table
//! and `--count`) consults it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::{Map, Value, json};

use crate::pr::PullRequest;
use crate::store::{read_json, store_path, write_json};

/// The store is unreadable or unwritable, or a PR reference is invalid.
#[derive(Debug, PartialEq, Eq)]
pub struct SnoozeError(String);

impl fmt::Display for SnoozeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SnoozeError {}

/// One stored snooze.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Snooze {
    /// Lifted only by the head moving or the reasons changing.
    OpenEnded { oid: String, reasons: Vec<String> },
    /// Also lifted when `until` passes. `until` stays the stored text: an
    /// unparseable one means "expired, show the PR", not a corrupt store.
    /// `reasons` is absent on entries written before reason tracking, and
    /// when they couldn't be captured; such entries rely on head and window.
    Windowed {
        oid: String,
        until: String,
        reasons: Option<Vec<String>>,
    },
}

impl Snooze {
    /// An open-ended snooze; the reasons are stored sorted so a later
    /// comparison doesn't depend on order.
    #[must_use]
    pub fn open_ended(oid: &str, reasons: &[&str]) -> Self {
        Self::OpenEnded {
            oid: oid.to_owned(),
            reasons: sorted(reasons),
        }
    }

    /// A snooze with a window. `None` reasons leaves it on head and window
    /// alone; an empty list means "captured, had none", which a later
    /// non-empty set must differ from.
    #[must_use]
    pub fn windowed(oid: &str, until: &str, reasons: Option<&[&str]>) -> Self {
        Self::Windowed {
            oid: oid.to_owned(),
            until: until.to_owned(),
            reasons: reasons.map(sorted),
        }
    }

    #[must_use]
    pub fn oid(&self) -> &str {
        match self {
            Self::OpenEnded { oid, .. } | Self::Windowed { oid, .. } => oid,
        }
    }

    fn reasons(&self) -> Option<&[String]> {
        match self {
            Self::OpenEnded { reasons, .. } => Some(reasons),
            Self::Windowed { reasons, .. } => reasons.as_deref(),
        }
    }

    /// True when the window has elapsed. An open-ended snooze never
    /// expires; an unparseable or offset-less `until` counts as expired, so
    /// the PR shows.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        match self {
            Self::OpenEnded { .. } => false,
            Self::Windowed { until, .. } => until
                .parse::<Timestamp>()
                .ok()
                .is_none_or(|until| until <= now),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        let entry = value.as_object()?;
        let oid = entry.get("oid")?.as_str()?.to_owned();
        // A null `reasons` reads as absent, as it always has.
        let reasons = match entry.get("reasons").filter(|r| !r.is_null()) {
            None => None,
            Some(reasons) => Some(
                reasons
                    .as_array()?
                    .iter()
                    .map(|r| r.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?,
            ),
        };
        // A missing `until` is corruption, never "forever".
        let until = entry.get("until")?;
        if until.is_null() {
            Some(Self::OpenEnded {
                oid,
                reasons: reasons?,
            })
        } else {
            Some(Self::Windowed {
                oid,
                until: until.as_str()?.to_owned(),
                reasons,
            })
        }
    }

    fn to_json(&self) -> Value {
        let (oid, until, reasons) = match self {
            Self::OpenEnded { oid, reasons } => (oid, Value::Null, Some(reasons)),
            Self::Windowed {
                oid,
                until,
                reasons,
            } => (oid, json!(until), reasons.as_ref()),
        };
        let mut entry = Map::new();
        entry.insert("oid".to_owned(), json!(oid));
        entry.insert("until".to_owned(), until);
        if let Some(reasons) = reasons {
            entry.insert("reasons".to_owned(), json!(reasons));
        }
        Value::Object(entry)
    }
}

fn sorted(reasons: &[&str]) -> Vec<String> {
    let mut reasons: Vec<String> = reasons.iter().map(|r| (*r).to_owned()).collect();
    reasons.sort_unstable();
    reasons
}

pub type Snoozes = BTreeMap<String, Snooze>;

#[must_use]
pub fn snooze_path() -> PathBuf {
    store_path("snooze.json")
}

/// The stored snoozes; a missing file is an empty store.
///
/// Anything else that prevents a clean read is an error: the caller decides
/// whether it is fatal (a write command must not clobber the file) or
/// degradable (the attention view shows more, never less).
pub fn load_snoozes(path: &Path) -> Result<Snoozes, SnoozeError> {
    let Some(data) = read_json(path).map_err(SnoozeError)? else {
        return Ok(Snoozes::new());
    };
    data.as_object()
        .and_then(|entries| {
            entries
                .iter()
                .map(|(url, entry)| Some((url.clone(), Snooze::from_json(entry)?)))
                .collect()
        })
        .ok_or_else(|| {
            SnoozeError(format!(
                "{} has an unexpected shape (want {{url: {{oid, until, reasons?}}}}; an entry with a null until must have reasons)",
                path.display()
            ))
        })
}

pub fn save_snoozes(snoozes: &Snoozes, path: &Path) -> Result<(), SnoozeError> {
    let data: Map<String, Value> = snoozes
        .iter()
        .map(|(url, snooze)| (url.clone(), snooze.to_json()))
        .collect();
    write_json(path, &Value::Object(data)).map_err(SnoozeError)
}

/// Canonicalize a full PR URL to the form GraphQL reports (`…/pull/<n>`).
///
/// Browser navigation state after the number (`/files`, `?diff=split`,
/// `#discussion_r1`) is dropped; anything fused to the digits
/// (`…/pull/42abc`) is a typo, and truncating it would snooze the wrong PR.
/// Only `https` URLs are canonical. A bare PR number is resolved through
/// `gh` instead (see the CLI); storing a key that can never match a fetched
/// PR's URL would silently do nothing.
pub fn normalize_pr_url(reference: &str) -> Result<String, SnoozeError> {
    let reference = reference.trim();
    canonical_pr_url(reference).ok_or_else(|| {
        SnoozeError(format!(
            "not a pull request URL: '{reference}' (pass a PR number with --repo, or a full https://…/pull/<n> URL)"
        ))
    })
}

fn canonical_pr_url(reference: &str) -> Option<String> {
    let rest = reference.strip_prefix("https://")?;
    let segment = |text: &str| !text.is_empty() && !text.contains(char::is_whitespace);
    let mut parts = rest.splitn(5, '/');
    let (host, owner, repo, pull) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let tail = parts.next()?;
    if !(segment(host) && segment(owner) && segment(repo) && pull == "pull") {
        return None;
    }
    let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (number, after) = tail.split_at(digits);
    if number.is_empty() || !(after.is_empty() || after.starts_with(['/', '?', '#'])) {
        return None;
    }
    Some(format!("https://{host}/{owner}/{repo}/pull/{number}"))
}

/// Why a snooze died, for the "snooze expired" warning.
pub const WINDOW_ELAPSED: &str = "snooze window elapsed";
pub const HEAD_MOVED: &str = "head moved since you snoozed it";
pub const STATUS_CHANGED: &str = "its status changed since you snoozed it";

/// The PRs split by the store: visible, hidden, and the URLs of dead
/// snoozes with why they died, for the caller to prune.
#[derive(Debug, Default)]
pub struct SnoozeSplit {
    pub visible: Vec<PullRequest>,
    pub hidden: Vec<PullRequest>,
    pub dead: BTreeMap<String, &'static str>,
}

/// Partition PRs by the store.
///
/// A PR is hidden only while its head is known and equals the snoozed oid,
/// the window (if any) is open, and its attention reasons still match the
/// ones recorded. An entry whose PR is absent from the search is kept while
/// its window is open (the PR may merely be beyond the search cap) and dies
/// once it elapses; an open-ended one is never pruned here, since nothing
/// here can tell a closed PR from one beyond the cap (`snooze --prune` is
/// the explicit clean-up).
#[must_use]
pub fn split_snoozed(prs: Vec<PullRequest>, snoozes: &Snoozes, now: Timestamp) -> SnoozeSplit {
    let mut split = SnoozeSplit::default();
    for pr in &prs {
        let Some(snooze) = snoozes.get(&pr.url) else {
            continue;
        };
        let head_matches = !snooze.oid().is_empty()
            && pr
                .head
                .as_ref()
                .is_some_and(|head| head.as_str() == snooze.oid());
        let current: Vec<&str> = pr.attention.iter().map(|reason| reason.as_str()).collect();
        let why = if snooze.is_expired(now) {
            Some(WINDOW_ELAPSED)
        } else if !head_matches {
            Some(HEAD_MOVED)
        } else if snooze
            .reasons()
            .is_some_and(|snoozed| snoozed != current.as_slice())
        {
            Some(STATUS_CHANGED)
        } else {
            None
        };
        if let Some(why) = why {
            split.dead.insert(pr.url.clone(), why);
        }
    }
    for (url, snooze) in snoozes {
        if !prs.iter().any(|pr| &pr.url == url) && snooze.is_expired(now) {
            split.dead.insert(url.clone(), WINDOW_ELAPSED);
        }
    }
    for pr in prs {
        let hidden = snoozes.contains_key(&pr.url) && !split.dead.contains_key(&pr.url);
        if hidden {
            split.hidden.push(pr)
        } else {
            split.visible.push(pr)
        }
    }
    split
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::Reason;
    use crate::pr::testing::{oid, pr, reasons};

    const URL: &str = "https://github.com/acme/widgets/pull/42";
    const NOW: &str = "2026-07-22T12:00:00Z";
    const LATER: &str = "2026-07-23T12:00:00+00:00";
    const EARLIER: &str = "2026-07-22T11:00:00+00:00";

    fn now() -> Timestamp {
        NOW.parse().unwrap()
    }

    fn snoozed_pr(head: &str) -> PullRequest {
        PullRequest {
            number: 42,
            url: URL.to_owned(),
            head: oid(head),
            ..pr()
        }
    }

    fn store(snooze: Snooze) -> Snoozes {
        Snoozes::from([(URL.to_owned(), snooze)])
    }

    fn live() -> Snooze {
        Snooze::windowed("cafe123", LATER, None)
    }

    fn numbers(prs: &[PullRequest]) -> Vec<u64> {
        prs.iter().map(|pr| pr.number).collect()
    }

    mod normalize {
        use super::*;

        #[test]
        fn canonical_url_unchanged() {
            assert_eq!(normalize_pr_url(URL), Ok(URL.to_owned()));
        }

        #[test]
        fn browser_navigation_state_stripped() {
            for suffix in [
                "/",
                "/files",
                "/files?diff=split",
                "#discussion_r1",
                "?notification_referrer_id=x",
            ] {
                assert_eq!(
                    normalize_pr_url(&format!("{URL}{suffix}")),
                    Ok(URL.to_owned()),
                    "{suffix}"
                );
            }
        }

        #[test]
        fn surrounding_whitespace_stripped() {
            assert_eq!(normalize_pr_url(&format!("  {URL}\n")), Ok(URL.to_owned()));
        }

        #[test]
        fn enterprise_host_accepted() {
            let url = "https://ghe.example.com/acme/widgets/pull/7";
            assert_eq!(normalize_pr_url(url), Ok(url.to_owned()));
        }

        #[test]
        fn non_pr_reference_rejected() {
            for bad in [
                "",
                "acme/widgets",
                "acme/widgets/1a",
                "acme/widgets/42",
                "acme/widgets#42",
                "acme/widgets/pull/42",
                "https://github.com/acme/widgets",
                "https://github.com/acme/widgets/issues/42",
                "http://github.com/acme/widgets/pull/42",
                "https://github.com/acme/widgets/pull/42abc",
                "https://github.com/acme/widgets/pull/",
                "https://github.com/acme/wid gets/pull/42",
            ] {
                assert!(normalize_pr_url(bad).is_err(), "{bad:?}");
            }
        }

        #[test]
        fn multi_digit_number_not_truncated() {
            let url = "https://github.com/acme/widgets/pull/423";
            assert_eq!(
                normalize_pr_url(&format!("{url}/files")),
                Ok(url.to_owned())
            );
        }

        #[test]
        fn error_names_the_reference_and_the_way_out() {
            assert_eq!(
                normalize_pr_url(" acme/widgets#42 ")
                    .unwrap_err()
                    .to_string(),
                "not a pull request URL: 'acme/widgets#42' (pass a PR number with --repo, or a full https://…/pull/<n> URL)"
            );
        }
    }

    mod store_io {
        use super::*;

        fn load_raw(raw: &str) -> Result<Snoozes, SnoozeError> {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("snooze.json");
            std::fs::write(&path, raw).unwrap();
            load_snoozes(&path)
        }

        #[test]
        fn missing_file_is_empty_store() {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(
                load_snoozes(&dir.path().join("snooze.json")),
                Ok(Snoozes::new())
            );
        }

        #[test]
        fn every_kind_of_entry_roundtrips() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("deep").join("snooze.json");
            let snoozes = Snoozes::from([
                ("a".to_owned(), live()),
                (
                    "b".to_owned(),
                    Snooze::windowed("cafe", LATER, Some(&["review"])),
                ),
                ("c".to_owned(), Snooze::open_ended("cafe", &["review"])),
            ]);
            save_snoozes(&snoozes, &path).unwrap();
            assert_eq!(load_snoozes(&path), Ok(snoozes));
            assert!(
                std::fs::read_to_string(&path)
                    .unwrap()
                    .contains("\"until\": null")
            );
        }

        #[test]
        fn reads_what_python_wrote() {
            let raw = r#"{"https://github.com/acme/widgets/pull/42": {"oid": "cafe", "reasons": ["review"], "until": null},
                          "https://github.com/acme/widgets/pull/7": {"oid": "beef", "until": "2027-09-03T12:30:37+00:00"}}"#;
            let snoozes = load_raw(raw).unwrap();
            assert_eq!(snoozes[URL], Snooze::open_ended("cafe", &["review"]));
            assert_eq!(
                snoozes["https://github.com/acme/widgets/pull/7"],
                Snooze::windowed("beef", "2027-09-03T12:30:37+00:00", None)
            );
        }

        #[test]
        fn null_reasons_read_as_absent() {
            let snoozes =
                load_raw(r#"{"u": {"oid": "c", "until": "t", "reasons": null}}"#).unwrap();
            assert_eq!(snoozes["u"], Snooze::windowed("c", "t", None));
        }

        #[test]
        fn invalid_content_raises() {
            assert!(load_raw("{not json").is_err());
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("snooze.json");
            std::fs::write(&path, b"\xff\xfe{\"a\": 1}").unwrap();
            assert!(load_snoozes(&path).is_err());
        }

        #[test]
        fn wrong_shape_raises() {
            for raw in [
                "[]",
                r#""oid""#,
                r#"{"url": 1}"#,
                r#"{"url": "bare-oid"}"#,
                r#"{"url": {"oid": "cafe"}}"#,
                r#"{"url": {"oid": "cafe", "until": 5}}"#,
                r#"{"url": {"oid": "cafe", "until": null}}"#,
                r#"{"url": {"oid": "cafe", "until": "t", "reasons": "review"}}"#,
                r#"{"url": {"oid": "cafe", "until": "t", "reasons": [1]}}"#,
            ] {
                let error = load_raw(raw).unwrap_err().to_string();
                assert!(error.ends_with(
                    " has an unexpected shape (want {url: {oid, until, reasons?}}; an entry with a null until must have reasons)"
                ), "{raw}: {error}");
            }
        }

        #[test]
        fn reasons_stored_sorted() {
            let snooze = Snooze::windowed("cafe", LATER, Some(&["review", "conflict"]));
            assert_eq!(
                snooze.reasons(),
                Some(["conflict".to_owned(), "review".to_owned()].as_slice())
            );
        }

        #[test]
        fn empty_reasons_kept_distinct_from_absent() {
            assert_eq!(
                Snooze::windowed("cafe", LATER, Some(&[])).reasons(),
                Some([].as_slice())
            );
            assert_eq!(Snooze::windowed("cafe", LATER, None).reasons(), None);
        }
    }

    mod expiry {
        use super::*;

        #[test]
        fn future_timestamp_is_live() {
            assert!(!live().is_expired(now()));
        }

        #[test]
        fn past_timestamp_is_expired() {
            assert!(Snooze::windowed("cafe", EARLIER, None).is_expired(now()));
        }

        #[test]
        fn the_deadline_itself_is_expired() {
            assert!(Snooze::windowed("cafe", "2026-07-22T12:00:00+00:00", None).is_expired(now()));
        }

        #[test]
        fn open_ended_entry_never_expires() {
            assert!(!Snooze::open_ended("cafe", &["review"]).is_expired(now()));
        }

        #[test]
        fn uncomparable_timestamp_counts_as_expired() {
            // The last one has no offset.
            for until in ["", "not-a-date", "2026-07-23T12:00:00"] {
                assert!(
                    Snooze::windowed("cafe", until, None).is_expired(now()),
                    "{until:?}"
                );
            }
        }
    }

    mod split {
        use super::*;

        #[test]
        fn matching_head_within_window_is_hidden() {
            let split = split_snoozed(vec![snoozed_pr("cafe123")], &store(live()), now());
            assert_eq!((split.visible.len(), numbers(&split.hidden)), (0, vec![42]));
            assert!(split.dead.is_empty());
        }

        #[test]
        fn moved_head_resurfaces_as_dead() {
            let split = split_snoozed(vec![snoozed_pr("beef456")], &store(live()), now());
            assert_eq!((numbers(&split.visible), split.hidden.len()), (vec![42], 0));
            assert_eq!(split.dead, BTreeMap::from([(URL.to_owned(), HEAD_MOVED)]));
        }

        #[test]
        fn elapsed_window_resurfaces_as_dead() {
            let snoozes = store(Snooze::windowed("cafe123", EARLIER, None));
            let split = split_snoozed(vec![snoozed_pr("cafe123")], &snoozes, now());
            assert_eq!(numbers(&split.visible), [42]);
            assert_eq!(
                split.dead,
                BTreeMap::from([(URL.to_owned(), WINDOW_ELAPSED)])
            );
        }

        #[test]
        fn unknown_head_or_stored_oid_resurfaces_as_dead() {
            let unknown_head = split_snoozed(vec![snoozed_pr("")], &store(live()), now());
            assert_eq!(numbers(&unknown_head.visible), [42]);
            assert!(unknown_head.dead.contains_key(URL));
            let empty_oid = store(Snooze::windowed("", LATER, None));
            let split = split_snoozed(vec![snoozed_pr("")], &empty_oid, now());
            assert_eq!(numbers(&split.visible), [42]);
            assert!(split.dead.contains_key(URL));
        }

        #[test]
        fn changed_reasons_resurface_as_dead() {
            // A review landed: the PR waiting on reviewers is now ready.
            let snoozes = store(Snooze::open_ended("cafe123", &["stale"]));
            let ready = PullRequest {
                attention: reasons(&[Reason::Ready]),
                ..snoozed_pr("cafe123")
            };
            let split = split_snoozed(vec![ready], &snoozes, now());
            assert_eq!(numbers(&split.visible), [42]);
            assert_eq!(
                split.dead,
                BTreeMap::from([(URL.to_owned(), STATUS_CHANGED)])
            );
        }

        #[test]
        fn unchanged_reasons_stay_hidden() {
            let snoozes = store(Snooze::open_ended("cafe123", &["unresolved", "conflict"]));
            let pr = PullRequest {
                attention: reasons(&[Reason::Conflict, Reason::Unresolved]),
                ..snoozed_pr("cafe123")
            };
            assert_eq!(
                numbers(&split_snoozed(vec![pr], &snoozes, now()).hidden),
                [42]
            );
        }

        #[test]
        fn entries_without_reasons_ignore_them() {
            let pr = PullRequest {
                attention: reasons(&[Reason::Ready]),
                ..snoozed_pr("cafe123")
            };
            assert_eq!(
                numbers(&split_snoozed(vec![pr], &store(live()), now()).hidden),
                [42]
            );
        }

        #[test]
        fn unsnoozed_pr_stays_visible() {
            let other = Snoozes::from([("https://github.com/x/y/pull/1".to_owned(), live())]);
            let split = split_snoozed(vec![snoozed_pr("cafe123")], &other, now());
            assert_eq!(numbers(&split.visible), [42]);
            assert!(split.hidden.is_empty() && split.dead.is_empty());
        }

        #[test]
        fn live_or_open_ended_entry_for_absent_pr_is_kept() {
            for snooze in [live(), Snooze::open_ended("cafe", &[])] {
                let snoozes = Snoozes::from([("https://github.com/x/y/pull/1".to_owned(), snooze)]);
                assert!(split_snoozed(Vec::new(), &snoozes, now()).dead.is_empty());
            }
        }

        #[test]
        fn elapsed_entry_for_absent_pr_is_dead() {
            let url = "https://github.com/x/y/pull/1".to_owned();
            let snoozes = Snoozes::from([(url.clone(), Snooze::windowed("cafe", EARLIER, None))]);
            let split = split_snoozed(Vec::new(), &snoozes, now());
            assert_eq!(split.dead, BTreeMap::from([(url, WINDOW_ELAPSED)]));
        }
    }
}
