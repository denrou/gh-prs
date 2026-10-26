//! Searches, hydration, single-PR lookups and the two write actions.
//!
//! Loading runs in two parallel phases (`CLAUDE.md`, Loading): one id-only
//! search per qualifier, then the union of ids hydrated in batches of
//! [`HYDRATE_BATCH`]. GitHub answers HTTP 502 to any request running past
//! ~10s, which a single hydrated 50-PR search reached; batches of 15 take
//! 2-5s. One failure anywhere fails the whole fetch: a partial result would
//! silently hide PRs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

use serde_json::Value;

use super::parse::{RawPr, Unrecognised, to_pull_request};
use super::queries::{COUNT_QUERY, SEARCH_QUERY, hydrate_query, pr_query};
use super::{GhError, Runner, graphql};
use crate::attention::{Staleness, attention_reasons};
use crate::merge::MergeMode;
use crate::pr::{Oid, PrState, PullRequest, Reason, Reported, Role};

/// Nodes per search. Searches sort by most recently updated, so a
/// truncated one keeps the PRs most likely to need attention.
pub const SEARCH_LIMIT: u64 = 100;
/// Per-PR cap on the review connections; must match `first:` in the fragment.
pub const REVIEW_PAGE_LIMIT: usize = 50;
/// Per-PR cap on labels; at the cap the list is reported incomplete, so a
/// mute exemption can't be missed.
pub(super) const LABEL_PAGE_LIMIT: usize = 20;
/// PRs hydrated per request: 2-5s, half GitHub's 10s budget with room for
/// the slowdown it applies to concurrent requests.
const HYDRATE_BATCH: usize = 15;
/// Concurrent requests per phase: GitHub throttles concurrent requests per
/// token, so a large union of PRs must not fan out unbounded.
const MAX_PARALLEL: usize = 8;

fn search_string(qualifier: Role) -> String {
    format!(
        "is:pr is:open archived:false sort:updated-desc {}:@me",
        qualifier.as_str()
    )
}

/// `gh` flags from (flag, value) pairs.
fn flags(pairs: &[(&str, String)]) -> Vec<String> {
    pairs
        .iter()
        .flat_map(|(flag, value)| [(*flag).to_owned(), value.clone()])
        .collect()
}

/// The exact number of open PRs matching one qualifier's search, without
/// hydrating anything. Exact even beyond the node cap.
pub fn count_prs(runner: &dyn Runner, qualifier: Role) -> Result<u64, GhError> {
    let context = format!("Count '{}'", qualifier.as_str());
    let data = graphql(
        runner,
        &context,
        &flags(&[
            ("-f", format!("query={COUNT_QUERY}")),
            ("-f", format!("q={}", search_string(qualifier))),
        ]),
    )?;
    let missing = || {
        GhError(format!(
            "{context}: response has no issueCount (unexpected shape)"
        ))
    };
    let count = data
        .get("results")
        .and_then(|results| results.get("issueCount"))
        .ok_or_else(missing)?;
    if count.is_null() {
        Ok(0)
    } else {
        count.as_u64().ok_or_else(missing)
    }
}

#[derive(Debug)]
struct SearchResult {
    viewer: String,
    ids: Vec<String>,
    issue_count: u64,
}

fn search(runner: &dyn Runner, qualifier: Role) -> Result<SearchResult, GhError> {
    let context = format!("Search '{}'", qualifier.as_str());
    let data = graphql(
        runner,
        &context,
        &flags(&[
            ("-f", format!("query={SEARCH_QUERY}")),
            ("-f", format!("q={}", search_string(qualifier))),
            ("-F", format!("limit={SEARCH_LIMIT}")),
        ]),
    )?;
    let results = data
        .get("results")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            GhError(format!(
                "{context}: response has no results block (unexpected shape)"
            ))
        })?;
    let no_id = || {
        GhError(format!(
            "{context}: result without a node id (unexpected shape)"
        ))
    };
    let nodes = match results.get("nodes") {
        None => &[][..],
        Some(nodes) if nodes.is_null() => &[][..],
        Some(nodes) => nodes.as_array().ok_or_else(no_id)?.as_slice(),
    };
    let mut ids = Vec::new();
    for node in nodes {
        // GitHub returns null (or an empty object) for an inaccessible match.
        if node.is_null() || node.as_object().is_some_and(serde_json::Map::is_empty) {
            continue;
        }
        let id = node
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(no_id)?;
        ids.push(id.to_owned());
    }
    Ok(SearchResult {
        viewer: data["viewer"]["login"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        ids,
        issue_count: results
            .get("issueCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// Look PR nodes up by id; a null entry is a PR that vanished meanwhile.
fn hydrate(
    runner: &dyn Runner,
    context: &str,
    ids: &[String],
    with_threads: bool,
) -> Result<Vec<Value>, GhError> {
    let mut args = flags(&[
        ("-f", format!("query={}", hydrate_query())),
        ("-F", format!("withThreads={with_threads}")),
    ]);
    for id in ids {
        args.extend(["-f".to_owned(), format!("ids[]={id}")]);
    }
    let mut data = graphql(runner, context, &args)?;
    match data.remove("nodes") {
        Some(Value::Array(nodes)) => Ok(nodes),
        Some(_) | None => Err(GhError(format!(
            "{context}: response has no nodes block (unexpected shape)"
        ))),
    }
}

type Task<'a, T> = Box<dyn FnOnce() -> Result<T, GhError> + Send + 'a>;

/// Run labelled `gh` calls in parallel, at most [`MAX_PARALLEL`] at a time;
/// results come back in submission order. Any failure fails them all, with
/// every failure listed: a partial result would silently hide PRs.
fn gather<T: Send>(tasks: Vec<(String, Task<'_, T>)>, what: &str) -> Result<Vec<T>, GhError> {
    let count = tasks.len();
    let (labels, tasks): (Vec<String>, Vec<_>) = tasks
        .into_iter()
        .map(|(l, t)| (l, Mutex::new(Some(t))))
        .unzip();
    let results: Vec<Mutex<Option<Result<T, String>>>> =
        (0..count).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..count.clamp(1, MAX_PARALLEL) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= count {
                        break;
                    }
                    let task = tasks[index]
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take();
                    let outcome = task.map(|task| match catch_unwind(AssertUnwindSafe(task)) {
                        Ok(result) => result.map_err(|e| e.0),
                        Err(panic) => {
                            let message = panic
                                .downcast_ref::<&str>()
                                .map(|s| (*s).to_owned())
                                .or_else(|| panic.downcast_ref::<String>().cloned())
                                .unwrap_or_default();
                            Err(format!("{} crashed: {message}", labels[index]))
                        }
                    });
                    *results[index]
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = outcome;
                }
            });
        }
    });
    let mut values = Vec::with_capacity(count);
    let mut errors = Vec::new();
    for (label, result) in labels.iter().zip(results) {
        match result.into_inner().unwrap_or_else(PoisonError::into_inner) {
            Some(Ok(value)) => values.push(value),
            Some(Err(error)) => errors.push(error),
            None => errors.push(format!("{label} never ran")),
        }
    }
    if errors.is_empty() {
        Ok(values)
    } else {
        errors.sort();
        Err(GhError(format!(
            "Some {what} failed; results would be incomplete:\n  {}",
            errors.join("\n  ")
        )))
    }
}

