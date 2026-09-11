# Chester

Chester is a Discord music bot written in Rust. It downloads and plays music from YouTube, maintains a SQLite-backed library, and provides Chronicle: voice recording, Whisper transcription, corpus search, and a local LLM assistant.

This document describes a Linux deployment from an empty machine. Windows and macOS are not supported deployment targets.

## What runs where

- Chester resolves its runtime files from a runtime root, not from where it was built.
- `data/jester.sqlite3` stores the local music library and metadata.
- `audio/` stores downloaded MP3 files.
- `data/chronicle.index-v<version>.sqlite3` stores Chronicle's local derived index.
  The configured `chronicle` database path is a base name; Chronicle adds its
  index-format version and rebuilds into a fresh file when that version changes.
- `.chronicle/` stores voice recordings and Chronicle configuration.
- `corpus/` contains the documents indexed by Chronicle.
- `yt-dlp` must be an executable file in the runtime root. The bot does not search `PATH` for it.
- SQLite is bundled into the Rust binary.

Chronicle uses Candle with CUDA. A CUDA-capable NVIDIA GPU is therefore required for Chronicle's embedding, transcription, and LLM features.

Music functionality does not need a GPU.

## Requirements

### Linux packages

The following example is for Debian or Ubuntu:

```bash
sudo apt update
sudo apt install -y \
  build-essential \
  ca-certificates \
  cmake \
  curl \
  ffmpeg \
  findutils \
  libopus-dev \
  libssl-dev \
  pkg-config \
  sqlite3
```

`xargs` is normally supplied by `findutils`; if your distribution does not provide an `xargs` package, install its `findutils` equivalent. 

You also need:

- Rust and Cargo, ideally installed with `rustup`.
- An NVIDIA driver and CUDA toolkit visible to the build and runtime. Verify with `nvidia-smi` and `nvcc --version`.
- A Discord application and bot token.

### Rust

Install Rust for the current user:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
```

The project uses Rust edition 2024. If the build reports an unsupported edition, update Rust with `rustup update`.

## Install from zero

Run these commands after installing the requirements:

```bash
git clone git@github.com:mercuridi/chester-rs.git chester-rs
cd chester-rs

mkdir -p .chronicle corpus audio
cp chronicle.config.example.toml .chronicle/config.toml

curl -L https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o yt-dlp
chmod 755 yt-dlp

printf '%s\n' 'DISCORD_TOKEN=replace-with-your-bot-token' > .env
cargo build --release
```

Keep `yt-dlp` in the runtime root; Chester uses `./yt-dlp` directly.

The Jester and Chronicle SQLite databases are local runtime state and are not committed. Chester creates their parent directories, database files, and schemas automatically on first startup.

## Discord setup

1. Create an application in the Discord Developer Portal and add a bot user.
2. Copy the bot token into `.env` as `DISCORD_TOKEN=...`. Do not commit `.env` or share the token.
3. Install the bot using the `bot` and `applications.commands` OAuth scopes.
4. Grant it permission to view channels, send messages, use slash commands, connect to voice channels, and speak. Recording also needs access to the relevant voice channel.
5. Enable the **Message Content Intent** for the bot. Chester requests this intent at startup and uses the `>` prefix for the registration command.

After Chester is running, use the prefix command below once in the target server to register application commands:

```text
>register
```

If commands are not visible, check the bot's OAuth scopes, application-command permissions, and startup logs.

## Configuration

By default Chester uses its current directory as the runtime root and loads
`.chronicle/config.toml` below it. Start with the supplied example:

```bash
cp chronicle.config.example.toml .chronicle/config.toml
```

The example configuration is enough for a basic deployment. It is strict: the
top-level `database`, `logging`, `chronicle`, and `discord` sections, and every nested
section shown below, are required. Unknown keys are rejected. Paths are relative
to the runtime root:

<!-- config-example:start -->
```toml
# Copy this file to .chronicle/config.toml and adjust it for the deployment.

[database]
jester = "sqlite://data/jester.sqlite3"
chronicle = "sqlite://data/chronicle.sqlite3"

