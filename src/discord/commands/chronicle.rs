use std::path::{Path, PathBuf};

use chrono::Local;
use serenity::model::id::{GuildId, UserId};
use titlecase::Titlecase;
use tracing::{debug, info};

use crate::{
    chronicle::{
        config::{AliasGroup, Config},
        recording::recorder::{RecordingManifest, notify_recording_user},
        transcription::{
            service::{TranscribedSegment, TranscriptionService},
            transcript::{
                TranscriptDocument, TranscriptEntry, TranscriptFrontmatter, TranscriptParticipant,
            },
        },
    },
    constants::{CHESTER_USER_ID, RECORDINGS_DIR, TRANSCRIPT_PAGE_LIMIT},
    discord::{
        autocomplete::{
            autocomplete_alias_group, autocomplete_existing_transcript,
            autocomplete_recording_session,
        },
        context::{Error, PoiseContext},
        voice::{ensure_vc, require_guild},
    },
};

/// Top-level Chronicle command
#[poise::command(
    slash_command,
    subcommands("chronicle_start", "ask", "chronicle_stop"),
    subcommand_required
)]
#[allow(clippy::unused_async)]
pub async fn chronicle(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

/// Start the Chronicle subsystem
#[poise::command(slash_command, rename = "start")]
pub async fn chronicle_start(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Chronicle start command requested");
    ctx.defer().await?;
    match ctx.data().chronicle.start_llm().await {
        Ok(()) => {
            ctx.say("Chronicle is ready.").await?;
        }
        Err(_error) if ctx.data().chronicle.is_llm_loaded()? => {
            ctx.say("Chronicle is already ready.").await?;
        }
        Err(error) => return Err(error.into()),
    }

    Ok(())
}

/// Stop the Chronicle subsystem
#[poise::command(slash_command, rename = "stop")]
pub async fn chronicle_stop(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Chronicle stop command requested");
    match ctx.data().chronicle.stop_llm().await {
        Ok(()) => {
            ctx.say("Chronicle stopped.").await?;
        }
        Err(_error) if ctx.data().chronicle.is_llm_loaded()? => {
            ctx.say("Chronicle LLM cannot be unloaded while an operation is running.")
                .await?;
        }
        Err(error) => return Err(error.into()),
    }

    Ok(())
}

/// Top-level recording command
#[poise::command(slash_command, subcommands("start", "stop"), subcommand_required)]
#[allow(clippy::unused_async)]
pub async fn recording(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

/// Start a recording session
#[poise::command(slash_command)]
pub async fn start(
    ctx: PoiseContext<'_>,
    #[description = "The session name"] session_name: String,
) -> Result<(), Error> {
    info!(user = %ctx.author().id, session = %session_name, "Recording start command requested");
    let (guild_id, voice_channel_id, _call) = ensure_vc(ctx).await?;
    let notification_channel_id = ctx.channel_id();

    let recorder = ctx
        .data()
        .recorder
        .get(guild_id)
        .await
        .ok_or_else(|| -> Error { "Failed to initialize the guild recorder.".into() })?;

    if recorder.is_recording().await {
        ctx.say("A recording is already in progress.").await?;
        return Ok(());
    }

    let session_name = session_name.replace(' ', "-").titlecase();

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

    ctx.say(format!(
        "Recording session `{}` started by <@{}>.",
        session_name,
        ctx.author().id
    ))
    .await?;

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

/// End a recording session.
#[poise::command(slash_command)]
pub async fn stop(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Recording stop command requested");
    let guild_id = require_guild(ctx)?;

    let recorder = ctx
        .data()
        .recorder
        .get(guild_id)
        .await
        .ok_or_else(|| -> Error { "Failed to initialize the guild recorder.".into() })?;

    if !recorder.is_recording().await {
        ctx.say("There is no recording in progress.").await?;
        return Ok(());
    }

    recorder.stop_recording().await?;

    ctx.say("Recording stopped.").await?;
    Ok(())
}

/// Top-level transcript command
#[poise::command(slash_command, subcommands("show", "generate"))]
#[allow(clippy::unused_async)]
pub async fn transcript(_ctx: PoiseContext<'_>) -> Result<(), Error> {
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
    info!(user = %ctx.author().id, session = %session, "Transcript display requested");
    let guild_id = require_guild(ctx)?;

    let recording_dir = PathBuf::from(RECORDINGS_DIR)
        .join(guild_id.to_string())
        .join(&session);

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
    info!(user = %ctx.author().id, session = %session, alias_group = %alias_group_id, "Transcript generation requested");
    let guild_id = require_guild(ctx)?;

    let recording_dir = PathBuf::from(RECORDINGS_DIR)
        .join(guild_id.to_string())
        .join(&session);

    let manifest = match load_recording_manifest(&recording_dir, guild_id, &session) {
        Ok(manifest) => manifest,
        Err(message) => {
            ctx.say(message).await?;
            return Ok(());
        }
    };

    let recordings = find_recordings(&recording_dir)?;
    debug!(
        recording_count = recordings.len(),
        "Located session recordings"
    );

    if recordings.is_empty() {
        ctx.say("No Opus recordings found in that session.").await?;
        return Ok(());
    }

    let config = &ctx.data().config;

    let alias_group =
        match validate_alias_group(config, guild_id, &alias_group_id, &manifest.participants) {
            Ok(alias_group) => alias_group,
            Err(error) => {
                ctx.say(error.to_string()).await?;
                return Ok(());
            }
        };

    let transcript_path = transcript_path(&recording_dir);

    ctx.defer().await?;

    if transcript_path.is_file() && !confirm_transcript_regeneration(ctx).await? {
        return Ok(());
    }

    let transcript = generate_transcript(
        &manifest,
        recordings,
        alias_group,
        &transcript_path,
        ctx.data().chronicle.transcription_service(),
    )
    .await?;

    display_transcript(ctx, &transcript).await?;

    Ok(())
}

/// Ask a natural-language question about the loaded corpus.
#[poise::command(slash_command)]
pub async fn ask(
    ctx: PoiseContext<'_>,
    #[description = "Question to ask Chronicle"] question: String,
) -> Result<(), Error> {
    info!(user = %ctx.author().id, question_len = question.len(), "Chronicle ask command requested");
    ctx.defer().await?;

    let answer = ctx.data().chronicle.ask(&question).await?;

    ctx.say(answer).await?;

    Ok(())
}

async fn display_transcript(
    ctx: PoiseContext<'_>,
    transcript: &TranscriptDocument,
) -> Result<(), Error> {
    let pages = paginate_transcript(transcript.body.lines().map(str::to_owned).collect());

    let page_refs: Vec<&str> = pages.iter().map(String::as_str).collect();

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
                let Some(split_at) = remaining
                    .char_indices()
                    .nth(TRANSCRIPT_PAGE_LIMIT)
                    .map(|(idx, _)| idx)
                else {
                    break;
                };

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
        return Err(format!("Recording session not found: `{session}`"));
    }

    let manifest_path = recording_dir.join("manifest.toml");

    let manifest = RecordingManifest::load(&manifest_path)
        .map_err(|error| format!("Failed to load recording manifest: {error}"))?;

    if manifest.guild_id != guild_id {
        return Err("Recording manifest belongs to a different guild.".to_string());
    }

    Ok(manifest)
}

fn find_recordings(recording_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
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
        anyhow::bail!("Alias group `{alias_group_id}` is not available in this guild.");
    }

    config.validate_participants(alias_group_id, participants)?;

    config
        .alias_group(alias_group_id)
        .ok_or_else(|| anyhow::anyhow!("Alias group `{alias_group_id}` was not found."))
}

fn build_transcript_entries(
    result: Vec<TranscribedSegment>,
    alias_group: &AliasGroup,
) -> anyhow::Result<Vec<TranscriptEntry>> {
    result
        .into_iter()
        .map(|segment| {
            let alias = alias_group
                .aliases
                .get(&segment.user_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("No alias configured for participant {}", segment.user_id)
                })?
                .clone();

            Ok(TranscriptEntry {
                start: segment.start,
                end: segment.end,
                user_id: segment.user_id,
                alias,
                text: segment.text,
            })
        })
        .collect()
}

