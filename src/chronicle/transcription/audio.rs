use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    path::Path,
};

use anyhow::{Result, anyhow};
use ogg::PacketReader;
use opus::{Channels, Decoder as OpusDecoder};
use rubato::{FftFixedIn, Resampler};

const OPUS_SAMPLE_RATE: usize = 48_000;
const WHISPER_SAMPLE_RATE: usize = 16_000;

/// Decoded audio ready for Whisper.
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Decode an Ogg/Opus recording into mono 16 kHz f32 PCM.
///
/// The returned samples are suitable for Candle Whisper.
pub fn load_opus(path: impl AsRef<Path>) -> Result<Audio> {
    let file = File::open(path)?;

    decode_ogg_opus(BufReader::new(file))
}

fn decode_ogg_opus<R>(reader: R) -> Result<Audio>
where
    R: Read + Seek,
{
    let mut packets = PacketReader::new(reader);

    let mut decoder: Option<OpusDecoder> = None;
    let mut pre_skip = 0usize;
    let mut decoded = Vec::<f32>::new();

    while let Some(packet) = packets.read_packet()? {
        let data = packet.data.as_slice();

        if data.starts_with(b"OpusHead") {
            let header = parse_opus_head(data)?;

            if header.channels != 1 {
                return Err(anyhow!(
                    "Expected mono Opus recording, got {} channels",
                    header.channels
                ));
            }

            pre_skip = header.pre_skip as usize;

            decoder = Some(OpusDecoder::new(OPUS_SAMPLE_RATE as u32, Channels::Mono)?);

            continue;
        }

        if data.starts_with(b"OpusTags") {
            continue;
        }

        let Some(decoder) = decoder.as_mut() else {
            return Err(anyhow!("Encountered Opus audio packet before OpusHead"));
        };

        // Opus permits up to 120 ms per packet at 48 kHz.
        let mut pcm = [0i16; OPUS_SAMPLE_RATE * 120 / 1000];
        let samples = decoder.decode(data, &mut pcm, false)?;

        decoded.extend(pcm[..samples].iter().map(|&sample| f32::from(sample) / 32768.0));
    }

    if decoder.is_none() {
        return Err(anyhow!("Ogg stream does not contain an OpusHead"));
    }

    // OpusHead's pre-skip is expressed in samples at the decoder's
    // 48 kHz output rate.
    if pre_skip > decoded.len() {
        return Err(anyhow!(
            "Opus pre-skip ({pre_skip}) exceeds decoded audio length ({})",
            decoded.len()
        ));
    }

    decoded.drain(..pre_skip);

    // tracing::debug!(
    //     first = ?decoded.iter().take(10).collect::<Vec<_>>(),
    //     peak = decoded
    //         .iter()
    //         .copied()
    //         .map(f32::abs)
    //         .fold(0.0, f32::max),
    //     "Audio before resampling"
    // );

    let samples = if OPUS_SAMPLE_RATE == WHISPER_SAMPLE_RATE {
        decoded
    } else {
        resample_48k_to_16k(&decoded)?
    };

    // {
    //     let min = samples.iter().copied().fold(f32::INFINITY, f32::min);
    //     let max = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    //     let rms = (
    //         samples
    //             .iter()
    //             .map(|x| (*x as f64) * (*x as f64))
    //             .sum::<f64>()
    //             / samples.len().max(1) as f64
    //     ).sqrt();

    //     tracing::debug!(
    //         samples = samples.len(),
    //         min,
    //         max,
    //         rms,
    //         "Audio after resampling"
    //     );
    // }

    Ok(Audio {
        samples,
        sample_rate: WHISPER_SAMPLE_RATE as u32,
    })
}

struct OpusHead {
    channels: u8,
    pre_skip: u16,
}

fn parse_opus_head(data: &[u8]) -> Result<OpusHead> {
    if data.len() < 19 || &data[..8] != b"OpusHead" {
        return Err(anyhow!("Invalid OpusHead"));
    }

    let version = data[8];

    if version != 1 {
        return Err(anyhow!("Unsupported OpusHead version: {version}"));
    }

    let channels = data[9];
    let pre_skip = u16::from_le_bytes([data[10], data[11]]);

    Ok(OpusHead { channels, pre_skip })
}

fn resample_48k_to_16k(input: &[f32]) -> Result<Vec<f32>> {
    let mut resampler = FftFixedIn::<f32>::new(OPUS_SAMPLE_RATE, WHISPER_SAMPLE_RATE, 1024, 1, 1)?;

    let mut output = Vec::with_capacity(input.len() * WHISPER_SAMPLE_RATE / OPUS_SAMPLE_RATE);

    let mut offset = 0;

    while offset < input.len() {
        let remaining = input.len() - offset;
        let chunk_len = remaining.min(1024);

        let mut chunk = vec![0.0f32; 1024];
        chunk[..chunk_len].copy_from_slice(&input[offset..offset + chunk_len]);

        let result = resampler.process(&[chunk], None)?;

        output.extend_from_slice(&result[0]);

        offset += chunk_len;
    }

    // The final chunk may have been zero-padded to the required input size.
    // Trim the output to the expected resampled length.
    let expected_len = input.len() * WHISPER_SAMPLE_RATE / OPUS_SAMPLE_RATE;

    output.truncate(expected_len);

    Ok(output)
}
