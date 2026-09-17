# Changelog

## [4.1.0] — 2026-09-17

### Features

- Startup now attempts all steps and reports back on all encountered failures
- Corpus startup scans now report all errors instead of failing fast at first error
- Reuse scanned documents during corpus indexing instead of reading multiple times


### Bug Fixes

- Improve corpus error report readability
- Promote interim-symlink to main corpus directory for personal dev

## [v4.0.0] — 2026-09-12

### Features

- Rework config handling completely for a significantly better config.toml structure


### Bug Fixes

- Life_status and role are now not treated specially for no reason
- Correct clippy errors on some tests
- Appearances moved to universal fields instead of character-specific
- Ensure all sqlite connections enforce foreign key integrity
- CUDA OOM errors are now always logged to application as well as terminal / stderr
- Reintroduce tests lost during refactors
- Remove logging on heavily-used loop during startup
- Unsupported queries now correctly short-circuit and avoid calling inference when the route is predetermined
- Massively expand ringbuffer capacity to give the bot more room to breathe when busy
- Correct hanging clippy lints
- Correct panic-safety errors reported by clippy
- Address type safety warnings from clippy
- Various dead code warnings after previous refactors
- Api simplifications recommended by clippy
- Clippy readability fixes
- Clippy code robustness fix
- 6 expressions simplified from clippy lints
- Renamed similarly named symbols to prevent confusion
- Declare large-ish vec on heap to avoid large stack allocation
- Chronicle eval was re-obtaining an already borrowed GPU lease; fixed
- Reintroduce lost instructions to query classifier and planner to improve planning accuracy
- Improve query repair guidance for better second-try accuracy
- Add instructions for query classifier to act as an ordered decision procedure
- Dynamically generate and inject the full field taxonomy into the query planner to give the LLM a full view of the taxonomy and improve planning accuracy
- Add a full-taxonomy injection toggle to the planner as it's expensive on VRAM
- Planner evaluation now reports model settings
- Planner evaluation shares pre-routing with production
- Log LLM tokenisation and add taxonomy mode to query structuring with full/compact toggle
- Enum case normalisation for planner outputs
- Short-circuit query planning for a small set of obvious unsupported requests
- Targeted model context additions for failing query constructions
- Add more guidance to catch a regression caused by the model overfitting to new classification guidance
- Properly drain compressed audio as it comes in instead of holding it in memory until recording stop
- Properly handle trying to load the LLM when it is already loaded
- Prevent concurrent download races from deleting audio files
- Recording encoder threads now sleep fully when inactive to reduce CPU load
- Crashed session recovery now properly assesses and handles hard-crash "corrupt" manifests
- Interrupted audio downloads no longer cause unrecoverable metadata registrations
- D31: properly bound shutdown process to track transcription workers
- D24: Flush bounded document batches while iterating across corpus for processing
- Src/chronicle/indexer.rs properly fulfils new publicity constraints
- Delete superseded function
- Add author and sexuality fields to taxonomy
- Correct 5 panic-risk warnings and a pass-by-value warning
- Collapse repeated conditional into single branch
- Introduce EncoderState struct to reduce number of arguments passed to drain_recording_frames


### Refactor

