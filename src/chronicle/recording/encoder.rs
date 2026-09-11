use std::{fs::File, path::Path};

use ogg::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Channels, Encoder as OpusEncoder};
use rtrb::Consumer;
use serenity::all::UserId;
use tokio::sync::oneshot;

use crate::{
    chronicle::recording::constants::RecordedFrame,
    chronicle::recording::constants::{
        MAX_OPUS_PACKET_SIZE, MONO_FRAME_SAMPLES, OPUS_SAMPLE_RATE, PCM_CHANNELS,
        STEREO_FRAME_SAMPLES,
    },
    discord::context::Error,
};

struct EncoderState {
    opus: OpusEncoder,
    mono_buffer: [i16; MONO_FRAME_SAMPLES],
    opus_packet: [u8; MAX_OPUS_PACKET_SIZE],
    granule_position: u64,
    encoded_packets: Vec<(Vec<u8>, u64)>,
}

pub fn run_encoder(
    user_id: UserId,
    path: &Path,
    consumer: Consumer<RecordedFrame>,
    stop_rx: oneshot::Receiver<u64>,
    initial_silence_ticks: u64,
) -> Result<(), Error> {
    let file = File::create(path)?;
    let mut ogg = PacketWriter::new(file);

    let mut opus = OpusEncoder::new(
        u32::try_from(OPUS_SAMPLE_RATE)?,
        Channels::Mono,
        Application::Audio,
    )?;

    let serial = rand::random::<u32>();

    let pre_skip = u16::try_from(opus.get_lookahead()?)?;

    write_opus_headers(
        &mut ogg,
        serial,
        user_id,
        u32::try_from(OPUS_SAMPLE_RATE)?,
        pre_skip,
    )?;

    let mut state = EncoderState {
        opus,
        mono_buffer: [0; MONO_FRAME_SAMPLES],
        opus_packet: [0; MAX_OPUS_PACKET_SIZE],
        granule_position: 0,
        encoded_packets: Vec::new(),
    };
    encode_initial_silence(initial_silence_ticks, &mut state)?;
    let (next_tick, final_tick) = drain_recording_frames(
        user_id,
        consumer,
        stop_rx,
        initial_silence_ticks,
        &mut state,
    )?;
    pad_final_silence(final_tick, next_tick, &mut state)?;
    write_audio_packets(&mut ogg, serial, state.encoded_packets)?;

    tracing::info!(?user_id, ?path, "Finished recording");

    Ok(())
}

fn encode_initial_silence(ticks: u64, state: &mut EncoderState) -> Result<(), Error> {
    for _ in 0..ticks {
        encode_silence_frame(state)?;
    }
    Ok(())
}

fn drain_recording_frames(
    user_id: UserId,
    mut consumer: Consumer<RecordedFrame>,
    mut stop_rx: oneshot::Receiver<u64>,
    initial_silence_ticks: u64,
    state: &mut EncoderState,
) -> Result<(u64, Option<u64>), Error> {
    let mut stopping = false;
    let mut final_tick = None;
    let mut next_tick = initial_silence_ticks;

    loop {
        while let Ok(chunk) = consumer.read_chunk(1) {
            let (first, second) = chunk.as_slices();
            let frame = first.first().copied().or_else(|| second.first().copied());
            chunk.commit_all();

            let Some(frame) = frame else {
                continue;
            };

            while next_tick < frame.tick {
                encode_silence_frame(state)?;
                next_tick += 1;
            }

            if frame.tick < next_tick {
                tracing::warn!(
                    ?user_id,
                    frame_tick = frame.tick,
                    expected_tick = next_tick,
                    "Ignoring out-of-order recording frame"
                );
                continue;
            }

            downmix_stereo_frame(&frame.samples, &mut state.mono_buffer);
            encode_mono_frame(state)?;
            next_tick += 1;
        }

        if stopping {
            break;
        }

        match stop_rx.try_recv() {
            Ok(tick) => {
                final_tick = Some(tick);
                stopping = true;
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                stopping = true;
            }
            Err(oneshot::error::TryRecvError::Empty) => {
                std::thread::yield_now();
            }
        }
    }

    Ok((next_tick, final_tick))
}

fn pad_final_silence(
    final_tick: Option<u64>,
    mut next_tick: u64,
    state: &mut EncoderState,
) -> Result<(), Error> {
    if let Some(final_tick) = final_tick {
        while next_tick < final_tick {
            encode_silence_frame(state)?;
            next_tick += 1;
        }
    }
    Ok(())
}

