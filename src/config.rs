//! User-authored settings, kept apart from the machine-managed stores.
//!
//! `config.json` holds preferences a human edits by hand; the snooze and
//! hide stores are written by the tool. Mixing them would let a hand-edit
//! corrupt machine state, and the two want opposite failure handling: the
//! view warns and falls back to defaults on a bad config (the tool never
//! writes it, so there is nothing to clobber).
//!
//! ```json
//! {"stale_after": "3d",     // the silence threshold; null disables both nudges
//!  "skip_weekends": false,  // true: count working days only
//!  "mute": [{"author": "centreon-renovate", "unless_labels": ["S-Python"],
//!            "repos": ["centreon/centreon-pulse"]}]}
//! ```
//!
//! The file is validated by hand rather than through serde: the error
//! messages name the exact key and rule at fault, and a bad mute rule must
//! disable the whole list (a partial rule set would hide a different set of
//! PRs than the one the user wrote down).

use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::attention::DEFAULT_STALE_AFTER;
use crate::duration::{CALENDAR_WEEK_DAYS, Duration, parse_duration};
use crate::mute::MuteRule;
use crate::store::store_path;

/// Days a `w` stands for once weekends stop counting. Five, not seven, so
/// "give it a week" still names the same weekday a week later.
pub const WORKING_WEEK_DAYS: u32 = 5;

/// The keys a mute rule may carry. Anything else is rejected rather than
/// ignored: a misspelt `unless_label` would otherwise silently turn an
/// exempting rule into an unconditional one.
const MUTE_RULE_KEYS: [&str; 3] = ["author", "unless_labels", "repos"];

/// The config file is unreadable or holds an invalid value.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Silence threshold for the stale nudges; `None` disables both.
    pub stale_after: Option<Duration>,
    /// Leave Saturdays and Sundays out of that threshold and of snooze
    /// windows, on the machine's local calendar.
    pub skip_weekends: bool,
    /// Review requests to hide from the attention view, in file order.
    pub mute: Vec<MuteRule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stale_after: Some(DEFAULT_STALE_AFTER),
            skip_weekends: false,
            mute: Vec::new(),
        }
    }
}

/// Days a `w` stands for under the given weekend policy, shared by the
/// config file, `--stale-after` and `snooze --for` so all three read a
/// duration the same way.
#[must_use]
pub const fn week_days(skip_weekends: bool) -> u32 {
    if skip_weekends {
        WORKING_WEEK_DAYS
    } else {
        CALENDAR_WEEK_DAYS
    }
}

#[must_use]
pub fn config_path() -> PathBuf {
    store_path("config.json")
}

/// The stored settings; a missing file is all defaults.
///
/// Anything that prevents a clean read or holds an invalid value is an
/// error; the caller (the view) decides to warn and use the defaults.
pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let fail = |detail: String| ConfigError(format!("{}{detail}", path.display()));
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(ConfigError(format!("cannot read {}: {e}", path.display()))),
    };
    let text = String::from_utf8(bytes).map_err(|e| fail(format!(" is not valid UTF-8: {e}")))?;
    let data: Value =
        serde_json::from_str(&text).map_err(|e| fail(format!(" is not valid JSON: {e}")))?;
    let Value::Object(data) = data else {
        return Err(fail(
            " has an unexpected shape (want a JSON object)".to_owned(),
        ));
    };
    // skip_weekends first: it decides how long a 'w' is in stale_after.
    let skip_weekends = parse_skip_weekends(&data).map_err(fail)?;
    Ok(Config {
        stale_after: parse_stale_after(&data, skip_weekends).map_err(fail)?,
        skip_weekends,
        mute: parse_mute(&data).map_err(fail)?,
    })
}

// The parsers below return the message after the file's path.

fn parse_skip_weekends(data: &Map<String, Value>) -> Result<bool, String> {
    match data.get("skip_weekends") {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| ": 'skip_weekends' must be true or false".to_owned()),
    }
}

