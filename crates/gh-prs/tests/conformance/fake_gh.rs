//! The fake `gh`: this test binary, invoked through a `gh` symlink.
//!
//! It answers by meaning rather than by exact arguments (a search for this
//! qualifier, a lookup of these ids, a `pr view` of this ref), so batching
//! and ordering are free to differ between implementations. The scenario's
//! `gh` block drives it:
//!
//! ```json
//! {
//!   "viewer": "me",
//!   "currentRepo": "acme/widgets",
//!   "searches": {"author": ["PR_1"], "review-requested": ["PR_2"]},
//!   "issueCounts": {"review-requested": 150},
//!   "nodes": {"PR_1": {"number": 1, "reviewDecision": "APPROVED"}},
//!   "responses": {"search:reviewed-by": {"exit": 1, "stderr": "HTTP 502"}}
//! }
//! ```
//!
//! - `nodes` are GraphQL `PullRequest` nodes by id, completed with the
//!   defaults in `complete_node` (an open, mergeable, fresh PR with no
//!   reviews, checks or labels in `acme/widgets`); a key given in the
//!   scenario replaces the default whole.
//! - `searches` lists the ids each qualifier's search returns (none when
//!   absent); `issueCounts` overrides the match count, for truncation and
//!   `--count`.
//! - `currentRepo` is the repository a bare PR number resolves against
//!   without `--repo`, standing in for the current directory's git remote.
//! - `responses` replaces the answer to one operation, for failures and
//!   malformed output: `search:<qualifier>`, `count:<qualifier>`, `hydrate`,
//!   `resource:<url>`, `view:<ref>`, `approve:<url>`, `merge:<url>`. Each
//!   is `{"exit", "stdout", "stderr"}`, all optional; a non-string `stdout`
//!   is serialized as JSON.
//!
//! Write calls (`pr review`, `pr merge`) are appended to the log the harness
//! compares, whatever they answer.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Map, Value, json};

pub const SCENARIO_VAR: &str = "GH_PRS_FAKE_SCENARIO";
pub const LOG_VAR: &str = "GH_PRS_FAKE_LOG";

const DEFAULT_REPO: &str = "acme/widgets";

struct Reply {
    exit: u8,
    stdout: String,
    stderr: String,
}

impl Reply {
    fn ok(value: Value) -> Self {
        Self {
            exit: 0,
            stdout: value.to_string(),
            stderr: String::new(),
        }
    }