// The guard below defines the behavior for invalid timestamps; the remaining
// cast is intentional because Rust has no checked `f64`-to-`u64` conversion.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn format_timestamp(seconds: f64) -> String {
    let total_tenths = if seconds.is_finite() && seconds >= 0.0 {
        (seconds * 10.0).round() as u64
    } else {
        0
    };

    let hours = total_tenths / 36_000;
    let minutes = (total_tenths % 36_000) / 600;
    let seconds = (total_tenths % 600) / 10;
    let tenths = total_tenths % 10;

    format!("{hours:02}:{minutes:02}:{seconds:02}.{tenths}")
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
                .to_std()
                .map_or(0.0, |duration| duration.as_secs_f64()),
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
    transcription: TranscriptionService,
) -> Result<TranscriptDocument, Error> {
    let result = transcription
        .transcribe_recordings(recordings.clone())
        .await?;

    for segment in &result {
        tracing::debug!(
            start = segment.start,
            end = segment.end,
            "Transcription segment"
        );
    }

    if result.is_empty() {
        return Err("No speech detected.".into());
    }

    let entries = build_transcript_entries(result, alias_group)?;

    if entries.is_empty() {
        return Err("No transcription results.".into());
    }

    let transcript = build_transcript_document(manifest, &recordings, &entries);

    transcript.save(transcript_path).map_err(|error| -> Error {
        format!("Transcription succeeded, but failed to save transcript: {error}").into()
    })?;

    Ok(transcript)
}

async fn confirm_transcript_regeneration(ctx: PoiseContext<'_>) -> Result<bool, Error> {
    let reply = ctx
        .send(
            poise::CreateReply::default()
                .content(
                    "A transcript already exists for this session. \
                     Regenerating it will replace the existing transcript. \
                     Continue?",
                )
                .components(vec![serenity::all::CreateActionRow::Buttons(vec![
                    serenity::all::CreateButton::new("transcript:regenerate")
                        .label("Regenerate")
                        .style(serenity::all::ButtonStyle::Danger),
                    serenity::all::CreateButton::new("transcript:cancel")
                        .label("Cancel")
                        .style(serenity::all::ButtonStyle::Secondary),
                ])])
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