/// Absent is the default threshold, null disables the nudges.
fn parse_stale_after(
    data: &Map<String, Value>,
    skip_weekends: bool,
) -> Result<Option<Duration>, String> {
    let Some(value) = data.get("stale_after") else {
        return Ok(Some(DEFAULT_STALE_AFTER));
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().ok_or_else(|| {
        ": 'stale_after' must be a duration string like \"3d\" (or null to disable the staleness nudges)"
            .to_owned()
    })?;
    parse_duration(text, week_days(skip_weekends))
        .map(Some)
        .map_err(|e| format!(": invalid 'stale_after': {e}"))
}

fn parse_mute(data: &Map<String, Value>) -> Result<Vec<MuteRule>, String> {
    let Some(rules) = data.get("mute") else {
        return Ok(Vec::new());
    };
    let rules = rules
        .as_array()
        .ok_or_else(|| ": 'mute' must be a list of rules".to_owned())?;
    rules
        .iter()
        .enumerate()
        .map(|(index, rule)| parse_mute_rule(rule).map_err(|e| format!(": 'mute[{index}]'{e}")))
        .collect()
}

fn parse_mute_rule(rule: &Value) -> Result<MuteRule, String> {
    let rule = rule
        .as_object()
        .ok_or_else(|| " must be an object like {\"author\": ...}".to_owned())?;
    let mut unknown: Vec<&str> = rule
        .keys()
        .map(String::as_str)
        .filter(|key| !MUTE_RULE_KEYS.contains(key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(format!(" has unknown key(s): {}", unknown.join(", ")));
    }
    let author = rule
        .get("author")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|author| !author.is_empty())
        .ok_or_else(|| " needs a non-empty 'author' login".to_owned())?;
    let labels = name_list(rule.get("unless_labels"))
        .ok_or_else(|| ": 'unless_labels' must be a list of non-empty label names".to_owned())?;
    // A repository without an owner can never equal a nameWithOwner, so the
    // rule would silently match nothing.
    let repos = name_list(rule.get("repos"))
        .filter(|repos| repos.iter().all(|repo| repo.contains('/')))
        .ok_or_else(|| {
            ": 'repos' must be a list of non-empty 'owner/name' repositories".to_owned()
        })?;
    Ok(MuteRule::new(
        author,
        labels,
        repos.into_iter().map(str::trim),
    ))
}

/// The strings of a list of non-blank strings; absent is an empty list,
/// anything else is `None`.
fn name_list(value: Option<&Value>) -> Option<Vec<&str>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().filter(|name| !name.trim().is_empty()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(body: &str) -> Result<Config, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, body).unwrap();
        load_config(&path)
            .map_err(|e| ConfigError(e.0.replace(&path.display().to_string(), "PATH")))
    }

    fn error(body: &str) -> String {
        load(body).unwrap_err().to_string()
    }

    fn mute(body: &str) -> Result<Vec<MuteRule>, ConfigError> {
        load(&format!(r#"{{"mute": {body}}}"#)).map(|config| config.mute)
    }

    fn mute_error(body: &str) -> String {
        mute(body).unwrap_err().to_string()
    }

    #[test]
    fn missing_file_is_all_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_config(&dir.path().join("nope.json")).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.stale_after, Some(DEFAULT_STALE_AFTER));
    }

    #[test]
    fn absent_key_keeps_default() {
        assert_eq!(load("{}").unwrap().stale_after, Some(DEFAULT_STALE_AFTER));
    }

    #[test]
    fn valid_duration_is_parsed() {
        assert_eq!(
            load(r#"{"stale_after": "1w"}"#).unwrap().stale_after,
            Some(Duration::days(7))
        );
    }

    #[test]
    fn null_disables_the_nudge() {
        assert_eq!(load(r#"{"stale_after": null}"#).unwrap().stale_after, None);
    }

    #[test]
    fn weekends_count_by_default() {
        assert!(!load(r#"{"stale_after": "3d"}"#).unwrap().skip_weekends);
    }

    #[test]
    fn skip_weekends_is_read() {
        assert!(load(r#"{"skip_weekends": true}"#).unwrap().skip_weekends);
    }

    #[test]
    fn skipping_weekends_makes_a_week_five_days() {
        let config = load(r#"{"stale_after": "1w", "skip_weekends": true}"#).unwrap();
        assert_eq!(config.stale_after, Some(Duration::days(5)));
    }

    #[test]
    fn non_boolean_skip_weekends_raises() {
        for value in [r#""yes""#, "1", "null", "[]"] {
            assert_eq!(
                error(&format!(r#"{{"skip_weekends": {value}}}"#)),
                "PATH: 'skip_weekends' must be true or false",
                "{value}"
            );
        }
    }

    #[test]
    fn invalid_or_zero_or_overflowing_duration_raises() {
        let huge = format!(r#"{{"stale_after": "{}d"}}"#, "9".repeat(100));
        for body in [
            r#"{"stale_after": "soon"}"#,
            r#"{"stale_after": "0d"}"#,
            huge.as_str(),
        ] {
            assert!(
                error(body).starts_with("PATH: invalid 'stale_after': invalid duration"),
                "{body}"
            );
        }
    }

    #[test]
    fn non_string_duration_raises() {
        assert_eq!(
            error(r#"{"stale_after": 3}"#),
            r#"PATH: 'stale_after' must be a duration string like "3d" (or null to disable the staleness nudges)"#
        );
    }

    #[test]
    fn unreadable_path_raises() {
        // A directory, not a file: any read failure but "not found" is an error.
        let dir = tempfile::tempdir().unwrap();
        let message = load_config(dir.path()).unwrap_err().to_string();
        assert!(message.starts_with("cannot read "), "{message}");
    }

    #[test]
    fn invalid_json_raises() {
        assert!(error("{not json").starts_with("PATH is not valid JSON: "));
    }

    #[test]
    fn non_object_json_raises() {
        assert_eq!(
            error("[1, 2, 3]"),
            "PATH has an unexpected shape (want a JSON object)"
        );
    }

    #[test]
    fn non_utf8_raises() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, b"\xff\xfe{}").unwrap();
        let message = load_config(&path).unwrap_err().to_string();
        assert!(message.contains(" is not valid UTF-8: "), "{message}");
    }

    #[test]
    fn absent_or_empty_mute_means_no_rules() {
        assert_eq!(load("{}").unwrap().mute, []);
        assert_eq!(mute("[]").unwrap(), []);
    }

    #[test]
    fn author_only_rule() {
        assert_eq!(
            mute(r#"[{"author": "renovate"}]"#).unwrap(),
            [MuteRule::new("renovate", [], [])]
        );
    }

    #[test]
    fn rule_with_exempting_labels_keeps_file_order() {
        let body = r#"[{"author": "renovate", "unless_labels": ["S-Python", "S-Rust"]}, {"author": "dependabot"}]"#;
        assert_eq!(
            mute(body).unwrap(),
            [
                MuteRule::new("renovate", ["S-Python", "S-Rust"], []),
                MuteRule::new("dependabot", [], []),
            ]
        );
    }

    #[test]
    fn author_and_repos_are_stripped() {
        let body =
            r#"[{"author": " renovate ", "repos": ["centreon/centreon-pulse", " acme/widgets "]}]"#;
        assert_eq!(
            mute(body).unwrap(),
            [MuteRule::new(
                "renovate",
                [],
                ["centreon/centreon-pulse", "acme/widgets"]
            )]
        );
    }

    #[test]
    fn rules_coexist_with_the_staleness_settings() {
        let config =
            load(r#"{"stale_after": "1w", "skip_weekends": true, "mute": [{"author": "bot"}]}"#)
                .unwrap();
        assert_eq!(config.stale_after, Some(Duration::days(5)));
        assert_eq!(config.mute, [MuteRule::new("bot", [], [])]);
    }

    #[test]
    fn non_list_raises() {
        for body in [r#"{"author": "renovate"}"#, r#""renovate""#, "null", "1"] {
            assert_eq!(
                mute_error(body),
                "PATH: 'mute' must be a list of rules",
                "{body}"
            );
        }
    }

    #[test]
    fn non_object_rule_raises() {
        for body in [r#"["renovate"]"#, "[null]", "[[]]"] {
            assert_eq!(
                mute_error(body),
                r#"PATH: 'mute[0]' must be an object like {"author": ...}"#,
                "{body}"
            );
        }
    }

    #[test]
    fn missing_or_blank_author_raises() {
        for body in [
            "[{}]",
            r#"[{"author": ""}]"#,
            r#"[{"author": "  "}]"#,
            r#"[{"author": 1}]"#,
            r#"[{"author": null}]"#,
        ] {
            assert_eq!(
                mute_error(body),
                "PATH: 'mute[0]' needs a non-empty 'author' login",
                "{body}"
            );
        }
    }

    #[test]
    fn malformed_unless_labels_raises() {
        for labels in [
            r#""S-Python""#,
            "null",
            "[1]",
            r#"[""]"#,
            r#"["S-Python", null]"#,
        ] {
            assert_eq!(
                mute_error(&format!(
                    r#"[{{"author": "renovate", "unless_labels": {labels}}}]"#
                )),
                "PATH: 'mute[0]': 'unless_labels' must be a list of non-empty label names",
                "{labels}"
            );
        }
    }

    #[test]
    fn malformed_repos_raises() {
        for repos in [
            r#""acme/widgets""#,
            "null",
            "[1]",
            r#"[""]"#,
            r#"["acme/widgets", null]"#,
            r#"["widgets"]"#,
        ] {
            assert_eq!(
                mute_error(&format!(r#"[{{"author": "renovate", "repos": {repos}}}]"#)),
                "PATH: 'mute[0]': 'repos' must be a list of non-empty 'owner/name' repositories",
                "{repos}"
            );
        }
    }

    #[test]
    fn unknown_key_raises() {
        let body = r#"[{"author": "renovate", "unless_label": ["S-Python"]}]"#;
        assert_eq!(
            mute_error(body),
            "PATH: 'mute[0]' has unknown key(s): unless_label"
        );
    }

    #[test]
    fn error_names_the_offending_rule() {
        assert!(
            mute_error(r#"[{"author": "renovate"}, {"author": ""}]"#)
                .starts_with("PATH: 'mute[1]'")
        );
    }

    #[test]
    fn a_week_is_five_days_in_working_time_seven_on_the_calendar() {
        assert_eq!(week_days(false), 7);
        assert_eq!(week_days(true), 5);
    }
}
