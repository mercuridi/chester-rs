# Changelog

## [0.2.1] — 2026-06-07

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

