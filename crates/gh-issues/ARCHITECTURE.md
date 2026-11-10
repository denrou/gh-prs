# Architecture of gh-issues

How gh-issues is put together and what it promises. The workspace's shape, the shared plumbing (`gh-common`) and the conformance harness are in the root `ARCHITECTURE.md`; this file is the functional spec of this tool: its conformance scenarios check it, and a change of behaviour updates it in the same pull request.

## What it is for

One place for the open issues that are yours to look at: those in repositories you own, and those you created, are assigned or commented on, wherever they live. It only reads GitHub.

It shows what needs you first (a reply, a triage, an assignment), then the whole backlog grouped by repository, with filters, `--json`, and `--count` for a status bar. `hide`, `snooze` and `mute` are the next pull request, and are written here as they land.

## Shape

A library crate plus a thin binary (`src/main.rs`: argv → `cli::run` → exit code), as gh-prs.

- **`issue.rs`** — `Issue`, `LastComment`, `Role`: why an issue was found (`assignee`, `author`, `commenter`, `owner`), one per search, and `Reason`: why it needs you.
- **`gh/`** — What gh-issues asks of GitHub, through `gh_common::gh`: `queries.rs` the GraphQL text (`ISSUE_FRAGMENT`), `parse.rs` the raw serde types and `to_issue()`, `fetch.rs` `fetch_issues()`.
- **`attention.rs`** — Pure attention policy: `attention_reasons()`. No `gh` calls.
- **`filter.rs`** — Pure: `Filter`, what the filter flags ask for, and `keeps()`.
- **`cli/`** — `args.rs` (clap definitions), `mod.rs` (the run and the exit codes), `render.rs` (the tables and `--json`).

## Loading (paged id searches, then batched hydration)

`fetch_issues()` runs one search per role, in parallel, each `is:issue is:open archived:false sort:updated-desc <qualifier>:@me`, the qualifier being `assignee`, `author`, `commenter`, or `user` for the repositories you own. A search returns node ids only, `SEARCH_PAGE` (100) at a time, and is read page after page: unlike a list of pull requests to act on, a backlog is wanted whole, and one repository's alone can pass a page. GitHub serves the first 1000 matches of a search and no more; past that, `on_warning` hears how many matched and that the rest is missing. A match the token cannot read comes back as a null node and is skipped, which is neither a truncation nor the end of the search: only a missing next page, the 1000 cap, or a cursor that stops moving ends it. The ordering is live, so an issue updated while the pages are read can come back twice (it is kept once), and one closed meanwhile can shift another onto a page already read: a run is a snapshot, not a guarantee, and the next one shows it.

The union of the ids, each issue once however many searches matched it, is then looked up in batches of `HYDRATE_BATCH` (50). An issue node is light (no checks, no reviews), so the batch is larger than gh-prs's 15. An issue's roles are the searches that matched it. A node that came back null (deleted or transferred meanwhile; GitHub reports it as a `NOT_FOUND` error beside the nodes it did find, which `nodes_by_id()` reads as the null it is) or positively `CLOSED` is dropped, since the searches only match open issues; an unknown state stays. The result is sorted most recently updated first, with a stable sort so equal timestamps keep the search order.

## Parsing and unknowns

Same two layers as gh-prs (root `ARCHITECTURE.md`, Parsing GitHub responses): serde reads a node into raw types where every nullable field is an `Option`, a wrong JSON type fails the fetch, and `to_issue()` then reads the unknowns. `number`, `title` and `repository` are required. The comment count is `None` when GitHub didn't send it, never zero. A last comment whose author is gone is still a comment, with no author. Titles and label names are attacker-controlled and lose their control characters at ingestion. Labels are read up to 100 per issue, GitHub's page maximum, so the list is whole in practice.

## Error handling

"Error" must never look like "nothing to do": a failed search or lookup fails the whole run, with every failure listed, rather than showing a partial list. Errors go to stderr with exit code `1`; a usage error, from clap, exits `2`.

## Attention logic (`attention.rs`)

The question is whose move it is. An issue needs you when any of these hold:

- **reply** — someone else spoke last in a thread you have a say in: you opened it, commented in it, or it is assigned to you. Your own last word calls for nothing: the ball is with them.
- **triage** — it lives in a repository you own, someone else opened it, and you have not touched it: not a comment, not an assignment to yourself. Their own follow-up comments do not make it a **reply**, since you never spoke; answering it is the triage, after which it is a thread you have a say in.
- **assigned** — it is assigned to you and neither of the above fired: standing work, listed once.

Your own backlog (issues you opened in your repositories, with nobody answering) needs nothing, which is most of it.

Unknowns read as someone else's word, yours to answer: a last commenter or an author whose account is gone is not you. An issue wrongly shown costs a glance; one wrongly withheld is a person left without an answer. Authorship is read from the issue's author as well as from the `author` search, so an issue of yours beyond the 1000 matches that search serves is still yours. Logins are compared without case.

The viewer's login comes with the searches. Without it nobody's turn can be told, so the fetch fails rather than guess.

Not covered: an issue that only mentions you (there is no search for it yet), and bots, whose comments count as anyone's until the mute rules exist.

## The view

By default, the "needs you" sections come first, when there are any: `Waiting for your reply`, `New in your repositories`, `Assigned to you`, each a table of repository (its bare name), number, title, who is waiting (the last commenter, else the author, `—` when gone) and the date of the last update. Then the whole backlog, those issues included.

`-a`/`--attention` shows the sections alone, and `✓ Nothing needs you.` when there are none. `--count` prints only the number of issues that need you, unstyled, for a status bar; on a failure it prints no number and exits `1`, so an error never reads as zero. `--count` with `--json` is a usage error.

The backlog is one table per repository, headed by its full name and its number of issues. Repositories come in the order of their most recently updated issue, and so do the issues within one: what moved last is on top. A row is the number (a link to the issue), the title, the labels and the date of the last update (`—` when GitHub didn't send it). A table where no issue carries a label has no labels column.

The title is cut to 80 columns. On a screen too narrow for the table, the title shrinks to 20 columns, then the labels to an ellipsis, then the title again; the number and the date never shrink. Column spacing and where a cut falls are not part of the contract.

With nothing to show, the view says `No open issues.`, or `No matching issues.` when there are issues and the filters kept none.

## Filters (`filter.rs`, applied in `cli/`)

Every filter given must hold:

- `-c`/`--created`: you are the author.
- `-o`/`--owned`: it lives in a repository you own.
- `-R`/`--repo`: it lives in that repository. `owner/name` is matched whole and a bare name against the name alone, whoever the owner. Repeated, any of them. Anything else (a URL, a trailing slash) is a usage error, since it would match nothing and read as "no issue there".
- `-l`/`--label`: it carries that label. Repeated, all of them, as `gh issue list` does.

Repository and label names are compared without case, as GitHub does. The filters also narrow `--attention` and `--count`. They are applied after the fetch, which is the same whatever the flags: an issue's `roles` in `--json` never depend on them (narrowing the searches themselves is issue #35). They apply to `--json` too.

## `--json`

A scripting interface: the field names and their order are part of the contract. One object per issue: `number`, `repo`, `title`, `author`, `url`, `updated_at`, `created_at`, `labels`, `assignees`, `comments` (`null` when unknown), `last_comment` (`{author, created_at}` or `null`), `roles` (sorted), `attention` (the reasons, sorted; empty when it doesn't need you). With `--attention`, only the issues that need you.
