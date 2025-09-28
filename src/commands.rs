use poise::serenity_prelude::{ClientBuilder, GatewayIntents, ChannelId};

use crate::definitions::{Context, Data, Error};

async fn get_vc_id(ctx: Context<'_>) -> Result<ChannelId, Error> {

    let guild_id = ctx.guild_id().unwrap();

    let voice_state = ctx.serenity_context()
        .cache
        .clone()
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&ctx.author().id).cloned());
    let voice_channel_id = match voice_state.and_then(|vs| vs.channel_id) {
        Some(c) => c,
        None => {
            return Err("The user is not in a voice channel.".into());
        }
    };

    Ok(voice_channel_id)
}

#[poise::command(prefix_command)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}

#[poise::command(prefix_command)]
pub async fn join(
    ctx: Context<'_>,
) -> Result<(), Error> {
    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let vc_id = get_vc_id(ctx).await.unwrap();

    // Grab the Songbird instance we registered earlier
    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let _join_result = manager.join(guild.id, vc_id).await;

    ctx.say("Joined your voice channel! 🎶").await?;
    Ok(())
}