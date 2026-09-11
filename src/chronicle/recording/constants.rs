// 2,604 complete 20 ms stereo frames: approximately 10 MB (9,999,360 bytes)
// of per-participant PCM buffering, or about 104 seconds at 48 kHz.
pub const RING_BUFFER_CAPACITY: usize = 2_604;
pub const STEREO_FRAME_SAMPLES: usize = 1_920;
pub const MONO_FRAME_SAMPLES: usize = 960;
pub const MAX_OPUS_PACKET_SIZE: usize = 4_000;
pub const PCM_CHANNELS: usize = 2;
pub const OPUS_SAMPLE_RATE: usize = 48_000;
pub const SILENCE_FRAME: [i16; STEREO_FRAME_SAMPLES] = [0; STEREO_FRAME_SAMPLES];

#[derive(Clone, Copy, Debug)]
pub struct RecordedFrame {
    pub tick: u64,
    pub samples: [i16; STEREO_FRAME_SAMPLES],
}

impl Default for RecordedFrame {
    fn default() -> Self {
        Self {
            tick: 0,
            samples: [0; STEREO_FRAME_SAMPLES],
        }
    }
}