    fn silent() -> Self {
        Self {
            exit: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn fail(stderr: String) -> Self {
        Self {
            exit: 1,
            stdout: String::new(),
            stderr,
        }
    }
}

pub fn invoked_as_gh() -> bool {
    std::env::args_os()
        .next()
        .is_some_and(|arg0| Path::new(&arg0).file_name() == Some(OsStr::new("gh")))
}

pub fn run() -> ExitCode {
    let scenario_path = std::env::var_os(SCENARIO_VAR).expect("fake gh run outside the harness");
    let text = std::fs::read_to_string(scenario_path).unwrap();
    let scenario: Value = serde_json::from_str(&text).unwrap();
    let gh = &scenario["gh"];
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let (op, reply) = answer(&args, gh);
    if op.starts_with("approve:") || op.starts_with("merge:") {
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

fn answer(args: &[&str], gh: &Value) -> (String, Reply) {
    match args {
        ["api", "graphql", rest @ ..] => graphql(rest, gh),
        ["pr", "view", reference, rest @ ..] => pr_view(reference, rest, gh),
        ["pr", "review", .., url] => (format!("approve:{url}"), Reply::silent()),
        ["pr", "merge", .., url] => (format!("merge:{url}"), Reply::silent()),
        _ => (
            "unsupported".to_owned(),
            Reply {
                exit: 2,
                stdout: String::new(),
                stderr: format!("fake gh: unsupported invocation: {args:?}\n"),
            },
        ),
    }
}

fn graphql(rest: &[&str], gh: &Value) -> (String, Reply) {
    let mut fields: Vec<(&str, &str)> = Vec::new();
    let mut iter = rest.iter();
    while let Some(flag) = iter.next() {
        let value = iter.next().unwrap();
        if matches!(*flag, "-f" | "-F") {
            fields.push(value.split_once('=').unwrap());
        }
    }
    let field = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let viewer = json!({ "login": gh["viewer"].as_str().unwrap_or("me") });

    let ids: Vec<&str> = fields
        .iter()
        .filter(|(k, _)| *k == "ids[]")
        .map(|(_, v)| *v)
        .collect();
    if !ids.is_empty() {
        let with_threads = field("withThreads") == Some("true");
        let nodes: Vec<Value> = ids
            .iter()
            .map(|id| node_by_id(gh, id, with_threads))
            .collect();
        return (
            "hydrate".to_owned(),
            Reply::ok(json!({ "data": { "nodes": nodes } })),
        );
    }
    if let Some(url) = field("url") {
        let resource = node_by_url(gh, url).map(|mut node| {
            node.as_object_mut()
                .unwrap()
                .entry("__typename")
                .or_insert(json!("PullRequest"));
            node
        });
        let data = json!({ "data": { "viewer": viewer, "resource": resource } });
        return (format!("resource:{url}"), Reply::ok(data));
    }
    let Some(query) = field("q") else {
        return (
            "unsupported".to_owned(),
            Reply::fail(format!("fake gh: unsupported query {rest:?}\n")),
        );
    };
    let qualifier = query
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
    if field("limit").is_some() {
        let nodes: Vec<Value> = ids.into_iter().map(|id| json!({ "id": id })).collect();
        let data = json!({
            "data": { "viewer": viewer, "results": { "issueCount": count, "nodes": nodes } }
        });
        (format!("search:{qualifier}"), Reply::ok(data))
    } else {
        let data = json!({ "data": { "results": { "issueCount": count } } });
        (format!("count:{qualifier}"), Reply::ok(data))
    }
}

fn pr_view(reference: &str, rest: &[&str], gh: &Value) -> (String, Reply) {
    let op = format!("view:{reference}");
    let flag = |name: &str| {
        rest.iter()
            .position(|arg| *arg == name)
            .map(|index| rest[index + 1])
    };
    let wanted: Vec<&str> = flag("--json").unwrap().split(',').collect();
    let node = if reference.starts_with("https://") {
        node_by_url(gh, reference)
    } else {
        let Some(repo) = flag("--repo").or_else(|| gh["currentRepo"].as_str()) else {
            let error = "failed to run git: fatal: not a git repository (or any of the parent directories): .git\n";
            return (op, Reply::fail(error.to_owned()));
        };
        all_nodes(gh, true).into_iter().find(|node| {
            node["repository"]["nameWithOwner"]
                .as_str()
                .unwrap()
                .eq_ignore_ascii_case(repo)
                && node["number"]
                    .as_u64()
                    .is_some_and(|n| n.to_string() == reference)
        })
    };
    let Some(node) = node else {
        let error = format!(
            "GraphQL: Could not resolve to a PullRequest with the number of {reference}. (repository.pullRequest)\n"
        );
        return (op, Reply::fail(error));
    };
    let picked: Map<String, Value> = wanted
        .iter()
        .map(|key| ((*key).to_owned(), node[*key].clone()))
        .collect();
    (op, Reply::ok(Value::Object(picked)))
}

fn all_nodes(gh: &Value, with_threads: bool) -> Vec<Value> {
    gh["nodes"]
        .as_object()
        .map(|nodes| {
            nodes
                .iter()
                .map(|(id, partial)| complete_node(id, partial, with_threads))
                .collect()
        })
        .unwrap_or_default()
}

fn node_by_id(gh: &Value, id: &str, with_threads: bool) -> Value {
    gh["nodes"].get(id).map_or(Value::Null, |partial| {
        complete_node(id, partial, with_threads)
    })
}

fn node_by_url(gh: &Value, url: &str) -> Option<Value> {
    all_nodes(gh, true)
        .into_iter()
        .find(|node| node["url"] == url)
}

/// A scenario node over the defaults: an open, mergeable PR in
/// `acme/widgets`, updated far in the future (so never stale), with no
/// reviews, checks, labels or threads. `reviewThreads` is dropped when the
/// query didn't ask for it, as GitHub does with the `@include` directive.
fn complete_node(id: &str, partial: &Value, with_threads: bool) -> Value {
    let number = partial["number"]
        .as_u64()
        .unwrap_or_else(|| panic!("node {id} has no number"));
    let repo = partial["repository"]["nameWithOwner"]
        .as_str()
        .unwrap_or(DEFAULT_REPO);
    let mut node = json!({
        "id": id,
        "number": number,
        "title": format!("PR {number}"),
        "url": format!("https://github.com/{repo}/pull/{number}"),
        "state": "OPEN",
        "updatedAt": "2099-01-01T00:00:00Z",
        "createdAt": "2099-01-01T00:00:00Z",
        "isDraft": false,
        "reviewDecision": null,
        "mergeable": "MERGEABLE",
        "headRefOid": format!("head-{number}"),
        "baseRef": { "associatedPullRequests": { "totalCount": 0 } },
        "repository": { "nameWithOwner": repo },
        "author": { "login": "octocat" },
        "labels": { "nodes": [] },
        "reviewRequests": { "nodes": [] },
        "latestReviews": { "nodes": [] },
        "latestOpinionatedReviews": { "nodes": [] },
        "reviewThreads": { "nodes": [] },
        "commits": { "nodes": [{ "commit": { "statusCheckRollup": null } }] },
    });
    let fields = node.as_object_mut().unwrap();
    for (key, value) in partial.as_object().unwrap() {
        fields.insert(key.clone(), value.clone());
    }
    if !with_threads {
        fields.remove("reviewThreads");
    }
    node
}
