use std::path::PathBuf;

use crate::{
    chronicle::{recorder::RecordingManifest, transcription::{audio::load_opus, whisper::transcriber::WhisperTranscriber}}, discord::{
        context::{Error, PoiseContext},
        voice::{ensure_vc, require_guild},
    },
    discord::autocomplete::{autocomplete_transcription_session, autocomplete_alias_group}
};

#[poise::command(
    slash_command,
    subcommands("start", "stop"),
    subcommand_required,
)]
pub async fn recording(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn start(
    ctx: PoiseContext<'_>,
    #[description = "The session name"] session_name: String,
) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    ensure_vc(ctx).await?;

    let recorder = ctx
        .data()
        .recorder
        .get(guild_id)
        .await
        .ok_or_else(|| -> Error {
            "Failed to initialize the guild recorder.".into()
        })?;

    if recorder.is_recording().await {
        ctx.say("A recording is already in progress.").await?;
        return Ok(());
    }

    let session_name = session_name
        .replace(' ', "-")
        .to_lowercase();

    recorder
        .start_recording(guild_id, session_name)
        .await?;

    ctx.say("Recording started.").await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn stop(
    ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    let recorder = ctx
        .data()
        .recorder
        .get(guild_id)
        .await
        .ok_or_else(|| -> Error {
            "Failed to initialize the guild recorder.".into()
        })?;

    if !recorder.is_recording().await {
        ctx.say("There is no recording in progress.").await?;
        return Ok(());
    }

    recorder.stop_recording().await?;

    ctx.say("Recording stopped.").await?;
    Ok(())
}


/// Transcribe a previously recorded session.
#[poise::command(slash_command)]
pub async fn transcribe(
    ctx: PoiseContext<'_>,
    #[description = "The session to transcribe"]
    #[autocomplete = "autocomplete_transcription_session"]
    session: String,
    #[description = "The alias group to use for transcription"]
    #[autocomplete = "autocomplete_alias_group"]
    alias_group_id: String,
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

    let manifest_path = recording_dir.join("manifest.toml");

    let manifest = match RecordingManifest::load(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            ctx.say(format!(
                "Failed to load recording manifest: {error}"
            ))
            .await?;
            return Ok(());
        }
    };

    if manifest.guild_id != guild_id {
        ctx.say("Recording manifest belongs to a different guild.")
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

    let config = &ctx.data().config;

    if !config.guild_has_alias_group(guild_id, &alias_group_id) {
        ctx.say(format!(
            "Alias group `{alias_group_id}` is not available in this guild."
        ))
        .await?;

        return Ok(());
    }

    config.validate_participants(
        &alias_group_id,
        manifest.participants,
    )?;

    let alias_group = config
        .alias_group(&alias_group_id)
        .expect("alias group was validated");

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
            let segments = transcriber.transcribe(&audio)?;

            let user_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("recording-"))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Invalid recording filename: {}",
                        path.display()
                    )
                })?
                .parse::<u64>()
                .map(serenity::all::UserId::new)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Invalid user ID in recording filename {}: {error}",
                        path.display()
                    )
                })?;

            for segment in segments {
                output.push((
                    segment.start,
                    segment.end,
                    user_id,
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
        let alias = alias_group
            .aliases
            .get(&user_id)
            .expect("participant aliases were validated");

        response.push_str(&format!(
            "[{start:.1}s–{end:.1}s] `{alias}`: {text}\n"
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