/// Open PRs the viewer is involved with through `qualifiers`, enriched and
/// with their attention reasons, most recently updated first.
///
/// `staleness` is the clock for the stale nudges (none disables them).
/// `on_warning` hears about degraded coverage instead of silence: a search
/// beyond the node cap, a `reviewed-by` PR whose review the 50-node cap hid,
/// an authored PR whose threads page came back full without flagging
/// `unresolved`, and enum values GitHub sent that this version doesn't know.
pub fn fetch_prs(
    runner: &dyn Runner,
    qualifiers: &[Role],
    staleness: Option<&Staleness>,
    on_warning: &mut dyn FnMut(&str),
) -> Result<Vec<PullRequest>, GhError> {
    let searches: Vec<SearchResult> = gather(
        qualifiers
            .iter()
            .map(|&q| {
                (
                    format!("Search '{}'", q.as_str()),
                    Box::new(move || search(runner, q)) as Task<'_, _>,
                )
            })
            .collect(),
        "PR searches",
    )?;

    // viewer.login is non-null in GitHub's schema; without it, review state
    // and explicit requests couldn't be classified.
    let viewer = searches
        .iter()
        .map(|s| s.viewer.as_str())
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    if !searches.is_empty() && viewer.is_empty() {
        return Err(GhError(
            "Could not determine the authenticated user from gh's response".to_owned(),
        ));
    }
    for (qualifier, result) in qualifiers.iter().zip(&searches) {
        if result.issue_count > SEARCH_LIMIT {
            on_warning(&format!(
                "search '{}' matched {} PRs; showing the newest {SEARCH_LIMIT}",
                qualifier.as_str(),
                result.issue_count
            ));
        }
    }

    // Each PR is hydrated once however many searches matched it, so every
    // field comes from one snapshot. Only authored PRs pay for threads.
    let mut order: Vec<&str> = Vec::new();
    let mut roles: HashMap<&str, BTreeSet<Role>> = HashMap::new();
    for (qualifier, result) in qualifiers.iter().zip(&searches) {
        for id in &result.ids {
            roles
                .entry(id)
                .or_insert_with(|| {
                    order.push(id);
                    BTreeSet::new()
                })
                .insert(*qualifier);
        }
    }
    let (authored, others): (Vec<&str>, Vec<&str>) = order
        .iter()
        .partition(|id| roles[*id].contains(&Role::Author));
    let batches: Vec<(Vec<String>, bool)> = [(authored, true), (others, false)]
        .into_iter()
        .flat_map(|(ids, with_threads)| {
            ids.chunks(HYDRATE_BATCH)
                .map(|chunk| {
                    (
                        chunk.iter().map(|id| (*id).to_owned()).collect(),
                        with_threads,
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let total = batches.len();
    let hydrated: Vec<Vec<Value>> = gather(
        batches
            .iter()
            .enumerate()
            .map(|(n, (ids, with_threads))| {
                let label = format!("PR lookup {}/{total}", n + 1);
                let context = label.clone();
                (
                    label,
                    Box::new(move || hydrate(runner, &context, ids, *with_threads)) as Task<'_, _>,
                )
            })
            .collect(),
        "PR lookups",
    )?;

    let mut prs: Vec<PullRequest> = Vec::new();
    let mut index_by_id: HashMap<String, usize> = HashMap::new();
    let mut unknown: Vec<Unrecognised> = Vec::new();
    let mut first_seen: BTreeMap<Unrecognised, String> = BTreeMap::new();
    for node in hydrated.into_iter().flatten() {
        if node.is_null() || node.as_object().is_some_and(serde_json::Map::is_empty) {
            continue;
        }
        let raw: RawPr = serde_json::from_value(node)
            .map_err(|e| GhError(format!("Failed to parse PR data: {e}")))?;
        let node_roles = raw
            .id
            .as_deref()
            .and_then(|id| roles.get(id))
            .ok_or_else(|| {
                GhError("Failed to parse PR data: a node without a searched id".to_owned())
            })?;
        let mut pr = to_pull_request(&raw, viewer, &mut unknown);
        for value in unknown.drain(..) {
            first_seen.entry(value).or_insert_with(|| pr.id());
        }
        // Closed or merged between the search and the lookup: the searches
        // only match open PRs. An unknown state stays.
        if matches!(pr.state, Reported::Known(PrState::Closed | PrState::Merged)) {
            continue;
        }
        pr.roles.clone_from(node_roles);
        match index_by_id.get(&pr.id()) {
            Some(&index) => prs[index] = pr,
            None => {
                index_by_id.insert(pr.id(), prs.len());
                prs.push(pr);
            }
        }
    }

    for pr in &mut prs {
        pr.attention = attention_reasons(pr, staleness);
        // The reviewed-by search asserts I reviewed this PR, so no review
        // in hand means the 50-node cap hid it. Drafts never flag anyway.
        let review_known = pr
            .my_review
            .as_ref()
            .is_some_and(|review| review.state != Reported::Missing);
        if !pr.is_draft && pr.roles.contains(&Role::ReviewedBy) && !review_known {
            on_warning(&format!(
                "{}: your review is not among its first {REVIEW_PAGE_LIMIT} latest reviews; new-commit detection may miss it",
                pr.id()
            ));
        }
        // Flagging on a hidden thread would fabricate the reason on every
        // PR past the cap, so inform instead. A flagged PR shows anyway.
        if !pr.is_draft
            && pr.is_authored()
            && pr.review_threads_truncated
            && !pr.attention.contains(&Reason::Unresolved)
        {
            on_warning(&format!(
                "{}: it has more than {REVIEW_PAGE_LIMIT} review threads; unresolved-comment detection may miss some",
                pr.id()
            ));
        }
    }
    warn_unrecognised(&first_seen, on_warning);
    prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(prs)
}

/// One warning per unknown value, however many PRs carry it: the contract
/// with GitHub changed, and the code should catch up.
fn warn_unrecognised(
    first_seen: &BTreeMap<Unrecognised, String>,
    on_warning: &mut dyn FnMut(&str),
) {
    for (Unrecognised { field, value }, id) in first_seen {
        on_warning(&format!(
            "{id}: GitHub sent {field} '{value}', which this version of gh-prs doesn't know; it is read as unknown"
        ));
    }
}

/// The `gh api` flags routing a request to the host a PR URL lives on: an
/// Enterprise host needs `--hostname` (and a `gh auth` session there).
fn host_args(url: &str) -> Vec<String> {
    let authority = url.split_once("://").map_or("", |(_, rest)| {
        rest.split(['/', '?', '#']).next().unwrap_or("")
    });
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("").to_lowercase();
    if host.is_empty() || host == "github.com" {
        Vec::new()
    } else {
        vec!["--hostname".to_owned(), host]
    }
}

/// One PR by canonical URL, enriched exactly like a search node.
///
/// The `author` role is set when the viewer wrote it; nothing else is (this
/// is the merge preflight and the prune lookup, not a view). A URL that
/// resolves to nothing, or to something that isn't a PR, is an error: the
/// preflight must never proceed on a PR it couldn't inspect.
pub fn fetch_pr(
    runner: &dyn Runner,
    url: &str,
    on_warning: &mut dyn FnMut(&str),
) -> Result<PullRequest, GhError> {
    let context = format!("Lookup of {url}");
    let mut args = host_args(url);
    args.extend(flags(&[
        ("-f", format!("query={}", pr_query())),
        ("-f", format!("url={url}")),
        ("-F", "withThreads=true".to_owned()),
    ]));
    let mut data = graphql(runner, &context, &args)?;
    let viewer = data["viewer"]["login"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if viewer.is_empty() {
        return Err(GhError(format!(
            "{context}: could not determine the authenticated user"
        )));
    }
    let resource = data
        .remove("resource")
        .filter(Value::is_object)
        .ok_or_else(|| {
            GhError(format!(
                "{context}: no such pull request (or no access to it)"
            ))
        })?;
    if resource["__typename"] != "PullRequest" {
        return Err(GhError(format!("{context}: not a pull request")));
    }
    let raw: RawPr = serde_json::from_value(resource)
        .map_err(|e| GhError(format!("{context}: failed to parse PR data: {e}")))?;
    let mut unknown = Vec::new();
    let mut pr = to_pull_request(&raw, &viewer, &mut unknown);
    let first_seen = unknown.into_iter().map(|value| (value, pr.id())).collect();
    warn_unrecognised(&first_seen, on_warning);
    if pr.author.as_deref() == Some(viewer.as_str()) {
        pr.roles.insert(Role::Author);
    }
    Ok(pr)
}

/// `gh pr view` with `--json`, its answer as an object.
fn pr_view(
    runner: &dyn Runner,
    context: &str,
    args: &[String],
) -> Result<serde_json::Map<String, Value>, GhError> {
    let output = runner.run(args)?;
    if !output.success {
        return Err(GhError(format!("{context} failed: {}", output.detail())));
    }
    let payload: Value = serde_json::from_str(&output.stdout)
        .map_err(|e| GhError(format!("{context}: invalid JSON from gh: {e}")))?;
    Ok(payload.as_object().cloned().unwrap_or_default())
}

/// The head commit of one PR, looked up by URL. A missing oid is an error:
/// a snooze recorded against an unknown head could hide newer work.
pub fn fetch_pr_head(runner: &dyn Runner, url: &str) -> Result<Oid, GhError> {
    let context = format!("Lookup of {url}");
    let args = ["pr", "view", url, "--json", "headRefOid"].map(str::to_owned);
    let payload = pr_view(runner, &context, &args)?;
    payload
        .get("headRefOid")
        .and_then(Value::as_str)
        .and_then(Oid::new)
        .ok_or_else(|| GhError(format!("{context}: response has no headRefOid")))
}

/// A PR reference resolved to its canonical URL and head commit.
///
/// `reference` is anything `gh pr view` accepts, most usefully a bare
/// number; `repo` (`owner/repo` or `host/owner/repo`) scopes it, and without
/// it gh uses the current directory's repository. Delegating to gh gives
/// the URL the right host, so Enterprise instances need nothing special.
pub fn resolve_pr(
    runner: &dyn Runner,
    reference: &str,
    repo: Option<&str>,
) -> Result<(String, Oid), GhError> {
    let mut args = ["pr", "view", reference, "--json", "url,headRefOid"]
        .map(str::to_owned)
        .to_vec();
    if let Some(repo) = repo {
        args.extend(["--repo".to_owned(), repo.to_owned()]);
    }
    let context = match repo {
        Some(repo) => format!("Lookup of PR {reference} in {repo}"),
        None => format!("Lookup of PR {reference}"),
    };
    let output = runner.run(&args)?;
    if !output.success {
        return Err(GhError(format!("{context} failed: {}", output.detail())));
    }
    let payload: Value = serde_json::from_str(&output.stdout)
        .map_err(|e| GhError(format!("{context}: invalid JSON from gh: {e}")))?;
    let Some(payload) = payload.as_object() else {
        return Err(GhError(format!("{context}: unexpected response from gh")));
    };
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| GhError(format!("{context}: response has no url")))?;
    let oid = payload
        .get("headRefOid")
        .and_then(Value::as_str)
        .and_then(Oid::new)
        .ok_or_else(|| GhError(format!("{context}: response has no headRefOid")))?;
    Ok((url.to_owned(), oid))
}

/// A state-changing `gh` command; a failure carries gh's own message.
fn run_action(runner: &dyn Runner, context: &str, args: &[String]) -> Result<(), GhError> {
    let output = runner.run(args)?;
    if output.success {
        return Ok(());
    }
    let detail = match output.detail() {
        "" => "gh exited non-zero",
        detail => detail,
    };
    Err(GhError(format!("{context} failed: {detail}")))
}

/// Submit an approving review (`gh pr review --approve`).
pub fn approve_pr(runner: &dyn Runner, url: &str) -> Result<(), GhError> {
    run_action(
        runner,
        &format!("Approving {url}"),
        &["pr", "review", "--approve", url].map(str::to_owned),
    )
}

/// Squash-merge one PR and delete its branch (`gh pr merge`).
///
/// `head` is the commit the preflight inspected, passed as
/// `--match-head-commit` so GitHub refuses the merge if the branch moved in
/// between: the merge lands exactly the state that was checked.
pub fn merge_pr(
    runner: &dyn Runner,
    url: &str,
    head: &Oid,
    mode: MergeMode,
) -> Result<(), GhError> {
    let mut args = [
        "pr",
        "merge",
        "--squash",
        "--delete-branch",
        "--match-head-commit",
        head.as_str(),
    ]
    .map(str::to_owned)
    .to_vec();
    match mode {
        MergeMode::Now => {}
        MergeMode::Admin => args.push("--admin".to_owned()),
        MergeMode::Auto => args.push("--auto".to_owned()),
    }
    args.push(url.to_owned());
    run_action(runner, &format!("Merging {url}"), &args)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use jiff::tz::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::duration::Duration;
    use crate::gh::Output;
    use crate::gh::queries::PR_FRAGMENT;
    use crate::pr::{Mergeable, ReviewState};

    fn ok(stdout: impl Into<String>) -> Result<Output, GhError> {
        Ok(Output {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }

    fn failed(stderr: &str) -> Result<Output, GhError> {
        Ok(Output {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        })
    }

    /// A runner answering every call with `reply`, recording the arguments.
    struct Recorder {
        reply: Box<dyn Fn() -> Result<Output, GhError> + Sync>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Recorder {
        fn new(reply: impl Fn() -> Result<Output, GhError> + Sync + 'static) -> Self {
            Self {
                reply: Box::new(reply),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for Recorder {
        fn run(&self, args: &[String]) -> Result<Output, GhError> {
            self.calls.lock().unwrap().push(args.to_vec());
            (self.reply)()
        }
    }

    fn answering(stdout: &'static str) -> Recorder {
        Recorder::new(move || ok(stdout))
    }

    fn failing(stderr: &'static str) -> Recorder {
        Recorder::new(move || failed(stderr))
    }

    fn message<T: std::fmt::Debug>(result: Result<T, GhError>) -> String {
        result.unwrap_err().0
    }

    /// A GraphQL node as hydration returns it, keyed `PR_<number>`.
    fn node(number: u64, overrides: Value) -> Value {
        let mut node = json!({
            "id": format!("PR_{number}"),
            "number": number,
            "title": "Fix parser",
            "url": format!("https://github.com/acme/widgets/pull/{number}"),
            "updatedAt": "2026-07-15T12:00:00Z",
            "createdAt": "2026-07-01T09:30:00Z",
            "isDraft": false,
            "reviewDecision": "REVIEW_REQUIRED",
            "mergeable": "MERGEABLE",
            "repository": {"nameWithOwner": "acme/widgets"},
            "author": {"login": "octocat"},
            "reviewRequests": {"nodes": []},
            "latestReviews": {"nodes": []},
            "latestOpinionatedReviews": {"nodes": []},
            "reviewThreads": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]},
            "baseRef": {"associatedPullRequests": {"totalCount": 0}},
        });
        for (key, value) in overrides.as_object().unwrap() {
            node[key] = value.clone();
        }
        node
    }

    fn id(node: &Value) -> String {
        node["id"].as_str().unwrap().to_owned()
    }

    /// One fake search: the viewer, the nodes it finds and its issue count,
    /// or the stderr of a failure.
    type FakeSearch = Result<(&'static str, Vec<Value>, u64), &'static str>;

    /// Answers searches from `searches` (viewer, nodes, issue count, or a
    /// failure) and hydrates from the nodes they list; records the lookups.
    #[derive(Default)]
    struct FakeGh {
        searches: HashMap<&'static str, FakeSearch>,
        overrides: HashMap<String, Value>,
        hydrate_failure: Option<&'static str>,
        lookups: Mutex<Vec<(Vec<String>, bool)>>,
    }

    impl FakeGh {
        fn with(searches: &[(&'static str, FakeSearch)]) -> Self {
            Self {
                searches: searches.iter().cloned().collect(),
                ..Self::default()
            }
        }

        fn lookups(&self) -> Vec<(Vec<String>, bool)> {
            let mut lookups = self.lookups.lock().unwrap().clone();
            lookups.sort();
            lookups
        }

        fn registry(&self, id: &str) -> Value {
            if let Some(node) = self.overrides.get(id) {
                return node.clone();
            }
            self.searches
                .values()
                .flat_map(|search| {
                    search
                        .as_ref()
                        .map(|(_, nodes, _)| nodes.clone())
                        .unwrap_or_default()
                })
                .find(|node| node["id"] == id)
                .unwrap_or(Value::Null)
        }
    }

    impl Runner for FakeGh {
        fn run(&self, args: &[String]) -> Result<Output, GhError> {
            let ids: Vec<String> = args
                .iter()
                .filter_map(|a| a.strip_prefix("ids[]="))
                .map(str::to_owned)
                .collect();
            if !ids.is_empty() {
                let with_threads = args.iter().any(|a| a == "withThreads=true");
                self.lookups
                    .lock()
                    .unwrap()
                    .push((ids.clone(), with_threads));
                if let Some(error) = self.hydrate_failure {
                    return failed(error);
                }
                let nodes: Vec<Value> = ids.iter().map(|id| self.registry(id)).collect();
                return ok(json!({"data": {"nodes": nodes}}).to_string());
            }
            let query = args.iter().find_map(|a| a.strip_prefix("q=")).unwrap();
            let qualifier = query
                .split_whitespace()
                .find_map(|t| t.strip_suffix(":@me"))
                .unwrap();
            match &self.searches[qualifier] {
                Err(error) => failed(error),
                Ok((viewer, nodes, count)) => {
                    let ids: Vec<Value> = nodes.iter().map(|n| json!({"id": n["id"]})).collect();
                    ok(json!({"data": {"viewer": {"login": viewer}, "results": {"issueCount": count, "nodes": ids}}})
                        .to_string())
                }
            }
        }
    }

    fn fetch(gh: &FakeGh, qualifiers: &[Role]) -> Result<(Vec<PullRequest>, Vec<String>), GhError> {
        let mut warnings = Vec::new();
        let prs = fetch_prs(gh, qualifiers, None, &mut |w| warnings.push(w.to_owned()))?;
        Ok((prs, warnings))
    }

    mod fragment {
        use super::*;

        #[test]
        fn review_connections_match_the_page_limit_constant() {
            assert_eq!(
                PR_FRAGMENT
                    .matches(&format!("first: {REVIEW_PAGE_LIMIT})"))
                    .count(),
                4
            );
        }

        #[test]
        fn labels_connection_matches_the_page_limit_constant() {
            assert!(PR_FRAGMENT.contains(&format!("labels(first: {LABEL_PAGE_LIMIT})")));
            assert_ne!(LABEL_PAGE_LIMIT, REVIEW_PAGE_LIMIT);
        }

        #[test]
        fn fragment_requests_every_field_the_parser_reads() {
            for field in [
                "headRefOid",
                "reviewRequests",
                "latestReviews",
                "latestOpinionatedReviews",
                "reviewThreads",
                "isResolved",
                "statusCheckRollup",
                "baseRef",
                "associatedPullRequests",
                "state",
                "labels",
            ] {
                assert!(PR_FRAGMENT.contains(field), "{field}");
            }
        }

        #[test]
        fn search_does_not_hydrate_nodes() {
            assert!(!SEARCH_QUERY.contains("prFields"));
            assert!(hydrate_query().contains("...prFields"));
        }
    }

    mod search_and_count {
        use super::*;

        #[test]
        fn happy_path_skips_null_nodes() {
            let runner = answering(
                r#"{"data": {"viewer": {"login": "me"}, "results": {"issueCount": 3, "nodes": [{"id": "PR_1"}, null, {}, {"id": "PR_2"}]}}}"#,
            );
            let result = search(&runner, Role::Author).unwrap();
            assert_eq!(
                (result.viewer.as_str(), result.ids, result.issue_count),
                ("me", vec!["PR_1".to_owned(), "PR_2".to_owned()], 3)
            );
            let call = &runner.calls()[0];
            assert!(call.contains(
                &"q=is:pr is:open archived:false sort:updated-desc author:@me".to_owned()
            ));
            assert!(call.contains(&"limit=100".to_owned()));
        }

        #[test]
        fn node_without_id_raises() {
            let runner = answering(
                r#"{"data": {"viewer": {"login": "me"}, "results": {"nodes": [{"number": 42}]}}}"#,
            );
            assert_eq!(
                message(search(&runner, Role::Author)),
                "Search 'author': result without a node id (unexpected shape)"
            );
        }

        #[test]
        fn nonzero_exit_raises_with_context() {
            assert_eq!(
                message(search(&failing("401"), Role::Author)),
                "Search 'author' failed: 401"
            );
        }

        #[test]
        fn missing_results_block_raises() {
            let runner = answering(r#"{"data": {"viewer": {"login": "me"}}}"#);
            assert_eq!(
                message(search(&runner, Role::Author)),
                "Search 'author': response has no results block (unexpected shape)"
            );
        }

        #[test]
        fn count_returns_issue_count() {
            assert_eq!(
                count_prs(
                    &answering(r#"{"data": {"results": {"issueCount": 143}}}"#),
                    Role::Author
                ),
                Ok(143)
            );
        }

        #[test]
        fn count_without_issue_count_raises() {
            assert_eq!(
                message(count_prs(
                    &answering(r#"{"data": {"results": {}}}"#),
                    Role::ReviewRequested
                )),
                "Count 'review-requested': response has no issueCount (unexpected shape)"
            );
        }
    }

    mod hydration {
        use super::*;

        #[test]
        fn passes_ids_and_threads_flag() {
            let runner = answering(r#"{"data": {"nodes": [{"id": "PR_1"}]}}"#);
            let nodes = hydrate(
                &runner,
                "PR lookup 1/1",
                &["PR_1".to_owned(), "PR_2".to_owned()],
                true,
            )
            .unwrap();
            assert_eq!(nodes[0]["id"], "PR_1");
            hydrate(&runner, "PR lookup 1/1", &["PR_1".to_owned()], false).unwrap();
            let calls = runner.calls();
            assert!(calls[0].contains(&"withThreads=true".to_owned()));
            assert!(calls[1].contains(&"withThreads=false".to_owned()));
            let ids: Vec<&String> = calls[0]
                .iter()
                .filter(|a| a.starts_with("ids[]="))
                .collect();
            assert_eq!(ids, ["ids[]=PR_1", "ids[]=PR_2"]);
        }

        #[test]
        fn missing_nodes_block_raises() {
            let runner = answering(r#"{"data": {}}"#);
            assert_eq!(
                message(hydrate(
                    &runner,
                    "PR lookup 1/1",
                    &["PR_1".to_owned()],
                    false
                )),
                "PR lookup 1/1: response has no nodes block (unexpected shape)"
            );
        }
    }

    mod fetching {
        use super::*;

        #[test]
        fn dedup_merges_roles_and_computes_attention() {
            let shared = node(1, json!({}));
            let authored = node(2, json!({"updatedAt": "2026-07-16T12:00:00Z"}));
            let gh = FakeGh::with(&[
                (
                    "author",
                    Ok(("me", vec![shared.clone(), authored.clone()], 2)),
                ),
                ("review-requested", Ok(("me", vec![shared.clone()], 1))),
            ]);
            let (prs, _) = fetch(&gh, &[Role::Author, Role::ReviewRequested]).unwrap();
            // Newest first: PR 2 (07-16) before PR 1 (07-15).
            assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), [2, 1]);
            assert_eq!(
                prs[1].roles,
                BTreeSet::from([Role::Author, Role::ReviewRequested])
            );
            assert_eq!(prs[0].roles, BTreeSet::from([Role::Author]));
            assert_eq!(prs[1].attention, BTreeSet::from([Reason::Review]));
            // Hydrated once, though two searches matched PR 1.
            let mut hydrated: Vec<String> =
                gh.lookups().into_iter().flat_map(|(ids, _)| ids).collect();
            hydrated.sort();
            assert_eq!(hydrated, [id(&shared), id(&authored)]);
        }

        #[test]
        fn only_authored_prs_are_hydrated_with_threads() {
            let (mine, both, theirs) = (node(1, json!({})), node(2, json!({})), node(3, json!({})));
            let gh = FakeGh::with(&[
                ("author", Ok(("me", vec![mine.clone(), both.clone()], 2))),
                (
                    "review-requested",
                    Ok(("me", vec![both.clone(), theirs.clone()], 2)),
                ),
            ]);
            fetch(&gh, &[Role::Author, Role::ReviewRequested]).unwrap();
            assert_eq!(
                gh.lookups(),
                [
                    (vec![id(&mine), id(&both)], true),
                    (vec![id(&theirs)], false)
                ]
            );
        }

        #[test]
        fn lookups_are_batched() {
            let nodes: Vec<Value> = (0..=HYDRATE_BATCH as u64)
                .map(|n| node(n, json!({})))
                .collect();
            let gh = FakeGh::with(&[(
                "review-requested",
                Ok(("me", nodes.clone(), nodes.len() as u64)),
            )]);
            let (prs, _) = fetch(&gh, &[Role::ReviewRequested]).unwrap();
            assert_eq!(prs.len(), nodes.len());
            let mut sizes: Vec<usize> = gh.lookups().iter().map(|(ids, _)| ids.len()).collect();
            sizes.sort_unstable();
            assert_eq!(sizes, [1, HYDRATE_BATCH]);
        }

        #[test]
        fn no_match_makes_no_lookup() {
            let gh = FakeGh::with(&[("author", Ok(("me", vec![], 0)))]);
            assert!(fetch(&gh, &[Role::Author]).unwrap().0.is_empty());
            assert!(gh.lookups().is_empty());
        }

        #[test]
        fn pr_gone_or_closed_since_the_search_is_dropped() {
            let (kept, gone, merged) = (
                node(1, json!({})),
                node(2, json!({})),
                node(3, json!({"state": "MERGED"})),
            );
            let mut gh =
                FakeGh::with(&[("author", Ok(("me", vec![kept, gone.clone(), merged], 3)))]);
            gh.overrides.insert(id(&gone), Value::Null);
            let (prs, _) = fetch(&gh, &[Role::Author]).unwrap();
            assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), [1]);
        }

        #[test]
        fn failed_lookup_fails_the_whole_fetch() {
            let mut gh = FakeGh::with(&[("author", Ok(("me", vec![node(1, json!({}))], 1)))]);
            gh.hydrate_failure = Some("gh: HTTP 502");
            assert_eq!(
                message(fetch(&gh, &[Role::Author])),
                "Some PR lookups failed; results would be incomplete:\n  PR lookup 1/1 failed: gh: HTTP 502"
            );
        }

        #[test]
        fn the_staleness_clock_reaches_the_reasons() {
            let gh = FakeGh::with(&[("author", Ok(("me", vec![node(1, json!({}))], 1)))]);
            let clock = Staleness {
                now: "2026-07-27T12:00:00Z".parse().unwrap(),
                after: Duration::days(3),
                skip_weekends: false,
                tz: TimeZone::UTC,
            };
            let prs = fetch_prs(&gh, &[Role::Author], Some(&clock), &mut |_| {}).unwrap();
            assert_eq!(prs[0].attention, BTreeSet::from([Reason::Stale]));
        }

        #[test]
        fn viewer_propagates_from_any_search() {
            let reviewed = node(
                1,
                json!({"latestReviews": {"nodes": [{"author": {"login": "me"}, "state": "APPROVED"}]}}),
            );
            let gh = FakeGh::with(&[
                ("author", Ok(("", vec![], 0))),
                ("review-requested", Ok(("me", vec![reviewed], 1))),
            ]);
            let (prs, _) = fetch(&gh, &[Role::Author, Role::ReviewRequested]).unwrap();
            assert_eq!(
                prs[0].my_review.as_ref().unwrap().state,
                Reported::Known(ReviewState::Approved)
            );
        }

        #[test]
        fn failed_searches_are_all_reported() {
            let gh = FakeGh::with(&[
                ("author", Err("HTTP 502")),
                ("review-requested", Err("HTTP 401")),
            ]);
            assert_eq!(
                message(fetch(&gh, &[Role::ReviewRequested, Role::Author])),
                "Some PR searches failed; results would be incomplete:\n  Search 'author' failed: HTTP 502\n  Search 'review-requested' failed: HTTP 401"
            );
        }

        #[test]
        fn a_crashing_call_is_aggregated_not_raised_raw() {
            let crashing = |_: &[String]| -> Result<Output, GhError> { panic!("shape drift") };
            let error = message(fetch_prs(&crashing, &[Role::Author], None, &mut |_| {}));
            assert_eq!(
                error,
                "Some PR searches failed; results would be incomplete:\n  Search 'author' crashed: shape drift"
            );
        }

        #[test]
        fn missing_viewer_raises() {
            let gh = FakeGh::with(&[("author", Ok(("", vec![node(1, json!({}))], 1)))]);
            assert_eq!(
                message(fetch(&gh, &[Role::Author])),
                "Could not determine the authenticated user from gh's response"
            );
        }

        #[test]
        fn malformed_node_raises() {
            let mut bad = node(1, json!({}));
            bad.as_object_mut().unwrap().remove("number");
            let gh = FakeGh::with(&[("author", Ok(("me", vec![bad], 1)))]);
            assert!(
                message(fetch(&gh, &[Role::Author]))
                    .starts_with("Failed to parse PR data: missing field `number`")
            );
        }

        #[test]
        fn truncation_triggers_warning() {
            let gh = FakeGh::with(&[("author", Ok(("me", vec![node(1, json!({}))], 250)))]);
            assert_eq!(
                fetch(&gh, &[Role::Author]).unwrap().1,
                ["search 'author' matched 250 PRs; showing the newest 100"]
            );
        }

        #[test]
        fn reviewed_by_without_parsed_review_warns_unless_draft() {
            let gh = FakeGh::with(&[("reviewed-by", Ok(("me", vec![node(42, json!({}))], 1)))]);
            assert_eq!(
                fetch(&gh, &[Role::ReviewedBy]).unwrap().1,
                [
                    "acme/widgets#42: your review is not among its first 50 latest reviews; new-commit detection may miss it"
                ]
            );
            let draft = FakeGh::with(&[(
                "reviewed-by",
                Ok(("me", vec![node(42, json!({"isDraft": true}))], 1)),
            )]);
            assert!(fetch(&draft, &[Role::ReviewedBy]).unwrap().1.is_empty());
        }

        fn full_threads(overrides: Value) -> Value {
            let threads: Vec<Value> = (0..REVIEW_PAGE_LIMIT)
                .map(|_| json!({"isResolved": false, "comments": {"nodes": [{"author": {"login": "me"}}]}}))
                .collect();
            let mut node = node(42, json!({"reviewThreads": {"nodes": threads}}));
            for (key, value) in overrides.as_object().unwrap() {
                node[key] = value.clone();
            }
            node
        }

        #[test]
        fn full_threads_page_without_flag_warns() {
            let gh = FakeGh::with(&[("author", Ok(("me", vec![full_threads(json!({}))], 1)))]);
            assert_eq!(
                fetch(&gh, &[Role::Author]).unwrap().1,
                [
                    "acme/widgets#42: it has more than 50 review threads; unresolved-comment detection may miss some"
                ]
            );
        }

        #[test]
        fn full_threads_page_does_not_warn_when_flagged_draft_or_not_mine() {
            let mut flagged = full_threads(json!({}));
            flagged["reviewThreads"]["nodes"][0]["comments"]["nodes"][0]["author"] =
                json!({"login": "reviewer"});
            let gh = FakeGh::with(&[("author", Ok(("me", vec![flagged], 1)))]);
            let (prs, warnings) = fetch(&gh, &[Role::Author]).unwrap();
            assert!(prs[0].attention.contains(&Reason::Unresolved));
            assert!(warnings.is_empty());
            let draft = FakeGh::with(&[(
                "author",
                Ok(("me", vec![full_threads(json!({"isDraft": true}))], 1)),
            )]);
            assert!(fetch(&draft, &[Role::Author]).unwrap().1.is_empty());
            let theirs = FakeGh::with(&[(
                "review-requested",
                Ok(("me", vec![full_threads(json!({}))], 1)),
            )]);
            assert!(
                fetch(&theirs, &[Role::ReviewRequested])
                    .unwrap()
                    .1
                    .is_empty()
            );
        }

        #[test]
        fn unrecognised_values_warn_once_each() {
            let gh = FakeGh::with(&[(
                "author",
                Ok((
                    "me",
                    vec![
                        node(1, json!({"mergeable": "MAYBE"})),
                        node(2, json!({"mergeable": "MAYBE"})),
                    ],
                    2,
                )),
            )]);
            let (prs, warnings) = fetch(&gh, &[Role::Author]).unwrap();
            assert!(
                prs.iter()
                    .all(|pr| pr.mergeable == Reported::Unrecognised("MAYBE".to_owned()))
            );
            assert_eq!(
                warnings,
                [
                    "acme/widgets#1: GitHub sent mergeable 'MAYBE', which this version of gh-prs doesn't know; it is read as unknown"
                ]
            );
        }

        #[test]
        fn known_values_do_not_warn() {
            let gh = FakeGh::with(&[(
                "author",
                Ok(("me", vec![node(1, json!({"mergeable": "CONFLICTING"}))], 1)),
            )]);
            let (prs, warnings) = fetch(&gh, &[Role::Author]).unwrap();
            assert_eq!(prs[0].mergeable, Reported::Known(Mergeable::Conflicting));
            assert!(warnings.is_empty());
        }
    }

    mod single_pr {
        use super::*;

        const URL: &str = "https://github.com/acme/widgets/pull/42";

        fn payload(resource: Value, viewer: &str) -> String {
            json!({"data": {"viewer": {"login": viewer}, "resource": resource}}).to_string()
        }

        fn pr_resource(overrides: Value) -> Value {
            let mut resource = node(42, overrides);
            resource["__typename"] = json!("PullRequest");
            resource
        }

        fn lookup(stdout: String) -> (Result<PullRequest, GhError>, Vec<Vec<String>>) {
            let runner = Recorder::new(move || ok(stdout.clone()));
            let result = fetch_pr(&runner, URL, &mut |_| {});
            (result, runner.calls())
        }

        #[test]
        fn enriches_like_a_search_node() {
            let pr = lookup(payload(pr_resource(json!({"state": "OPEN"})), "me"))
                .0
                .unwrap();
            assert_eq!(pr.id(), "acme/widgets#42");
            assert_eq!(pr.state, Reported::Known(PrState::Open));
            assert!(pr.roles.is_empty() && pr.attention.is_empty());
        }

        #[test]
        fn viewer_authored_gets_the_author_role() {
            let pr = lookup(payload(pr_resource(json!({})), "octocat"))
                .0
                .unwrap();
            assert_eq!(pr.roles, BTreeSet::from([Role::Author]));
        }

        #[test]
        fn passes_url_as_a_variable_with_threads() {
            let (_, calls) = lookup(payload(pr_resource(json!({})), "me"));
            let args = &calls[0];
            assert_eq!(&args[..2], ["api", "graphql"]);
            assert!(args.contains(&format!("url={URL}")));
            assert!(args.contains(&"withThreads=true".to_owned()));
            assert!(!args.contains(&"--hostname".to_owned()));
        }

        #[test]
        fn enterprise_host_is_routed() {
            let runner = Recorder::new(|| ok(payload(pr_resource(json!({})), "me")));
            fetch_pr(
                &runner,
                "https://GHE.example.com/acme/widgets/pull/42",
                &mut |_| {},
            )
            .unwrap();
            let args = &runner.calls()[0];
            let at = args.iter().position(|a| a == "--hostname").unwrap();
            assert_eq!(args[at + 1], "ghe.example.com");
        }

        #[test]
        fn missing_or_foreign_resource_is_an_error() {
            assert_eq!(
                message(lookup(payload(Value::Null, "me")).0),
                format!("Lookup of {URL}: no such pull request (or no access to it)")
            );
            assert_eq!(
                message(lookup(payload(json!({"__typename": "Issue"}), "me")).0),
                format!("Lookup of {URL}: not a pull request")
            );
        }

        #[test]
        fn missing_viewer_is_an_error() {
            assert_eq!(
                message(lookup(payload(pr_resource(json!({})), "")).0),
                format!("Lookup of {URL}: could not determine the authenticated user")
            );
        }

        #[test]
        fn malformed_node_is_an_error() {
            let mut resource = pr_resource(json!({}));
            resource.as_object_mut().unwrap().remove("repository");
            assert!(
                message(lookup(payload(resource, "me")).0)
                    .starts_with(&format!("Lookup of {URL}: failed to parse PR data: "))
            );
        }

        #[test]
        fn gh_failure_is_an_error() {
            assert_eq!(
                message(fetch_pr(&failing("401"), URL, &mut |_| {})),
                format!("Lookup of {URL} failed: 401")
            );
        }
    }

    mod lookups {
        use super::*;

        const URL: &str = "https://github.com/acme/widgets/pull/42";

        #[test]
        fn head_lookup_passes_url_through() {
            let runner = answering(r#"{"headRefOid": "cafe123"}"#);
            assert_eq!(
                fetch_pr_head(&runner, URL),
                Ok(Oid::new("cafe123").unwrap())
            );
            assert_eq!(
                runner.calls(),
                [["pr", "view", URL, "--json", "headRefOid"]]
            );
        }

        #[test]
        fn head_lookup_failures() {
            assert_eq!(
                message(fetch_pr_head(&failing("no such PR"), "url")),
                "Lookup of url failed: no such PR"
            );
            assert!(
                message(fetch_pr_head(&answering("not json"), "url"))
                    .starts_with("Lookup of url: invalid JSON from gh: ")
            );
            for payload in [
                "null",
                "{}",
                r#"{"headRefOid": ""}"#,
                r#"{"headRefOid": null}"#,
                r#"{"headRefOid": 5}"#,
            ] {
                assert_eq!(
                    message(fetch_pr_head(&answering(payload), "url")),
                    "Lookup of url: response has no headRefOid",
                    "{payload}"
                );
            }
        }

        #[test]
        fn bare_number_uses_current_repo_or_the_given_one() {
            let runner = answering(
                r#"{"url": "https://github.com/acme/widgets/pull/42", "headRefOid": "cafe123"}"#,
            );
            let (url, head) = resolve_pr(&runner, "42", None).unwrap();
            assert_eq!(
                (url.as_str(), head.as_str()),
                ("https://github.com/acme/widgets/pull/42", "cafe123")
            );
            resolve_pr(&runner, "42", Some("acme/widgets")).unwrap();
            assert_eq!(
                runner.calls(),
                [
                    vec!["pr", "view", "42", "--json", "url,headRefOid"],
                    vec![
                        "pr",
                        "view",
                        "42",
                        "--json",
                        "url,headRefOid",
                        "--repo",
                        "acme/widgets"
                    ],
                ]
            );
        }

        #[test]
        fn resolve_failures() {
            assert_eq!(
                message(resolve_pr(
                    &failing("not found"),
                    "42",
                    Some("acme/widgets")
                )),
                "Lookup of PR 42 in acme/widgets failed: not found"
            );
            assert!(
                message(resolve_pr(&answering("not json"), "42", None))
                    .starts_with("Lookup of PR 42: invalid JSON from gh: ")
            );
            assert_eq!(
                message(resolve_pr(&answering("[]"), "42", None)),
                "Lookup of PR 42: unexpected response from gh"
            );
            for payload in [
                "{}",
                r#"{"url": "", "headRefOid": "cafe"}"#,
                r#"{"url": 5, "headRefOid": "cafe"}"#,
            ] {
                assert_eq!(
                    message(resolve_pr(&answering(payload), "42", None)),
                    "Lookup of PR 42: response has no url",
                    "{payload}"
                );
            }
            for payload in [
                r#"{"url": "u"}"#,
                r#"{"url": "u", "headRefOid": ""}"#,
                r#"{"url": "u", "headRefOid": 5}"#,
            ] {
                assert_eq!(
                    message(resolve_pr(&answering(payload), "42", None)),
                    "Lookup of PR 42: response has no headRefOid",
                    "{payload}"
                );
            }
        }
    }

    mod actions {
        use super::*;

        const URL: &str = "https://github.com/acme/widgets/pull/42";

        fn head() -> Oid {
            Oid::new("cafe").unwrap()
        }

        #[test]
        fn approve_command_and_failure() {
            let runner = answering("");
            approve_pr(&runner, URL).unwrap();
            assert_eq!(runner.calls(), [["pr", "review", "--approve", URL]]);
            assert_eq!(
                message(approve_pr(&failing("Can not approve your own PR"), URL)),
                format!("Approving {URL} failed: Can not approve your own PR")
            );
        }

        #[test]
        fn merge_squashes_deletes_and_pins_the_head() {
            let runner = answering("");
            merge_pr(&runner, URL, &head(), MergeMode::Now).unwrap();
            assert_eq!(
                runner.calls(),
                [[
                    "pr",
                    "merge",
                    "--squash",
                    "--delete-branch",
                    "--match-head-commit",
                    "cafe",
                    URL
                ]]
            );
        }

        #[test]
        fn merge_modes_add_their_flag() {
            for (mode, flag) in [(MergeMode::Admin, "--admin"), (MergeMode::Auto, "--auto")] {
                let runner = answering("");
                merge_pr(&runner, URL, &head(), mode).unwrap();
                let call = &runner.calls()[0];
                assert_eq!(call[call.len() - 2], flag);
                assert_eq!(call[call.len() - 1], URL);
            }
        }

        #[test]
        fn merge_failure_carries_gh_message() {
            assert_eq!(
                message(merge_pr(
                    &failing("head commit mismatch"),
                    URL,
                    &head(),
                    MergeMode::Now
                )),
                format!("Merging {URL} failed: head commit mismatch")
            );
            assert_eq!(
                message(merge_pr(&failing(""), URL, &head(), MergeMode::Now)),
                format!("Merging {URL} failed: gh exited non-zero")
            );
        }
    }
}