[logging]
# Protected Chronicle questions and answers are excluded unless explicitly enabled.
content = false

[chronicle.indexing]
corpus_dir = "corpus"
excluded_note_ids = []
max_chunk_tokens = 480
chunk_overlap_tokens = 0

# The first `/chronicle start` downloads these files into the Hugging Face cache.
[chronicle.llm.model]
repo = "Qwen/Qwen2.5-7B-Instruct-GGUF"
revision = "main"
file = "qwen2.5-7b-instruct-q3_k_m.gguf"

[chronicle.llm.tokenizer]
repo = "Qwen/Qwen2.5-7B-Instruct"
file = "tokenizer.json"

[chronicle.llm.generation]
max_tokens = 512
context_limit = 8192
temperature = 0.2
seed = 42
system_prompt = """\
    You are Chronicle, a concise and thoughtful assistant.\
    Answer only from the supplied Chronicle context.\
    If the context is insufficient, say so plainly.\
    Do not invent facts.\
"""
max_reply_length = 1900

[chronicle.retrieval]
limit = 5
candidate_limit = 15
distance_threshold = 0.8
near_duplicate_threshold = 0.85
max_chunks_per_document = 2
pagerank_weight = 0.15

[chronicle.synthesis]
retrieval_limit = 12
candidate_limit = 40
max_chunks_per_document = 3
batch_token_budget = 1800
max_batches = 6

# Discord users allowed to retrieve GM-only notes and `[!secret]` callouts.
[chronicle.access]
gm_user_ids = []

# Map Discord users to names used in transcripts.
[discord]
alias_groups = {}
guilds = {}

# To add mappings, replace the empty maps above with tables such as:
# [discord.alias_groups.main]
# name = "Main names"
#
# [discord.alias_groups.main.aliases]
# "123456789012345678" = "Alice"
# "234567890123456789" = "Bob"
#
# [discord.guilds."345678901234567890"]
# alias_groups = ["main"]
```
<!-- config-example:end -->

To add transcript aliases, replace the empty `discord` maps above with the
following nested tables:

```toml
[discord.alias_groups.main]
name = "Main names"

[discord.alias_groups.main.aliases]
"123456789012345678" = "Alice"
"234567890123456789" = "Bob"

