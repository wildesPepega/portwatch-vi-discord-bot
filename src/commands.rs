//! Slash commands. Setup commands need the Manage Server permission; the rest are for everyone.

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, CreateEmbed, CreateEmbedFooter, GuildChannel, Mentionable as _, MessageId,
    Permissions, Role,
};

use crate::store::{DEFAULT_UPDATES_COUNT, UpdatesMode};
use crate::tracker::{SITE_URL, effort_byline, format_pct};
use crate::widget::{
    BRAND_COLOR, Data, image_attachment, post_widget, progress_embed, site_button_row,
    updates_embed,
};

pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;

pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![
        widget(),
        alerts(),
        settings(),
        progress(),
        leaderboard(),
        help(),
    ]
}

/// Ephemeral text reply.
async fn reply(ctx: Context<'_>, text: impl Into<String>) -> Result<(), Error> {
    ctx.send(CreateReply::default().content(text).ephemeral(true))
        .await?;
    Ok(())
}

fn guild_id(ctx: Context<'_>) -> Result<u64, Error> {
    ctx.guild_id()
        .map(|g| g.get())
        .ok_or_else(|| anyhow::anyhow!("This command only works in a server."))
}

/// Checks the bot can post the widget/alerts in `channel`. Returns the missing permissions, if any.
fn missing_permissions(
    ctx: Context<'_>,
    channel: &GuildChannel,
    needed: Permissions,
) -> Option<Permissions> {
    let bot = ctx.cache().current_user().id;
    let guild = ctx.guild()?;
    // Not enough in the cache to tell: let Discord decide when we post.
    let member = guild.members.get(&bot)?;
    let perms = guild.user_permissions_in(channel, member);
    let missing = needed - perms;
    (!missing.is_empty()).then_some(missing)
}

fn ensure_same_guild(ctx: Context<'_>, channel: &GuildChannel) -> Result<(), Error> {
    if Some(channel.guild_id) != ctx.guild_id() {
        anyhow::bail!("Please pick a channel from this server.");
    }
    Ok(())
}

const WIDGET_PERMS: Permissions = Permissions::VIEW_CHANNEL
    .union(Permissions::SEND_MESSAGES)
    .union(Permissions::EMBED_LINKS)
    .union(Permissions::ATTACH_FILES)
    .union(Permissions::READ_MESSAGE_HISTORY);

/// Permissions requested by the invite link printed at startup.
pub const INVITE_PERMISSIONS: Permissions = WIDGET_PERMS;

const ALERT_PERMS: Permissions = Permissions::VIEW_CHANNEL
    .union(Permissions::SEND_MESSAGES)
    .union(Permissions::EMBED_LINKS);

// ---------------------------------------------------------------------------------------------
// /widget

