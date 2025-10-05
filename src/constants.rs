// Constants with Discord-imposed limitations
pub const AUTOCOMPLETE_MAX_CHOICES: usize = 25; // max  25
pub const AUTOCOMPLETE_MAX_LENGTH: usize = 100; // max 100
pub const LIBRARY_ROW_MAX_WIDTH:   usize =  56; // max  56

// constants for autocomplete display
pub const ELLIPSIS: &str = "…";
pub const AUTOCOMPLETE_SEPARATOR: &str = " | ";
pub const ELLIPSIS_LEN: usize = ELLIPSIS.len();
pub const AUTOCOMPLETE_SEPARATOR_LEN: usize = AUTOCOMPLETE_SEPARATOR.len();

// constants for library pagination
pub const MAX_RESULTS_PER_PAGE:         usize = 20;
pub const LIBRARY_SEPARATOR: &str = " ";
pub const ROW_SEPARATOR: &str = "-";

// CMD: library
pub const LIBRARY_COLUMN_WIDTH_TITLE:   usize = 16;
pub const LIBRARY_COLUMN_WIDTH_ARTIST:  usize = 14;
pub const LIBRARY_COLUMN_WIDTH_ORIGIN:  usize = 14;
pub const LIBRARY_COLUMN_WIDTH_TAGS:    usize = 12;

// CMD: library_title
pub const LIB_TIT_COLUMN_WIDTH_TITLE:   usize = 56;

// CMD: library_artist
pub const LIB_ART_COLUMN_WIDTH_ARTIST:  usize = 23;
pub const LIB_ART_COLUMN_WIDTH_TITLE:   usize = 30;

// CMD: library_origin
pub const LIB_ORI_COLUMN_WIDTH_ORIGIN:  usize = 23;
pub const LIB_ORI_COLUMN_WIDTH_TITLE:   usize = 30;

// CMD: library_tag
pub const LIB_TAG_COLUMN_WIDTH_TAGS:    usize = 12;
pub const LIB_TAG_COLUMN_WIDTH_TITLE:   usize = 44;