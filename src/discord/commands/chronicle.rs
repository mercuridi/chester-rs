use crate::discord::{context::{Error, PoiseContext}, voice::{ensure_vc, require_guild}};

#[poise::command(slash_command, subcommands("record"), subcommand_required)]
pub async fn chronicle(
    _ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn record(ctx: PoiseContext<'_>) -> Result<(), Error> {
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