/// Set up the live progress widget.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    subcommands("widget_setup", "widget_updates", "widget_remove", "widget_repost"),
    subcommand_required
)]
pub async fn widget(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Post the live progress widget in a channel. It updates itself every 30 seconds.
#[poise::command(slash_command, guild_only, rename = "setup")]
pub async fn widget_setup(
    ctx: Context<'_>,
    #[description = "Channel for the widget"]
    #[channel_types("Text", "News")]
    channel: GuildChannel,
) -> Result<(), Error> {
    let guild = guild_id(ctx)?;
    ensure_same_guild(ctx, &channel)?;
    if let Some(missing) = missing_permissions(ctx, &channel, WIDGET_PERMS) {
        return reply(
            ctx,
            format!(
                "❌ I'm missing these permissions in {}: **{missing}**",
                channel.mention()
            ),
        )
        .await;
    }
    ctx.defer_ephemeral().await?;

    let state = ctx.data();
    let snap = state.current().await?;
    let http = ctx.serenity_context().http.clone();

    // Remove the old widget, if any, so there's only ever one per server.
    let old = state.store.get(guild).await;
    if let (Some(ch), Some(msg)) = (old.widget_channel, old.widget_message) {
        let _ = ChannelId::new(ch)
            .delete_message(&http, MessageId::new(msg))
            .await;
    }

    let id = post_widget(&http, state, guild, channel.id, &snap, &old)
        .await
        .map_err(|e| anyhow::anyhow!("I couldn't post in {}: {e}", channel.mention()))?;
    state
        .store
        .update(guild, |c| {
            c.widget_channel = Some(channel.id.get());
            c.widget_message = Some(id.get());
            c.widget_error = None;
        })
        .await?;

    reply(
        ctx,
        format!(
            "✅ The PortWatch VI widget is live in {}. It refreshes every {} seconds.\n\
             Tip: lock the channel so the widget stays at the top, or use `/widget repost` to move it to the bottom.",
            channel.mention(),
            state.interval.as_secs()
        ),
    )
    .await
}

/// Choose what the "latest updates" embed under the widget shows, or turn it off.
#[poise::command(slash_command, guild_only, rename = "updates")]
pub async fn widget_updates(
    ctx: Context<'_>,
    #[description = "What to show under the widget"] show: UpdatesMode,
    #[description = "How many entries (1-5, default 3)"]
    #[min = 1]
    #[max = 5]
    count: Option<u8>,
) -> Result<(), Error> {
    let guild = guild_id(ctx)?;
    let state = ctx.data();
    let cfg = state
        .store
        .update(guild, |c| {
            c.updates = show;
            if count.is_some() {
                c.updates_count = count;
            }
            c.clone()
        })
        .await?;

    let mut text = match show {
        UpdatesMode::Off => "✅ The updates embed is now **off**.".to_owned(),
        _ => format!(
            "✅ The widget now shows the latest **{}** {}.",
            cfg.updates_count(),
            if show == UpdatesMode::News {
                "news articles"
            } else {
                "tracker changes"
            }
        ),
    };
    if cfg.widget_channel.is_some() {
        text.push_str(&format!(
            " The widget picks this up within {} seconds.",
            state.interval.as_secs()
        ));
    } else {
        text.push_str(" Set up the widget with `/widget setup` to see it.");
    }
    let mut reply = CreateReply::default().content(text).ephemeral(true);
    if let Some(preview) = state
        .snapshot
        .read()
        .await
        .as_ref()
        .and_then(|snap| updates_embed(&snap.overview, show, cfg.updates_count()))
    {
        reply = reply.embed(preview);
    }
    ctx.send(reply).await?;
    Ok(())
}

/// Stop the widget and delete its message.
#[poise::command(slash_command, guild_only, rename = "remove")]
pub async fn widget_remove(ctx: Context<'_>) -> Result<(), Error> {
    let guild = guild_id(ctx)?;
    let state = ctx.data();
    let cfg = state.store.get(guild).await;
    let Some(channel) = cfg.widget_channel else {
        return reply(ctx, "There's no widget set up on this server.").await;
    };
    if let Some(msg) = cfg.widget_message {
        let _ = ChannelId::new(channel)
            .delete_message(ctx.http(), MessageId::new(msg))
            .await;
    }
    state
        .store
        .update(guild, |c| {
            c.widget_channel = None;
            c.widget_message = None;
            c.widget_error = None;
        })
        .await?;
    state.forget_uploaded(guild).await;
    reply(ctx, "🗑️ Widget removed.").await
}

/// Post the widget again as a new message (e.g. to move it to the bottom of the channel).
#[poise::command(slash_command, guild_only, rename = "repost")]
pub async fn widget_repost(ctx: Context<'_>) -> Result<(), Error> {
    let guild = guild_id(ctx)?;
    let state = ctx.data();
    let cfg = state.store.get(guild).await;
    let Some(channel) = cfg.widget_channel.map(ChannelId::new) else {
        return reply(ctx, "There's no widget yet. Use `/widget setup` first.").await;
    };
    ctx.defer_ephemeral().await?;
    let snap = state.current().await?;
    let http = ctx.serenity_context().http.clone();
    if let Some(msg) = cfg.widget_message {
        let _ = channel.delete_message(&http, MessageId::new(msg)).await;
    }
    let id = post_widget(&http, state, guild, channel, &snap, &cfg)
        .await
        .map_err(|e| anyhow::anyhow!("I couldn't post in <#{channel}>: {e}"))?;
    state
        .store
        .update(guild, |c| {
            c.widget_message = Some(id.get());
            c.widget_error = None;
        })
        .await?;
    reply(ctx, format!("✅ Widget reposted in <#{channel}>.")).await
}

// ---------------------------------------------------------------------------------------------
// /alerts

/// Get notified about new leaders, milestones and articles.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    subcommands("alerts_setup", "alerts_disable"),
    subcommand_required
)]
pub async fn alerts(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Post alerts in a channel when there's a new leader, a new milestone or a new article.
#[poise::command(slash_command, guild_only, rename = "setup")]
pub async fn alerts_setup(
    ctx: Context<'_>,
    #[description = "Channel for alerts"]
    #[channel_types("Text", "News")]
    channel: GuildChannel,
    #[description = "Role to ping with each alert (optional)"] role: Option<Role>,
) -> Result<(), Error> {
    let guild = guild_id(ctx)?;
    ensure_same_guild(ctx, &channel)?;
    if let Some(missing) = missing_permissions(ctx, &channel, ALERT_PERMS) {
        return reply(
            ctx,
            format!(
                "❌ I'm missing these permissions in {}: **{missing}**",
                channel.mention()
            ),
        )
        .await;
    }
    if let Some(r) = &role
        && r.id.get() == guild
    {
        return reply(
            ctx,
            "❌ Pinging @everyone isn't supported. Pick a regular role.",
        )
        .await;
    }

    ctx.data()
        .store
        .update(guild, |c| {
            c.alert_channel = Some(channel.id.get());
            c.alert_role = role.as_ref().map(|r| r.id.get());
            c.alert_error = None;
        })
        .await?;

    let mut text = format!("🔔 Alerts will be posted in {}", channel.mention());
    if let Some(r) = &role {
        text.push_str(&format!(" and ping {}", r.mention()));
        if !r.mentionable {
            text.push_str(
                ".\n⚠️ That role isn't mentionable: give me the **Mention @everyone, @here and All Roles** permission or make the role mentionable",
            );
        }
    }
    text.push('.');
    reply(ctx, text).await
}

/// Stop posting alerts.
#[poise::command(slash_command, guild_only, rename = "disable")]
pub async fn alerts_disable(ctx: Context<'_>) -> Result<(), Error> {
    let guild = guild_id(ctx)?;
    ctx.data()
        .store
        .update(guild, |c| {
            c.alert_channel = None;
            c.alert_role = None;
            c.alert_error = None;
        })
        .await?;
    reply(ctx, "🔕 Alerts disabled.").await
}

// ---------------------------------------------------------------------------------------------
// /settings

/// Show this server's PortWatch VI settings.
#[poise::command(slash_command, guild_only, default_member_permissions = "MANAGE_GUILD")]
pub async fn settings(ctx: Context<'_>) -> Result<(), Error> {
    let guild = guild_id(ctx)?;
    let cfg = ctx.data().store.get(guild).await;

    let widget = match (cfg.widget_channel, cfg.widget_message, &cfg.widget_error) {
        (None, ..) => "Not set up. Use `/widget setup`.".to_owned(),
        (Some(ch), _, Some(err)) => {
            format!("<#{ch}> · ⚠️ **Paused:** {err}\nFix it, then run `/widget setup` again.")
        }
        (Some(ch), Some(msg), None) => {
            format!(
                "<#{ch}> · ✅ live ([jump to widget](https://discord.com/channels/{guild}/{ch}/{msg}))"
            )
        }
        (Some(ch), None, None) => format!("<#{ch}> · ⏳ posting on the next refresh"),
    };
    let alerts = match (cfg.alert_channel, cfg.alert_role, &cfg.alert_error) {
        (None, ..) => "Off. Use `/alerts setup`.".to_owned(),
        (Some(ch), _, Some(err)) => {
            format!("<#{ch}> · ⚠️ **Paused:** {err}\nFix it, then run `/alerts setup` again.")
        }
        (Some(ch), Some(role), None) => format!("<#{ch}> · pings <@&{role}>"),
        (Some(ch), None, None) => format!("<#{ch}> · no ping"),
    };

    let embed = CreateEmbed::new()
        .title("PortWatch VI settings")
        .color(BRAND_COLOR)
        .field("Live widget", widget, false)
        .field("Alerts", alerts, false)
        .field(
            "Updates embed",
            match cfg.updates {
                UpdatesMode::Off => "Off".to_owned(),
                UpdatesMode::News => format!("Latest {} news articles", cfg.updates_count()),
                UpdatesMode::Changes => format!("Latest {} tracker changes", cfg.updates_count()),
            },
            false,
        )
        .field(
            "Refresh interval",
            format!("{} seconds", ctx.data().interval.as_secs()),
            false,
        );
    ctx.send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Public commands

/// Show the current progress of the GTA VI PC port race.
#[poise::command(slash_command)]
pub async fn progress(
    ctx: Context<'_>,
    #[description = "Only show it to me"] private: Option<bool>,
) -> Result<(), Error> {
    ctx.defer_or_broadcast().await.ok();
    let snap = ctx.data().current().await?;
    ctx.send(
        CreateReply::default()
            .embed(
                progress_embed(&snap, ctx.data().interval, false)
                    .timestamp(serenity::Timestamp::now()),
            )
            .attachment(image_attachment(&snap))
            .components(vec![site_button_row()])
            .ephemeral(private.unwrap_or(false)),
    )
    .await?;
    Ok(())
}

/// Show the top efforts to run GTA VI on PC.
#[poise::command(slash_command)]
pub async fn leaderboard(
    ctx: Context<'_>,
    #[description = "How many efforts to show (default 10)"]
    #[min = 3]
    #[max = 25]
    count: Option<u8>,
) -> Result<(), Error> {
    let snap = ctx.data().current().await?;
    let o = &snap.overview;
    let count = usize::from(count.unwrap_or(10));

    let mut desc = String::new();
    for (i, r) in o.board().take(count).enumerate() {
        let medal = match i {
            0 => "🥇".to_owned(),
            1 => "🥈".to_owned(),
            2 => "🥉".to_owned(),
            n => format!("`#{}`", n + 1),
        };
        let line = format!(
            "{medal} **[{name}]({SITE_URL}#effort-{id})** {by}\n\u{2003}**{score}** · {ms}\n",
            name = r.effort.name,
            id = r.effort.id,
            by = effort_byline(o, &r.effort),
            score = format_pct(r.score.score),
            ms = r.score.reached.label(),
        );
        if desc.len() + line.len() > 4000 {
            break;
        }
        desc.push_str(&line);
    }
    if desc.is_empty() {
        desc.push_str("No efforts tracked yet.");
    }

    let embed = CreateEmbed::new()
        .title("PortWatch VI leaderboard")
        .url(format!("{SITE_URL}#race"))
        .color(BRAND_COLOR)
        .description(desc)
        .footer(CreateEmbedFooter::new(format!(
            "{} efforts tracked · scored from groundwork (0%) to a game you can finish (100%)",
            o.total
        )))
        .timestamp(serenity::Timestamp::now());
    ctx.send(CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// How to use the PortWatch VI bot.
#[poise::command(slash_command)]
pub async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let embed = CreateEmbed::new()
        .title("PortWatch VI bot")
        .url(SITE_URL)
        .color(BRAND_COLOR)
        .description(
            "Tracks [PortWatch VI](https://portwatch-vi.xyz/), the live tracker of every effort to run GTA VI on PC.",
        )
        .field(
            "Setup (Manage Server)",
            format!("`/widget setup #channel` · post the live progress widget (refreshes every {}s)\n\
             `/widget updates <show> [count]` · latest news / changelog under the widget, or off (default: {DEFAULT_UPDATES_COUNT} news)\n\
             `/widget repost` · post it again at the bottom of the channel\n\
             `/widget remove` · remove the widget\n\
             `/alerts setup #channel [@role]` · alerts for new leaders, milestones and articles\n\
             `/alerts disable` · turn alerts off\n\
             `/settings` · show this server's settings",
                ctx.data().interval.as_secs()
            ),
            false,
        )
        .field(
            "For everyone",
            "`/progress` · current progress as an image\n\
             `/leaderboard [count]` · top efforts",
            false,
        );
    ctx.send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}
