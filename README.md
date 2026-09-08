# Chester

Chester is a Discord music bot written in Rust. It downloads and plays music from YouTube, maintains a SQLite-backed library, and provides Chronicle: voice recording, Whisper transcription, corpus search, and a local LLM assistant.

This document describes a Linux deployment from an empty machine. Windows and macOS are not supported deployment targets.

## What runs where

- Chester itself is a Rust binary started from the repository root.
- `data/jester.sqlite3` stores the local music library and metadata.
- `audio/` stores downloaded MP3 files.
- `data/chronicle.index-v<version>.sqlite3` stores Chronicle's local derived index.
  The configured `chronicle` database path is a base name; Chronicle adds its
  index-format version and rebuilds into a fresh file when that version changes.
- `.chronicle/` stores voice recordings and Chronicle configuration.
- `corpus/` contains the documents indexed by Chronicle.
- `yt-dlp` must be an executable file in the repository root. The bot does not search `PATH` for it.
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

Keep `yt-dlp` beside `Cargo.toml`; both the bot and `download.sh` use `./yt-dlp`.

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

Chester loads `.chronicle/config.toml` relative to the repository root. Start with the supplied example:

```bash
cp chronicle.config.example.toml .chronicle/config.toml
```

The example configuration is enough for a basic deployment. Paths are relative to the repository root:

```toml
[database]
jester = "sqlite://data/jester.sqlite3"
chronicle = "sqlite://data/chronicle.sqlite3"

[chronicle]
corpus_dir = "corpus"

llm_repo = "Qwen/Qwen2.5-7B-Instruct-GGUF"
llm_revision = "main"
llm_model_file = "qwen2.5-7b-instruct-q3_k_m.gguf"
llm_tokenizer_repo = "Qwen/Qwen2.5-7B-Instruct"
llm_tokenizer_file = "tokenizer.json"

llm_max_tokens = 512
# 8192 is the conservative default for the bundled 7B model. Higher context
# limits require substantially more GPU memory.
llm_context_limit = 8192
llm_temperature = 0.2
llm_seed = 42
llm_system_prompt = """\
You are Chronicle, a concise and thoughtful assistant.
Answer only from the supplied Chronicle context.
If the context is insufficient, say so plainly.
Do not invent facts.
"""
# Maximum number of Unicode characters in a generated Discord reply.
llm_max_reply_length = 1900
retrieval_limit = 5
retrieval_candidate_limit = 15
retrieval_distance_threshold = 0.8
retrieval_near_duplicate_threshold = 0.85
retrieval_max_chunks_per_document = 2
synthesis_retrieval_limit = 12
synthesis_candidate_limit = 40
synthesis_max_chunks_per_document = 3
synthesis_batch_token_budget = 1800
synthesis_max_batches = 6
max_chunk_tokens = 480
# Target content tokens shared by adjacent chunks, excluding special model tokens.
# Section boundaries may reduce the effective overlap. Zero disables overlap.
chunk_overlap_tokens = 0
```

The configuration schema is strict: unknown keys are rejected, and both chunking settings must be present. The loader validates `llm_max_tokens` (1–32768), `llm_context_limit` (greater than `llm_max_tokens`, up to 32768), `llm_temperature` (0.0–2.0), `llm_max_reply_length` (1–2000), `retrieval_limit` (1–100), `retrieval_candidate_limit` (at least `retrieval_limit`, up to 1000), a finite non-negative `retrieval_distance_threshold`, a `retrieval_near_duplicate_threshold` between 0.0 and 1.0, a positive `retrieval_max_chunks_per_document`, `synthesis_retrieval_limit` (1–100), `synthesis_candidate_limit` (at least `synthesis_retrieval_limit`, up to 1000), a positive `synthesis_max_chunks_per_document`, `synthesis_batch_token_budget` (no greater than the available LLM prompt budget), `synthesis_max_batches` (1–100), `max_chunk_tokens` (3–512), and `chunk_overlap_tokens` (no greater than the chunk budget minus 3). Retrieval examines the candidate limit, discards chunks beyond the distance threshold, removes exact and near-duplicate chunks, preserves intentional similarity between adjacent chunks from the same document, limits the number of chunks from each document, and sends at most `retrieval_limit` accepted chunks to the LLM. Chronicle then fits those chunks to the tokenizer-based context budget, preserving ranked order and truncating only when the highest-ranked result cannot otherwise fit. If the question is blank, the corpus is empty, or no chunk meets the threshold, Chronicle returns a short-circuit message instead of invoking the LLM. Startup downloads the BGE embedding model if it is not already cached. The first `/chronicle start` downloads the configured LLM model and tokenizer into the Hugging Face cache.

### Alias and guild configuration

Alias groups map Discord user IDs to names used in transcripts. A guild can enable one or more groups:

```toml
[alias_groups.main]
name = "Main names"

[alias_groups.main.aliases]
"123456789012345678" = "Alice"
"234567890123456789" = "Bob"

[guilds."345678901234567890"]
alias_groups = ["main"]
```

Use Discord's developer mode to copy user and server IDs. Every participant in a recording must have an alias in the selected group or transcript generation will stop with a validation error. Group IDs referenced by a guild must exist.

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
| `/chronicle scene` | Add a scene marker to an active recording |
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
after every indexing pass. Scores are retained for the forthcoming retrieval
reranker and do not yet alter retrieval ranking.
SQLite FTS5 BM25 and vector retrieval each fetch `retrieval_candidate_limit`
candidates. Equal-weight reciprocal rank fusion (constant 60) merges the lists,
then existing duplicate removal, document caps, and context budgeting apply.
`retrieval_distance_threshold` applies only to vector candidates. Lexical query
words are quoted as literals rather than interpreted as FTS operators.

Startup incrementally updates both indexes, removing deleted or newly ineligible
notes. FTS5 triggers maintain the lexical index as chunks change; opening the
database does not rebuild it. Model loading and `/chronicle ask` remain unchanged.
`gm_user_ids` in `[chronicle]` grants the listed Discord users access to every
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

A fixed fictional vault and retrieval evaluation suite live in
[`tests/fixtures/chronicle`](tests/fixtures/chronicle/README.md). Run:

```sh
cargo run -- --chronicle-eval tests/fixtures/chronicle/suite.toml /tmp/chronicle-report.json
```

Use a new output filename. The command compares lexical, vector, and hybrid retrieval
in a temporary database without loading the chat LLM or connecting to Discord.
Reports include recall, precision, reciprocal rank, evidence coverage and per-candidate
selection diagnostics. Enable normal retrieval diagnostics with
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
Visibility remains unenforced.

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
