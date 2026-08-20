use std::sync::Arc;
use serenity_voice_model::id::UserId;
use songbird::events::{Event, EventContext, EventHandler};

use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
};

use ogg::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Channels, Encoder as OpusEncoder};
use rtrb::{Consumer, Producer, RingBuffer};
use serenity::model::{
    id::{ChannelId, GuildId},
};
use tokio::{
    sync::{oneshot, Mutex},
    task::JoinHandle,
};

use crate::definitions::Error;

const RING_BUFFER_CAPACITY: usize = 96_000; // 1 second of stereo i16
const STEREO_FRAME_SAMPLES: usize = 1_920;  // 960 samples/channel
const MONO_FRAME_SAMPLES: usize = 960;
const MAX_OPUS_PACKET_SIZE: usize = 4_000;

pub struct RecordingSession {
    pub users: HashMap<UserId, UserRecording>,
}

pub struct UserRecording {
    pub producer: Producer<i16>,
    pub stop_tx: oneshot::Sender<()>,
    pub encoder: JoinHandle<Result<(), Error>>,
}

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

fn recording_path(user_id: UserId) -> PathBuf {
    PathBuf::from(format!(
        ".chronicle/audio/recording-{}.opus",
        user_id
    ))
}

fn ensure_recording_directory() -> Result<(), std::io::Error> {
    std::fs::create_dir_all(".chronicle/audio")
}

#[derive(Clone)]
pub struct Recorder {
    pub id: u64,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, UserId>>>,
    pub recording: Arc<Mutex<Option<RecordingSession>>>,
    pub audio_config: SongbirdPcmConfig,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            id: rand::random(),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
            recording: Arc::new(Mutex::new(None)),
            audio_config: SongbirdPcmConfig::default(),
        }
    }

    pub async fn start_recording(&self) -> Result<bool, Error> {
        ensure_recording_directory()?;

        let mut recording = self.recording.lock().await;

        if recording.is_some() {
            return Ok(false);
        }

        *recording = Some(RecordingSession {
            users: HashMap::new(),
        });

        tracing::info!("Recording started");

        Ok(true)
    }

    pub async fn stop_recording(&self) -> Result<bool, Error> {
        let session = {
            let mut recording = self.recording.lock().await;

            let Some(session) = recording.take() else {
                return Ok(false);
            };

            session
        };

        tracing::info!(
            users = session.users.len(),
            "Stopping recording"
        );

        for (_, user_recording) in session.users {
            let UserRecording {
                producer,
                stop_tx,
                encoder,
            } = user_recording;

            // Tell the encoder that no more data should be expected.
            let _ = stop_tx.send(());

            // The producer must remain alive while the encoder drains the
            // samples already committed to the ring buffer. Once the
            // encoder has been told to stop, dropping the producer is safe.
            drop(producer);

            match encoder.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(%error, "User recording encoder failed");
                }
                Err(error) => {
                    tracing::error!(%error, "User recording encoder task failed");
                }
            }
        }

        tracing::info!("Recording stopped");

        Ok(true)
    }

    pub async fn is_recording(&self) -> bool {
        self.recording.lock().await.is_some()
    }

    async fn create_user_recording(
        &self,
        user_id: UserId,
    ) -> Result<UserRecording, Error> {
        let (producer, consumer) =
            RingBuffer::<i16>::new(RING_BUFFER_CAPACITY);

        let (stop_tx, stop_rx) = oneshot::channel();

        let audio_config = self.audio_config.clone();

        let path = recording_path(user_id);

        let encoder = tokio::task::spawn_blocking(move || {
            run_encoder(
                user_id,
                path,
                consumer,
                stop_rx,
                audio_config,
            )
        });

        Ok(UserRecording {
            producer,
            stop_tx,
            encoder,
        })
    }

}

#[async_trait::async_trait]
impl EventHandler for Recorder {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {

        match ctx {
            EventContext::SpeakingStateUpdate(state) => {

                if let Some(user_id) = state.user_id {
                    let mut mappings = self.ssrc_to_user.lock().await;

                    mappings.insert(state.ssrc, user_id);
                }
            }

            EventContext::VoiceTick(tick) => {
                for (&ssrc, voice_data) in &tick.speaking {
                    let Some(audio) = &voice_data.decoded_voice else {
                        continue;
                    };

                    let user_id = {
                        let mappings = self.ssrc_to_user.lock().await;
                        mappings.get(&ssrc).copied()
                    };

                    let Some(user_id) = user_id else {
                        continue;
                    };

                    let mut recording = self.recording.lock().await;

                    let Some(session) = recording.as_mut() else {
                        // Bot is receiving voice, but recording hasn't been
                        // requested.
                        continue;
                    };

                    if !session.users.contains_key(&user_id) {
                        let user_recording = match self.create_user_recording(user_id).await {
                            Ok(recording) => recording,

                            Err(error) => {
                                tracing::error!(
                                    %error,
                                    ?user_id,
                                    "Failed to create user recording"
                                );

                                continue;
                            }
                        };

                        session.users.insert(user_id, user_recording);

                        tracing::info!(
                            ?user_id,
                            "Started recording user"
                        );
                    }

                    let user_recording = session
                        .users
                        .get_mut(&user_id)
                        .expect("user recording was just inserted");

                    if user_recording.producer.slots() < audio.len() {
                        tracing::warn!(
                            ?user_id,
                            available = user_recording.producer.slots(),
                            required = audio.len(),
                            "Recording ring buffer full; dropping PCM frame"
                        );

                        continue;
                    }

                    match user_recording.producer.write_chunk(audio.len()) {
                        Ok(mut chunk) => {
                            let (first, second) = chunk.as_mut_slices();

                            let first_len = first.len();

                            first.copy_from_slice(&audio[..first_len]);

                            if !second.is_empty() {
                                second.copy_from_slice(&audio[first_len..]);
                            }

                            chunk.commit_all();
                        }

                        Err(error) => {
                            tracing::warn!(
                                ?user_id,
                                ?error,
                                "Failed to write PCM to recording ring buffer"
                            );
                        }
                    }
                }
            }

            _ => {}
        }

