"""GitHub Pull Requests TUI."""

from __future__ import annotations

import re
from concurrent.futures import ThreadPoolExecutor, as_completed

from rich.text import Text as RichText
from textual import work
from textual.app import App, ComposeResult
from textual.worker import Worker
from textual.binding import Binding
from textual.containers import Horizontal, ScrollableContainer, Vertical
from textual.screen import ModalScreen, Screen
from textual.widgets import (
    Button,
    DataTable,
    Footer,
    Header,
    Input,
    Label,
    ListItem,
    ListView,
    Static,
)

from gh_prs.gh import (
    PullRequest,
    approve_pr,
    enrich_pr,
    fetch_pr_body,
    fetch_pr_diff,
    fetch_prs,
    get_current_user,
    merge_pr,
    open_in_browser,
    parse_diff,
)

REVIEW_LABELS = {
    "APPROVED": "Approved",
    "CHANGES_REQUESTED": "Changes req",
    "REVIEW_REQUIRED": "Review req",
    "": "—",
}

# Ordered list of (qualifier, display_label) for role filters.
# Key bindings 1-5 map positionally to this list.
ROLE_FILTERS = [
    ("", "All"),
    ("author", "Author"),
    ("review-requested", "Review Req"),
    ("assignee", "Assigned"),
    ("involves", "Participant"),
]


class FilterInput(ModalScreen[str | None]):
    """Modal for entering a filter string."""

    BINDINGS = [Binding("escape", "cancel", "Cancel")]

    def __init__(self, current_filter: str = "") -> None:
        super().__init__()
        self._current = current_filter

    def compose(self) -> ComposeResult:
        with Vertical(id="filter-dialog"):
            yield Label("Filter PRs (regex on repo, title, author, branch):")
            yield Input(
                value=self._current,
                placeholder="e.g. centreon|feat",
                id="filter-input",
            )

    def on_mount(self) -> None:
        self.query_one("#filter-input", Input).focus()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self.dismiss(event.value)

    def action_cancel(self) -> None:
        self.dismiss(None)


class _MenuItem(ListItem):
    """A ListView item carrying a semantic kind and optional value."""

    def __init__(self, label: str, kind: str, value: str = "") -> None:
        super().__init__(Label(label))
        self.kind = kind
        self.value = value


