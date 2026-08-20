use std::collections::HashMap;
use std::sync::Arc;
use std::path::Path;

use serenity_voice_model::id::UserId;
use songbird::events::{Event, EventContext, EventHandler};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct SongbirdPcmConfig {
    pub sample_rate: u32,
    pub channels: u16,
}

impl Default for SongbirdPcmConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
        }
    }
}

#[derive(Clone)]
pub struct Recorder {
    pub id: u64,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, UserId>>>,
    pub recordings: Arc<Mutex<HashMap<UserId, Vec<i16>>>>,
    pub audio_config: SongbirdPcmConfig,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            id: rand::random(),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
            recordings: Arc::new(Mutex::new(HashMap::new())),
            audio_config: SongbirdPcmConfig::default(),
        }
    }

    pub async fn save_user_recording(
        &self,
        user_id: UserId,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), hound::Error> {
        let recordings = self.recordings.lock().await;

        let Some(samples) = recordings.get(&user_id) else {
            return Ok(());
        };

        write_wav(path, samples, &self.audio_config)?;

        Ok(())
    }

    pub async fn recorded_users(&self) -> Vec<UserId> {
        let recordings = self.recordings.lock().await;

        recordings.keys().copied().collect()
    }

    pub async fn recording_count(&self) -> usize {
        self.recordings.lock().await.len()
    }

    pub async fn save_all_recordings_and_clear_memory(&self) -> Result<(), hound::Error> {
        let mut recordings = self.recordings.lock().await;

        for (&user_id, samples) in recordings.iter() {
            let path = format!(".chronicle/audio/recording-{}.wav", user_id);

            write_wav(path, samples, &self.audio_config)?;
        }

        recordings.clear();

        Ok(())
    }

}

#[async_trait::async_trait]
impl EventHandler for Recorder {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        println!("RECORDER EVENT");

        match ctx {
            EventContext::SpeakingStateUpdate(state) => {
                println!("SPEAKING STATE");

                if let Some(user_id) = state.user_id {
                    let mut mappings = self.ssrc_to_user.lock().await;

                    mappings.insert(state.ssrc, user_id);
                }
            }

            EventContext::VoiceTick(tick) => {
                println!(
                    "VOICE TICK: speaking={}, silent={}",
                    tick.speaking.len(),
                    tick.silent.len()
                );

                for (&ssrc, voice_data) in &tick.speaking {
                    println!("VOICE: processing SSRC {}", ssrc);

                    let Some(audio) = &voice_data.decoded_voice else {
                        println!(
                            "VOICE: SSRC {} has no decoded audio",
                            ssrc
                        );
                        continue;
                    };

                    println!(
                        "VOICE: SSRC {} has {} samples",
                        ssrc,
                        audio.len()
                    );

                    let user_id = {
                        let mappings = self.ssrc_to_user.lock().await;

                        println!(
                            "VOICE: currently have {} SSRC mappings",
                            mappings.len()
                        );

                        mappings.get(&ssrc).copied()
                    };

                    let Some(user_id) = user_id else {
                        println!(
                            "VOICE: SSRC {} has no user mapping",
                            ssrc
                        );
                        continue;
                    };

                    println!(
                        "VOICE: SSRC {} maps to user {}",
                        ssrc,
                        user_id
                    );

                    let mut recordings = self.recordings.lock().await;

                    let recording = recordings
                        .entry(user_id)
                        .or_default();

                    recording.extend_from_slice(audio);

                    println!(
                        "VOICE: user {} now has {} samples",
                        user_id,
                        recording.len()
                    );
                }
            }

            _ => {}
        }

        None
    }
}

pub fn write_wav(
    path: impl AsRef<Path>,
    samples: &[i16],
    config: &SongbirdPcmConfig,
) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: config.channels,
        sample_rate: config.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(path, spec)?;

    for &sample in samples {
        writer.write_sample(sample)?;
    }

    writer.finalize()?;

    Ok(())
}


fn downmix_stereo_to_mono(interleaved: &[i16]) -> Vec<i16> {
    let (chunks, remainder) = interleaved.as_chunks::<2>();

    debug_assert!(
        remainder.is_empty(),
        "interleaved stereo buffer had an odd number of samples"
    );

    chunks
        .iter()
        .map(|&[l, r]| ((l as i32 + r as i32) / 2) as i16)
        .collect()
}