        None
    }
}

fn downmix_stereo_frame(
    interleaved: &[i16],
    mono: &mut [i16; MONO_FRAME_SAMPLES],
) {
    debug_assert_eq!(interleaved.len(), STEREO_FRAME_SAMPLES);

    for (index, pair) in interleaved.chunks_exact(2).enumerate() {
        let left = pair[0] as i32;
        let right = pair[1] as i32;

        mono[index] = ((left + right) / 2) as i16;
    }
}

fn run_encoder(
    user_id: UserId,
    path: PathBuf,
    mut consumer: Consumer<i16>,
    mut stop_rx: oneshot::Receiver<()>,
    config: SongbirdPcmConfig,
) -> Result<(), Error> {
    let file = File::create(&path)?;
    let mut ogg = PacketWriter::new(file);

    let mut opus = OpusEncoder::new(
        config.sample_rate,
        Channels::Mono,
        Application::Audio,
    )?;

    let mut stereo_buffer = Vec::<i16>::with_capacity(STEREO_FRAME_SAMPLES);
    let mut mono_buffer = [0i16; MONO_FRAME_SAMPLES];

    let mut opus_packet = [0u8; MAX_OPUS_PACKET_SIZE];

    let mut granule_position = 0u64;
    let serial = rand::random::<u32>();

    write_opus_headers(
        &mut ogg,
        serial,
        user_id,
        config.sample_rate,
    )?;

    let mut stopping = false;

    loop {
        while stereo_buffer.len() < STEREO_FRAME_SAMPLES {
            match consumer.read_chunk(
                STEREO_FRAME_SAMPLES - stereo_buffer.len()
            ) {
                Ok(chunk) => {
                    let (first, second) = chunk.as_slices();

                    stereo_buffer.extend_from_slice(first);
                    stereo_buffer.extend_from_slice(second);

                    chunk.commit_all();
                }

                Err(_) => break,
            }
        }

        while stereo_buffer.len() >= STEREO_FRAME_SAMPLES {
            let frame = &stereo_buffer[..STEREO_FRAME_SAMPLES];

            downmix_stereo_frame(frame, &mut mono_buffer);

            let encoded_len =
                opus.encode(&mono_buffer, &mut opus_packet)?;

            if encoded_len > 0 {
                let packet = opus_packet[..encoded_len].to_vec();

                granule_position += MONO_FRAME_SAMPLES as u64;

                ogg.write_packet(
                    packet,
                    serial,
                    PacketWriteEndInfo::NormalPacket,
                    granule_position,
                )?;
            }

            stereo_buffer.drain(..STEREO_FRAME_SAMPLES);
        }

        if stopping {
            // Producer has stopped, so no more samples can arrive.
            // We've drained everything available.
            break;
        }

        match stop_rx.try_recv() {
            Ok(()) => {
                stopping = true;
            }

            Err(oneshot::error::TryRecvError::Empty) => {
                std::thread::yield_now();
            }

            Err(oneshot::error::TryRecvError::Closed) => {
                stopping = true;
            }
        }
    }

    // At this point, stop has been requested and the producer is no
    // longer supplying data. Any complete frames already in the ring
    // have been processed above.
    //
    // We intentionally discard an incomplete final frame because
    // Opus requires a valid frame size.
    
    tracing::info!(
        ?user_id,
        ?path,
        "Finished recording"
    );

    Ok(())
}

fn write_opus_headers<W: std::io::Write>(
    ogg: &mut PacketWriter<W>,
    serial: u32,
    user_id: UserId,
    sample_rate: u32,
) -> std::io::Result<()> {
    let mut opus_head = Vec::with_capacity(19);

    opus_head.extend_from_slice(b"OpusHead");
    opus_head.push(1); // OpusHead version

    opus_head.push(1); // channel count: mono

    opus_head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip

    opus_head.extend_from_slice(&sample_rate.to_le_bytes());

    opus_head.extend_from_slice(&0i16.to_le_bytes()); // output gain

    opus_head.push(0); // channel mapping family

    ogg.write_packet(
        opus_head,
        serial,
        PacketWriteEndInfo::EndPage,
        0,
    )?;

    let vendor = b"chronicle";

    let comment = format!("USER_ID={}", user_id);

    let mut opus_tags = Vec::new();

    opus_tags.extend_from_slice(b"OpusTags");
    opus_tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    opus_tags.extend_from_slice(vendor);
    opus_tags.extend_from_slice(&1u32.to_le_bytes());
    opus_tags.extend_from_slice(&(comment.len() as u32).to_le_bytes());
    opus_tags.extend_from_slice(comment.as_bytes());

    ogg.write_packet(
        opus_tags,
        serial,
        PacketWriteEndInfo::EndPage,
        0,
    )?;

    Ok(())
}