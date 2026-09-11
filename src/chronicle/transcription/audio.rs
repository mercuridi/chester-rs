use std::{
    collections::VecDeque,
    fs::File,
    io::{BufReader, Read, Seek},
    path::Path,
};

use crate::chronicle::recording::constants::OPUS_SAMPLE_RATE;
use anyhow::{Result, anyhow};
use ogg::PacketReader;
use opus::{Channels, Decoder as OpusDecoder};
use rubato::{FftFixedIn, Resampler};

const WHISPER_SAMPLE_RATE: usize = 16_000;

const RESAMPLER_CHUNK: usize = 1024;
const OUTPUT_CHUNK: usize = 16_000;

/// A bounded stream of mono 16 kHz PCM decoded from an Ogg/Opus recording.
pub struct OpusAudioStream<R: Read + Seek> {
    packets: PacketReader<R>,
    decoder: Option<OpusDecoder>,
    resampler: FftFixedIn<f32>,
    pre_skip_remaining: usize,
    input_buffer: Vec<f32>,
    pending_output: VecDeque<f32>,
    output_delay_remaining: usize,
    expected_output: usize,
    input_samples: usize,
    raw_output_samples: usize,
    output_emitted: usize,
    end_of_input: bool,
    finished: bool,
}

pub fn open_opus(path: impl AsRef<Path>) -> Result<OpusAudioStream<BufReader<File>>> {
    let file = File::open(path)?;

    OpusAudioStream::new(BufReader::new(file))
}

impl<R> OpusAudioStream<R>
where
    R: Read + Seek,
{
    fn new(reader: R) -> Result<Self> {
        Ok(Self {
            packets: PacketReader::new(reader),
            decoder: None,
            resampler: FftFixedIn::new(
                OPUS_SAMPLE_RATE,
                WHISPER_SAMPLE_RATE,
                RESAMPLER_CHUNK,
                1,
                1,
            )?,
            pre_skip_remaining: 0,
            input_buffer: Vec::with_capacity(RESAMPLER_CHUNK),
            pending_output: VecDeque::with_capacity(OUTPUT_CHUNK * 2),
            output_delay_remaining: 0,
            expected_output: 0,
            input_samples: 0,
            raw_output_samples: 0,
            output_emitted: 0,
            end_of_input: false,
            finished: false,
        })
    }

    /// Return the next bounded chunk of 16 kHz PCM, or `None` at EOF.
    pub fn next_chunk(&mut self) -> Result<Option<Vec<f32>>> {
        while self.pending_output.len() < OUTPUT_CHUNK && !self.finished {
            self.fill_output()?;
        }

        if self.pending_output.is_empty() {
            Ok(None)
        } else {
            Ok(Some(self.pending_output.drain(..).collect()))
        }
    }

    fn fill_output(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }

        if self.end_of_input {
            return self.flush_resampler();
        }

        let Some(packet) = self.packets.read_packet()? else {
            self.end_of_input = true;
            return self.flush_resampler();
        };

        let data = packet.data.as_slice();

        if data.starts_with(b"OpusHead") {
            let header = parse_opus_head(data)?;
            if header.channels != 1 {
                return Err(anyhow!(
                    "Expected mono Opus recording, got {} channels",
                    header.channels
                ));
            }
            self.pre_skip_remaining = header.pre_skip as usize;
            self.output_delay_remaining = self.resampler.output_delay();
            self.decoder = Some(OpusDecoder::new(
                u32::try_from(OPUS_SAMPLE_RATE)?,
                Channels::Mono,
            )?);
            return Ok(());
        }

        if data.starts_with(b"OpusTags") {
            return Ok(());
        }

        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| anyhow!("Encountered Opus audio packet before OpusHead"))?;
        let mut pcm = [0i16; OPUS_SAMPLE_RATE * 120 / 1000];
        let samples = decoder.decode(data, &mut pcm, false)?;

        let skip = self.pre_skip_remaining.min(samples);
        self.pre_skip_remaining -= skip;
        self.input_buffer.extend(
            pcm[skip..samples]
                .iter()
                .map(|&sample| f32::from(sample) / 32768.0),
        );
        self.input_samples = self.input_samples.saturating_add(samples - skip);
        self.expected_output = self.input_samples * WHISPER_SAMPLE_RATE / OPUS_SAMPLE_RATE;

        self.process_full_input()
    }

    fn process_full_input(&mut self) -> Result<()> {
        while self.input_buffer.len() >= RESAMPLER_CHUNK {
            let input: Vec<f32> = self.input_buffer.drain(..RESAMPLER_CHUNK).collect();
            let result = self.resampler.process(&[input], None)?;
            self.append_output(&result[0]);
        }
        Ok(())
    }

    fn flush_resampler(&mut self) -> Result<()> {
        if self.decoder.is_none() {
            return Err(anyhow!("Ogg stream does not contain an OpusHead"));
        }
        if self.pre_skip_remaining > 0 {
            return Err(anyhow!(
                "Opus pre-skip ({}) exceeds decoded audio length",
                self.pre_skip_remaining
            ));
        }

        self.expected_output = self.input_samples * WHISPER_SAMPLE_RATE / OPUS_SAMPLE_RATE;

        if !self.input_buffer.is_empty() {
            let input = std::mem::take(&mut self.input_buffer);
            let result = self.resampler.process_partial(Some(&[input]), None)?;
            self.append_output(&result[0]);
        }

        while self.raw_output_samples < self.expected_output + self.output_delay_remaining {
            let result = self.resampler.process_partial::<Vec<f32>>(None, None)?;
            if result[0].is_empty() {
                return Err(anyhow!("Resampler produced no output while flushing"));
            }
            self.append_output(&result[0]);
        }

        self.finished = true;
        Ok(())
    }

    fn append_output(&mut self, output: &[f32]) {
        self.raw_output_samples += output.len();
        for &sample in output {
            if self.output_delay_remaining > 0 {
                self.output_delay_remaining -= 1;
            } else if self.output_emitted < self.expected_output {
                self.pending_output.push_back(sample);
                self.output_emitted += 1;
            }
        }
    }
}

