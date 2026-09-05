#!/usr/bin/env python3
"""Review Jester tracks interactively with Textual; save proposals, never the database.

Run: python3 scripts/preview_jester_taxonomy.py [review.sql] [--all]
Requires Textual (the terminal application framework built on Rich).
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass, replace
from importlib.metadata import version
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SQL = ROOT / "data/jester-taxonomy-review.sql"
DEFAULT_DB = ROOT / "data/jester.sqlite3"
INSERT_PREFIX = "INSERT INTO proposed_taxonomy VALUES\n"
INSERT_END = ";\n\n-- The source manifest"
LITERAL = r"(?:'(?:[^']|'')*'|NULL)"
ROW_PATTERN = re.compile(r"^\(" + r",\s*".join([f"({LITERAL})"] * 8) + r"\)", re.M)


def taxonomy_values() -> dict[str, list[str]]:
    source = (ROOT / "src/jester/db/taxonomy.rs").read_text()
    return {
        name.lower(): re.findall(r'"([a-z]+)"', source.split(f"pub const {name}:", 1)[1].split("];", 1)[0])
        for name in ("MOODS", "INTENSITIES", "FUNCTIONS", "TEXTURES", "ENVIRONMENTS")
    }


@dataclass(frozen=True)
class Track:
    title: str
    artist: str
    origin: str
    mood: str | None
    intensity: str | None
    function_tag: str | None
    textures: str
    environments: str

    @property
    def key(self) -> tuple[str, str, str]:
        return self.title, self.artist, self.origin

    @property
    def classified(self) -> bool:
        return self.mood is not None and self.intensity is not None

    def values(self) -> tuple[str | None, ...]:
        return (self.title, self.artist, self.origin, self.mood, self.intensity,
                self.function_tag, self.textures, self.environments)


def parse_rows(sql: str) -> list[tuple[Track, re.Match]]:
    try:
        start = sql.index(INSERT_PREFIX) + len(INSERT_PREFIX)
        end = sql.index(INSERT_END, start)
    except ValueError as error:
        raise ValueError("Cannot find the proposed_taxonomy INSERT block.") from error
    matches = list(ROW_PATTERN.finditer(sql, start, end))
    tracks = [Track(*(None if value == "NULL" else value[1:-1].replace("''", "'")
                      for value in match.groups())) for match in matches]
    # Parse only this single INSERT in memory, and ensure our editable spans cover it.
    # Never execute the review's UPDATE/DELETE statements or open the live DB for writes.
    with sqlite3.connect(":memory:") as connection:
        connection.execute("CREATE TABLE proposed_taxonomy (title, artist, origin, mood, intensity, function_tag, textures, environments)")
        connection.execute(INSERT_PREFIX + sql[start:end] + ";")
        parsed = connection.execute("SELECT * FROM proposed_taxonomy").fetchall()
    if parsed != [t.values() for t in tracks] or len({t.key for t in tracks}) != len(tracks):
        raise ValueError("Unsupported or duplicate proposal rows; refusing to rewrite this file.")
    return list(zip(tracks, matches))


def load_proposals(sql_path: Path) -> list[tuple[str | None, ...]]:
    """Compatibility helper for scripts that used the old static preview."""
    return [t.values() for t, _ in sorted(parse_rows(sql_path.read_text()), key=lambda pair: pair[0].key)]


class ReviewStore:
    def __init__(self, path: Path):
        self.path = path.resolve()
        self.text = self.path.read_text()
        self.allowed = taxonomy_values()
        self.tracks = {t.key: t for t, _ in parse_rows(self.text)}

    def note(self, key: tuple[str, str, str]) -> str:
        for track, match in parse_rows(self.text):
            if track.key == key:
                previous = self.text[:match.start()].rstrip().splitlines()[-1]
                return previous.removeprefix("-- ") if previous.startswith("-- ") else ""
        return ""

    def validate(self, track: Track) -> None:
        for value, category in [(track.mood, "moods"), (track.intensity, "intensities")]:
            if value not in self.allowed[category]:
                raise ValueError("Choose both a mood and an intensity before saving.")
        if track.function_tag is not None and track.function_tag not in self.allowed["functions"]:
            raise ValueError("Invalid scene function.")
        for values, category in [(track.textures, "textures"), (track.environments, "environments")]:
            items = values.split("|") if values else []
            if len(items) != len(set(items)) or not set(items) <= set(self.allowed[category]):
                raise ValueError(f"Invalid {category}.")

    def save(self, track: Track) -> None:
        self.validate(track)
        if self.path.read_text() != self.text:
            raise ValueError("The SQL changed outside this editor. Restart to load it; nothing was overwritten.")
        if track.key not in self.tracks:
            raise ValueError("Track is not in this review.")
        match = next(m for t, m in parse_rows(self.text) if t.key == track.key)
        def quote(value: str | None) -> str:
            return "NULL" if value is None else "'" + value.replace("'", "''") + "'"
        row = "(" + ", ".join(map(quote, track.values())) + ")"
        start = match.start()
        # Replace the decision comment too: a completed row must not say NEEDS LISTENING.
        comment_start = self.text.rfind("\n", 0, start - 1) + 1
        if self.text[comment_start:start].startswith("-- "):
            start = comment_start
        updated = self.text[:start] + "-- USER CLASSIFIED: saved with the interactive taxonomy editor.\n" + row + self.text[match.end():]
        tracks = [t for t, _ in parse_rows(updated)]
        complete = sum(t.classified for t in tracks)
        updated = re.sub(r"^-- \d+ definitions have a proposal; \d+ explicitly abstain and need listening\.$",
                         f"-- {complete} definitions have a proposal; {len(tracks)-complete} explicitly abstain and need listening.", updated, flags=re.M)
        if "-- Interactive edits:" not in updated:
            updated = "-- Interactive edits: USER CLASSIFIED rows supersede the original Markdown review notes.\n" + updated
        # Exclusive backup of the initial file, then atomic replacement of the working SQL.
        backup = self.path.with_name(self.path.name + ".bak")
        try:
            with backup.open("x") as output:
                output.write(self.text)
        except FileExistsError:
            pass
        temp_name = None
        try:
            with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=self.path.parent, delete=False) as output:
                temp_name = output.name
                output.write(updated)
                output.flush()
                os.fsync(output.fileno())
            os.chmod(temp_name, self.path.stat().st_mode & 0o777)
            if self.path.read_text() != self.text:
                raise ValueError("The SQL changed during saving; nothing was overwritten.")
            os.replace(temp_name, self.path)
        finally:
            if temp_name and os.path.exists(temp_name):
                os.unlink(temp_name)
        self.text = updated
        self.tracks[track.key] = track


def recording_links(db_path: Path) -> dict[tuple[str, str, str], list[str]]:
    result: dict[tuple[str, str, str], list[str]] = {}
    if not db_path.exists():
        return result
    with sqlite3.connect(db_path.resolve().as_uri() + "?mode=ro", uri=True) as connection:
        for title, artist, origin, video_id in connection.execute(
            "SELECT t.track_title, a.artist, o.origin, t.id FROM tracks t "
            "JOIN artists a ON a.id=t.artist_id JOIN origins o ON o.id=t.origin_id"
        ):
            if re.fullmatch(r"[A-Za-z0-9_-]{11}", video_id):
                result.setdefault((title, artist, origin), []).append("https://www.youtube.com/watch?v=" + video_id)
    return result


def recording_ids(db_path: Path) -> dict[tuple[str, str, str], list[str]]:
    result: dict[tuple[str, str, str], list[str]] = {}
    if not db_path.exists():
        return result
    with sqlite3.connect(db_path.resolve().as_uri() + "?mode=ro", uri=True) as connection:
        for title, artist, origin, video_id in connection.execute(
            "SELECT t.track_title, a.artist, o.origin, t.id FROM tracks t "
            "JOIN artists a ON a.id=t.artist_id JOIN origins o ON o.id=t.origin_id"
        ):
            if re.fullmatch(r"[A-Za-z0-9_-]{11}", video_id):
                result.setdefault((title, artist, origin), []).append(video_id)
    return result


class LocalPlayer:
    def __init__(self, audio_dir: Path):
        self.audio_dir = audio_dir
        self.process: subprocess.Popen[bytes] | None = None

    def stop(self) -> None:
        process = self.process
        self.process = None
        if process is None or process.poll() is not None:
            return
        process.terminate()
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()

    def play(self, video_ids: list[str]) -> str:
        self.stop()
        audio_path = next(
            (self.audio_dir / f"{video_id}.mp3" for video_id in video_ids
             if (self.audio_dir / f"{video_id}.mp3").is_file()),
            None,
        )
        if audio_path is None:
            return "No local audio file available."
        ffplay = shutil.which("ffplay")
        if ffplay is None:
            return "ffplay is not available; playback disabled."
        try:
            self.process = subprocess.Popen(
                [ffplay, "-nodisp", "-autoexit", "-loglevel", "error", str(audio_path)],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
        except OSError as error:
            return f"Could not start ffplay: {error}"
        return f"Playing {audio_path.name}"


# Import lazily so data validation and --help also work without the UI dependency.
def make_app(
    store: ReviewStore,
    links: dict,
    include_all: bool = False,
    recordings: dict | None = None,
    audio_dir: Path = ROOT / "audio",
):
    if int(version("textual").split(".")[0]) < 1:
        raise ValueError("Textual 1.0 or newer is required; the installed Textual is " + version("textual"))
    from rich.text import Text
    from textual.app import App, ComposeResult
    from textual.binding import Binding
    from textual.containers import Horizontal, Vertical, VerticalScroll
    from textual.screen import ModalScreen
    from textual.widgets import Button, Footer, Header, Label, Link, Select, SelectionList, Static

    class DiscardDialog(ModalScreen[bool]):
        def compose(self) -> ComposeResult:
            with Vertical(id="quit-dialog"):
                yield Static("There are unsaved selections. Saved tracks are already on disk.")
                yield Button("Keep editing", id="cancel", variant="primary")
                yield Button("Discard unsaved selections and quit", id="discard", variant="error")

        def on_button_pressed(self, event: Button.Pressed) -> None:
            self.dismiss(event.button.id == "discard")

    class TaxonomyApp(App):
        TITLE = "Jester · Track taxonomy"
        CSS = """
        Screen { layout: vertical; }
        #body { padding: 1 2; }
        #track-title { text-style: bold; color: $accent; height: auto; }
        #metadata, #progress, #note, #playback, #state { height: auto; margin-bottom: 1; }
        #note { color: $text-muted; }
        #recordings { height: auto; margin-bottom: 1; }
        #primary { height: auto; }
        .axis { width: 1fr; height: auto; margin-right: 1; }
        Select { width: 100%; }
        #annotations { height: 13; }
        .annotation { width: 1fr; height: 100%; margin-right: 1; }
        SelectionList { height: 1fr; }
        #actions { height: auto; margin-top: 1; }
        Button { margin-right: 1; min-width: 12; }
        DiscardDialog { align: center middle; background: $background 70%; }
        #quit-dialog { width: 60; height: auto; padding: 2; border: thick $accent; }
        #quit-dialog Button { width: 100%; margin-top: 1; }
        """
        BINDINGS = [
            Binding("ctrl+s", "save_next", "Save & next", priority=True),
            Binding("ctrl+right", "next_track", "Next", priority=True),
            Binding("ctrl+left", "previous_track", "Previous", priority=True),
            Binding("ctrl+q", "request_quit", "Quit", priority=True),
            Binding("ctrl+c", "request_quit", show=False, priority=True),
        ]

        def __init__(self):
            super().__init__()
            self.keys = sorted(key for key, track in store.tracks.items() if include_all or not track.classified)
            self.index = 0
            self.drafts: dict[tuple, Track] = {}
            self.player = LocalPlayer(audio_dir)

        def compose(self) -> ComposeResult:
            yield Header()
            with VerticalScroll(id="body"):
                yield Static(id="progress")
                yield Static(id="track-title")
                yield Static(id="metadata")
                yield Static(id="note")
                yield Static(id="playback")
                yield Vertical(id="recordings")
                with Horizontal(id="primary"):
                    for id, category, label in [("mood", "moods", "Mood (required)"), ("intensity", "intensities", "Intensity (required)"), ("function_tag", "functions", "Scene function (optional)")]:
                        with Vertical(classes="axis"):
                            yield Label(label)
                            yield Select([("— Choose —" if id != "function_tag" else "— None —", "")] + [(v, v) for v in store.allowed[category]], value="", allow_blank=False, id=id)
                yield Static("Intensity: subtle = background · measured = restrained · driving = propulsive · fierce = overwhelming")
                with Horizontal(id="annotations"):
                    for category in ("textures", "environments"):
                        with Vertical(classes="annotation"):
                            yield Label(category.title() + " (Space to toggle)")
                            yield SelectionList(*[(v, v) for v in store.allowed[category]], id=category)
                yield Static(id="state")
                with Horizontal(id="actions"):
                    yield Button("Previous", id="previous")
                    yield Button("Skip / next", id="next")
                    yield Button("Save & next", id="save", variant="primary")
                    yield Button("Quit", id="quit")
            yield Footer()

        async def on_mount(self) -> None:
            await self.show_track()

        def current(self) -> Track:
            key = self.keys[self.index]
            return self.drafts.get(key, store.tracks[key])

        def capture(self) -> None:
            if not self.keys:
                return
            track = store.tracks[self.keys[self.index]]
            fields = {id: self.query_one("#" + id, Select).value or None for id in ("mood", "intensity", "function_tag")}
            for category in ("textures", "environments"):
                selected = self.query_one("#" + category, SelectionList).selected
                fields[category] = "|".join(v for v in store.allowed[category] if v in selected)
            draft = replace(track, **fields)
            if draft != track:
                self.drafts[track.key] = draft
            else:
                self.drafts.pop(track.key, None)

        async def show_track(self) -> None:
            if not self.keys:
                self.player.stop()
                self.query_one("#track-title", Static).update("No unclassified tracks left. Launch with --all to review every track.")
                for selector in ("#primary", "#annotations", "#save", "#next", "#previous"):
                    self.query_one(selector).disabled = True
                return
            track = self.current()
            remaining = sum(not t.classified for t in store.tracks.values())
            self.query_one("#progress", Static).update(f"Track {self.index+1} / {len(self.keys)} in this session · {remaining} unclassified in library · {len(self.drafts)} unsaved drafts")
            self.query_one("#track-title", Static).update(Text(track.title))
            self.query_one("#metadata", Static).update(Text(f"{track.artist} — {track.origin}"))
            self.query_one("#note", Static).update(Text(store.note(track.key)))
            playback = self.player.play((recordings or {}).get(track.key, []))
            self.query_one("#playback", Static).update(Text(playback))
            container = self.query_one("#recordings", Vertical)
            await container.remove_children()
            urls = links.get(track.key, [])
            if urls:
                await container.mount(*[Link(f"Listen {i+1}: {url}", url=url) for i, url in enumerate(urls)])
            else:
                await container.mount(Static("No matching recording link available."))
            for field in ("mood", "intensity", "function_tag"):
                self.query_one("#" + field, Select).value = getattr(track, field) or ""
            for category in ("textures", "environments"):
                widget = self.query_one("#" + category, SelectionList)
                widget.deselect_all()
                for value in getattr(track, category).split("|"):
                    if value:
                        widget.select(value)
            self.query_one("#state", Static).update("Save writes to the review SQL only. Skip keeps selections in memory; quitting discards unsaved drafts.")

        async def move(self, offset: int) -> None:
            if self.keys:
                self.capture()
                self.index = (self.index + offset) % len(self.keys)
                await self.show_track()

        async def action_next_track(self) -> None:
            await self.move(1)

        async def action_previous_track(self) -> None:
            await self.move(-1)

        async def action_save_next(self) -> None:
            if not self.keys:
                return
            self.capture()
            track = self.current()
            try:
                store.save(track)
            except (OSError, ValueError) as error:
                self.notify(str(error), severity="error", timeout=8)
                return
            self.drafts.pop(track.key, None)
            # In pending mode, move to another unclassified track, wrapping past skipped ones.
            for offset in range(1, len(self.keys)+1):
                next_index = (self.index + offset) % len(self.keys)
                if include_all or not store.tracks[self.keys[next_index]].classified:
                    self.index = next_index
                    break
            await self.show_track()
            remaining = sum(not store.tracks[key].classified for key in self.keys)
            self.notify("Saved. All tracks in this queue are classified." if not remaining else "Saved to review SQL.")

        def action_request_quit(self) -> None:
            self.capture()
            if self.drafts:
                self.push_screen(DiscardDialog(), lambda discard: self.exit() if discard else None)
            else:
                self.exit()

        async def on_button_pressed(self, event: Button.Pressed) -> None:
            actions = {"previous": self.action_previous_track, "next": self.action_next_track, "save": self.action_save_next}
            if event.button.id in actions:
                await actions[event.button.id]()
            elif event.button.id == "quit":
                self.action_request_quit()

    return TaxonomyApp()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sql_file", nargs="?", type=Path, default=DEFAULT_SQL)
    parser.add_argument("--all", action="store_true", help="Include already-classified tracks")
    parser.add_argument("--database", type=Path, default=DEFAULT_DB, help="Read-only source of recording links")
    args = parser.parse_args()
    try:
        store = ReviewStore(args.sql_file)
        links = recording_links(args.database)
        recordings = recording_ids(args.database)
        app = make_app(store, links, args.all, recordings)
    except ModuleNotFoundError as error:
        parser.exit(1, f"Missing dependency: {error.name}. Run with the Python environment where Textual is installed.\n")
    except (OSError, ValueError, sqlite3.Error) as error:
        parser.exit(1, f"Cannot open taxonomy review: {error}\n")
    try:
        app.run()
    finally:
        app.player.stop()


if __name__ == "__main__":
    main()
