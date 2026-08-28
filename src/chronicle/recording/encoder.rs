use std::{fs::File, path::PathBuf};

use ogg::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Channels, Encoder as OpusEncoder};
use rtrb::Consumer;
use serenity::all::UserId;
use tokio::sync::oneshot;

use crate::{
    constants::{
        MAX_OPUS_PACKET_SIZE, MONO_FRAME_SAMPLES, PCM_CHANNELS, SAMPLE_RATE, STEREO_FRAME_SAMPLES,
    },
    discord::context::Error,
};

pub fn run_encoder(
    user_id: UserId,
    path: PathBuf,
    mut consumer: Consumer<i16>,
    mut stop_rx: oneshot::Receiver<()>,
    initial_silence_ticks: u64,
) -> Result<(), Error> {
    let file = File::create(&path)?;
    let mut ogg = PacketWriter::new(file);

    let mut opus = OpusEncoder::new(SAMPLE_RATE, Channels::Mono, Application::Audio)?;

    let mut stereo_buffer = Vec::<i16>::with_capacity(STEREO_FRAME_SAMPLES);
    let mut mono_buffer = [0i16; MONO_FRAME_SAMPLES];

    let mut opus_packet = [0u8; MAX_OPUS_PACKET_SIZE];

    let mut granule_position = 0u64;
    let serial = rand::random::<u32>();

    let pre_skip = u16::try_from(opus.get_lookahead()?)?;

    write_opus_headers(&mut ogg, serial, user_id, SAMPLE_RATE, pre_skip)?;

    for _ in 0..initial_silence_ticks {
        mono_buffer.fill(0);

        let encoded_len = opus.encode(&mono_buffer, &mut opus_packet)?;

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
    }

    let mut stopping = false;

    loop {
        while stereo_buffer.len() < STEREO_FRAME_SAMPLES {
            match consumer.read_chunk(STEREO_FRAME_SAMPLES - stereo_buffer.len()) {
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

            let encoded_len = opus.encode(&mono_buffer, &mut opus_packet)?;

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

    tracing::info!(?user_id, ?path, "Finished recording");

    Ok(())
}

fn downmix_stereo_frame(interleaved: &[i16], mono: &mut [i16; MONO_FRAME_SAMPLES]) {
    debug_assert_eq!(interleaved.len(), STEREO_FRAME_SAMPLES);

    for (index, pair) in interleaved.chunks_exact(PCM_CHANNELS).enumerate() {
        let left = i32::from(pair[0]);
        let right = i32::from(pair[1]);

        mono[index] = i32::midpoint(left, right) as i16;
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
    opus_tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    opus_tags.extend_from_slice(vendor);
    opus_tags.extend_from_slice(&1u32.to_le_bytes());
    opus_tags.extend_from_slice(&(comment.len() as u32).to_le_bytes());
    opus_tags.extend_from_slice(comment.as_bytes());

    ogg.write_packet(opus_tags, serial, PacketWriteEndInfo::EndPage, 0)?;

    Ok(())
}
