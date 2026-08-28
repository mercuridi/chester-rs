use std::path::{Path, PathBuf};

use chrono::Local;
use serenity::model::id::{GuildId, UserId};
use titlecase::Titlecase;

use crate::{
    chronicle::{config::{AliasGroup, Config}, recording::recorder::{RecordingManifest, notify_recording_user}, transcription::{audio::load_opus, transcript::{TranscriptDocument, TranscriptEntry, TranscriptFrontmatter, TranscriptParticipant}, whisper::transcriber::WhisperTranscriber}}, constants::{CHESTER_USER_ID, TRANSCRIPT_PAGE_LIMIT}, discord::{autocomplete::{autocomplete_alias_group, autocomplete_existing_transcript, autocomplete_recording_session}, context::{Error, PoiseContext}, voice::{ensure_vc, require_guild}}
};

#[poise::command(
    slash_command,
    subcommands("chronicle_start", "ask", "chronicle_stop"),
    subcommand_required,
)]
pub async fn chronicle(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "start")]
pub async fn chronicle_start(ctx: PoiseContext<'_>) -> Result<(), Error> {
    match ctx.data().chronicle.start_llm() {
        Ok(()) => {
            ctx.say("Chronicle LLM is ready.").await?;
        }
        Err(_error) if ctx.data().chronicle.is_llm_loaded()? => {
            ctx.say("Chronicle LLM is already loaded.").await?;
        }
        Err(error) => return Err(error.into()),
    }

    Ok(())
}

#[poise::command(slash_command, rename = "stop")]
pub async fn chronicle_stop(ctx: PoiseContext<'_>) -> Result<(), Error> {
    match ctx.data().chronicle.stop_llm() {
        Ok(()) => {
            ctx.say("Chronicle LLM unloaded.").await?;
        }
        Err(_error) if ctx.data().chronicle.is_llm_loaded()? => {
            ctx.say("Chronicle LLM cannot be unloaded while an operation is running.").await?;
        }
        Err(error) => return Err(error.into()),
    }

    Ok(())
}

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

    let (guild_id, voice_channel_id, _call) = ensure_vc(ctx).await?;
    let notification_channel_id = ctx.channel_id();

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
        .titlecase();

    let started = recorder
        .start_recording(
            guild_id,
            voice_channel_id,
            notification_channel_id,
            ctx.author().id,
            session_name.clone(),
        )
        .await?;

    if !started {
        ctx.say("A recording is already in progress.").await?;
        return Ok(());
    }

    ctx.say(format!("Recording session `{}` started by <@{}>.", session_name, ctx.author().id)).await?;

    let voice_states = ctx
        .serenity_context()
        .cache
        .guild(guild_id)
        .map(|guild| guild.voice_states.clone())
        .unwrap_or_default();

    for (user_id, voice_state) in voice_states {
        if user_id == CHESTER_USER_ID {
            continue;
        }

        if user_id == ctx.author().id {
            continue;
        }

        if voice_state.channel_id != Some(voice_channel_id) {
            continue;
        }

        notify_recording_user(
            &ctx.serenity_context().http,
            notification_channel_id,
            user_id,
        )
        .await?;
    }

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

#[poise::command(
    slash_command,
    subcommands("show", "generate")
)]
pub async fn transcript(
    _ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    Ok(())
}

/// Display an existing transcript.
#[poise::command(slash_command)]
pub async fn show(
    ctx: PoiseContext<'_>,

    #[description = "The session whose transcript to display"]
    #[autocomplete = "autocomplete_existing_transcript"]
    session: String,
) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    let recording_dir = PathBuf::from(format!(
        ".chronicle/recordings/{}/{}",
        guild_id,
        session,
    ));

    let transcript_path = transcript_path(&recording_dir);

    let transcript = match TranscriptDocument::load(&transcript_path) {
        Ok(transcript) => transcript,
        Err(error) => {
            ctx.say(format!(
                "No readable transcript exists for `{session}`: {error}"
            ))
            .await?;

            return Ok(());
        }
    };

    display_transcript(ctx, &transcript).await?;

    Ok(())
}

/// Generate a transcript from a recorded session.
#[poise::command(slash_command)]
pub async fn generate(
    ctx: PoiseContext<'_>,

    #[description = "The session to transcribe"]
    #[autocomplete = "autocomplete_recording_session"]
    session: String,

    #[description = "The alias group to use for transcription"]
    #[autocomplete = "autocomplete_alias_group"]
    alias_group_id: String,
) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    let recording_dir = PathBuf::from(format!(
        ".chronicle/recordings/{}/{}",
        guild_id,
        session,
    ));

    let manifest = match load_recording_manifest(
        &recording_dir,
        guild_id,
        &session,
    ) {
        Ok(manifest) => manifest,
        Err(message) => {
            ctx.say(message).await?;
            return Ok(());
        }
    };

    let recordings = find_recordings(&recording_dir)?;

    if recordings.is_empty() {
        ctx.say("No Opus recordings found in that session.")
            .await?;
        return Ok(());
    }

    let config = &ctx.data().config;

    let alias_group = match validate_alias_group(
        config,
        guild_id,
        &alias_group_id,
        &manifest.participants,
    ) {
        Ok(alias_group) => alias_group,
        Err(error) => {
            ctx.say(error.to_string()).await?;
            return Ok(());
        }
    };

    let transcript_path = transcript_path(&recording_dir);

    ctx.defer().await?;

    if transcript_path.is_file() {
        if !confirm_transcript_regeneration(ctx).await? {
            return Ok(());
        }
    }

    let transcript = generate_transcript(
        &manifest,
        recordings,
        alias_group,
        &transcript_path,
        ctx.data().chronicle.runtime(),
    )
    .await?;

    display_transcript(ctx, &transcript).await?;

    Ok(())
}

