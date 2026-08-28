# Changelog

## [2.0.0] — 2026-08-28

### Bug Fixes

- Prevent mutex deadlock in user recording session creation
- Improve internal folder timestamp handling
- Remove unused module files
- Improve recording toggle handling
- Proper pre-skip handling in opus
- Opus files were not being read out due to event attachment ordering and pre-skip incorrectness
- Opus/whisper sample rates wrong way around
- Add extra tracing to whisper transcribe path for debug
- Add sha2 and hex to deps
- Upgrade model and comment out tracing
- Tell the model it's transcribing english
- Timestamp parser prep for overlapping windows
- Always pad mel segments out to expected size
- Add extra model options to constants
- Improve main.rs logging ergonomics
- Improve timestamp outputs in transcripts
- Skip chester, special announcement for recording start, and correct bad timestamp outputs
- Defer reply on transcription to allow for longer reply times
- Indexer now returns the db and embedder model instead of loading a second embedder for main
- Extract configs for chronicle to external config file
- Delete empty ask.rs


### Features

- Project scaffolding for chronicle
- Set up chronicle db
- Set up initial call listening and command framework
- Audio is now saveable to .wav per-user
- Stereo to mono downmixing function and cargo.toml reorganisation
- Rework recording to use ringbuffers and instantly write frames to opus-compressed output
- Add silence to user recordings when not speaking and backfill silence for late joiners
- Properly separate out recordings per-guild with a recorder manager
- Opus to raw data audio module for whisper transcription
- Minimal implementation of whisper transcription
- Overlapping segments for whisper transcription for improved accuracy
- Add basic deduplication on transcripts
- Configurable name replacement for outputted transcripts
- Autocompletes for transcribe command
- Split out chronicle command and improve session autocomplete display
- Transcript pagination
- Transcript caching
- Rework transcribe into transcript; split generate/show commands for clarity
- Llm mvp; chunker, embedder, indexer, scanner, db, command


### Refactor

- Rename player/music layer to "jester"
- Extract encoder implementation to its own module
- Extract all constants and move library sync module
- Rename browse -> library
- Big refactor across large sections of the codebase
- Rename jester back to player for clarity
- Rework joining vc to be its own operation separated from playing audio
- Clean up leave semantics
- Deconstruct large and unwieldy whisper.rs
- Better structure in transcribe command
- Clean split between jester and chronicle

## [v1.0.0] — 2026-08-17

### Features

- New library output displays because the tables were not working
- Chester now fulfils its purpose as a simple music bot. v.1.0.0 released.

## [v0.3.0] — 2026-06-07

### Bug Fixes

- Rework project structure
- Library incomplete function displaying incorrect data fixed
- Reworked library command backends for maintainability
- Add indexes to sqlite db and slightly improve metadata autocomplete lookup speed
- Add newline at end of each changelog section


### Features

- New `library incomplete` mode to find tracks which someone has added but not filled out the information for
- Implement new /fix command for tidying up tracks with bad metadata
- Automatically ensure library integrity upon every startup

## [v0.2.1] — 2026-06-07

### Bug Fixes

- Extract core download logic to stop logic module depending on command module
- Remove duplicated lookup functionality from library.rs and move to track_resolver
- Introduce MetadataKind enum to significantly improve SQL query safety
- Extract all database interaction to repository.rs
- Introduce new service module to simplify command structures
- Clean up repeated logic for requiring guild presence
- Implement tracing over println spam
- Ellipsis len is not ellipsis display width; this is now fixed and display is more stable
- Updated changelog format

## [v0.2.0] — 2026-06-07

### Bug Fixes

- Initialise changelog & ensure no publish


### Features

- Add changelog generator and semver convention to repo