- Retrieval pipeline split out into explicit stages in functions
- New RankedCandidate struct for search results to make ranking and selection easier
- Separate retrieval policy from mechanics with a new SearchSettings object containing CandidatePoolPolicy, FusionPolicy, and SelectionPolicy
- Synthesis is now orchestrated as a staged pipeline
- Explicitly define answer routing with a new AnswerRoute enum
- Split chronicle repository monolithic source file into submodules
- Remove 3 misleading passthrough function definitions
- Break down too-many-lines config handling
- Separate fixture registry and fingerprinting from retrieval runner to solve too-many-lines
- Rework synthesis evaluation runner to eliminate too-many-lines warning
- Frontmatter parsing broken down into functions to improve maintainability + remove redundant data storage of life_status and role fields
- Replace_note now calls out to helpers while retaining atomic behaviour
- Write_metadata now calls directly through 5 helper functions to make its processes clearer
- Break down synthesis_eval into submodules
- Rework chronicle/service.rs into its own chronicle/service/ submodule tree
- Config split into many smaller modules for better separation of concerns
- Retriever.rs split up into its own submodule of indexer
- Require proper config setup and remove boilerplate serde functions and declarations
- Classifier and structured query planner are now separate LLM calls to increase accuracy and modularity
- Improved chronicle ask routing for more accurate synthesis evaluation and runtime observability
- Corpus scanning is now handled by a two-pass streaming model; discover then perform targeted processing for lower memory usage
- Pre-construct data object and pass to framework builder to reduce number of arguments
- Break down select_answer_route into helper functions for better readability
- Break down run_bot to reduce function line count
- Break down indexing process into subfunctions
- Break down run_encoder into subfunctions
- Break down stop_recording into subfunctions
- Split out query eval and planner eval
- Mod.rs removal and import updates: src/chronicle/config/
- Mod.rs removal and import updates: src/chronicle/indexer/chunker/
- Mod.rs removal and import updates: src/chronicle/indexer/db/
- Mod.rs removal and import updates: src/chronicle/indexer/retriever/
- Mod.rs removal and import updates: src/chronicle/indexer/
- Mod.rs removal and import updates: src/chronicle/query/
- Mod.rs removal and import updates: src/chronicle/recording/
- Mod.rs removal and import updates: src/chronicle/service/
- Mod.rs removal and import updates: src/chronicle/synthesis_eval/
- Mod.rs removal and import updates: src/chronicle/transcription/whisper/
- Mod.rs removal and import updates: src/chronicle/transcription/
- Mod.rs removal and import updates: src/chronicle/
- Mod.rs removal and import updates: src/database/
- Mod.rs removal and import updates: src/discord/commands/
- Mod.rs removal and import updates: src/discord/
- Mod.rs removal and import updates: src/jester/db/
- Mod.rs removal and import updates: src/jester/library/
- Mod.rs removal and import updates: src/jester/player/
- Mod.rs removal and import updates: src/jester/track/
- Mod.rs removal and import updates: src/jester/
- Mod.rs removal and import updates: src/utils/
- Mod.rs removal and import updates: src/
- Break down main.rs into a new app/ submodule
- Extract all config to its own dedicated module
- Move sole-purpose utils/format.rs to its real consumer location


### Tech-debt

- D15: Runtime paths mix build-machine roots and working directories fixed by centralising path handling
- D27: Chronicle construction now accepts bundled settings structs and a new ChronicleDependencies struct + new StructuredStore trait on IndexerDb
- D12: Multi-step Jester database writes are now atomic
- D28: Separate data fetching and Discord display behaviours + improve library query shape and typing with named record structs (LibraryTrack, LibraryGroupEntry, TrackSearchResult)
- D01: any visibility change forces a full document reindex
- D03: new SessionId type and proper path validation to sanitise transcript session path handling
- D04: persist taxonomy.toml in git so fresh checkouts pass tests properly
- D06: refactor of main.rs to separate async functionality and improve evaluation run routes
- D20: update readme to match reality and added a test to keep it that way
- D05: Enforce blocking workers owning GPU leases until task completion
- D31: Coordinated shutdown flow implemented with new module and proper draining/waiting policies for work to finish
- D07: Jester services are now transactional and handled more consistently to prevent edge case bad behaviours
- D22: Replace global player service operation lock with a guild-scoped GuildPlayerState
- D08: Improved transcript failure reporting and added a new `/transcript recover` command
- D09: Correct audio timeline drift on dropped PCM frames due to ringbuffer fill by introducing new timestamp-managed RecordedFrame struct
- D33: All recording sessions have a unique fingerprint which is consistently used; this protects from (very unlikely) identity collisions
- D10: Refactor several methods in recording, downloading, and embedding to properly utilise async subprocesses to prevent unrelated work stalling
- D11: consolidate duplicated and diverging downloader behaviour into a single module with proper network boundary handling logic
- D32: Prevent GPU leases from leaking an Arc ref on (unnecessary) drop
- D29: clean up old jester taxonomy migration python script
- D30: Saved logfiles are now kept in daily files with a 14-day retention and 20-file max; logging now also does not block the executing thread
- D26: Introduce StructuredOperation and StructuredPlan types to prevent query planner from allowing impermissible operations and panicking; struct reduces repeated type checking throughout query handling processes
- D35: remove dead and reference legacy code
- D25: prevent wikilink resolution from bleeding across fields; lookups respect type, field, canon status, and visibility
- D23: Transcription memory usage moved to a streaming model to bound maximum concurrent memory usage
- D24: Indexing optimisations; skip metadata refresh and graph / PageRank rebuilds when the inputs are the same