#[poise::command(slash_command)]
pub async fn ask(
    ctx: PoiseContext<'_>,
    #[description = "Question to ask Chronicle"] question: String,
) -> Result<(), Error> {
    ctx.defer().await?;

    let answer = ctx.data().chronicle.ask(&question).await?;

    ctx.say(answer).await?;

    Ok(())
}

async fn display_transcript(
    ctx: PoiseContext<'_>,
    transcript: &TranscriptDocument,
) -> Result<(), Error> {
    let pages = paginate_transcript(
        transcript
            .body
            .lines()
            .map(str::to_owned)
            .collect(),
    );

    let page_refs: Vec<&str> =
        pages.iter().map(String::as_str).collect();

    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}

fn paginate_transcript(lines: Vec<String>) -> Vec<String> {
    let mut pages = Vec::new();
    let mut current = String::new();

    for line in lines {
        let line_len = line.chars().count();
        let separator_len = usize::from(!current.is_empty());

        if current.chars().count() + separator_len + line_len <= TRANSCRIPT_PAGE_LIMIT {
            if !current.is_empty() {
                current.push('\n');
            }

            current.push_str(&line);
            continue;
        }

        // Store the current page before starting the next one.
        if !current.is_empty() {
            pages.push(std::mem::take(&mut current));
        }

        // A single transcript entry is larger than one page.
        // Split it at a valid UTF-8 boundary.
        if line_len > TRANSCRIPT_PAGE_LIMIT {
            let mut remaining = line.as_str();

            while remaining.chars().count() > TRANSCRIPT_PAGE_LIMIT {
                let split_at = remaining
                    .char_indices()
                    .nth(TRANSCRIPT_PAGE_LIMIT)
                    .map(|(idx, _)| idx)
                    .unwrap();

                pages.push(remaining[..split_at].to_owned());
                remaining = &remaining[split_at..];
            }

            if !remaining.is_empty() {
                current.push_str(remaining);
            }
        } else {
            current.push_str(&line);
        }
    }

    if !current.is_empty() {
        pages.push(current);
    }

    pages
}

fn load_recording_manifest(
    recording_dir: &Path,
    guild_id: GuildId,
    session: &str,
) -> Result<RecordingManifest, String> {
    if !recording_dir.is_dir() {
        return Err(format!(
            "Recording session not found: `{session}`"
        ));
    }

    let manifest_path = recording_dir.join("manifest.toml");

    let manifest = RecordingManifest::load(&manifest_path)
        .map_err(|error| {
            format!("Failed to load recording manifest: {error}")
        })?;

    if manifest.guild_id != guild_id {
        return Err(
            "Recording manifest belongs to a different guild.".to_string()
        );
    }

    Ok(manifest)
}

fn find_recordings(
    recording_dir: &Path,
) -> std::io::Result<Vec<PathBuf>> {
    let mut recordings = Vec::new();

    for entry in std::fs::read_dir(recording_dir)? {
        let path = entry?.path();

        if path.extension().and_then(|ext| ext.to_str()) == Some("opus") {
            recordings.push(path);
        }
    }

    Ok(recordings)
}

fn validate_alias_group<'a>(
    config: &'a Config,
    guild_id: GuildId,
    alias_group_id: &str,
    participants: &Vec<UserId>,
) -> anyhow::Result<&'a AliasGroup> {
    if !config.guild_has_alias_group(guild_id, alias_group_id) {
        anyhow::bail!(
            "Alias group `{alias_group_id}` is not available in this guild."
        );
    }

    config.validate_participants(
        alias_group_id,
        participants,
    )?;

    Ok(config
        .alias_group(alias_group_id)
        .expect("alias group was validated"))
}

async fn transcribe_recordings(
    recordings: Vec<PathBuf>,
    runtime: crate::chronicle::runtime::GpuRuntime,
) -> Result<
    Vec<(f64, f64, UserId, String)>,
    Error,
> {
    let _gpu_lease = runtime.acquire_transcription().map_err(|error| -> Error {
        error.to_string().into()
    })?;

    tokio::task::spawn_blocking(move || {
        let mut transcriber = WhisperTranscriber::new_cuda()?;
        let mut output: Vec<(f64, f64, UserId, String)> = Vec::new();

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
                .map(UserId::new)
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
    })
}

