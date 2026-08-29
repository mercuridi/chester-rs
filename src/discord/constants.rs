pub const AUTOCOMPLETE_MAX_CHOICES: usize = 25;
pub const AUTOCOMPLETE_MAX_LENGTH: usize = 100;
pub const AUTOCOMPLETE_SEPARATOR: &str = " | ";
pub const AUTOCOMPLETE_SEPARATOR_LEN: usize = AUTOCOMPLETE_SEPARATOR.len();
pub const ELLIPSIS: &str = "…";
pub const ELLIPSIS_LEN: usize = 1;
pub const ELLIPSIS_DISPLAY_WIDTH: usize = 1;
pub const MAX_RESULTS_PER_PAGE: usize = 15;
pub const TITLE_MAX_CHARS: usize = 36;
pub const META_MAX_CHARS: usize = 40;
pub const MESSAGE_MAX_CHARS: usize = 2_000;
use serenity::model::id::UserId;

pub const CHESTER_USER_ID: UserId = UserId::new(1_407_798_091_934_863_360);