pub trait AudioSource {
    fn next_chunk(&mut self) -> Result<Option<Vec<f32>>>;
}

impl<R> AudioSource for OpusAudioStream<R>
where
    R: Read + Seek,
{
    fn next_chunk(&mut self) -> Result<Option<Vec<f32>>> {
        OpusAudioStream::next_chunk(self)
    }
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogg::{PacketWriteEndInfo, PacketWriter};
    use opus::{Application, Channels, Encoder};

    use super::{OpusAudioStream, parse_opus_head};

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
    fn streams_opus_in_bounded_chunks_and_applies_pre_skip() -> anyhow::Result<()> {
        let mut encoder = Encoder::new(48_000, Channels::Mono, Application::Audio)?;
        let pre_skip = u16::try_from(encoder.get_lookahead()?)?;
        let mut bytes = Vec::new();
        let mut writer = PacketWriter::new(Cursor::new(&mut bytes));
        let serial = 7;

        writer.write_packet(
            opus_head(pre_skip),
            serial,
            PacketWriteEndInfo::NormalPacket,
            0,
        )?;
        writer.write_packet(
            b"OpusTags\x00\x00".to_vec(),
            serial,
            PacketWriteEndInfo::NormalPacket,
            0,
        )?;

        let pcm = [0i16; 960];
        for index in 0..40 {
            let mut encoded_packet = [0u8; 4_000];
            let encoded_len = encoder.encode(&pcm, &mut encoded_packet)?;
            writer.write_packet(
                encoded_packet[..encoded_len].to_vec(),
                serial,
                if index == 39 {
                    PacketWriteEndInfo::EndStream
                } else {
                    PacketWriteEndInfo::NormalPacket
                },
                u64::try_from((index + 1) * pcm.len())?,
            )?;
        }
        drop(writer);

        let mut stream = OpusAudioStream::new(Cursor::new(bytes))?;
        let mut output = Vec::new();
        while let Some(chunk) = stream.next_chunk()? {
            assert!(chunk.len() <= 16_000);
            output.extend(chunk);
        }

        let expected = (40 * 960 - usize::from(pre_skip)) / 3;
        assert_eq!(output.len(), expected);
        assert!(stream.next_chunk()?.is_none());
        Ok(())
    }

    fn opus_head(pre_skip: u16) -> Vec<u8> {
        let mut bytes = b"OpusHead".to_vec();
        bytes.extend([1, 1]);
        bytes.extend_from_slice(&pre_skip.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes());
        bytes.push(0);
        bytes
    }
}