class PRDetailScreen(Screen[str | None]):
    """Full-screen PR detail: content on the left, navigation menu on the right."""

    BINDINGS = [
        Binding("escape", "back", "Back"),
        Binding("j", "menu_down", "Down", show=False),
        Binding("k", "menu_up", "Up", show=False),
    ]

    def __init__(self, pr: PullRequest, current_user: str = "") -> None:
        super().__init__()
        self._pr = pr
        self._current_user = current_user
        self._body: str = ""
        self._file_diffs: list[tuple[str, str]] = []
        self._current_kind: str = "overview"
        self._current_value: str = ""

    def compose(self) -> ComposeResult:
        yield Header()
        with Horizontal(id="detail-layout"):
            with ScrollableContainer(id="detail-content"):
                yield Static("", id="detail-body", markup=False)
            yield ListView(id="detail-menu")
        yield Footer()

    def on_mount(self) -> None:
        self._rebuild_menu(loading=True)
        self._show_overview()
        self.query_one("#detail-menu", ListView).focus()
        self._fetch_detail()

    def _rebuild_menu(self, *, loading: bool = False) -> None:
        menu = self.query_one("#detail-menu", ListView)
        menu.clear()
        menu.append(_MenuItem("Overview", kind="overview"))
        if self._file_diffs:
            menu.append(_MenuItem("─" * 26, kind="separator"))
            for filename, _ in self._file_diffs:
                menu.append(
                    _MenuItem(filename.split("/")[-1], kind="file", value=filename)
                )
        elif loading:
            menu.append(_MenuItem("  loading diffs…", kind="separator"))
        menu.append(_MenuItem("─" * 26, kind="separator"))
        menu.append(_MenuItem("Approve", kind="approve"))
        menu.append(_MenuItem("Merge", kind="merge"))
        for item in menu.query(_MenuItem):
            if item.kind == "separator":
                item.disabled = True

    @work(thread=True)
    def _fetch_detail(self) -> None:
        pr = self._pr
        with ThreadPoolExecutor(max_workers=2) as pool:
            body_f = pool.submit(fetch_pr_body, pr)
            diff_f = pool.submit(fetch_pr_diff, pr)
            body = body_f.result()
            diff_text = diff_f.result()
        file_diffs = parse_diff(diff_text)

        def _update() -> None:
            self._body = body
            self._file_diffs = file_diffs
            self._rebuild_menu(loading=False)
            if self._current_kind == "overview":
                self._show_overview()

        self.app.call_from_thread(_update)

    def _show_overview(self) -> None:
        p = self._pr
        review = REVIEW_LABELS.get(p.review_decision, p.review_decision)
        text = RichText()
        text.append(f"Title:    {p.title}\n", style="bold")
        text.append(f"Repo:     {p.repo}\n")
        text.append(f"Number:   #{p.number}\n")
        text.append(f"Author:   {p.author}\n")
        text.append(f"Branch:   {p.head_ref or '—'}\n")
        text.append(f"Draft:    {'Yes' if p.is_draft else 'No'}\n")
        text.append(f"Review:   {review}\n")
        text.append(f"Created:  {p.created_date}\n")
        text.append(f"Updated:  {p.updated_date}\n")
        text.append(f"URL:      {p.url}\n")
        if self._body:
            text.append("\n" + "─" * 60 + "\n", style="dim")
            text.append("\n" + self._body + "\n")
        self.query_one("#detail-body", Static).update(text)

    def _show_file_diff(self, filename: str) -> None:
        chunk = next((c for f, c in self._file_diffs if f == filename), "")
        text = RichText(no_wrap=False)
        text.append(filename + "\n\n", style="bold")
        for line in chunk.splitlines():
            if line.startswith("+") and not line.startswith("+++"):
                text.append(line + "\n", style="green")
            elif line.startswith("-") and not line.startswith("---"):
                text.append(line + "\n", style="red")
            elif line.startswith("@@"):
                text.append(line + "\n", style="cyan")
            else:
                text.append(line + "\n")
        self.query_one("#detail-body", Static).update(text)

    def _show_content(self, kind: str, value: str = "") -> None:
        self._current_kind = kind
        self._current_value = value
        self.query_one("#detail-content", ScrollableContainer).scroll_home(
            animate=False
        )
        if kind == "overview":
            self._show_overview()
        elif kind == "file":
            self._show_file_diff(value)

    def on_list_view_highlighted(self, event: ListView.Highlighted) -> None:
        if event.item is None or not isinstance(event.item, _MenuItem):
            return
        if event.item.kind in ("overview", "file"):
            self._show_content(event.item.kind, event.item.value)

    def on_list_view_selected(self, event: ListView.Selected) -> None:
        if not isinstance(event.item, _MenuItem):
            return
        if event.item.kind == "approve":
            self._do_approve()
        elif event.item.kind == "merge":
            self._trigger_merge()

    @work(thread=True)
    def _do_approve(self) -> None:
        pr = self._pr
        try:
            approve_pr(pr.repo, pr.number)
            pr.review_decision = "APPROVED"
            pr._attention = False
            self.app.call_from_thread(
                lambda: self.notify("Approved", severity="information")
            )
            if self._current_kind == "overview":
                self.app.call_from_thread(self._show_overview)
        except RuntimeError as e:
            msg = str(e)
            self.app.call_from_thread(lambda: self.notify(msg, severity="error"))

    def _trigger_merge(self) -> None:
        msg = f"Squash-merge and delete branch for:\n[b]{self._pr.id}[/b]\n\nAre you sure?"

        def on_confirm(confirmed: bool) -> None:
            if confirmed:
                self._do_merge()

        self.push_screen(ConfirmScreen(msg), callback=on_confirm)

    @work(thread=True)
    def _do_merge(self) -> None:
        pr = self._pr
        try:
            merge_pr(pr.repo, pr.number)
            self.app.call_from_thread(lambda: self.dismiss("merged"))
        except RuntimeError as e:
            msg = str(e)
            self.app.call_from_thread(lambda: self.notify(msg, severity="error"))

    def action_back(self) -> None:
        self.dismiss(None)

    def action_menu_down(self) -> None:
        self.query_one("#detail-menu", ListView).action_cursor_down()

    def action_menu_up(self) -> None:
        self.query_one("#detail-menu", ListView).action_cursor_up()