fn write_audio_packets<W: std::io::Write>(
    ogg: &mut PacketWriter<W>,
    serial: u32,
    encoded_packets: Vec<(Vec<u8>, u64)>,
) -> std::io::Result<()> {
    let packet_count = encoded_packets.len();
    for (index, (packet, granule_position)) in encoded_packets.into_iter().enumerate() {
        let end_info = if index + 1 == packet_count {
            PacketWriteEndInfo::EndStream
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        ogg.write_packet(packet, serial, end_info, granule_position)?;
    }
    Ok(())
}

fn encode_mono_frame(state: &mut EncoderState) -> Result<(), Error> {
    let encoded_len = state
        .opus
        .encode(&state.mono_buffer, &mut state.opus_packet)?;

    if encoded_len > 0 {
        state.granule_position += MONO_FRAME_SAMPLES as u64;
        state.encoded_packets.push((
            state.opus_packet[..encoded_len].to_vec(),
            state.granule_position,
        ));
    }

    Ok(())
}

fn encode_silence_frame(state: &mut EncoderState) -> Result<(), Error> {
    state.mono_buffer.fill(0);
    encode_mono_frame(state)
}

fn downmix_stereo_frame(interleaved: &[i16], mono: &mut [i16; MONO_FRAME_SAMPLES]) {
    debug_assert_eq!(interleaved.len(), STEREO_FRAME_SAMPLES);

    for (index, pair) in interleaved.chunks_exact(PCM_CHANNELS).enumerate() {
        mono[index] = i16::midpoint(pair[0], pair[1]);
    }
}

fn write_opus_headers<W: std::io::Write>(
    ogg: &mut PacketWriter<W>,
    serial: u32,
    user_id: UserId,
    sample_rate: u32,
    pre_skip: u16,
) -> std::io::Result<()> {
    let mut opus_head = Vec::with_capacity(19);

    opus_head.extend_from_slice(b"OpusHead");
    opus_head.push(1);
    opus_head.push(1);
    opus_head.extend_from_slice(&pre_skip.to_le_bytes());
    opus_head.extend_from_slice(&sample_rate.to_le_bytes());
    opus_head.extend_from_slice(&0i16.to_le_bytes());
    opus_head.push(0);

    ogg.write_packet(opus_head, serial, PacketWriteEndInfo::EndPage, 0)?;

    let vendor = b"chronicle";

    let comment = format!("USER_ID={user_id}");

    let mut opus_tags = Vec::new();

    opus_tags.extend_from_slice(b"OpusTags");
    let vendor_len = u32::try_from(vendor.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Opus vendor string is too long",
        )
    })?;
    opus_tags.extend_from_slice(&vendor_len.to_le_bytes());
    opus_tags.extend_from_slice(vendor);
    opus_tags.extend_from_slice(&1u32.to_le_bytes());
    let comment_len = u32::try_from(comment.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Opus comment is too long")
    })?;
    opus_tags.extend_from_slice(&comment_len.to_le_bytes());
    opus_tags.extend_from_slice(comment.as_bytes());

    ogg.write_packet(opus_tags, serial, PacketWriteEndInfo::EndPage, 0)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{downmix_stereo_frame, run_encoder};
    use crate::chronicle::recording::constants::{
        MONO_FRAME_SAMPLES, RecordedFrame, STEREO_FRAME_SAMPLES,
    };
    use ogg::PacketReader;
    use rtrb::RingBuffer;
    use serenity::all::UserId;
    use std::{fs::File, io::BufReader};
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    #[test]
    fn downmixes_stereo_pairs_with_midpoint() {
        let mut stereo = [0i16; STEREO_FRAME_SAMPLES];
        stereo[0] = 100;
        stereo[1] = 300;
        stereo[2] = -100;
        stereo[3] = -300;
        stereo[4] = i16::MIN;
        stereo[5] = i16::MAX;
        let mut mono = [0i16; MONO_FRAME_SAMPLES];

        downmix_stereo_frame(&stereo, &mut mono);

        assert_eq!(mono[0], 200);
        assert_eq!(mono[1], -200);
        assert_eq!(mono[2], 0);
        assert!(mono[3..].iter().all(|sample| *sample == 0));
    }

    fn frame(tick: u64, value: i16) -> RecordedFrame {
        let mut samples = [0; STEREO_FRAME_SAMPLES];
        samples[0] = value;
        samples[1] = value;
        RecordedFrame { tick, samples }
    }

    fn encode_test_file(frames: &[RecordedFrame], final_tick: u64) -> anyhow::Result<usize> {
        let directory = tempdir()?;
        let path = directory.path().join("recording.opus");
        let (mut producer, consumer) = RingBuffer::new(frames.len().max(1));

        for frame in frames {
            let mut chunk = producer.write_chunk(1)?;
            let (first, second) = chunk.as_mut_slices();
            if let Some(slot) = first.first_mut() {
                *slot = *frame;
            } else if let Some(slot) = second.first_mut() {
                *slot = *frame;
            }
            chunk.commit_all();
        }

        let (stop_tx, stop_rx) = oneshot::channel();
        stop_tx
            .send(final_tick)
            .map_err(|tick| anyhow::anyhow!("failed to send final tick {tick}"))?;
        run_encoder(UserId::new(1), &path, consumer, stop_rx, 0)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        let mut packets = PacketReader::new(BufReader::new(File::open(path)?));
        let mut audio_packets = 0;
        while let Some(packet) = packets.read_packet()? {
            if !packet.data.starts_with(b"OpusHead") && !packet.data.starts_with(b"OpusTags") {
                audio_packets += 1;
            }
        }

        Ok(audio_packets)
    }

    #[test]
    fn fills_interior_dropped_ticks_with_silence() -> anyhow::Result<()> {
        let samples = encode_test_file(&[frame(0, 100), frame(2, 200)], 3)?;

        assert_eq!(samples, 3);
        Ok(())
    }

    #[test]
    fn pads_trailing_dropped_ticks_until_session_end() -> anyhow::Result<()> {
        let samples = encode_test_file(&[frame(0, 100)], 3)?;

        assert_eq!(samples, 3);
        Ok(())
    }

    #[test]
    fn participants_with_different_drops_keep_equal_timelines() -> anyhow::Result<()> {
        let participant_with_drop = encode_test_file(&[frame(0, 100), frame(4, 200)], 5)?;
        let participant_without_drop = encode_test_file(
            &[
                frame(0, 100),
                frame(1, 100),
                frame(2, 100),
                frame(3, 100),
                frame(4, 200),
            ],
            5,
        )?;

        assert_eq!(participant_with_drop, participant_without_drop);
        assert_eq!(participant_with_drop, 5);
        Ok(())
    }
}
