//! PortWatcher, a Discord bot with a live progress widget for https://portwatch-vi.xyz/.

mod commands;
mod render;
mod store;
mod tracker;
mod widget;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use poise::serenity_prelude as serenity;
use tracing::{error, info, warn};

use commands::Error;
use widget::{BotState, Data};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            warn!(command = %ctx.command().qualified_name, "command failed: {error:#}");
            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .content(format!("❌ {error}"))
                        .ephemeral(true),
                )
                .await;
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                error!("error while handling an error: {e}");
            }
        }
    }
}

async fn on_event(
    framework: poise::FrameworkContext<'_, Data, Error>,
    event: &serenity::FullEvent,
) -> Result<(), Error> {
    // Forget servers the bot was removed from (not ones that are just unavailable during an outage).
    if let serenity::FullEvent::GuildDelete { incomplete, .. } = event
        && !incomplete.unavailable
    {
        let guild = incomplete.id.get();
        info!(guild, "removed from server, deleting its settings");
        framework.user_data.store.remove_guild(guild).await?;
        framework.user_data.forget_uploaded(guild).await;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,serenity=warn,poise=warn".into()),
        )
        .init();

    let tracker_url = env_or("TRACKER_URL", "https://portwatch-vi.xyz/data/tracker.json");

    // `portwatch-bot --preview out.png`: render the widget once without connecting to Discord.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--preview") {
        let out = args.get(2).map_or("preview.png", String::as_str);
        let tracker = tracker::Fetcher::new(tracker_url)?.fetch().await?;
        let overview = tracker::Overview::from_tracker(&tracker);
        std::fs::write(out, render::render_png(&render::build_svg(&overview))?)?;
        println!("wrote {out}");
        return Ok(());
    }

    let token =
        std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is not set (see .env.example)")?;
    let data_file = PathBuf::from(env_or("DATA_FILE", "data/guilds.json"));
    let interval = Duration::from_secs(
        env_or("UPDATE_INTERVAL_SECS", "30")
            .parse::<u64>()
            .context("UPDATE_INTERVAL_SECS must be a number")?
            .max(10),
    );
    // Optional: register commands in one server instantly while developing.
    let dev_guild = std::env::var("DEV_GUILD_ID")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok());

    let store = store::Store::load(data_file).await?;
    let fetcher = tracker::Fetcher::new(tracker_url)?;
    let state: Data = Arc::new(BotState::new(store, fetcher, interval));

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            on_error: |e| Box::pin(on_error(e)),
            event_handler: |fw, ev| Box::pin(on_event(fw, ev)),
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                info!(user = %ready.user.name, servers = ready.guilds.len(), "logged in");
                // Plain lines (no log formatting) so they're easy to copy and to match on (Pterodactyl).
                println!(
                    "Invite link: https://discord.com/oauth2/authorize?client_id={}&scope=bot+applications.commands&permissions={}",
                    ready.application.id,
                    commands::INVITE_PERMISSIONS.bits()
                );
                println!("PortWatcher is online as {} in {} server(s)", ready.user.name, ready.guilds.len());
                let cmds = &framework.options().commands;
                match dev_guild {
                    Some(g) => {
                        poise::builtins::register_in_guild(ctx, cmds, serenity::GuildId::new(g)).await?;
                        info!(guild = g, "commands registered in dev server");
                    }
                    None => {
                        poise::builtins::register_globally(ctx, cmds).await?;
                        info!("commands registered globally (can take a few minutes to show up)");
                    }
                }
                ctx.set_activity(Some(serenity::ActivityData::watching("portwatch-vi.xyz")));

                // Setup runs again after a reconnect; only start the loop once.
                static STARTED: std::sync::Once = std::sync::Once::new();
                STARTED.call_once(|| {
                    tokio::spawn(widget::run_updater(ctx.http.clone(), state.clone()));
                });
                Ok(state)
            })
        })
        .build();

    let intents = serenity::GatewayIntents::GUILDS;
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .context("creating Discord client")?;

    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("shutting down");
            shard_manager.shutdown_all().await;
        }
    });

    client
        .start_autosharded()
        .await
        .context("Discord client stopped")?;
    Ok(())
}
