use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    path::Path,
};

use anyhow::{Result, anyhow};
use ogg::PacketReader;
use opus::{Channels, Decoder as OpusDecoder};
use rubato::{FftFixedIn, Resampler};

use crate::chronicle::recording::constants::OPUS_SAMPLE_RATE;

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
    let mut decoded_samples = Vec::<f32>::new();

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

            decoder = Some(OpusDecoder::new(
                u32::try_from(OPUS_SAMPLE_RATE)?,
                Channels::Mono,
            )?);

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

        decoded_samples.extend(
            pcm[..samples]
                .iter()
                .map(|&sample| f32::from(sample) / 32768.0),
        );
    }

    if decoder.is_none() {
        return Err(anyhow!("Ogg stream does not contain an OpusHead"));
    }

    // OpusHead's pre-skip is expressed in samples at the decoder's
    // 48 kHz output rate.
    if pre_skip > decoded_samples.len() {
        return Err(anyhow!(
            "Opus pre-skip ({pre_skip}) exceeds decoded audio length ({})",
            decoded_samples.len()
        ));
    }

    decoded_samples.drain(..pre_skip);

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
        decoded_samples
    } else {
        resample_48k_to_16k(&decoded_samples)?
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
        sample_rate: u32::try_from(WHISPER_SAMPLE_RATE)?,
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
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let mut resampler = FftFixedIn::<f32>::new(OPUS_SAMPLE_RATE, WHISPER_SAMPLE_RATE, 1024, 1, 1)?;
    let output_delay = resampler.output_delay();

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

    // Flush enough zero-padded frames to recover samples held by the FFT overlap.
    let expected_len = input.len() * WHISPER_SAMPLE_RATE / OPUS_SAMPLE_RATE;
    while output.len() < output_delay + expected_len {
        let result = resampler.process_partial::<Vec<f32>>(None, None)?;
        if result[0].is_empty() {
            return Err(anyhow!("Resampler produced no output while flushing"));
        }
        output.extend_from_slice(&result[0]);
    }

    output.drain(..output_delay);
    output.truncate(expected_len);

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{parse_opus_head, resample_48k_to_16k};

    fn header(version: u8, channels: u8, pre_skip: u16) -> Vec<u8> {
        let mut bytes = b"OpusHead".to_vec();
        bytes.push(version);
        bytes.push(channels);
        bytes.extend_from_slice(&pre_skip.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn parses_supported_opus_header() -> anyhow::Result<()> {
        let parsed = parse_opus_head(&header(1, 1, 312))?;
        assert_eq!(parsed.channels, 1);
        assert_eq!(parsed.pre_skip, 312);
        Ok(())
    }

    #[test]
    fn rejects_short_or_malformed_opus_headers() {
        assert!(parse_opus_head(b"OpusHead").is_err());
        assert!(parse_opus_head(&[0; 19]).is_err());
    }

    #[test]
    fn rejects_unsupported_opus_version() {
        let error = parse_opus_head(&header(2, 1, 0))
            .err()
            .map(|error| error.to_string());
        assert!(
            error
                .as_deref()
                .unwrap_or_default()
                .contains("Unsupported OpusHead version: 2")
        );
    }

    #[test]
    fn resampling_empty_audio_is_empty() -> anyhow::Result<()> {
        assert!(resample_48k_to_16k(&[])?.is_empty());
        Ok(())
    }

    #[test]
    fn resampling_produces_exact_one_third_length() -> anyhow::Result<()> {
        for length in [3072, 4096] {
            let input = vec![0.0; length];
            assert_eq!(resample_48k_to_16k(&input)?.len(), length / 3);
        }
        Ok(())
    }

    #[test]
    fn resampling_silence_remains_silent() -> anyhow::Result<()> {
        let output = resample_48k_to_16k(&vec![0.0; 3072])?;
        assert!(output.iter().all(|sample| sample.abs() < f32::EPSILON));
        Ok(())
    }
}
