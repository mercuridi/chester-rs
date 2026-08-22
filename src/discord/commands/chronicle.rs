use std::path::PathBuf;

use crate::{
    chronicle::transcription::{audio::load_opus, whisper::transcriber::{WhisperTranscriber}}, discord::{
        context::{Error, PoiseContext},
        voice::{ensure_vc, require_guild},
    },
};

#[poise::command(
    slash_command,
    subcommands("record", "transcribe"),
    subcommand_required
)]
pub async fn chronicle(
    _ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn record(
    ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    if let Some(recorder) = ctx.data().recorder.get(guild_id).await {
        if recorder.is_recording().await {
            recorder.stop_recording().await?;

            ctx.say("Recording stopped.").await?;
            return Ok(());
        }
    }

    ensure_vc(ctx).await?;

    let recorder = ctx
        .data()
        .recorder
        .get(guild_id)
        .await
        .ok_or_else(|| -> Error {
            "Failed to initialize the guild recorder.".into()
        })?;

    recorder.start_recording(guild_id).await?;

    ctx.say("Recording started.").await?;

    Ok(())
}

#[poise::command(slash_command)]
pub async fn transcribe(
    ctx: PoiseContext<'_>,
    session: String,
) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    let recording_dir = PathBuf::from(format!(
        ".chronicle/recordings/{}/{}",
        guild_id,
        session
    ));

    if !recording_dir.is_dir() {
        ctx.say(format!(
            "Recording session not found: `{session}`"
        ))
        .await?;

        return Ok(());
    }

    let mut recordings = Vec::new();

    for entry in std::fs::read_dir(&recording_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|ext| ext.to_str()) == Some("opus") {
            recordings.push(path);
        }
    }

    if recordings.is_empty() {
        ctx.say("No Opus recordings found in that session.")
            .await?;

        return Ok(());
    }

    ctx.say(format!(
        "Transcribing {} recording(s)...",
        recordings.len()
    ))
    .await?;

    let result = tokio::task::spawn_blocking(move || {
        let mut transcriber = WhisperTranscriber::new_cuda()?;

        let mut output = Vec::new();

        for path in recordings {
            let audio = load_opus(&path)?;

            // tracing::debug!(
            //     samples = audio.samples.len(),
            //     duration_secs = audio.samples.len() as f64 / audio.sample_rate as f64,
            //     sample_rate = audio.sample_rate,
            //     min = audio.samples.iter().copied().fold(f32::INFINITY, f32::min),
            //     max = audio.samples.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            //     rms = (
            //         audio.samples
            //             .iter()
            //             .map(|x| (*x as f64) * (*x as f64))
            //             .sum::<f64>()
            //             / audio.samples.len().max(1) as f64
            //     ).sqrt(),
            //     "Loaded audio"
            // );

            let segments = transcriber.transcribe(&audio)?;

            let user_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("recording-"))
                .unwrap_or("unknown");

            for segment in segments {
                output.push((
                    segment.start,
                    segment.end,
                    user_id.to_owned(),
                    segment.text,
                ));
            }
        }

        output.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok::<_, anyhow::Error>(output)
    })
    .await
    .map_err(|error| -> Error {
        format!("Transcription task failed: {error}").into()
    })?
    .map_err(|error| -> Error {
        format!("Transcription failed: {error}").into()
    })?;

    if result.is_empty() {
        ctx.say("No speech detected.").await?;
        return Ok(());
    }

    let mut response = String::new();

    for (start, end, user_id, text) in result {
        response.push_str(&format!(
            "[{start:.1}s–{end:.1}s] `{user_id}`: {text}\n"
        ));
    }

    // Discord messages have a 2000-character limit.
    if response.len() > 1900 {
        response.truncate(1900);
        response.push_str("\n…");
    }

    ctx.say(format!("```text\n{response}```")).await?;

    Ok(())
}