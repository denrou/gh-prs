//! Black-box conformance suite (docs/rust-port.md, Testing).
//!
//! Every directory under `scenarios/` runs the binary named by `GH_PRS_BIN`
//! (default: this crate's binary) once and compares the outcome with its
//! `expected/`. `BLESS=1` writes `expected/` from the run instead; scenarios
//! are blessed against the Python version, so the Rust one must reproduce
//! what Python does.
//!
//! A scenario is `scenario.json`:
//!
//! ```json
//! {
//!   "args": ["--no-color"],
//!   "env": {"TZ": "Europe/Paris"},
//!   "files": {"config.json": {...}, "snooze.json": {...}},
//!   "gh": {...}
//! }
//! ```
//!
//! `files` are written to the config directory before the run (a string is
//! written verbatim, for corrupt-file cases); `gh` drives the fake `gh`
//! described in `fake_gh.rs`. `"loose_stderr": true` skips the stderr
//! comparison, for usage errors whose wording comes from the argument
//! parser.
//!
//! The comparison is exact for the exit code, the store files afterwards and
//! the write calls `gh` received, exact on the parsed value for `--json`, and
//! whitespace-insensitive for the rest of stdout (column spacing is not part
//! of the contract) and for stderr taken as a whole (nor is where a long
//! message wraps). Two things that change on every run are
//! replaced before comparing (and blessing): the temporary directory, by
//! `$TMP`, and any timestamp dated within a week of today (a hide's `since`,
//! a `--for` deadline), by `<RECENT>`. Fixtures therefore date their PRs in
//! 2001 (always stale) or 2099 (always fresh).

// Deliberate: a broken fixture or harness failure should panic loudly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod fake_gh;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use libtest_mimic::{Arguments, Failed, Trial};
use serde_json::{Value, json};

/// The store files a run may read or write, all compared afterwards.
const STORE_FILES: [&str; 3] = ["config.json", "hidden.json", "snooze.json"];

fn main() -> ExitCode {
    if fake_gh::invoked_as_gh() {
        return fake_gh::run();
    }
    let args = Arguments::from_args();
    let target = target_binary();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/scenarios");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|dir| dir.join("scenario.json").is_file())
        .collect();
    dirs.sort();
    let trials = dirs
        .into_iter()
        .map(|dir| {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let target = target.clone();
            Trial::test(name, move || run_scenario(&dir, &target))
        })
        .collect();
    libtest_mimic::run(&args, trials).exit_code()
}

fn target_binary() -> PathBuf {
    match std::env::var_os("GH_PRS_BIN") {
        // Canonicalized because the child runs in a temporary directory,
        // where a relative path would resolve differently.
        Some(path) => fs::canonicalize(&path)
            .unwrap_or_else(|e| panic!("GH_PRS_BIN={}: {e}", path.to_string_lossy())),
        None => PathBuf::from(env!("CARGO_BIN_EXE_gh-prs")),
    }
}

