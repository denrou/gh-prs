//! What the fake `gh` (`gh_conformance::fake`) answers for gh-prs.
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

use gh_conformance::fake::{Fields, Reply, viewer};
use serde_json::{Map, Value, json};

const DEFAULT_REPO: &str = "acme/widgets";

/// Whether `op` writes to GitHub.
pub fn is_write(op: &str) -> bool {
    op.starts_with("approve:") || op.starts_with("merge:")
}

pub fn answer(args: &[&str], gh: &Value) -> (String, Reply) {
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
    let fields = Fields::parse(rest);
    let ids = fields.all("ids[]");
    if !ids.is_empty() {
        let with_threads = fields.get("withThreads") == Some("true");
        let nodes: Vec<Value> = ids
            .iter()
            .map(|id| node_by_id(gh, id, with_threads))
            .collect();
        return (
            "hydrate".to_owned(),
            Reply::ok(json!({ "data": { "nodes": nodes } })),
        );
    }
    if let Some(url) = fields.get("url") {
        let resource = node_by_url(gh, url).map(|mut node| {
            node.as_object_mut()
                .unwrap()
                .entry("__typename")
                .or_insert(json!("PullRequest"));
            node
        });
        let data = json!({ "data": { "viewer": viewer(gh), "resource": resource } });
        return (format!("resource:{url}"), Reply::ok(data));
    }
    fields.search(gh).unwrap_or_else(|| {
        (
            "unsupported".to_owned(),
            Reply::fail(format!("fake gh: unsupported query {rest:?}\n")),
        )
    })
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