[discord.guilds."345678901234567890"]
alias_groups = ["main"]
```

The loader validates the LLM, retrieval, synthesis, indexing, access, and Discord
settings, including cross-setting token budgets. Retrieval examines the candidate
limit, applies the vector distance threshold, removes duplicates, caps chunks per
document, and returns at most `limit` accepted chunks. Startup downloads the BGE
embedding model if needed; the first `/chronicle start` downloads the configured
LLM model and tokenizer into the Hugging Face cache.

### Runtime location

Release binaries can run from any working directory. Select the deployment
directory explicitly with `--runtime-root`; Chester resolves `.env`, logs,
audio, recordings, `yt-dlp`, cookies, and relative settings in `config.toml`
from that one directory:

```bash
/opt/chester/chester-rs --runtime-root /srv/chester
```

Use `--config FILE` to select a different configuration file. A relative
config path is resolved below the runtime root, while relative paths inside
the configuration remain rooted at `--runtime-root`:

```bash
/opt/chester/chester-rs --runtime-root /srv/chester --config config/production.toml
```

Use Discord's developer mode to copy user and server IDs. Every participant in a
recording must have an alias in the selected group or transcript generation will
stop with a validation error. Group IDs referenced by a guild must exist.

## Run Chester

Run from the repository root:

```bash
cargo run                 # debug build
cargo run --release       # optimized build
```

With logging overrides:

```bash
RUST_LOG=chester_rs=debug cargo run --release
RUST_LOG=chester_rs=info,chester_rs::chronicle=debug cargo run --release
RUST_LOG=warn cargo run --release
```

Chester writes logs to both the terminal and a timestamped logfile under
`logs/application/`. Logfiles rotate daily and are retained for 14 days, with a
maximum of 20 files. File writes use a bounded asynchronous queue; when full,
the oldest queued record is discarded. The logfile uses the same `RUST_LOG`
filtering as terminal output.

Chronicle questions and answers are excluded from both sinks by default. Set
`[logging].content = true` in `.chronicle/config.toml` only when protected
content logging is explicitly required.

At startup the bot opens the two SQLite databases, indexes `corpus/`, verifies `yt-dlp` and `ffmpeg`, synchronizes missing music, and then connects to Discord. A failure in any of those stages prevents login.

## Commands

The main application commands are:

| Command | Purpose |
| --- | --- |
| `/join`, `/leave` | Join or leave the caller's voice channel |
| `/play`, `/pause`, `/loop_track`, `/now_playing` | Control playback |
| `/download` | Add a YouTube track to the library |
| `/library all`, `artist`, `origin`, `taxonomy`, `incomplete` | Browse the library |
| `/set_metadata title`, `artist`, `origin` | Edit track metadata |
| `/fix` | Fill missing metadata |
| `/set_taxonomy`, `/add_texture`, `/add_environment`, `/add_label`, `/reset_taxonomy` | Manage controlled taxonomy and custom labels |
| `/recording start`, `/recording stop` | Record a voice session; start accepts an optional initial scene |
| `/recording scene` | Add a scene marker to an active recording |
| `/transcript generate`, `/transcript show` | Create or display a transcript; generation can ignore scene markers |
| `/chronicle start`, `/chronicle stop`, `/chronicle ask` | Load, unload, or query the local assistant |
| `/help` | Show command help |

Chronicle recording and transcription produce files below `.chronicle/recordings/<guild-id>/`. Obtain consent from voice participants before recording. Participants in the recording will be notified that they are being recorded.

## Troubleshooting

- **`yt-dlp missing or not executable`:** verify `./yt-dlp` exists, is executable, and is a Linux binary (`chmod 755 yt-dlp`).
- **`ffmpeg missing or not executable`:** install it and confirm `command -v ffmpeg` returns a path.
- **CUDA or Candle build errors:** verify the NVIDIA driver, CUDA toolkit, `nvidia-smi`, `nvcc`, and the Candle revision all match the project's expected toolchain.
- **`DISCORD_TOKEN` missing:** create `.env` in the repository root or export the variable in the service environment.
- **`Failed to read config file`:** ensure `.chronicle/config.toml` exists and is valid TOML.
- **No slash commands:** run `>register` and register commands in guild.
- **Startup fails around SQLite:** verify that the configured database parent directory is writable and that the database URLs point to valid SQLite locations. Chester creates missing database files and schemas automatically.

## Music taxonomy

Jester classifies tracks with a small controlled taxonomy intended for quick, consistent TTRPG music selection. A track may remain unclassified after download, but `/set_taxonomy` assigns exactly one primary mood and intensity, plus an optional scene function. These are the primary axes intended for predictable playlist filtering; for example, an `eerie`, `subtle`, `exploratory` selection can return only tracks matching those values.

Textures and environments are controlled multi-value annotations. Use them when a track can suit more than one sound or location: a cue may be both `orchestral` and `choral`, or both `desert` and `ruined`. Custom labels are free-form and are useful for campaign-specific references such as places, NPCs, or factions; they are not part of the controlled playlist taxonomy.

Use `/set_taxonomy` to set mood, intensity, and function; `/add_texture` and `/add_environment` to append controlled values; `/add_label` to append a custom label; and `/reset_taxonomy` to clear all of a track's taxonomy, textures, environments, and labels. `/library taxonomy` groups the collection by every controlled value and custom label.

### Allowed values

- Moods: `serene`, `warm`, `playful`, `whimsical`, `hopeful`, `wistful`, `somber`, `mysterious`, `eerie`, `ominous`, `menacing`, `tense`, `anxious`, `majestic`, `chaotic`, `triumphant`
- Intensities: `subtle`, `measured`, `driving`, `fierce`
- Functions: `exploratory`, `investigative`, `traveling`, `social`, `romantic`, `combative`, `climactic`, `stealthy`, `ceremonial`, `celebratory`, `contemplative`, `conversational`
- Textures: `ambient`, `acoustic`, `orchestral`, `electronic`, `synthetic`, `folk`, `piano`, `percussive`, `choral`, `vocal`, `minimalist`, `ethereal`, `dissonant`
- Environments: `desert`, `forest`, `tundra`, `mountainous`, `coastal`, `oceanic`, `swampy`, `urban`, `rural`, `underground`, `ruined`, `sacred`, `otherworldly`, `celestial`, `infernal`

## License

See [LICENSE](LICENSE).

## Chronicle hybrid retrieval

Chronicle indexes canon Markdown notes with YAML frontmatter. Required metadata is
`id` (unique non-empty string), `type` (non-empty string), `status` (`canon`,
`draft`, `deprecated`, or `speculative`), and `visibility` (`player`, `secret`, or
`mixed`). `aliases` is an optional string list and `summary` an optional string.
Other frontmatter fields are accepted but excluded from search and model context.
Notes without frontmatter, non-canon notes, and template notes are skipped;
malformed metadata and duplicate eligible IDs fail ingestion with a diagnostic.

Searchable content contains the filename stem, aliases, tags, summary, and Markdown body.
Chronicle resolves frontmatter and Markdown-body wikilinks into a derived,
visibility-aware document graph. Links may target a note ID, vault-relative path,
filename title, or declared alias; display aliases, heading references, and block
references resolve to their containing document. Dangling and ambiguous links do
not create graph edges. The graph and player/GM-specific PageRank scores rebuild
after every indexing pass.
SQLite FTS5 BM25 and vector retrieval each fetch `retrieval_candidate_limit`
candidates. Reciprocal rank fusion (constant 60) merges the lists with a
low-weight PageRank prior for those same candidates; `pagerank_weight` controls
the prior and `0.0` disables it. PageRank never introduces a document absent
from both candidate lists. Duplicate removal, document caps, and context
budgeting then apply.
`retrieval_distance_threshold` applies only to vector candidates. Lexical query
words are quoted as literals rather than interpreted as FTS operators.

Startup incrementally updates both indexes, removing deleted or newly ineligible
notes. FTS5 triggers maintain the lexical index as chunks change; opening the
database does not rebuild it. Model loading and `/chronicle ask` remain unchanged.
`gm_user_ids` in `[chronicle.access]` grants the listed Discord users access to every
canon note. Other callers can retrieve `player` notes and the player-visible
parts of `mixed` notes, but never `secret` notes or protected passages.

Use an Obsidian-style `[!secret]` callout for protected passages in a `mixed`
note:

```markdown
> [!secret]- GM notes
> This passage is retrieved only for configured GMs.
```

The callout ends at the first non-quoted line. Secret callouts are removed
before player-visible chunks are embedded, so their text cannot enter a player
LLM prompt. Corpus indexing rejects nested secret callouts and callouts in notes
whose frontmatter visibility is not `mixed`.

### Retrieval evaluation and diagnostics

Chronicle has four offline evaluation commands. They use the checked-in fixture
suites by default and write timestamped JSON reports under `logs/evaluation/`;
you can provide a suite path and a new report path after any command.

| Command | What it evaluates | Model required |
| --- | --- | --- |
| `cargo run -- --chronicle-eval` | Lexical, vector, and hybrid retrieval quality, including ranking and visibility diagnostics | Embedding model |
| `cargo run -- --chronicle-query-eval` | Deterministic structured-query execution, counts, lists, and expected results | No model |
| `cargo run -- --chronicle-query-planner-eval` | Route classification and structured-plan generation against the query suite | Configured local LLM |
| `cargo run -- --chronicle-synthesis-eval` | Planner routing plus bounded map/reduce synthesis and its deterministic rubric checks | Configured local LLM and embedding model |

The query commands are intentionally separate: `--chronicle-query-eval` checks
the database executor without model variability, while
`--chronicle-query-planner-eval` checks the natural-language planner. The old
nested `--chronicle-query-eval ... --planner` form is no longer supported.

A fixed fictional vault and retrieval evaluation suite live in
[`tests/fixtures/chronicle`](tests/fixtures/chronicle/README.md). Run:

```sh
cargo run -- --chronicle-eval
```

This uses the checked-in suite and writes a timestamped report below
`logs/evaluation/`. To override them, pass `[SUITE.toml] [REPORT.json]`; existing
report files are never overwritten. The command compares lexical, vector, and hybrid
retrieval in a temporary database without loading application configuration, the chat
LLM, or Discord. Reports include recall, precision, reciprocal rank, evidence
coverage, visibility checks, and per-candidate selection diagnostics. Enable normal
retrieval diagnostics with
`RUST_LOG=info,chester_rs::chronicle::indexer::retriever=debug`; these stay outside prompts.

### Structured counts and lists

Chronicle now plans each standalone question before answering. Supported counts
and lists run as parameterized SQLite queries and are rendered directly:

- “How many characters are recorded?”
- “List the organisations.”
- “How many living NPCs are recorded?”
- “List the missing PCs.”

Supported filters are character `role` (`pc`, `npc`, `ex-pc`) and
`character_status` (`alive`, `dead`, `missing`, `unknown`), combined with AND.
Only canon notes participate; templates remain excluded. Counts use distinct note
IDs. Lists show at most 20 matches, further bounded by the reply length, and report
the full matching total when abbreviated. Display names come from filenames.
Missing metadata means no known value; zero matches means none are recorded, not
proof of absence. Explicit `unknown` status is queryable and differs from omission.

The planner uses separate JSON-only instructions, a 256-token output budget, and
zero-temperature generation. An invalid planner response receives one corrective
retry; a second invalid response falls back to a labelled best-effort retrieval
answer, while a validated unsupported count/list falls back with an explicit
non-exhaustive qualification. Ordinary factual
questions retain hybrid retrieval and answer generation. Ambiguous references such
as “List them” ask for clarification; conversation memory is not implemented.
Negation, OR, location/relationship restrictions, non-canon selection, historical
queries, numeric totals, and arbitrary SQL are unsupported for structured execution.
Structured queries enforce the caller's player or GM visibility scope, just as
retrieval does.

Startup refreshes these properties from frontmatter, including existing unchanged
notes. Metadata-only edits reuse embeddings when their prepared searchable chunks
are unchanged. No embedding-version bump or full reindex is required for this feature.
Malformed character fields fail ingestion with the note path in the diagnostic.

See the [structured evaluation guide](tests/fixtures/chronicle-query/README.md) for
deterministic and real-planner checks. Structured plans are logged at debug level;
planning JSON and database bookkeeping are not passed into answer context.

### Bounded synthesis

Broad open-ended requests such as “Summarise the history of the Ember Kingdom”
are routed to bounded synthesis. Chronicle retrieves one larger, diverse hybrid
evidence set using the `synthesis_*` limits, splits it into context-safe batches,
and turns each batch into hidden evidence notes. If those notes do not fit the
final context, Chronicle recursively reduces them before producing one concise
narrative answer. It never performs follow-up retrieval during synthesis; the
initial retrieved passages are the complete evidence boundary. Source labels are
kept only in intermediate prompts and logs, never shown to the user. The final
prompt asks Chronicle to disclose material gaps or conflicting evidence, but not
to add a boilerplate coverage disclaimer when the evidence is adequate.

The fictional kingdom corpus in
[`tests/fixtures/chronicle-synthesis`](tests/fixtures/chronicle-synthesis/README.md)
records expected route, required coverage, prohibited claims, and deliberate gaps
without prescribing exact answer prose. It is intended for future model-backed
synthesis evaluation alongside the deterministic unit and service tests.

Run the synthesis evaluation with:

```sh
cargo run -- --chronicle-synthesis-eval
```

The query evaluation details and fixture-specific options are documented in
[`tests/fixtures/chronicle-query`](tests/fixtures/chronicle-query/README.md);
the retrieval and synthesis fixture guides document their individual report
schemas and suite overrides.
