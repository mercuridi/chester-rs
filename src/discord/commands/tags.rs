use crate::discord::autocomplete::{autocomplete_tag, autocomplete_track};
use crate::discord::context::{Error, PoiseContext};
use crate::jester::db::{
    add_track_tag, clear_track_tags, create_tag, remove_track_tag, require_track,
};
use crate::jester::track::VideoId;

/// Manage the shared tag vocabulary and apply tags to tracks.
#[poise::command(
    slash_command,
    subcommands("create", "add", "remove", "clear"),
    subcommand_required
)]
#[allow(clippy::unused_async)]
pub async fn tags(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

/// Add a tag to the shared vocabulary. Requires membership in the configured GM list.
#[poise::command(slash_command)]
async fn create(
    ctx: PoiseContext<'_>,
    #[description = "New tag name"] name: String,
) -> Result<(), Error> {
    if !ctx.data().config.is_chronicle_gm(ctx.author().id) {
        return Err("Only a configured Chester GM can create tags.".into());
    }
    let name = create_tag(&ctx.data().db_pool, &name).await?;
    ctx.say(format!("Tag `{name}` is available for tracks."))
        .await?;
    Ok(())
}

/// Apply an existing shared tag to a track.
#[poise::command(slash_command)]
async fn add(
    ctx: PoiseContext<'_>,
    #[description = "Track to tag"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "Existing tag to apply"]
    #[autocomplete = "autocomplete_tag"]
    tag: String,
) -> Result<(), Error> {
    let info = require_track(&ctx.data().db_pool, &VideoId::from(track)).await?;
    let tag = add_track_tag(&ctx.data().db_pool, &info.id, &tag).await?;
    ctx.say(format!("Added `{tag}` to `{}`.", info.title))
        .await?;
    Ok(())
}

/// Remove a tag from a track.
#[poise::command(slash_command)]
async fn remove(
    ctx: PoiseContext<'_>,
    #[description = "Track to update"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "Tag to remove"]
    #[autocomplete = "autocomplete_tag"]
    tag: String,
) -> Result<(), Error> {
    let info = require_track(&ctx.data().db_pool, &VideoId::from(track)).await?;
    let tag = remove_track_tag(&ctx.data().db_pool, &info.id, &tag).await?;
    ctx.say(format!("Removed `{tag}` from `{}`.", info.title))
        .await?;
    Ok(())
}

/// Remove every tag from a track.
#[poise::command(slash_command)]
async fn clear(
    ctx: PoiseContext<'_>,
    #[description = "Track to clear"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    let info = require_track(&ctx.data().db_pool, &VideoId::from(track)).await?;
    clear_track_tags(&ctx.data().db_pool, &info.id).await?;
    ctx.say(format!("Cleared tags from `{}`.", info.title))
        .await?;
    Ok(())
}
