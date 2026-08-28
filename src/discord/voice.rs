use std::sync::Arc;

use serenity::model::id::{ChannelId, GuildId};
use songbird::Call;
use tokio::sync::Mutex;

use crate::discord::context::{Error, PoiseContext};

pub async fn get_vc_id(ctx: PoiseContext<'_>) -> Result<ChannelId, Error> {
    let guild_id = require_guild(ctx)?;

    let voice_state = ctx
        .serenity_context()
        .cache
        .clone()
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&ctx.author().id).cloned());
    let voice_channel_id = match voice_state.and_then(|vs| vs.channel_id) {
        Some(c) => c,
        None => return Err("The user is not in a voice channel.".into()),
    };

    Ok(voice_channel_id)
}

pub fn require_guild(ctx: PoiseContext<'_>) -> Result<GuildId, Error> {
    ctx.guild_id()
        .ok_or_else(|| "This command can only be used in a server.".into())
}

pub async fn join_vc(
    ctx: PoiseContext<'_>,
    guild_id: GuildId,
    vc_id: ChannelId,
) -> Result<Arc<Mutex<Call>>, Error> {
    tracing::debug!(?guild_id, ?vc_id, "Joining voice channel");

    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird was not initialized")
        .clone();

    let (recorder, is_new) = ctx.data().recorder.get_or_create(guild_id).await;

    // Create the Call and attach receive handlers BEFORE joining.
    let call = manager.get_or_insert(guild_id);

    if is_new {
        if let Err(error) = recorder.attach_to_call(&call).await {
            ctx.data().recorder.remove(guild_id).await;
            manager.remove(guild_id).await?;
            return Err(error);
        }

        tracing::debug!(?guild_id, "Created and attached recorder");
    }

    // Only now start the actual voice connection.
    manager.join(guild_id, vc_id).await?;

    tracing::debug!(?guild_id, ?vc_id, "Joined voice channel");

    Ok(call)
}

pub async fn leave_vc(ctx: PoiseContext<'_>, guild_id: GuildId) -> Result<(), Error> {
    let recorder = ctx.data().recorder.remove(guild_id).await;

    let recording_error = if let Some(recorder) = recorder {
        recorder.stop_recording().await.err()
    } else {
        None
    };

    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird was not initialized")
        .clone();

    let voice_result = manager.remove(guild_id).await;

    if let Some(error) = recording_error {
        tracing::error!(
            ?guild_id,
            %error,
            "Failed to fully stop recording while leaving voice channel"
        );
    }

    voice_result?;

    Ok(())
}

pub async fn ensure_vc(
    ctx: PoiseContext<'_>,
) -> Result<(GuildId, ChannelId, Arc<Mutex<Call>>), Error> {
    let guild_id = require_guild(ctx)?;
    let vc_id = get_vc_id(ctx).await?;
    let call = join_vc(ctx, guild_id, vc_id).await?;

    Ok((guild_id, vc_id, call))
}