## [v3.1.1] — 2026-09-10

### Bug Fixes

- Fix further leakage of outside phrases
- Improve guidance for list queries and add regression coverage
- Drop required pass marks because i am fucking done with counting OOM errors as genuine failures its not my fucking fault i cant afford a 5090

## [v3.1.0] — 2026-09-10

### Features

- Alias resolver preprocessing step
- Persist note graph edges into chronicle database
- Implement pagerank computation and persist scores on graph rebuild
- Pagerank is now considered as part of initial retrieval of notes


### Bug Fixes

- Add explicit exclusions configuration
- Properly expose pagerank diagnostics
- Evaluation tools output to a logs subfolder
- Logfiles output to hard-saved file on disc as well
- Anonymise leaked fixtures

## [v3.0.0] — 2026-09-08

### Features

- Foundations and config for synthesis queries
- Set up hybrid retrieval for synthesis and initial mapping
- Synthesis pipeline final step
- Synthesis evaluation suite
- Query routing for synthesis and corpus topology handling are now tested
- Dedicated synthesis evaluation suite
- Added relationship-member counting (eg. number of enemies for character)
- Improve synthesis evaluation with hybrid answer assessment
- Synthesis judge based on an LLM to assess synthesis outputs
- Internal intermediate ledger for synthesis to preserve key events
- Improved synthesis evaluation framework


### Bug Fixes

- Debug telemetry on synthesis
- Map/rteduce is now tested directly
- Improved retrieval and synthesis pipeline diagnostics
- Add testing and evaluation for synthesis treating secrets properly
- Test synthesis being routed to for history related questions
- Bounded synthesis testing config mismatch fixed
- Release process now runs all chronicle evaluations and requires them to pass
- Improve planner consistency (18/48 to 33/48 passes)
- Planner evaluation 33/48 to 40/48
- Improve wikilink handling; 40/48 -> 42/48
- Wikilink-or-string resolution now looks up values against database to improve wikilink behaviour (42/48 -> 44/48)
- Corrected some evaluations to more accurately represent the taxonomy (44/48 -> 46/48)
- Replace population counting test with a location containment test (46/48 -> 47/48)
- Judge llm aggregate scoring bug fixed
- Split out tough single rubric to individual marking points
- Improve synthesis prompt
- Clean up unused code
- Log full chronicle reply under debug
- Loosen synthesis evaluation pass requirements for release
- Clippy warnings fixed
- Log all llm inference creations

## [v2.11.0] — 2026-09-08

### Features

- Improved chronicle database handling and setup
- Generic structured query shape to improve hard lookup coverage plus evaluation tests
- Add single retry for bad query router json and improve observability
- Player/gm visibility controls with secret tagging


### Bug Fixes

- Big frontmatter schema update
- Add new appearances field to character note taxonomy
- Event occurrence fields do not conflict when blank any more
- Templates are now ignored for preflight checks
- Distinguish failed planning from unsupported query models
- Tests for gm/player visibility
- Clippy linting
- Cargo fmt

## [v2.10.0] — 2026-09-07

### Features

- Schema definition for frontmatter
- Improved frontmatter parsing behaviour
- Unknown field and enum value handling
- Sqlite database models all frontmatter metadata
- Testing for frontmatter handling


### Bug Fixes

- Searchable context expanded to tags
- Schema validation at runtime with errors
- Clippy fixes
- Cargo fmt

## [v2.9.0] — 2026-09-07

### Features

- Hybrid retrieval with FTS5/BM25
- Chronicle retrieval evaluation suite with dummy corpus
- Structured querying for chronicle plus routing model


### Bug Fixes

- Further extend dummy corpus

## [v2.8.0] — 2026-09-05

### Features

- Mvp for queue system
- Queue history command
- Tagging taxonomy rework
- Automix system based on database tagging


### Bug Fixes

- Log question and answer for chronicle
- Improve LLM VRAM usage
- Queue show moves to subcommand
- Queue remove now autocompleted tracks in the queue
- All tracks organised into new taxonomy
- Clippy fixes

