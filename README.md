# Chester

Chester is a Discord music bot written in Rust. It downloads and plays music from YouTube, maintains a SQLite-backed library, and provides Chronicle: voice recording, Whisper transcription, corpus search, and a local LLM assistant.

This document describes a Linux deployment from an empty machine. Windows and macOS are not supported deployment targets.

## What runs where

- Chester itself is a Rust binary started from the repository root.
- `data/jester.sqlite3` stores the local music library and metadata.
- `audio/` stores downloaded MP3 files.
- `data/chronicle.sqlite3` stores Chronicle's local index database.
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
llm_context_limit = 32768
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
max_chunk_length = 2000
```

The loader validates `llm_max_tokens` (1–32768), `llm_context_limit` (greater than `llm_max_tokens`, up to 32768), `llm_temperature` (0.0–2.0), `llm_max_reply_length` (1–2000), `retrieval_limit` (1–100), `retrieval_candidate_limit` (at least `retrieval_limit`, up to 1000), a finite non-negative `retrieval_distance_threshold`, a `retrieval_near_duplicate_threshold` between 0.0 and 1.0, and a positive `retrieval_max_chunks_per_document`. Retrieval examines the candidate limit, discards chunks beyond the distance threshold, removes exact and near-duplicate chunks, limits the number of chunks from each document, and sends at most `retrieval_limit` accepted chunks to the LLM. Chronicle then fits those chunks to the tokenizer-based context budget, preserving ranked order and truncating only when the highest-ranked result cannot otherwise fit. Adjacent chunks are retained because future chunker improvements may introduce intentional overlap. If the question is blank, the corpus is empty, or no chunk meets the threshold, Chronicle returns a short-circuit message instead of invoking the LLM. Startup downloads the BGE embedding model if it is not already cached. The first `/chronicle start` downloads the configured LLM model and tokenizer into the Hugging Face cache.

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
| `/library all`, `artist`, `origin`, `tags`, `incomplete` | Browse the library |
| `/set_metadata title`, `artist`, `origin` | Edit track metadata |
| `/fix` | Fill missing metadata |
| `/add_tag`, `/reset_tags` | Manage track tags |
| `/recording start`, `/recording stop` | Record a voice session; start accepts an optional initial scene |
| `/chronicle scene` | Add a scene marker to an active recording |
| `/transcript generate`, `/transcript show` | Create or display a transcript; generation can ignore scene markers |
| `/chronicle start`, `/chronicle stop`, `/chronicle ask` | Load, unload, or query the local assistant |
| `/help` | Show command help |

Chronicle recording and transcription produce files below `.chronicle/recordings/<guild-id>/`. Obtain consent from voice participants before recording. Participants in the recording will be notified that they are being recorded.

## Library downloads and migration

The bot automatically synchronizes tracks in the Jester database at startup. To manually download every track with the helper script:

```bash
./download.sh
./download.sh --parallel
```

The script reads IDs from `data/jester.sqlite3`, writes MP3 files to `audio/`, and skips existing files. Parallel mode uses eight jobs; edit `PARALLEL_JOBS` in the script if the host or network needs a lower limit.

## Troubleshooting

- **`yt-dlp missing or not executable`:** verify `./yt-dlp` exists, is executable, and is a Linux binary (`chmod 755 yt-dlp`).
- **`ffmpeg missing or not executable`:** install it and confirm `command -v ffmpeg` returns a path.
- **CUDA or Candle build errors:** verify the NVIDIA driver, CUDA toolkit, `nvidia-smi`, `nvcc`, and the Candle revision all match the project's expected toolchain.
- **`DISCORD_TOKEN` missing:** create `.env` in the repository root or export the variable in the service environment.
- **`Failed to read config file`:** ensure `.chronicle/config.toml` exists and is valid TOML.
- **No slash commands:** run `>register` and register commands in guild.
- **Startup fails around SQLite:** verify that the configured database parent directory is writable and that the database URLs point to valid SQLite locations. Chester creates missing database files and schemas automatically.

## License

See [LICENSE](LICENSE).
