use std::sync::Arc;

use serenity::model::{guild::Guild, id::{ChannelId, GuildId}};
use songbird::{Call, CoreEvent};
use tokio::sync::Mutex;

use crate::discord::context::{Error, PoiseContext};

pub async fn get_vc_id(ctx: PoiseContext<'_>) -> Result<ChannelId, Error> {
    let guild_id = require_guild(ctx)?;

    let voice_state = ctx.serenity_context()
        .cache
        .clone()
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&ctx.author().id).cloned());
    let voice_channel_id = match voice_state.and_then(|vs| vs.channel_id) {
        Some(c) => c,
        None => return Err("The user is not in a voice channel.".into())
    };

    Ok(voice_channel_id)
}

pub fn require_guild(ctx: PoiseContext<'_>) -> Result<GuildId, Error> {
    ctx.guild_id().ok_or_else(|| "This command can only be used in a server.".into())
}

pub async fn join_vc(
    ctx: PoiseContext<'_>,
    guild: Guild,
    vc_id: ChannelId,
) -> Result<Arc<Mutex<Call>>, Error> {
    tracing::debug!("Preparing to join voice chat");

    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Error getting the Songbird client from the manager")
        .clone();

    let recorder = ctx.data().recorder.clone();

    tracing::info!(
        recorder_id = recorder.id,
        "Using recorder for voice connection"
    );

    // Get/create the Call first, install handlers, THEN join.
    let call = manager.get_or_insert(guild.id);

    {
        let mut call_lock = call.lock().await;

        call_lock.add_global_event(
            CoreEvent::SpeakingStateUpdate.into(),
            recorder.clone(),
        );

        call_lock.add_global_event(
            CoreEvent::VoiceTick.into(),
            recorder.clone(),
        );
    }

    tracing::debug!("Voice event handlers installed");

    manager.join(guild.id, vc_id).await?;

    tracing::debug!("Joined voice chat");

    Ok(call)
}