fn build_transcript_entries(
    result: Vec<(f64, f64, UserId, String)>,
    alias_group: &AliasGroup,
) -> Vec<TranscriptEntry> {
    result
        .into_iter()
        .map(|(start, end, user_id, text)| {
            let alias = alias_group
                .aliases
                .get(&user_id)
                .expect("participant aliases were validated")
                .clone();

            TranscriptEntry {
                start,
                end,
                user_id,
                alias,
                text,
            }
        })
        .collect()
}

fn format_timestamp(seconds: f64) -> String {
    let total_tenths = (seconds * 10.0).round() as u64;

    let hours = total_tenths / 36_000;
    let minutes = (total_tenths % 36_000) / 600;
    let seconds = (total_tenths % 600) / 10;
    let tenths = total_tenths % 10;

    format!("{:02}:{:02}:{:02}.{}", hours, minutes, seconds, tenths)
}

fn format_transcript(entries: &[TranscriptEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            format!(
                "`[{}–{}]` `{}`: {}",
                format_timestamp(entry.start),
                format_timestamp(entry.end),
                entry.alias,
                entry.text
            )
        })
        .collect()
}

fn build_transcript_document(
    manifest: &RecordingManifest,
    recordings: &[PathBuf],
    entries: &[TranscriptEntry],
) -> TranscriptDocument {
    let body = format_transcript(entries).join("\n");

    let word_count = body.split_whitespace().count();
    let character_count = body.chars().count();

    let participants = entries
        .iter()
        .map(|entry| TranscriptParticipant {
            user_id: entry.user_id.get().to_string(),
            alias: entry.alias.clone(),
        })
        .collect::<Vec<_>>();

    let participants = {
        let mut participants = participants;
        participants.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        participants.dedup_by(|a, b| a.user_id == b.user_id);
        participants
    };

    TranscriptDocument {
        frontmatter: TranscriptFrontmatter {
            schema_version: 1,
            recording_date: manifest.started_at,
            ended_at: manifest.ended_at,
            duration_seconds: (manifest.ended_at - manifest.started_at)
                .num_milliseconds() as f64
                / 1000.0,
            participants,
            recording_count: recordings.len(),
            entry_count: entries.len(),
            word_count,
            character_count,
            transcribed_at: Local::now(),
        },
        body,
    }
}

fn transcript_path(recording_dir: &Path) -> PathBuf {
    recording_dir.join("transcript.md")
}

async fn generate_transcript(
    manifest: &RecordingManifest,
    recordings: Vec<PathBuf>,
    alias_group: &AliasGroup,
    transcript_path: &Path,
    runtime: crate::chronicle::runtime::GpuRuntime,
) -> Result<TranscriptDocument, Error> {
    let result = transcribe_recordings(recordings.clone(), runtime).await?;

    for (start, end, _, _) in &result {
        println!("TRANSCRIPTION: start={start}, end={end}");
    }

    if result.is_empty() {
        return Err("No speech detected.".into());
    }

    let entries = build_transcript_entries(result, alias_group);

    if entries.is_empty() {
        return Err("No transcription results.".into());
    }

    let transcript = build_transcript_document(
        manifest,
        &recordings,
        &entries,
    );

    transcript
        .save(transcript_path)
        .map_err(|error| -> Error {
            format!(
                "Transcription succeeded, but failed to save transcript: {error}"
            )
            .into()
        })?;

    Ok(transcript)
}

async fn confirm_transcript_regeneration(
    ctx: PoiseContext<'_>,
) -> Result<bool, Error> {
    let reply = ctx
        .send(
            poise::CreateReply::default()
                .content(
                    "A transcript already exists for this session. \
                     Regenerating it will replace the existing transcript. \
                     Continue?",
                )
                .components(vec![
                    serenity::all::CreateActionRow::Buttons(vec![
                        serenity::all::CreateButton::new("transcript:regenerate")
                            .label("Regenerate")
                            .style(serenity::all::ButtonStyle::Danger),
                        serenity::all::CreateButton::new("transcript:cancel")
                            .label("Cancel")
                            .style(serenity::all::ButtonStyle::Secondary),
                    ]),
                ])
                .ephemeral(true),
        )
        .await?;

    let interaction = reply
        .message()
        .await?
        .await_component_interaction(ctx)
        .author_id(ctx.author().id)
        .timeout(std::time::Duration::from_secs(30))
        .await;

    let Some(interaction) = interaction else {
        return Ok(false);
    };

    let regenerate = interaction.data.custom_id == "transcript:regenerate";

    interaction
        .create_response(
            ctx.serenity_context(),
            serenity::all::CreateInteractionResponse::UpdateMessage(
                serenity::all::CreateInteractionResponseMessage::new()
                    .content(if regenerate {
                        "Regenerating transcript..."
                    } else {
                        "Transcript regeneration cancelled."
                    })
                    .components(Vec::new()),
            ),
        )
        .await?;

    Ok(regenerate)
}
