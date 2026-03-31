"""GitHub Pull Requests TUI."""

from __future__ import annotations

import re
from concurrent.futures import ThreadPoolExecutor, as_completed

from textual import work
from textual.app import App, ComposeResult
from textual.worker import Worker
from textual.binding import Binding
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, DataTable, Footer, Header, Input, Label, Static

from gh_prs.gh import (
    PullRequest,
    approve_pr,
    enrich_pr,
    fetch_prs,
    merge_pr,
    open_in_browser,
)

REVIEW_LABELS = {
    "APPROVED": "Approved",
    "CHANGES_REQUESTED": "Changes req",
    "REVIEW_REQUIRED": "Review req",
    "": "—",
}


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


class DetailScreen(ModalScreen[None]):
    """Shows full details of a pull request."""

    BINDINGS = [
        Binding("escape", "close", "Close"),
        Binding("q", "close", "Close"),
    ]

    def __init__(self, pr: PullRequest) -> None:
        super().__init__()
        self._pr = pr

    def compose(self) -> ComposeResult:
        p = self._pr
        review = REVIEW_LABELS.get(p.review_decision, p.review_decision)
        text = (
            f"[b]Title:[/b]   {p.title}\n"
            f"[b]Repo:[/b]    {p.repo}\n"
            f"[b]Number:[/b]  #{p.number}\n"
            f"[b]Author:[/b]  {p.author}\n"
            f"[b]Branch:[/b]  {p.head_ref}\n"
            f"[b]Draft:[/b]   {'Yes' if p.is_draft else 'No'}\n"
            f"[b]Review:[/b]  {review}\n"
            f"[b]Updated:[/b] {p.updated_at}\n"
            f"[b]Created:[/b] {p.created_at}\n"
            f"[b]URL:[/b]     {p.url}\n"
        )
        with Vertical(id="detail-dialog"):
            yield Static(text, markup=True)
            yield Label("[dim]Press Escape or q to close[/dim]")

    def action_close(self) -> None:
        self.dismiss(None)


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
    #confirm-dialog {
        align: center middle;
        width: 60;
        height: auto;
        max-height: 10;
        border: thick $warning;
        padding: 1 2;
        background: $surface;
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
        Binding("enter", "show_detail", "Details"),
        Binding("/", "filter", "Filter"),
        Binding("c", "clear_filter", "Clear filter"),
        Binding("g", "refresh_list", "Refresh"),
        Binding("s", "toggle_select", "Select"),
        Binding("a", "select_all", "Select all"),
        Binding("j", "cursor_down", "Down", show=False),
        Binding("k", "cursor_up", "Up", show=False),
        Binding("q", "quit", "Quit"),
    ]

    def __init__(self) -> None:
        super().__init__()
        self._prs: list[PullRequest] = []
        self._filtered: list[PullRequest] = []
        self._selected: set[str] = set()
        self._filter_text: str = ""
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
            " ", "#", "Repo", "Author", "Title", "Draft", "Review", "Updated"
        )
        self._load_prs()

    def _cancel_enrich(self) -> None:
        if self._enrich_worker and self._enrich_worker.is_running:
            self._enrich_worker.cancel()
            self._enrich_worker = None

    @work(thread=True)
    def _load_prs(self) -> None:
        self._update_status("Fetching pull requests...")
        try:
            prs = fetch_prs()
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
        with ThreadPoolExecutor(max_workers=4) as pool:
            futures = {pool.submit(enrich_pr, pr): pr for pr in prs}
            for future in as_completed(futures):
                try:
                    future.result()
                except Exception:
                    pass
                done += 1
                self._update_status(f"Loading details... {done}/{total}")
                self.call_from_thread(self._apply_filter_and_render)

    def _update_status(self, text: str) -> None:
        def _set() -> None:
            self.query_one("#status-bar", Static).update(text)

        self.call_from_thread(_set)

    def _apply_filter_and_render(self) -> None:
        if self._filter_text:
            try:
                pat = re.compile(self._filter_text, re.IGNORECASE)
            except re.error:
                self.query_one("#status-bar", Static).update(
                    f"Invalid regex: {self._filter_text}"
                )
                return
            self._filtered = [
                p
                for p in self._prs
                if pat.search(p.repo)
                or pat.search(p.title)
                or pat.search(p.author)
                or pat.search(p.head_ref)
            ]
        else:
            self._filtered = list(self._prs)

        table = self.query_one("#prs-table", DataTable)
        table.clear()
        for p in self._filtered:
            sel = "*" if p.id in self._selected else " "
            review = REVIEW_LABELS.get(p.review_decision, p.review_decision)
            table.add_row(
                sel,
                str(p.number),
                p.repo_short,
                p.author,
                p.title,
                "draft" if p.is_draft else "",
                review,
                p.updated_date,
                key=p.id,
            )

        total = len(self._prs)
        shown = len(self._filtered)
        selected = len(self._selected)
        filter_info = f"  filter: '{self._filter_text}'" if self._filter_text else ""
        sel_info = f"  selected: {selected}" if selected else ""
        self.query_one("#status-bar", Static).update(
            f"{shown}/{total} PRs{filter_info}{sel_info}"
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
        for i, pr in enumerate(prs, 1):
            self._update_status(f"Approving {pr.id} ({i}/{len(prs)})...")
            try:
                approve_pr(pr.repo, pr.number)
                self._update_status(f"Approved {pr.id}")
            except RuntimeError as e:
                errors.append(str(e))

        if errors:
            self._update_status(f"Errors: {'; '.join(errors)}")
        else:
            self._update_status(f"Approved {len(prs)} PR(s) — refreshing...")
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

    def action_show_detail(self) -> None:
        pr = self._get_current_pr()
        if pr:
            self.push_screen(DetailScreen(pr))

    def action_filter(self) -> None:
        def on_dismiss(value: str | None) -> None:
            if value is not None:
                self._filter_text = value
                self._apply_filter_and_render()

        self.push_screen(FilterInput(self._filter_text), callback=on_dismiss)

    def action_clear_filter(self) -> None:
        self._filter_text = ""
        self._apply_filter_and_render()

    def action_refresh_list(self) -> None:
        self._selected.clear()
        self._load_prs()


def main() -> None:
    app = PullRequestsApp()
    app.run()


if __name__ == "__main__":
    main()
