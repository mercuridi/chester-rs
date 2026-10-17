#!/usr/bin/env python3
"""Review Jester tracks, listen locally, and assign the new shared tags."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any
from urllib.parse import unquote

try:
    from textual.app import App, ComposeResult
    from textual.containers import Horizontal, Vertical
    from textual.widgets import Button, Footer, Header, Input, SelectionList, Static
except ImportError:
    print(
        "Textual is required. Install it with: python3 -m pip install -r "
        "scripts/requirements-jester-tag-review.txt",
        file=sys.stderr,
    )
    raise SystemExit(2)


def normalize_tag(raw: str) -> str:
    tag = " ".join(raw.split()).lower()
    if not tag:
        raise ValueError("Tag cannot be empty.")
    if len(tag) > 100:
        raise ValueError("Tags must be 100 characters or fewer.")
    return tag


def configured_paths(args: argparse.Namespace) -> tuple[Path, Path, Path]:
    root = args.root.resolve()
    config_path = args.config if args.config.is_absolute() else root / args.config
    config_path = config_path.resolve()
    if args.database:
        database_path = args.database if args.database.is_absolute() else root / args.database
    else:
        with config_path.open("rb") as config_file:
            config = tomllib.load(config_file)
        url = config.get("database", {}).get("jester", "sqlite://data/jester.sqlite3")
        if not url.startswith("sqlite://"):
            raise ValueError("The configured Jester database must use a sqlite:// URL.")
        raw_path = unquote(url[len("sqlite://") :].split("?", 1)[0])
        if raw_path in (":memory:", ""):
            raise ValueError("The review tool needs a file-backed Jester database.")
        database_path = Path(raw_path)
        if not database_path.is_absolute():
            database_path = root / database_path
    audio_dir = args.audio_dir or root / "audio"
    if not audio_dir.is_absolute():
        audio_dir = root / audio_dir
    progress_path = args.progress or root / "data" / "jester-tag-review-progress.json"
    if not progress_path.is_absolute():
        progress_path = root / progress_path
    return database_path.resolve(), audio_dir.resolve(), progress_path.resolve()


def require_tag_schema(connection: sqlite3.Connection) -> None:
    tables = {
        row[0]
        for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('tags', 'track_tags')"
        )
    }
    if tables != {"tags", "track_tags"}:
        raise ValueError(
            "The Jester database has not been upgraded for tags yet. "
            "Start the current Chester build once, then launch this review tool."
        )


def load_tracks(connection: sqlite3.Connection, audio_dir: Path) -> list[dict[str, Any]]:
    connection.row_factory = sqlite3.Row
    rows = connection.execute(
        """
        SELECT tracks.id, tracks.track_title AS title, artists.artist, origins.origin,
               tracks.mood, tracks.intensity, tracks.function_tag,
               (SELECT group_concat(texture, ', ') FROM track_textures
                    WHERE track_id = tracks.id) AS textures,
               (SELECT group_concat(environment, ', ') FROM track_environments
                    WHERE track_id = tracks.id) AS environments,
               (SELECT group_concat(label, ', ') FROM track_labels
                    WHERE track_id = tracks.id) AS labels
        FROM tracks
        LEFT JOIN artists ON artists.id = tracks.artist_id
        LEFT JOIN origins ON origins.id = tracks.origin_id
        ORDER BY tracks.track_title COLLATE NOCASE, tracks.id
        """
    ).fetchall()
    tracks = []
    for row in rows:
        track = dict(row)
        track["audio_path"] = audio_dir / f"{track['id']}.mp3"
        track["has_audio"] = track["audio_path"].is_file()
        tracks.append(track)
    return tracks


def read_progress(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {"version": 1, "current_track_id": None, "tracks": {}}
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"Could not read progress file {path}: {error}") from error
    if state.get("version") != 1 or not isinstance(state.get("tracks"), dict):
        raise ValueError(f"Unsupported or invalid progress file: {path}")
    return state


def write_progress(path: Path, state: dict[str, Any], tracks: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    state["missing_audio"] = [
        {
            "id": track["id"],
            "title": track["title"],
            "artist": track["artist"],
            "origin": track["origin"],
        }
        for track in tracks
        if not track["has_audio"]
    ]
    temp_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=path.parent, delete=False
        ) as temporary:
            temp_name = temporary.name
            json.dump(state, temporary, ensure_ascii=False, indent=2)
            temporary.write("\n")
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temp_name, path)
    finally:
        if temp_name and os.path.exists(temp_name):
            os.unlink(temp_name)


class TagReviewApp(App[None]):
    TITLE = "Jester Tag Review"
    CSS = """
    #review { height: 1fr; padding: 1 2; }
    #progress { height: 2; color: $text-muted; }
    #track-title { height: auto; text-style: bold; }
    #metadata { height: auto; padding-bottom: 1; }
    #legacy { height: 1fr; border: round $primary; padding: 1; }
    #tag-panel { width: 42%; border: round $accent; padding: 1; }
    #tag-list { height: 1fr; }
    #new-tag-row { height: 3; }
    #new-tag { width: 1fr; }
    #buttons { height: 3; align-horizontal: center; }
    #buttons Button { margin: 0 1; }
    #status { height: 2; color: $text-muted; }
    """
    BINDINGS = [
        ("space", "toggle_playback", "Play/stop"),
        ("ctrl+s", "save_next", "Save and next"),
        ("ctrl+right", "save_next", "Save and next"),
        ("ctrl+left", "previous", "Previous"),
        ("ctrl+q", "quit", "Quit"),
    ]

    def __init__(
        self,
        connection: sqlite3.Connection,
        database_path: Path,
        audio_dir: Path,
        progress_path: Path,
    ) -> None:
        super().__init__()
        self.connection = connection
        self.database_path = database_path
        self.audio_dir = audio_dir
        self.progress_path = progress_path
        self.tracks = load_tracks(connection, audio_dir)
        self.state = read_progress(progress_path)
        self.index = 0
        current_id = self.state.get("current_track_id")
        if current_id:
            self.index = next(
                (index for index, track in enumerate(self.tracks) if track["id"] == current_id),
                0,
            )
        self.player: subprocess.Popen[bytes] | None = None
        self.known_tags = [
            row[0]
            for row in connection.execute("SELECT tag FROM tags ORDER BY tag COLLATE NOCASE")
        ]

    def compose(self) -> ComposeResult:
        yield Header()
        with Vertical(id="review"):
            yield Static(id="progress")
            yield Static(id="track-title")
            yield Static(id="metadata")
            with Horizontal():
                yield Static(id="legacy", markup=False)
                with Vertical(id="tag-panel"):
                    yield Static("New tags (space toggles selection)")
                    yield SelectionList[str](id="tag-list")
                    with Horizontal(id="new-tag-row"):
                        yield Input(placeholder="Create a new tag…", id="new-tag")
                        yield Button("Add tag", id="add-tag", variant="primary")
            yield Static(id="status")
            with Horizontal(id="buttons"):
                yield Button("Play / Stop", id="play", variant="default")
                yield Button("Previous", id="previous")
                yield Button("Save & next", id="save-next", variant="success")
                yield Button("Skip for now", id="skip", variant="warning")
        yield Footer()

    def on_mount(self) -> None:
        if not self.tracks:
            self.query_one("#track-title", Static).update("No tracks found in the Jester database.")
            self.query_one("#legacy", Static).update(f"Database: {self.database_path}")
            self.query_one("#tag-list", SelectionList).disabled = True
            self.query_one("#save-next", Button).disabled = True
            self.query_one("#skip", Button).disabled = True
            return
        self.update_track_view()
        write_progress(self.progress_path, self.state, self.tracks)

    def current_track(self) -> dict[str, Any] | None:
        return self.tracks[self.index] if self.tracks else None

    def current_tag_values(self) -> set[str]:
        selection = self.query_one("#tag-list", SelectionList)
        return set(selection.selected)

    def update_track_view(self) -> None:
        track = self.current_track()
        if track is None:
            return
        self.stop_playback()
        status = self.state["tracks"].get(track["id"], {}).get("status", "not reviewed")
        self.query_one("#progress", Static).update(
            f"Track {self.index + 1} of {len(self.tracks)}  ·  "
            f"{self.reviewed_count()} reviewed  ·  {self.skipped_count()} skipped  ·  "
            f"{self.missing_audio_count()} missing audio"
        )
        self.query_one("#track-title", Static).update(track["title"] or "Untitled track")
        self.query_one("#metadata", Static).update(
            f"Artist: {track['artist'] or 'Unknown'}    Origin: {track['origin'] or 'Unknown'}\n"
            f"Track ID: {track['id']}    Review state: {status}"
        )
        old = [
            ("Mood", track["mood"]),
            ("Intensity", track["intensity"]),
            ("Function", track["function_tag"]),
            ("Textures", track["textures"]),
            ("Environments", track["environments"]),
            ("Old labels", track["labels"]),
        ]
        old_text = "OLD CLASSIFICATIONS (reference only)\n\n" + "\n".join(
            f"{name}: {value or '—'}" for name, value in old
        )
        old_text += "\n\nA label here will not become a new tag unless you add it."
        self.query_one("#legacy", Static).update(old_text)
        self.query_one("#tag-list", SelectionList).clear_options()
        current_tags = {
            row[0]
            for row in self.connection.execute(
                "SELECT tag FROM track_tags WHERE track_id = ?", (track["id"],)
            )
        }
        options = [(tag, tag, tag in current_tags) for tag in self.known_tags]
        self.query_one("#tag-list", SelectionList).add_options(options)
        has_audio = track["has_audio"]
        self.query_one("#play", Button).disabled = not has_audio
        self.query_one("#status", Static).update(
            f"{'Audio ready' if has_audio else 'MISSING AUDIO — this track is listed in the progress report for follow-up.'}  "
            f"|  Selected tags: {', '.join(sorted(current_tags)) or 'none'}"
        )
        self.query_one("#previous", Button).disabled = self.index == 0
        self.query_one("#save-next", Button).disabled = False
        self.query_one("#skip", Button).disabled = False
        self.state["current_track_id"] = track["id"]
        write_progress(self.progress_path, self.state, self.tracks)

    def reviewed_count(self) -> int:
        return sum(entry.get("status") == "reviewed" for entry in self.state["tracks"].values())

    def skipped_count(self) -> int:
        return sum(entry.get("status") == "skipped" for entry in self.state["tracks"].values())

    def missing_audio_count(self) -> int:
        return sum(not track["has_audio"] for track in self.tracks)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "add-tag":
            self.add_new_tag()
        elif event.button.id == "play":
            self.action_toggle_playback()
        elif event.button.id == "previous":
            self.action_previous()
        elif event.button.id == "save-next":
            self.action_save_next()
        elif event.button.id == "skip":
            self.skip_current()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        if event.input.id == "new-tag":
            self.add_new_tag()

    def add_new_tag(self) -> None:
        input_widget = self.query_one("#new-tag", Input)
        selected_before = self.current_tag_values()
        try:
            tag = normalize_tag(input_widget.value)
        except ValueError as error:
            self.notify(str(error), severity="error")
            return
        self.connection.execute("INSERT OR IGNORE INTO tags (tag) VALUES (?)", (tag,))
        self.connection.commit()
        if tag not in self.known_tags:
            self.known_tags.append(tag)
            self.known_tags.sort(key=str.casefold)
        input_widget.value = ""
        self.update_track_view()
        selection = self.query_one("#tag-list", SelectionList)
        for selected_tag in selected_before | {tag}:
            selection.select(selected_tag)
        self.notify(f"Added and selected tag: {tag}")

    def save_current(self) -> None:
        track = self.current_track()
        if track is None:
            return
        tags = sorted(self.current_tag_values())
        with self.connection:
            self.connection.executemany(
                "INSERT OR IGNORE INTO tags (tag) VALUES (?)", ((tag,) for tag in tags)
            )
            self.connection.execute("DELETE FROM track_tags WHERE track_id = ?", (track["id"],))
            self.connection.executemany(
                "INSERT INTO track_tags (track_id, tag) VALUES (?, ?)",
                ((track["id"], tag) for tag in tags),
            )
        self.state["tracks"][track["id"]] = {"status": "reviewed", "tags": tags}
        self.persist_and_advance()

    def action_save_next(self) -> None:
        self.save_current()

    def skip_current(self) -> None:
        track = self.current_track()
        if track is None:
            return
        if self.index >= len(self.tracks) - 1:
            self.state["tracks"][track["id"]] = {"status": "skipped"}
            write_progress(self.progress_path, self.state, self.tracks)
            self.update_track_view()
            return
        self.state["tracks"][track["id"]] = {"status": "skipped"}
        self.persist_and_advance()

    def persist_and_advance(self) -> None:
        self.stop_playback()
        self.index = min(self.index + 1, len(self.tracks) - 1)
        self.state["current_track_id"] = self.tracks[self.index]["id"]
        write_progress(self.progress_path, self.state, self.tracks)
        self.update_track_view()

    def action_previous(self) -> None:
        if self.index > 0:
            self.index -= 1
            self.state["current_track_id"] = self.tracks[self.index]["id"]
            write_progress(self.progress_path, self.state, self.tracks)
            self.update_track_view()

    def action_toggle_playback(self) -> None:
        track = self.current_track()
        if track is None or not track["has_audio"]:
            return
        if self.player and self.player.poll() is None:
            self.stop_playback()
            self.notify("Playback stopped")
            return
        executable = shutil.which("ffplay")
        if not executable:
            self.notify("ffplay was not found. Install FFmpeg to play audio.", severity="error")
            return
        self.player = subprocess.Popen(
            [executable, "-nodisp", "-autoexit", "-loglevel", "error", str(track["audio_path"])],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.notify("Playing audio; press Space or Play / Stop to stop.")

    def stop_playback(self) -> None:
        if self.player and self.player.poll() is None:
            self.player.terminate()
            try:
                self.player.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.player.kill()
                self.player.wait()
        self.player = None

    def on_unmount(self) -> None:
        self.stop_playback()
        self.connection.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd(), help="Chester runtime root")
    parser.add_argument(
        "--config", type=Path, default=Path(".chronicle/config.toml"), help="Chester config file"
    )
    parser.add_argument("--database", type=Path, help="Override configured Jester SQLite file")
    parser.add_argument("--audio-dir", type=Path, help="Override runtime audio directory")
    parser.add_argument("--progress", type=Path, help="Override resumable progress JSON path")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        database_path, audio_dir, progress_path = configured_paths(args)
        if not database_path.is_file():
            raise ValueError(f"Jester database does not exist: {database_path}")
        connection = sqlite3.connect(database_path)
        connection.execute("PRAGMA foreign_keys = ON")
        require_tag_schema(connection)
        TagReviewApp(connection, database_path, audio_dir, progress_path).run()
        return 0
    except (OSError, ValueError, sqlite3.Error) as error:
        print(f"Tag review could not start: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
