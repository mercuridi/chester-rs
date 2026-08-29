use serenity::model::id::UserId;

pub const CHESTER_USER_ID: UserId = UserId::new(1_407_798_091_934_863_360);

// library display
pub const ELLIPSIS: &str = "…";
pub const ELLIPSIS_LEN: usize = ELLIPSIS.len();
pub const ELLIPSIS_DISPLAY_WIDTH: usize = 1;
pub const MAX_RESULTS_PER_PAGE: usize = 15;
pub const TITLE_MAX_CHARS: usize = 36;
pub const META_MAX_CHARS: usize = 40;

// chronicle recording
pub const RING_BUFFER_CAPACITY: usize = 96_000; // 1 second of stereo i16
pub const STEREO_FRAME_SAMPLES: usize = 1_920; // 960 samples/channel
pub const MONO_FRAME_SAMPLES: usize = 960; // number of samples in a mono frame of audio
pub const MAX_OPUS_PACKET_SIZE: usize = 4_000; // max size of finalised opus packet
pub const PCM_CHANNELS: usize = 2; // 2 is stereo
pub const SAMPLE_RATE: u32 = 48_000; // sample frequency
pub const SILENCE_FRAME: [i16; STEREO_FRAME_SAMPLES] = [0; STEREO_FRAME_SAMPLES];

// transcription
pub const MODEL_ID: &str = "distil-whisper/distil-large-v3";
pub const MODEL_REVISION: &str = "main";
// pub const MODEL_ID: &str = "distil-whisper/distil-medium.en";
// pub const MODEL_REVISION: &str = "main";
// pub const MODEL_ID: &str = "openai/whisper-small.en";
// pub const MODEL_REVISION: &str = "refs/pr/10";
pub const MODEL_SAMPLE_RATE: u32 = 16_000;
pub const TRANSCRIPT_PAGE_LIMIT: usize = 1900;

// project paths
pub const PROJECT_ROOT: &str = env!("CARGO_MANIFEST_DIR");
pub const RECORDINGS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.chronicle/recordings");

// library sync
pub const AUDIO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/audio");
pub const DOWNLOAD_CONCURRENCY: usize = 4;
pub const MAX_RETRIES: usize = 3;
pub const YTDLP_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/yt-dlp");
pub const COOKIES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/cookies.txt");

// autocomplete
pub const AUTOCOMPLETE_MAX_CHOICES: usize = 25; // capped by Discord
pub const AUTOCOMPLETE_MAX_LENGTH: usize = 100;
pub const AUTOCOMPLETE_SEPARATOR: &str = " | ";
pub const AUTOCOMPLETE_SEPARATOR_LEN: usize = AUTOCOMPLETE_SEPARATOR.len();

// discord
pub const DISCORD_MESSAGE_MAX_CHARS: usize = 2_000;
