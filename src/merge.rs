////////////////////////////////////////////////////////////////////////////////
/// Imports

use std::{
    collections::HashMap,
    fs::{
        self,
        write
    }, 
    path::{
        PathBuf,
        Path
    },
    sync::Arc,
    sync::Weak
};

use serde::{
    Deserialize,
    Serialize
};

use tokio::sync::Mutex;

use songbird::{
    driver::Bitrate,
    input::{
        cached::Compressed,
        cached::Memory,
        File as SongbirdFile,
        Input
    },
    Call,
    SerenityInit,
    EventContext,
    EventHandler as VoiceEventHandler,
    Event as VoiceEvent
};

use poise::serenity_prelude as serenity;

use serenity::{
    ChannelId,
    GuildId,
    prelude::TypeMapKey,
    async_trait
};

use yt_dlp::{
    fetcher::deps::{
        Libraries,
        LibraryInstaller
    },
    Youtube
};

// Read the bot token from a .env
use dotenv::dotenv;

////////////////////////////////////////////////////////////////////////////////
/// Type and struct definitions

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;
enum CachedSound {
    Compressed(Compressed),
    Uncompressed(Memory),
}

impl From<&CachedSound> for Input {
    fn from(obj: &CachedSound) -> Self {
        use CachedSound::*;
        match obj {
            Compressed(c) => c.new_handle().into(),
            Uncompressed(u) => u
                .new_handle()
                .try_into()
                .expect("Failed to create decoder for Memory source."),
        }
    }
}

struct Handler;

#[serenity::async_trait]
impl serenity::all::EventHandler for Handler {
    async fn ready(&self, _: serenity::Context, ready: serenity::all::Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

struct Data {} // User data, which is stored and accessible in all command invocations

struct SoundStore;

impl TypeMapKey for SoundStore {
    type Value = Arc<Mutex<HashMap<String, CachedSound>>>;
}

struct LoopPlaySound {
    call_lock: Weak<Mutex<Call>>,
    sources: Arc<Mutex<HashMap<String, CachedSound>>>,
}

#[async_trait]
impl VoiceEventHandler for LoopPlaySound {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<VoiceEvent> {
        if let Some(call_lock) = self.call_lock.upgrade() {
            let src = {
                let sources = self.sources.lock().await;
                sources
                    .get("loop")
                    .expect("Handle placed into cache at startup.")
                    .into()
            };

            let mut handler = call_lock.lock().await;
            let sound = handler.play_input(src);
            let _ = sound.set_volume(0.5);
        }

        None
    }
}

////////////////////////////////////////////////////////////////////////////////
/// Function definitions

#[tokio::main]
async fn main() {
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("Encountered an error setting the CWD to top-level");

    handle_libraries().await.unwrap();

    dotenv().ok();
    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN in .env");
    let intents = 
            serenity::GatewayIntents::MESSAGE_CONTENT 
        |   serenity::GatewayIntents::GUILD_VOICE_STATES
        |   serenity::GatewayIntents::GUILDS
        |   serenity::GatewayIntents::GUILD_MESSAGES;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                register(),
                join(),
            ],
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {})
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .register_songbird()
        .framework(framework)
        .event_handler(Handler)
        .await
        .expect("Error creating client");

    // Obtain a lock to the data owned by the client, and insert the client's
    // voice manager into it. This allows the voice manager to be accessible by
    // event handlers and framework commands.
    {
        let mut data = client.data.write().await;

        // Loading the audio ahead of time.
        let mut audio_map = HashMap::new();

        // TODO
        // Load audio files here as seen below in example
        // Write a function to do it programmatically
        // Will need to update audio map in download function

        // Creation of a compressed source.
        // This is a full song, making this a much less memory-heavy choice.
        let song_src = Compressed::new(
            SongbirdFile::new("media/audio/SPa8bPqQfmo.mp3").into(),
            Bitrate::BitsPerSecond(128_000),
        )
        .await
        .expect("These parameters are well-defined.");
        let _ = song_src.raw.spawn_loader();

        // Below commented block - writes out intermediate interpretation of
        // cached tracks as dca files?
        // Not sure if implementing this is worth it - leave commented for later investigation
        // once base functionality is working

        // Compressed sources are internally stored as DCA1 format files.
        // Because `Compressed` implements `std::io::Read`, we can save these
        // to disk and use them again later if we want!
        // let mut creator = song_src.new_handle();
        // std::thread::spawn(move || {
        //     let mut out_file = std::fs::File::create("ckick-dca1.dca").unwrap();
        //     std::io::copy(&mut creator, &mut out_file).expect("Error writing out song!");
        // });

        audio_map.insert("song".into(), CachedSound::Compressed(song_src));

        data.insert::<SoundStore>(Arc::new(Mutex::new(audio_map)));
    }

    tokio::spawn(async move {
        let _ = client
            .start()
            .await
            .map_err(|why| println!("Client ended: {:?}", why));
    });

    let _signal_err = tokio::signal::ctrl_c().await;
    println!("Received Ctrl-C, shutting down.");
}

async fn handle_libraries() -> Result<(), Error> {
    // make path if it doesn't exist
    if !Path::new("libs/").exists() {
        fs::create_dir("libs")?;
    }

    let destination = PathBuf::from("libs");
    let installer = LibraryInstaller::new(destination);

    // install ffmpeg if it isn't there
    if fs::metadata("libs/ffmpeg").is_err() {
        let _ffmpeg = installer.install_ffmpeg(None).await.unwrap();
    }

    // install ytdlp if it isn't there
    if fs::metadata("libs/yt-dlp").is_err() {
        let _youtube = installer.install_youtube(None).await.unwrap();
    }

    Ok(())
}

#[poise::command(slash_command)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn join(ctx: Context<'_>) -> Result<(), Error> {
    let (guild_id, channel_id) = get_call_info(ctx).await?;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Ok(handler_lock) = manager.join(guild_id, channel_id).await {
        let call_lock_for_evt = Arc::downgrade(&handler_lock);

        // let mut handler = handler_lock.lock().await;
        // check_msg(
        //     msg.channel_id
        //         .say(&ctx.http, &format!("Joined {}", connect_to.mention()))
        //         .await,
        // );

        // let sources_lock = ctx
        //     .data
        //     .read()
        //     .await
        //     .get::<SoundStore>()
        //     .cloned()
        //     .expect("Sound cache was installed at startup.");
        // let sources_lock_for_evt = sources_lock.clone();
        // let sources = sources_lock.lock().await;
        // let source = sources
        //     .get("song")
        //     .expect("Handle placed into cache at startup.");

        // let song = handler.play_input(source.into());
        // let _ = song.set_volume(1.0);
        // let _ = song.enable_loop();

        // // Play a guitar chord whenever the main backing track loops.
        // let _ = song.add_event(
        //     Event::Track(TrackEvent::Loop),
        //     LoopPlaySound {
        //         call_lock: call_lock_for_evt,
        //         sources: sources_lock_for_evt,
        //     },
        // );
    }

    Ok(())
}

async fn get_call_info(ctx: Context<'_>) -> Result<(GuildId, ChannelId), Error> {

    let guild_id = ctx.guild_id().unwrap();

    let cache = ctx.serenity_context().cache.clone();

    let voice_state = cache
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&ctx.author().id).cloned());
    let voice_channel_id = match voice_state.and_then(|vs| vs.channel_id) {
        Some(c) => c,
        None => {
            return Err("The user is not in a voice channel.".into());
        }
    };

    Ok((guild_id, voice_channel_id))
}