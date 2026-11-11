# Architecture of gh-issues

How gh-issues is put together and what it promises. The workspace's shape, the shared plumbing (`gh-common`) and the conformance harness are in the root `ARCHITECTURE.md`; this file is the functional spec of this tool: its conformance scenarios check it, and a change of behaviour updates it in the same pull request.

## What it is for

One place for the open issues that are yours to look at: those in repositories you own, and those you created, are assigned or commented on, wherever they live. It only reads GitHub.

It is early: today it lists them, as lines or as `--json`. The grouped view and its filters, the "needs you" reasons, and `hide`, `snooze` and `mute` are the next pull requests, and are written here as they land.

## Shape

A library crate plus a thin binary (`src/main.rs`: argv → `cli::run` → exit code), as gh-prs.

- **`issue.rs`** — `Issue`, `LastComment`, and `Role`: why an issue was found (`assignee`, `author`, `commenter`, `owner`), one per search.
- **`gh/`** — What gh-issues asks of GitHub, through `gh_common::gh`: `queries.rs` the GraphQL text (`ISSUE_FRAGMENT`), `parse.rs` the raw serde types and `to_issue()`, `fetch.rs` `fetch_issues()`.
- **`cli/`** — `args.rs` (clap definitions), `mod.rs` (the run and the exit codes), `render.rs` (the list and `--json`).

## Loading (paged id searches, then batched hydration)

`fetch_issues()` runs one search per role, in parallel, each `is:issue is:open archived:false sort:updated-desc <qualifier>:@me`, the qualifier being `assignee`, `author`, `commenter`, or `user` for the repositories you own. A search returns node ids only, `SEARCH_PAGE` (100) at a time, and is read page after page: unlike a list of pull requests to act on, a backlog is wanted whole, and one repository's alone can pass a page. GitHub serves the first 1000 matches of a search and no more; past that, `on_warning` hears how many matched and that the rest is missing. A match the token cannot read comes back as a null node and is skipped, which is neither a truncation nor the end of the search: only a missing next page, the 1000 cap, or a cursor that stops moving ends it. The ordering is live, so an issue updated while the pages are read can come back twice (it is kept once), and one closed meanwhile can shift another onto a page already read: a run is a snapshot, not a guarantee, and the next one shows it.

The union of the ids, each issue once however many searches matched it, is then looked up in batches of `HYDRATE_BATCH` (50). An issue node is light (no checks, no reviews), so the batch is larger than gh-prs's 15. An issue's roles are the searches that matched it. A node that came back null (deleted or transferred meanwhile; GitHub reports it as a `NOT_FOUND` error beside the nodes it did find, which `nodes_by_id()` reads as the null it is) or positively `CLOSED` is dropped, since the searches only match open issues; an unknown state stays. The result is sorted most recently updated first, with a stable sort so equal timestamps keep the search order.

## Parsing and unknowns

Same two layers as gh-prs (root `ARCHITECTURE.md`, Parsing GitHub responses): serde reads a node into raw types where every nullable field is an `Option`, a wrong JSON type fails the fetch, and `to_issue()` then reads the unknowns. `number`, `title` and `repository` are required. The comment count is `None` when GitHub didn't send it, never zero. A last comment whose author is gone is still a comment, with no author. Titles and label names are attacker-controlled and lose their control characters at ingestion. Labels are read up to 100 per issue, GitHub's page maximum, so the list is whole in practice.

## Error handling

"Error" must never look like "nothing to do": a failed search or lookup fails the whole run, with every failure listed, rather than showing a partial list. Errors go to stderr with exit code `1`; a usage error, from clap, exits `2`.

## `--json`

A scripting interface: the field names and their order are part of the contract. One object per issue: `number`, `repo`, `title`, `author`, `url`, `updated_at`, `created_at`, `labels`, `assignees`, `comments` (`null` when unknown), `last_comment` (`{author, created_at}` or `null`), `roles` (sorted).
