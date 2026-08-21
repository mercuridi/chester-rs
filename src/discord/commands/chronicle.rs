use crate::discord::{context::{Error, PoiseContext}, voice::ensure_vc};

#[poise::command(slash_command, subcommands("record"), subcommand_required)]
pub async fn chronicle(
    _ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn record(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let recorder = ctx.data().recorder.clone();

    if recorder.is_recording().await {
        recorder.stop_recording().await?;
        ctx.say("Recording stopped.").await?;
    } else {
        let (_, call) = ensure_vc(ctx).await?;
        recorder.attach_to_call(&call).await;
        recorder.start_recording().await?;
        ctx.say("Recording started.").await?;
    }

    Ok(())
}