// for library display
pub const ELLIPSIS: &str = "…";
pub const ELLIPSIS_LEN: usize = ELLIPSIS.len();
pub const ELLIPSIS_DISPLAY_WIDTH: usize = 1;

// for chronicle recording
pub const RING_BUFFER_CAPACITY: usize = 96_000; // 1 second of stereo i16
pub const STEREO_FRAME_SAMPLES: usize = 1_920;  // 960 samples/channel
pub const MONO_FRAME_SAMPLES: usize = 960;      // number of samples in a mono frame of audio
pub const MAX_OPUS_PACKET_SIZE: usize = 4_000;  // max size of finalised opus packet
pub const PCM_CHANNELS: usize = 2;                 // 2 is stereo
pub const SAMPLE_RATE: u32 = 48_000;             // sample frequency