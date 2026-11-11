//! Loading issues: paged id searches, then batched hydration.
//!
//! Each role's search returns node ids only, [`SEARCH_PAGE`] at a time; the
//! union is then looked up in batches of [`HYDRATE_BATCH`], each issue once
//! however many searches matched it. Any failed search or lookup fails the
//! whole fetch: a partial result would silently hide issues.

use std::collections::{BTreeSet, HashMap, HashSet};

use gh_common::gh::{GhError, Runner, Task, gather, nodes_by_id, search_ids};
use serde_json::Value;

use super::parse::{RawIssue, to_issue};
use super::queries::{SEARCH_QUERY, hydrate_query};
use crate::issue::{Issue, Role};

/// Ids per search request, GitHub's maximum.
pub const SEARCH_PAGE: u64 = 100;
/// GitHub serves the first 1000 matches of a search and no more.
pub const SEARCH_MAX: u64 = 1000;
/// Issues hydrated per request. An issue node is light (no checks, no
/// reviews), so a batch stays far from the ~10s after which GitHub answers
/// HTTP 502.
const HYDRATE_BATCH: usize = 50;

fn search_string(role: Role) -> String {
    format!(
        "is:issue is:open archived:false sort:updated-desc {}:@me",
        role.qualifier()
    )
}

struct Found {
    ids: Vec<String>,
    /// Every match.
    matched: u64,
    /// GitHub had more to serve than it would.
    capped: bool,
}