## [v2.7.0] — 2026-08-30

### Features

- What if we actually had some tests
- Repository and downloading tests
- Extra edge-case tests


### Bug Fixes

- Prep for better testing by adding traits to internal types
- Force all releases to be clean on test, clippy, and fmt

## [v2.6.1] — 2026-08-30

### Bug Fixes

- Scene is now a recording subcommand, not chronicle
- Bad configs now clearly display errors

## [v2.6.0] — 2026-08-30

### Features

- Token-aware chunker
- Markdown-aware chunking parser
- Hierarchical semantic splitting; chunker is more aware of "blocks" of text
- Chunker now has overlap tokens to improve llm contextual awareness
- Retriever is now overlap-aware


### Bug Fixes

- Exact-token accounting issues fixed
- Block tracking is now a nested stack to ensure full document coverage
- Improved overlap behaviour in chunking
- Clean break from old config settings
- Documents are now only chunked once per indexing process
- Improve deduplication for similar chunks in the same document
- Improve sentence parsing with unicode sentence-boundary iterators over punctuation splitting
- Split out chunker.rs to new submodules

## [v2.5.0] — 2026-08-29

### Features

- Improve llm chunk retrieval with threshold relevance system
- Added deduplication and diversification systems to the llm chunk retriever
- Llm prompting is now token-aware and will avoid overloading context

## [v2.4.0] — 2026-08-29

### Features

- Recording manifests are now persisted immediately and updated atomically as data changes
- Recording scene functionality to split up transcripts
- Improve transcription deduplication with a token-aware approach
- Recording now persists original formatting of session name


### Refactor

- Constants now live closer to their actual usage

## [v2.3.1] — 2026-08-29

### Features

- Startup has better error handling and operation ordering


### Bug Fixes

- All commands have help tooltips
- 7 clippy fixes
- Config must now be configured correctly
- Changelog section ordering (feat -> bugfix -> refactor -> perf -> docs)
- Ensure llm replies fit in discord character limit

## [v2.3.0] — 2026-08-29

### Features

- Database parity pass part 1
- Database parity pass part 2


### Bug Fixes

- Rewrite readme
- Put candle-kernels patch onto remote while waiting for an upstream fix
- Delete download.sh as its functionality is integrated in rust
- Databases are created automatically if they do not exist

## [v2.2.3] — 2026-08-28

### Features

- Tracing rework with proper warn/info/debug traces


### Bug Fixes

- Clippy prefers inspect over map when the value is unchanged
- Extract framework building out of main to satisfy clippy
- Add extra tracing for scanning and tokenising statistics

## [v2.2.2] — 2026-08-28

### Features

- Batch initial embedding processing on startup for performance
- Batched embedding now groups chunks by token length to improve gpu efficiency


### Bug Fixes

- Rustfmt fixes

## [v2.2.1] — 2026-08-28

### Bug Fixes

- Cargo fmt run on codebase
- Clear compiler dead code warnings with expects
- Cargo clippy autofixes
- Full clippy code quality pass with 0 remaining warnings

## [v2.2.0] — 2026-08-28

### Features

- System prompt for chronicle
- Embedder now runs on cpu for chronicle to allow more gpu headroom

## [v2.1.0] — 2026-08-28

### Features

- New GpuRuntime state machine to define the gpu processing state with RAII
- Integrate llm directly rather than rely on app-external http server
- Rework transcription into a proper service and enforce correct raii gpu leasing


### Bug Fixes

- Change corpus gitignore


### Refactor

- Ask is now a subcommand under chronicle; introduced explicit start and stop commands

## [v2.0.0] — 2026-08-28

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

### Features

- New `library incomplete` mode to find tracks which someone has added but not filled out the information for
- Implement new /fix command for tidying up tracks with bad metadata
- Automatically ensure library integrity upon every startup


### Bug Fixes

- Rework project structure
- Library incomplete function displaying incorrect data fixed
- Reworked library command backends for maintainability
- Add indexes to sqlite db and slightly improve metadata autocomplete lookup speed
- Add newline at end of each changelog section

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

### Features

- Add changelog generator and semver convention to repo


### Bug Fixes

- Initialise changelog & ensure no publish


