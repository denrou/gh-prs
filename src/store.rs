//! The files gh-prs keeps under `$XDG_CONFIG_HOME/gh-prs/` (`~/.config` by
//! default), and the JSON I/O they share.
//!
//! Every way a file can fail to load (unreadable, not UTF-8, not JSON) comes
//! back as a message naming the file, which each caller wraps in its own
//! error type; a missing file is `None`. Writes go through a temporary file
//! and an atomic rename, so a crash mid-write can't leave a truncated store
//! behind (which would then read as corrupt). Shape validation stays with
//! each caller: only it knows what an entry must look like.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::Value;

/// `$XDG_CONFIG_HOME/gh-prs/<name>`, or `~/.config/gh-prs/<name>`.
#[must_use]
pub fn store_path(name: &str) -> PathBuf {
    path_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::home_dir(),
        name,
    )
}

/// [`store_path`] with the environment passed in, so tests don't have to
/// mutate the process environment. An empty `XDG_CONFIG_HOME` counts as
/// unset, as the XDG spec says.
fn path_from(xdg_config_home: Option<OsString>, home: Option<PathBuf>, name: &str) -> PathBuf {
    let config_home = xdg_config_home
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.unwrap_or_default().join(".config"));
    config_home.join("gh-prs").join(name)
}

/// Parse `path` as JSON; `None` when the file doesn't exist.
pub fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let text = String::from_utf8(bytes)
        .map_err(|e| format!("{} is not valid UTF-8: {e}", path.display()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))
}

/// Write `data` to `path` as indented JSON with sorted keys, creating the
/// directory if needed.
pub fn write_json(path: &Path, data: &Value) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    // serde_json's map keeps keys sorted (no `preserve_order` feature), as
    // Python's sort_keys did, so a store rewritten by either shows no diff.
    let text = serde_json::to_string_pretty(data)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, text + "\n").map_err(fail)?;
    fs::rename(&tmp, path).map_err(fail)
}

/// A timestamp as the stores write it: to the second, with the offset of
/// `tz` (`2027-09-03T12:30:37+00:00`), the form Python's
/// `isoformat(timespec="seconds")` produced.
#[must_use]
pub fn format_timestamp(timestamp: Timestamp, tz: &TimeZone) -> String {
    timestamp
        .to_zoned(tz.clone())
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn uses_xdg_config_home() {
        let path = path_from(Some("/xdg".into()), Some("/home/me".into()), "config.json");
        assert_eq!(path, PathBuf::from("/xdg/gh-prs/config.json"));
    }

    #[test]
    fn falls_back_to_home_config() {
        for xdg in [None, Some(OsString::new())] {
            let path = path_from(xdg, Some("/home/me".into()), "snooze.json");
            assert_eq!(path, PathBuf::from("/home/me/.config/gh-prs/snooze.json"));
        }
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_json(&dir.path().join("nope.json")), Ok(None));
    }

    #[test]
    fn write_creates_directories_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("store.json");
        let data = json!({"b": {"until": null, "oid": "x"}, "a": []});
        write_json(&path, &data).unwrap();
        assert_eq!(read_json(&path), Ok(Some(data)));
        assert!(!dir.path().join("deep").join("store.json.tmp").exists());
    }

    #[test]
    fn written_like_python_did() {
        // Sorted keys, two-space indent, trailing newline: a store rewritten
        // by the other version shows no diff.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        write_json(
            &path,
            &json!({"u": {"until": null, "reasons": ["review"], "oid": "c"}}),
        )
        .unwrap();
        let expected = "{\n  \"u\": {\n    \"oid\": \"c\",\n    \"reasons\": [\n      \"review\"\n    ],\n    \"until\": null\n  }\n}\n";
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn unreadable_file_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_json(dir.path()).unwrap_err();
        assert!(
            error.starts_with(&format!("cannot read {}: ", dir.path().display())),
            "{error}"
        );
    }

    #[test]
    fn invalid_content_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        fs::write(&path, b"\xff\xfe{}").unwrap();
        assert!(
            read_json(&path)
                .unwrap_err()
                .contains(" is not valid UTF-8: ")
        );
        fs::write(&path, "{not json").unwrap();
        assert!(
            read_json(&path)
                .unwrap_err()
                .contains(" is not valid JSON: ")
        );
    }

    #[test]
    fn timestamps_are_written_to_the_second_with_an_offset() {
        let at: Timestamp = "2027-09-03T12:30:37.123456Z".parse().unwrap();
        assert_eq!(
            format_timestamp(at, &TimeZone::UTC),
            "2027-09-03T12:30:37+00:00"
        );
        let paris = TimeZone::get("Europe/Paris").unwrap();
        assert_eq!(format_timestamp(at, &paris), "2027-09-03T14:30:37+02:00");
    }
}
