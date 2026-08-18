use crate::definitions::{Error, PoiseContext};

#[poise::command(slash_command, subcommands("record", "save"), subcommand_required)]
pub async fn chronicle(
    _ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn record(
    _ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    Ok(())
    // set up toggle to turn on/off session recording
}

#[poise::command(slash_command)]
pub async fn save(
    ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    let recorder = ctx.data().recorder.clone();

    let count = recorder.recording_count().await;

    println!("SAVE: recordings = {}", count);

    recorder.save_all_recordings_and_clear_memory().await?;

    Ok(())
}