class ConfirmScreen(ModalScreen[bool]):
    """Confirmation dialog for destructive actions."""

    BINDINGS = [
        Binding("escape", "cancel", "Cancel"),
        Binding("n", "cancel", "No"),
    ]

    def __init__(self, message: str) -> None:
        super().__init__()
        self._message = message

    def compose(self) -> ComposeResult:
        with Vertical(id="confirm-dialog"):
            yield Static(self._message, markup=True)
            with Horizontal(id="confirm-buttons"):
                yield Button("Yes", variant="error", id="btn-yes")
                yield Button("No", variant="primary", id="btn-no")

    def on_button_pressed(self, event: Button.Pressed) -> None:
        self.dismiss(event.button.id == "btn-yes")

    def action_cancel(self) -> None:
        self.dismiss(False)


class PullRequestsApp(App[None]):
    """GitHub Pull Requests TUI."""

    TITLE = "GitHub Pull Requests"

    CSS = """
    Screen {
        background: $surface;
    }
    #detail-layout {
        height: 1fr;
    }
    #detail-content {
        width: 3fr;
        min-width: 40;
        padding: 0 1;
    }
    #detail-menu {
        width: 1fr;
        min-width: 30;
        border-left: thick $accent;
    }
    #status-bar {
        height: 1;
        dock: bottom;
        margin-bottom: 1;
        padding: 0 1;
        background: $primary-background;
        color: $text;
    }
    #filter-dialog {
        align: center middle;
        width: 60;
        height: auto;
        max-height: 8;
        border: thick $accent;
        padding: 1 2;
        background: $surface;
    }
    #detail-dialog {
        align: center middle;
        width: 80;
        height: auto;
        max-height: 20;
        border: thick $accent;
        padding: 1 2;
        background: $surface;
    }
    ConfirmScreen {
        align: center middle;
    }
    #confirm-dialog {
        width: 60;
        height: auto;
        max-height: 12;
        border: thick $warning;
        padding: 1 2;
        background: $boost;
        color: $text;
    }
    #confirm-dialog Static {
        width: 100%;
        color: $text;
        text-style: bold;
        margin-bottom: 1;
    }
    #confirm-buttons {
        margin-top: 1;
        align: center middle;
        height: 3;
    }
    #confirm-buttons Button {
        margin: 0 1;
    }
    """

    BINDINGS = [
        Binding("A", "approve", "Approve"),
        Binding("M", "merge", "Merge (squash)"),
        Binding("o", "open_browser", "Open in browser"),
        Binding("enter", "show_detail", "Details", show=False),
        Binding("/", "filter", "Filter"),
        Binding("c", "clear_filter", "Clear filter"),
        Binding("g", "refresh_list", "Refresh"),
        Binding("s", "toggle_select", "Select"),
        Binding("a", "select_all", "Select all"),
        Binding("j", "cursor_down", "Down", show=False),
        Binding("k", "cursor_up", "Up", show=False),
        Binding("1", "set_role('')", "All", show=False),
        Binding("2", "set_role('author')", "Author", show=False),
        Binding("3", "set_role('review-requested')", "Review Req", show=False),
        Binding("4", "set_role('assignee')", "Assigned", show=False),
        Binding("5", "set_role('involves')", "Participant", show=False),
        Binding("q", "quit", "Quit"),
    ]

    def __init__(self) -> None:
        super().__init__()
        self._prs: list[PullRequest] = []
        self._filtered: list[PullRequest] = []
        self._selected: set[str] = set()
        self._filter_text: str = ""
        self._role_filter: str = ""
        self._current_user: str = ""
        self._enrich_worker: Worker[None] | None = None

    def compose(self) -> ComposeResult:
        yield Header()
        yield DataTable(id="prs-table")
        yield Static("Loading...", id="status-bar")
        yield Footer()

    def on_mount(self) -> None:
        table = self.query_one("#prs-table", DataTable)
        table.cursor_type = "row"
        table.zebra_stripes = True
        table.add_columns(
            " ",
            "!",
            "#",
            "Role",
            "Repo",
            "Author",
            "Title",
            "Draft",
            "Review",
            "Created",
            "Updated",
        )
        self._load_prs()

    def _cancel_enrich(self) -> None:
        if self._enrich_worker and self._enrich_worker.is_running:
            self._enrich_worker.cancel()
            self._enrich_worker = None

    @work(thread=True)
    def _load_prs(self) -> None:
        self._update_status("Fetching pull requests...")
        # Fetch current user and PRs in parallel — they are independent.
        with ThreadPoolExecutor(max_workers=2) as boot:
            user_future = (
                boot.submit(get_current_user) if not self._current_user else None
            )
            prs_future = boot.submit(fetch_prs)
            if user_future is not None:
                self._current_user = user_future.result()
            try:
                prs = prs_future.result()
            except RuntimeError as e:
                self._update_status(str(e))
                return

        def _set_and_render() -> None:
            self._prs = prs
            self._apply_filter_and_render()

        self.call_from_thread(self._cancel_enrich)
        self.call_from_thread(_set_and_render)
        self._start_enrich()

    def _start_enrich(self) -> None:
        """Launch enrichment, cancelling any previous run."""
        self.call_from_thread(self._cancel_enrich)
        self._enrich_worker = self._enrich_prs()

    @work(thread=True)
    def _enrich_prs(self) -> None:
        """Fetch branch/review details for each PR concurrently."""
        prs = list(self._prs)
        total = len(prs)
        done = 0
        current_user = self._current_user
        # Re-render every BATCH completions instead of after every single PR,
        # turning O(n²) table redraws into O(n²/BATCH).
        BATCH = 10
        with ThreadPoolExecutor(max_workers=8) as pool:
            futures = {pool.submit(enrich_pr, pr, current_user): pr for pr in prs}
            for future in as_completed(futures):
                try:
                    future.result()
                except Exception:
                    pass
                done += 1
                self._update_status(f"Loading details... {done}/{total}")
                if done % BATCH == 0 or done == total:
                    self.call_from_thread(self._apply_filter_and_render)

    def _update_status(self, text: str) -> None:
        def _set() -> None:
            self.query_one("#status-bar", Static).update(text)

        self.call_from_thread(_set)

    def _role_abbrev(self, pr: PullRequest) -> str:
        """Single-char abbreviations for each role a PR matches."""
        parts = []
        if "author" in pr.roles:
            parts.append("A")
        if "review-requested" in pr.roles:
            parts.append("R")
        if "assignee" in pr.roles:
            parts.append("a")
        if "involves" in pr.roles and not pr.roles & {
            "author",
            "review-requested",
            "assignee",
        }:
            parts.append("P")
        return "".join(parts) or "·"

    def _apply_filter_and_render(self) -> None:
        candidates = self._prs

        # Role filter
        if self._role_filter:
            candidates = [p for p in candidates if self._role_filter in p.roles]

        # Text filter
        if self._filter_text:
            try:
                pat = re.compile(self._filter_text, re.IGNORECASE)
            except re.error:
                self.query_one("#status-bar", Static).update(
                    f"Invalid regex: {self._filter_text}"
                )
                return
            candidates = [
                p
                for p in candidates
                if pat.search(p.repo)
                or pat.search(p.title)
                or pat.search(p.author)
                or pat.search(p.head_ref)
            ]

        self._filtered = candidates

        table = self.query_one("#prs-table", DataTable)
        table.clear()
        for p in self._filtered:
            sel = "*" if p.id in self._selected else " "
            attn = "*" if p.needs_attention() else " "
            review = REVIEW_LABELS.get(p.review_decision, p.review_decision)
            table.add_row(
                sel,
                attn,
                str(p.number),
                self._role_abbrev(p),
                p.repo_short,
                p.author,
                p.title,
                "draft" if p.is_draft else "",
                review,
                p.created_date,
                p.updated_date,
                key=p.id,
            )

        total = len(self._prs)
        shown = len(self._filtered)
        selected = len(self._selected)
        role_label = next(
            (lbl for q, lbl in ROLE_FILTERS if q == self._role_filter), "All"
        )
        role_info = f"  role: {role_label}" if self._role_filter else ""
        filter_info = f"  filter: '{self._filter_text}'" if self._filter_text else ""
        sel_info = f"  selected: {selected}" if selected else ""
        self.query_one("#status-bar", Static).update(
            f"{shown}/{total} PRs{role_info}{filter_info}{sel_info}  [1-5: role filter]"
        )

    def action_cursor_down(self) -> None:
        self.query_one("#prs-table", DataTable).action_cursor_down()

    def action_cursor_up(self) -> None:
        self.query_one("#prs-table", DataTable).action_cursor_up()

    def _get_current_pr(self) -> PullRequest | None:
        table = self.query_one("#prs-table", DataTable)
        if table.row_count == 0:
            return None
        row_key, _ = table.coordinate_to_cell_key(table.cursor_coordinate)
        for p in self._filtered:
            if p.id == row_key.value:
                return p
        return None

    def _get_target_prs(self) -> list[PullRequest]:
        """Return selected PRs if any, otherwise the current row's PR."""
        if self._selected:
            return [p for p in self._filtered if p.id in self._selected]
        pr = self._get_current_pr()
        return [pr] if pr else []

    def action_toggle_select(self) -> None:
        pr = self._get_current_pr()
        if not pr:
            return
        if pr.id in self._selected:
            self._selected.discard(pr.id)
        else:
            self._selected.add(pr.id)
        self._apply_filter_and_render()

    def action_select_all(self) -> None:
        if len(self._selected) == len(self._filtered):
            self._selected.clear()
        else:
            self._selected = {p.id for p in self._filtered}
        self._apply_filter_and_render()

    def action_approve(self) -> None:
        targets = self._get_target_prs()
        if targets:
            self._do_approve(targets)

    @work(thread=True)
    def _do_approve(self, prs: list[PullRequest]) -> None:
        errors = []
        approved_ids: set[str] = set()
        for i, pr in enumerate(prs, 1):
            self._update_status(f"Approving {pr.id} ({i}/{len(prs)})...")
            try:
                approve_pr(pr.repo, pr.number)
                approved_ids.add(pr.id)
                self._update_status(f"Approved {pr.id}")
            except RuntimeError as e:
                errors.append(str(e))

        if errors:
            self._update_status(f"Errors: {'; '.join(errors)}")
        else:
            self._update_status(f"Approved {len(prs)} PR(s) — refreshing...")

        # Optimistically mark approved PRs so the UI doesn't show stale
        # review status while waiting for GitHub's API to propagate.
        def _mark_approved() -> None:
            for pr in self._prs:
                if pr.id in approved_ids:
                    pr.review_decision = "APPROVED"
            self._apply_filter_and_render()

        self.call_from_thread(_mark_approved)
        self._refresh_after_action()

    def action_merge(self) -> None:
        targets = self._get_target_prs()
        if not targets:
            return
        names = ", ".join(f"[b]{p.id}[/b]" for p in targets)
        msg = f"Squash-merge and delete branch for:\n{names}\n\nAre you sure?"

        def on_confirm(confirmed: bool) -> None:
            if confirmed:
                self._do_merge(targets)

        self.push_screen(ConfirmScreen(msg), callback=on_confirm)

    @work(thread=True)
    def _do_merge(self, prs: list[PullRequest]) -> None:
        errors = []
        for i, pr in enumerate(prs, 1):
            self._update_status(f"Merging {pr.id} ({i}/{len(prs)})...")
            try:
                merge_pr(pr.repo, pr.number)
                self._update_status(f"Merged {pr.id}")
            except RuntimeError as e:
                errors.append(str(e))

        if errors:
            self._update_status(f"Errors: {'; '.join(errors)}")
        else:
            self._update_status(f"Merged {len(prs)} PR(s) — refreshing...")
        self._refresh_after_action()

    def _refresh_after_action(self) -> None:
        """Re-fetch PRs after approve/merge, thread-safe."""
        try:
            new_prs = fetch_prs()
        except RuntimeError as e:
            self._update_status(f"Refresh failed: {e}")
            return

        def _update() -> None:
            self._selected.clear()
            self._prs = new_prs
            self._apply_filter_and_render()

        self.call_from_thread(_update)
        self._start_enrich()

    def action_open_browser(self) -> None:
        pr = self._get_current_pr()
        if pr:
            open_in_browser(pr)

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        event.stop()
        self.action_show_detail()

    def action_show_detail(self) -> None:
        pr = self._get_current_pr()
        if not pr:
            return

        def on_dismissed(result: str | None) -> None:
            if result == "merged":
                self._selected.discard(pr.id)
                self._load_prs()

        self.push_screen(PRDetailScreen(pr, self._current_user), callback=on_dismissed)

    def action_filter(self) -> None:
        def on_dismiss(value: str | None) -> None:
            if value is not None:
                self._filter_text = value
                self._apply_filter_and_render()

        self.push_screen(FilterInput(self._filter_text), callback=on_dismiss)

    def action_clear_filter(self) -> None:
        self._filter_text = ""
        self._apply_filter_and_render()

    def action_set_role(self, role: str) -> None:
        self._role_filter = role
        self._apply_filter_and_render()

    def action_refresh_list(self) -> None:
        self._selected.clear()
        self._load_prs()


def main() -> None:
    app = PullRequestsApp()
    app.run()


if __name__ == "__main__":
    main()
