# chester-rs

Chester is a Discord music bot written in Rust using the Poise and Songbird libraries.
Developed on macOS/Linux. Windows deployments YMMV.

## Setup
### System Dependencies
- ffmpeg
- cargo / rustup
- sqlite3
- libssl-dev
- cmake

### Other Dependencies
- yt-dlp
    - You will need to install yt-dlp yourself: https://github.com/yt-dlp/yt-dlp/wiki/Installation
    - Remember to give the yt-dlp binary execute permissions with `chmod`
    - `download.sh` assumes a binary `yt-dlp` file has been placed in the top level - you may need to edit this!

## Execution

### Chester
- `cargo run` for debug build
- `cargo run --release` for optimised build

Logging defaults to useful `info` messages for Chester and `warn` messages for dependencies.
Use `RUST_LOG` to change the filter while developing:

- `RUST_LOG=chester_rs=debug cargo run` enables detailed project tracing.
- `RUST_LOG=chester_rs=info,chester_rs::chronicle=debug cargo run` enables debug output only for Chronicle.
- `RUST_LOG=warn cargo run` shows warnings and errors only.

### download.sh
- This script reads the database in `database/jester/jester.sqlite3` and downloads all relevant audio files automatically
- `-p` can be passed as a flag to enable parallel download execution - this enormously speeds up large sequential downloads