/// Every id one role's search matches, most recently updated first. The
/// pages of one search are read in turn: each needs the cursor of the one
/// before.
fn search(runner: &dyn Runner, role: Role) -> Result<Found, GhError> {
    let context = format!("Search '{}'", role.name());
    let query = search_string(role);
    let mut ids: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut after: Option<String> = None;
    let mut asked = 0;
    loop {
        let page = search_ids(
            runner,
            &context,
            SEARCH_QUERY,
            &query,
            SEARCH_PAGE,
            after.as_deref(),
        )?;
        asked += SEARCH_PAGE;
        // The ordering is live, so an issue updated between two pages
        // comes back twice.
        for id in page.ids {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
        // `Some(capped)` once there is nothing more to read.
        let done = match page.next {
            None => Some(page.issue_count > SEARCH_MAX),
            Some(_) if asked >= SEARCH_MAX => Some(true),
            // A cursor that doesn't move would loop forever. A page with
            // no readable id is no reason to stop: GitHub answers null for
            // a match the token cannot read.
            Some(next) if after.as_ref() == Some(&next) => Some(false),
            Some(next) => {
                after = Some(next);
                None
            }
        };
        if let Some(capped) = done {
            return Ok(Found {
                ids,
                matched: page.issue_count,
                capped,
            });
        }
    }
}

/// The open issues the viewer holds one of `roles` on, most recently
/// updated first.
///
/// `on_warning` hears about degraded coverage instead of silence: a search
/// that matched more than GitHub serves.
///
/// Narrowing the searches by filter (fewer roles, one repository) is not
/// done: issue #35 has the idea.
pub fn fetch_issues(
    runner: &dyn Runner,
    roles: &[Role],
    on_warning: &mut dyn FnMut(&str),
) -> Result<Vec<Issue>, GhError> {
    let searches: Vec<Found> = gather(
        roles
            .iter()
            .map(|&role| {
                (
                    format!("Search '{}'", role.name()),
                    Box::new(move || search(runner, role)) as Task<'_, _>,
                )
            })
            .collect(),
        "issue searches",
    )?;
    for (role, found) in roles.iter().zip(&searches) {
        if found.capped {
            on_warning(&format!(
                "search '{}' matched {} issues; GitHub serves the {SEARCH_MAX} most recently updated, the rest is missing",
                role.name(),
                found.matched
            ));
        }
    }

    // Each issue is hydrated once however many searches matched it, so
    // every field comes from one snapshot.
    let mut order: Vec<&str> = Vec::new();
    let mut granted: HashMap<&str, BTreeSet<Role>> = HashMap::new();
    for (role, found) in roles.iter().zip(&searches) {
        for id in &found.ids {
            granted
                .entry(id)
                .or_insert_with(|| {
                    order.push(id);
                    BTreeSet::new()
                })
                .insert(*role);
        }
    }
    let batches: Vec<Vec<String>> = order
        .chunks(HYDRATE_BATCH)
        .map(|chunk| chunk.iter().map(|id| (*id).to_owned()).collect())
        .collect();
    let total = batches.len();
    let query = hydrate_query();
    let hydrated: Vec<Vec<Value>> = gather(
        batches
            .iter()
            .enumerate()
            .map(|(n, ids)| {
                let label = format!("Issue lookup {}/{total}", n + 1);
                let context = label.clone();
                let query = query.as_str();
                (
                    label,
                    Box::new(move || nodes_by_id(runner, &context, query, &[], ids)) as Task<'_, _>,
                )
            })
            .collect(),
        "issue lookups",
    )?;

    let mut issues: Vec<Issue> = Vec::new();
    for node in hydrated.into_iter().flatten() {
        // Deleted or transferred between the search and the lookup.
        if node.is_null() || node.as_object().is_some_and(serde_json::Map::is_empty) {
            continue;
        }
        let raw: RawIssue = serde_json::from_value(node)
            .map_err(|e| GhError(format!("Failed to parse issue data: {e}")))?;
        let roles = raw
            .id
            .as_deref()
            .and_then(|id| granted.get(id))
            .ok_or_else(|| {
                GhError("Failed to parse issue data: a node without a searched id".to_owned())
            })?;
        if raw.is_closed() {
            continue;
        }
        let mut issue = to_issue(&raw);
        issue.roles.clone_from(roles);
        issues.push(issue);
    }
    // Stable, so issues updated at the same instant keep the search order.
    issues.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(issues)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gh_common::gh::Output;
    use serde_json::json;

    use super::*;

    fn ok(value: &Value) -> Result<Output, GhError> {
        Ok(Output {
            success: true,
            stdout: value.to_string(),
            stderr: String::new(),
        })
    }

    fn variable<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
        args.iter()
            .find_map(|arg| arg.strip_prefix(key)?.strip_prefix('='))
    }

    fn node(id: &str, number: u64, updated_at: &str) -> Value {
        json!({
            "id": id,
            "number": number,
            "title": format!("Issue {number}"),
            "state": "OPEN",
            "updatedAt": updated_at,
            "repository": {"nameWithOwner": "acme/widgets"},
        })
    }

    /// A `gh` that serves `searches` (ids per qualifier, paged) and `nodes`
    /// by id, recording every call.
    fn github<'a>(
        searches: &'a [(&'a str, Vec<&'a str>)],
        nodes: &'a [Value],
        calls: &'a Mutex<Vec<Vec<String>>>,
    ) -> impl Runner + 'a {
        move |args: &[String]| {
            calls.lock().unwrap().push(args.to_vec());
            let ids: Vec<&str> = args
                .iter()
                .filter_map(|arg| arg.strip_prefix("ids[]="))
                .collect();
            if !ids.is_empty() {
                let found: Vec<Value> = ids
                    .iter()
                    .map(|id| {
                        nodes
                            .iter()
                            .find(|node| node["id"] == *id)
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect();
                return ok(&json!({"data": {"nodes": found}}));
            }
            let q = variable(args, "q").unwrap();
            let all = searches
                .iter()
                .find(|(qualifier, _)| q.ends_with(&format!(" {qualifier}:@me")))
                .map(|(_, ids)| ids.clone())
                .unwrap_or_default();
            let limit: usize = variable(args, "limit").unwrap().parse().unwrap();
            let start: usize = variable(args, "after").map_or(0, |after| after.parse().unwrap());
            let end = all.len().min(start + limit);
            let page: Vec<Value> = all[start..end].iter().map(|id| json!({"id": id})).collect();
            ok(&json!({"data": {
                "viewer": {"login": "me"},
                "results": {
                    "issueCount": all.len(),
                    "pageInfo": {"hasNextPage": end < all.len(), "endCursor": end.to_string()},
                    "nodes": page,
                },
            }}))
        }
    }

    fn fetch(runner: &dyn Runner, roles: &[Role]) -> (Result<Vec<Issue>, GhError>, Vec<String>) {
        let mut warnings = Vec::new();
        let result = fetch_issues(runner, roles, &mut |warning| {
            warnings.push(warning.to_owned());
        });
        (result, warnings)
    }

    #[test]
    fn searches_ask_for_open_issues_by_role() {
        assert_eq!(
            search_string(Role::Owner),
            "is:issue is:open archived:false sort:updated-desc user:@me"
        );
        assert_eq!(
            search_string(Role::Commenter),
            "is:issue is:open archived:false sort:updated-desc commenter:@me"
        );
    }

    #[test]
    fn roles_are_the_union_of_the_searches_and_each_issue_is_looked_up_once() {
        let searches = [("author", vec!["I_1", "I_2"]), ("user", vec!["I_2", "I_3"])];
        let nodes = [
            node("I_1", 1, "2026-11-01T00:00:00Z"),
            node("I_2", 2, "2026-11-03T00:00:00Z"),
            node("I_3", 3, "2026-11-02T00:00:00Z"),
        ];
        let calls = Mutex::new(Vec::new());
        let runner = github(&searches, &nodes, &calls);
        let (issues, warnings) = fetch(&runner, &[Role::Author, Role::Owner]);
        let issues = issues.unwrap();
        // Most recently updated first.
        let numbers: Vec<u64> = issues.iter().map(|issue| issue.number).collect();
        assert_eq!(numbers, [2, 3, 1]);
        let roles: Vec<Vec<&str>> = issues
            .iter()
            .map(|issue| issue.roles.iter().map(|role| role.name()).collect())
            .collect();
        assert_eq!(
            roles,
            [vec!["author", "owner"], vec!["owner"], vec!["author"]]
        );
        assert!(warnings.is_empty());
        let looked_up: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .filter(|arg| arg.starts_with("ids[]="))
            .cloned()
            .collect();
        assert_eq!(looked_up, ["ids[]=I_1", "ids[]=I_2", "ids[]=I_3"]);
    }

    #[test]
    fn a_search_is_read_page_after_page() {
        let ids: Vec<String> = (0..230).map(|n| format!("I_{n}")).collect();
        let searches = [("user", ids.iter().map(String::as_str).collect())];
        let nodes: Vec<Value> = ids
            .iter()
            .enumerate()
            .map(|(n, id)| node(id, n as u64, "2026-11-01T00:00:00Z"))
            .collect();
        let calls = Mutex::new(Vec::new());
        let runner = github(&searches, &nodes, &calls);
        let (issues, warnings) = fetch(&runner, &[Role::Owner]);
        assert_eq!(issues.unwrap().len(), 230);
        assert!(warnings.is_empty());
        let calls = calls.lock().unwrap();
        let cursors: Vec<Option<&str>> = calls
            .iter()
            .filter(|args| variable(args, "q").is_some())
            .map(|args| variable(args, "after"))
            .collect();
        assert_eq!(cursors, [None, Some("100"), Some("200")]);
        // 230 issues in batches of 50.
        let lookups = calls
            .iter()
            .filter(|args| variable(args, "q").is_none())
            .count();
        assert_eq!(lookups, 5);
    }

    /// A `gh` whose search always answers `page` and whose lookup finds
    /// nothing, counting the searches.
    fn searching<'a>(page: &'a Value, searched: &'a Mutex<u32>) -> impl Runner + 'a {
        move |args: &[String]| {
            if variable(args, "q").is_none() {
                return ok(&json!({"data": {"nodes": []}}));
            }
            *searched.lock().unwrap() += 1;
            ok(&json!({"data": {"viewer": {"login": "me"}, "results": page}}))
        }
    }

    #[test]
    fn matches_beyond_what_github_serves_are_reported() {
        let page = json!({
            "issueCount": 1500,
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"id": "I_1"}],
        });
        let searched = Mutex::new(0);
        let (issues, warnings) = fetch(&searching(&page, &searched), &[Role::Owner]);
        assert!(issues.is_ok());
        assert_eq!(
            warnings,
            [
                "search 'owner' matched 1500 issues; GitHub serves the 1000 most recently updated, the rest is missing"
            ]
        );
    }

    #[test]
    fn paging_stops_at_what_github_serves() {
        // A GitHub that would go on forever, each page with a new cursor.
        let cursor = Mutex::new(0_u32);
        let searched = Mutex::new(0_u32);
        let runner = |args: &[String]| {
            if variable(args, "q").is_none() {
                return ok(&json!({"data": {"nodes": []}}));
            }
            *searched.lock().unwrap() += 1;
            let mut cursor = cursor.lock().unwrap();
            *cursor += 1;
            ok(&json!({"data": {"results": {
                "issueCount": 5000,
                "pageInfo": {"hasNextPage": true, "endCursor": cursor.to_string()},
                "nodes": [{"id": format!("I_{cursor}")}],
            }}}))
        };
        let (issues, warnings) = fetch(&runner, &[Role::Owner]);
        assert!(issues.is_ok());
        assert_eq!(
            u64::from(*searched.lock().unwrap()),
            SEARCH_MAX / SEARCH_PAGE
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn unreadable_matches_are_not_a_truncation_and_do_not_end_the_search() {
        // Two matches the token cannot read on the first page, a readable
        // one on the second.
        let searched = Mutex::new(0_u32);
        let runner = |args: &[String]| {
            if variable(args, "q").is_none() {
                return ok(&json!({"data": {"nodes": [node("I_3", 3, "2026-11-01T00:00:00Z")]}}));
            }
            *searched.lock().unwrap() += 1;
            let results = if variable(args, "after").is_none() {
                json!({
                    "issueCount": 3,
                    "pageInfo": {"hasNextPage": true, "endCursor": "2"},
                    "nodes": [null, {}],
                })
            } else {
                json!({
                    "issueCount": 3,
                    "pageInfo": {"hasNextPage": false, "endCursor": "3"},
                    "nodes": [{"id": "I_3"}],
                })
            };
            ok(&json!({"data": {"results": results}}))
        };
        let (issues, warnings) = fetch(&runner, &[Role::Commenter]);
        assert_eq!(issues.unwrap().len(), 1);
        assert_eq!(*searched.lock().unwrap(), 2);
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_cursor_that_does_not_move_ends_the_search() {
        let page = json!({
            "issueCount": 1,
            "pageInfo": {"hasNextPage": true, "endCursor": "same"},
            "nodes": [{"id": "I_1"}],
        });
        let searched = Mutex::new(0);
        let (issues, warnings) = fetch(&searching(&page, &searched), &[Role::Owner]);
        assert!(issues.is_ok());
        // The second page repeats the first one's cursor.
        assert_eq!(*searched.lock().unwrap(), 2);
        assert!(warnings.is_empty());
    }

    #[test]
    fn an_issue_seen_on_two_pages_is_looked_up_once() {
        let calls = Mutex::new(Vec::new());
        let runner = |args: &[String]| {
            calls.lock().unwrap().push(args.to_vec());
            if variable(args, "q").is_none() {
                return ok(&json!({"data": {"nodes": [node("I_1", 1, "2026-11-01T00:00:00Z")]}}));
            }
            let more = variable(args, "after").is_none();
            ok(&json!({"data": {"results": {
                "issueCount": 1,
                "pageInfo": {"hasNextPage": more, "endCursor": "1"},
                "nodes": [{"id": "I_1"}],
            }}}))
        };
        let (issues, _) = fetch(&runner, &[Role::Owner]);
        assert_eq!(issues.unwrap().len(), 1);
        let looked_up = calls
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .filter(|arg| arg.starts_with("ids[]="))
            .count();
        assert_eq!(looked_up, 1);
    }

    #[test]
    fn closed_and_vanished_issues_are_dropped() {
        let searches = [("author", vec!["I_1", "I_2", "I_3"])];
        let mut closed = node("I_2", 2, "2026-11-02T00:00:00Z");
        closed["state"] = json!("CLOSED");
        let mut unknown = node("I_1", 1, "2026-11-01T00:00:00Z");
        unknown["state"] = json!("SOMETHING_NEW");
        // I_3 is gone: the lookup answers null.
        let nodes = [unknown, closed];
        let calls = Mutex::new(Vec::new());
        let runner = github(&searches, &nodes, &calls);
        let (issues, _) = fetch(&runner, &[Role::Author]);
        let numbers: Vec<u64> = issues.unwrap().iter().map(|issue| issue.number).collect();
        assert_eq!(numbers, [1]);
    }

    #[test]
    fn a_failed_search_fails_the_fetch() {
        let runner = |args: &[String]| {
            if variable(args, "q").is_some_and(|q| q.ends_with("user:@me")) {
                return Ok(Output {
                    success: false,
                    stdout: String::new(),
                    stderr: "HTTP 502".to_owned(),
                });
            }
            ok(
                &json!({"data": {"viewer": {"login": "me"}, "results": {"issueCount": 0, "nodes": []}}}),
            )
        };
        let (issues, _) = fetch(&runner, &[Role::Author, Role::Owner]);
        assert_eq!(
            issues.unwrap_err().0,
            "Some issue searches failed; results would be incomplete:\n  Search 'owner' failed: HTTP 502"
        );
    }

    #[test]
    fn a_node_of_the_wrong_shape_fails_the_fetch() {
        let searches = [("author", vec!["I_1"])];
        let nodes = [json!({"id": "I_1", "number": "one", "title": "x"})];
        let calls = Mutex::new(Vec::new());
        let runner = github(&searches, &nodes, &calls);
        let (issues, _) = fetch(&runner, &[Role::Author]);
        assert!(
            issues
                .unwrap_err()
                .0
                .starts_with("Failed to parse issue data: ")
        );
    }

    #[test]
    fn no_role_is_no_request() {
        let runner = |_: &[String]| -> Result<Output, GhError> { panic!("no call expected") };
        let (issues, warnings) = fetch(&runner, &[]);
        assert!(issues.unwrap().is_empty());
        assert!(warnings.is_empty());
    }
}
