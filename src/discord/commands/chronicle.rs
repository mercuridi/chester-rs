use crate::definitions::{Error, PoiseContext};



#[poise::command(slash_command, subcommands("record"), subcommand_required)]
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
