//! What the fake `gh` (`gh_conformance::fake`) answers for gh-issues.
//!
//! It answers by meaning rather than by exact arguments (a search for this
//! qualifier, a lookup of these ids), so batching and ordering are free to
//! change. The scenario's `gh` block drives it:
//!
//! ```json
//! {
//!   "searches": {"author": ["I_1"], "user": ["I_1", "I_2"]},
//!   "nodes": {"I_1": {"number": 1, "labels": {"nodes": [{"name": "bug"}]}}}
//! }
//! ```
//!
//! - `nodes` are GraphQL `Issue` nodes by id, completed with the defaults
//!   in `complete_node` (an open issue by `octocat` in `acme/widgets`, with
//!   no label, assignee or comment); a key given in the scenario replaces
//!   the default whole. An id without a node is answered `null`, as GitHub
//!   does for an issue that vanished.
//! - `searches` and `issueCounts` are keyed by search qualifier (`author`,
//!   `assignee`, `commenter`, and `user` for the repositories you own).
//! - `responses` keys are `search:<qualifier>` and `hydrate`.
//!
//! gh-issues only reads, so there is no write call to log.

use gh_conformance::fake::{Fields, Reply, lookup};
use serde_json::{Value, json};

const DEFAULT_REPO: &str = "acme/widgets";

pub fn answer(args: &[&str], gh: &Value) -> (String, Reply) {
    let unsupported = || {
        (
            "unsupported".to_owned(),
            Reply {
                exit: 2,
                stdout: String::new(),
                stderr: format!("fake gh: unsupported invocation: {args:?}\n"),
            },
        )
    };
    let ["api", "graphql", rest @ ..] = args else {
        return unsupported();
    };
    let fields = Fields::parse(rest);
    let ids = fields.all("ids[]");
    if !ids.is_empty() {
        let nodes: Vec<Value> = ids
            .iter()
            .map(|id| {
                gh["nodes"]
                    .get(id)
                    .map_or(Value::Null, |partial| complete_node(id, partial))
            })
            .collect();
        return ("hydrate".to_owned(), lookup(nodes));
    }
    fields.search(gh).unwrap_or_else(unsupported)
}

/// A scenario node over the defaults.
fn complete_node(id: &str, partial: &Value) -> Value {
    let number = partial["number"]
        .as_u64()
        .unwrap_or_else(|| panic!("node {id} has no number"));
    let repo = partial["repository"]["nameWithOwner"]
        .as_str()
        .unwrap_or(DEFAULT_REPO);
    let mut node = json!({
        "id": id,
        "number": number,
        "title": format!("Issue {number}"),
        "url": format!("https://github.com/{repo}/issues/{number}"),
        "state": "OPEN",
        "updatedAt": "2099-01-01T00:00:00Z",
        "createdAt": "2099-01-01T00:00:00Z",
        "repository": { "nameWithOwner": repo },
        "author": { "login": "octocat" },
        "labels": { "nodes": [] },
        "assignees": { "nodes": [] },
        "comments": { "totalCount": 0, "nodes": [] },
    });
    let fields = node.as_object_mut().unwrap();
    for (key, value) in partial.as_object().unwrap() {
        fields.insert(key.clone(), value.clone());
    }
    node
}
