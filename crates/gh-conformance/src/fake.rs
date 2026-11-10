//! The fake `gh`: the test binary itself, invoked through a `gh` symlink.
//!
//! What it answers is each tool's business (an [`Answer`]); this module is
//! the part they share: running as `gh`, logging the write calls, replacing
//! an answer from the scenario, and answering a search.
//!
//! The scenario's `gh` block holds, for every tool:
//!
//! ```json
//! {
//!   "viewer": "me",
//!   "searches": {"author": ["ID_1"]},
//!   "issueCounts": {"author": 150},
//!   "responses": {"search:author": {"exit": 1, "stderr": "HTTP 502"}}
//! }
//! ```
//!
//! - `searches` lists the ids each qualifier's search returns (none when
//!   absent), served a page at a time when the query pages; `issueCounts`
//!   overrides the match count, for truncation and `--count`.
//! - `responses` replaces the answer to one operation, for failures and
//!   malformed output. Each is `{"exit", "stdout", "stderr"}`, all
//!   optional; a non-string `stdout` is serialized as JSON.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

pub const SCENARIO_VAR: &str = "GH_FAKE_SCENARIO";
pub const LOG_VAR: &str = "GH_FAKE_LOG";

/// The operation an invocation stands for (`search:author`, `hydrate`,
/// `merge:<url>`) and its answer, from the arguments and the scenario's
/// `gh` block.
pub type Answer = fn(&[&str], &Value) -> (String, Reply);

pub struct Reply {
    pub exit: u8,
    pub stdout: String,
    pub stderr: String,
}

impl Reply {
    #[must_use]
    pub fn ok(value: Value) -> Self {
        Self {
            exit: 0,
            stdout: value.to_string(),
            stderr: String::new(),
        }
    }

    #[must_use]
    pub fn silent() -> Self {
        Self {
            exit: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[must_use]
    pub fn fail(stderr: String) -> Self {
        Self {
            exit: 1,
            stdout: String::new(),
            stderr,
        }
    }
}

#[must_use]
pub fn invoked_as_gh() -> bool {
    std::env::args_os()
        .next()
        .is_some_and(|arg0| Path::new(&arg0).file_name() == Some(OsStr::new("gh")))
}

#[must_use]
pub fn run(answer: Answer, is_write: fn(&str) -> bool) -> ExitCode {
    let scenario_path = std::env::var_os(SCENARIO_VAR).expect("fake gh run outside the harness");
    let text = std::fs::read_to_string(scenario_path).unwrap();
    let scenario: Value = serde_json::from_str(&text).unwrap();
    let gh = &scenario["gh"];
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let (op, reply) = answer(&args, gh);
    if is_write(&op) {
        let log = std::env::var_os(LOG_VAR).unwrap();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(&args).unwrap()).unwrap();
    }
    let reply = match gh["responses"].get(&op) {
        Some(custom) => Reply {
            exit: custom["exit"].as_u64().unwrap_or(0).try_into().unwrap(),
            stdout: stdout_text(&custom["stdout"]),
            stderr: custom["stderr"].as_str().unwrap_or_default().to_owned(),
        },
        None => reply,
    };
    print!("{}", reply.stdout);
    eprint!("{}", reply.stderr);
    ExitCode::from(reply.exit)
}

/// A custom reply's stdout: absent is empty, a string is verbatim, and any
/// other JSON value is serialized.
fn stdout_text(stdout: &Value) -> String {
    if stdout.is_null() {
        String::new()
    } else {
        stdout
            .as_str()
            .map_or_else(|| stdout.to_string(), str::to_owned)
    }
}

/// The scenario's viewer, as GraphQL returns it.
#[must_use]
pub fn viewer(gh: &Value) -> Value {
    json!({ "login": gh["viewer"].as_str().unwrap_or("me") })
}

/// The variables of a `gh api graphql` call (`-f key=value`, `-F key=value`).
pub struct Fields<'a>(Vec<(&'a str, &'a str)>);

impl<'a> Fields<'a> {
    /// From the arguments after `api graphql`.
    #[must_use]
    pub fn parse(rest: &[&'a str]) -> Self {
        let mut fields = Vec::new();
        let mut iter = rest.iter();
        while let Some(flag) = iter.next() {
            let value = iter.next().unwrap();
            if matches!(*flag, "-f" | "-F") {
                fields.push(value.split_once('=').unwrap());
            }
        }
        Self(fields)
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    /// Every value of a repeated variable (`ids[]`).
    #[must_use]
    pub fn all(&self, key: &str) -> Vec<&'a str> {
        self.0
            .iter()
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .collect()
    }

    /// The answer to a search (`q` with a `<qualifier>:@me` term), or `None`
    /// when this isn't one: ids with `limit` (`search:<qualifier>`, from the
    /// `after` cursor on), the count alone without (`count:<qualifier>`).
    #[must_use]
    pub fn search(&self, gh: &Value) -> Option<(String, Reply)> {
        let qualifier = self
            .get("q")?
            .split_whitespace()
            .find_map(|token| token.strip_suffix(":@me"))
            .unwrap();
        let ids: Vec<Value> = gh["searches"][qualifier]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let count = gh["issueCounts"][qualifier]
            .as_u64()
            .unwrap_or(ids.len() as u64);
        let Some(limit) = self.get("limit") else {
            let data = json!({ "data": { "results": { "issueCount": count } } });
            return Some((format!("count:{qualifier}"), Reply::ok(data)));
        };
        let limit: usize = limit.parse().unwrap();
        let start: usize = self.get("after").map_or(0, |after| after.parse().unwrap());
        let end = ids.len().min(start + limit);
        let nodes: Vec<Value> = ids[start.min(end)..end]
            .iter()
            .map(|id| json!({ "id": id }))
            .collect();
        let data = json!({
            "data": {
                "viewer": viewer(gh),
                "results": {
                    "issueCount": count,
                    "pageInfo": { "hasNextPage": end < ids.len(), "endCursor": end.to_string() },
                    "nodes": nodes,
                },
            }
        });
        Some((format!("search:{qualifier}"), Reply::ok(data)))
    }
}