fn run_scenario(dir: &Path, target: &Path) -> Result<(), Failed> {
    let scenario: Value = read_json(&dir.join("scenario.json"));
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    let home = tmp.path().join("home");
    let config_home = tmp.path().join("config");
    let store = config_home.join("gh-prs");
    let work = tmp.path().join("work");
    let log = tmp.path().join("gh-writes.log");
    for path in [&bin, &home, &store, &work] {
        fs::create_dir_all(path).unwrap();
    }
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), bin.join("gh")).unwrap();
    if let Some(files) = scenario["files"].as_object() {
        for (name, content) in files {
            let text = content.as_str().map_or_else(
                || serde_json::to_string_pretty(content).unwrap(),
                str::to_owned,
            );
            fs::write(store.join(name), text).unwrap();
        }
    }

    let args: Vec<&str> = scenario["args"]
        .as_array()
        .map(|args| args.iter().map(|a| a.as_str().unwrap()).collect())
        .unwrap_or_default();
    let mut command = Command::new(target);
    command
        .args(&args)
        .current_dir(&work)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("TZ", "UTC")
        .env("COLUMNS", "200")
        // Deliberate: lets the Python version print UTF-8 under env_clear.
        // Remove with the Python code at the switch.
        .env("PYTHONUTF8", "1")
        .env(fake_gh::SCENARIO_VAR, dir.join("scenario.json"))
        .env(fake_gh::LOG_VAR, &log);
    if let Some(env) = scenario["env"].as_object() {
        for (key, value) in env {
            command.env(key, value.as_str().unwrap());
        }
    }
    let output = command.output().unwrap();

    let volatile = Volatile::new(tmp.path());
    let stdout = volatile.scrub(&String::from_utf8_lossy(&output.stdout));
    let stderr = volatile.scrub(&String::from_utf8_lossy(&output.stderr));
    let result = json!({
        "exit_code": output.status.code(),
        "files": STORE_FILES
            .iter()
            .map(|name| (name.to_string(), read_store(&store.join(name), &volatile)))
            .collect::<serde_json::Map<_, _>>(),
        "writes": fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect::<Vec<Value>>(),
    });

    let expected = dir.join("expected");
    if std::env::var_os("BLESS").is_some() {
        fs::create_dir_all(&expected).unwrap();
        fs::write(expected.join("stdout"), &stdout).unwrap();
        fs::write(expected.join("stderr"), &stderr).unwrap();
        let text = serde_json::to_string_pretty(&result).unwrap() + "\n";
        fs::write(expected.join("result.json"), text).unwrap();
        return Ok(());
    }

    let mut problems = Vec::new();
    let want: Value = read_json(&expected.join("result.json"));
    for key in ["exit_code", "files", "writes"] {
        if want[key] != result[key] {
            problems.push(format!(
                "{key} differs\n--- expected\n{}\n--- actual\n{}",
                serde_json::to_string_pretty(&want[key]).unwrap(),
                serde_json::to_string_pretty(&result[key]).unwrap(),
            ));
        }
    }
    let want_stdout = fs::read_to_string(expected.join("stdout")).unwrap();
    let json_output = args.contains(&"--json") && output.status.success();
    if !same_output(&want_stdout, &stdout, json_output) {
        problems.push(format!(
            "stdout differs\n--- expected\n{want_stdout}--- actual\n{stdout}"
        ));
    }
    let want_stderr = fs::read_to_string(expected.join("stderr")).unwrap();
    let loose_stderr = scenario["loose_stderr"].as_bool().unwrap_or(false);
    if !loose_stderr && words(&want_stderr) != words(&stderr) {
        problems.push(format!(
            "stderr differs\n--- expected\n{want_stderr}--- actual\n{stderr}"
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n\n").into())
    }
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A store file as parsed JSON, its raw text when it isn't JSON, or null
/// when it doesn't exist.
fn read_store(path: &Path, volatile: &Volatile) -> Value {
    match fs::read_to_string(path) {
        Ok(text) => {
            let text = volatile.scrub(&text);
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        }
        Err(_) => Value::Null,
    }
}

/// What differs between two runs of the same scenario.
struct Volatile {
    tmp: Vec<String>,
    recent_days: Vec<String>,
    timestamp: regex::Regex,
}

impl Volatile {
    fn new(tmp: &Path) -> Self {
        let mut paths = vec![tmp.display().to_string()];
        if let Ok(canonical) = fs::canonicalize(tmp) {
            paths.push(canonical.display().to_string());
        }
        // Longest first, so /private/var/... isn't left half-replaced.
        paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
        let now = jiff::Zoned::now();
        let recent_days = (-8..=8)
            .flat_map(|offset: i64| {
                let day = now
                    .date()
                    .checked_add(jiff::Span::new().days(offset))
                    .unwrap();
                let utc = now
                    .with_time_zone(jiff::tz::TimeZone::UTC)
                    .date()
                    .checked_add(jiff::Span::new().days(offset))
                    .unwrap();
                [day.to_string(), utc.to_string()]
            })
            .collect();
        let timestamp = regex::Regex::new(
            r"(\d{4}-\d{2}-\d{2})(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:\d{2})?)?",
        )
        .unwrap();
        Self {
            tmp: paths,
            recent_days,
            timestamp,
        }
    }

    fn scrub(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for path in &self.tmp {
            text = text.replace(path.as_str(), "$TMP");
        }
        self.timestamp
            .replace_all(&text, |found: &regex::Captures<'_>| {
                if self.recent_days.contains(&found[1].to_owned()) {
                    "<RECENT>".to_owned()
                } else {
                    found[0].to_owned()
                }
            })
            .into_owned()
    }
}

fn same_output(expected: &str, actual: &str, as_json: bool) -> bool {
    if as_json {
        let parse = |text: &str| serde_json::from_str::<Value>(text).ok();
        return parse(expected).is_some() && parse(expected) == parse(actual);
    }
    normalize(expected) == normalize(actual)
}

/// stderr as one run of words: the Python version wrapped long messages to
/// the console width, the Rust one leaves that to the terminal.
fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Runs of whitespace collapsed to one space, trailing blank lines dropped.
fn normalize(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}
