//! Shared bot state, the 30-second update loop, the widget message and change alerts.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, CreateActionRow, CreateAttachment, CreateButton, CreateEmbed, CreateEmbedFooter,
    CreateMessage, EditMessage, Http, MessageId,
};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::render;
use crate::store::{AlertState, GuildConfig, Store, UpdatesMode};
use crate::tracker::{Fetcher, Milestone, NewsItem, Overview, SITE_URL, effort_byline, format_pct};

pub const IMAGE_NAME: &str = "portwatch.png";
pub const BRAND_COLOR: u32 = 0xff2e7e;
/// How many widget edits run at once. Serenity queues the rest behind Discord's rate limits.
const CONCURRENCY: usize = 8;

pub struct Snapshot {
    pub overview: Overview,
    pub png: Arc<Vec<u8>>,
    /// Hash of the rendered SVG: changes only when what the image shows changes.
    pub image_hash: u64,
    /// When the site was last read successfully.
    pub fetched_at: DateTime<Utc>,
    /// When the bot last tried to read the site.
    pub checked_at: DateTime<Utc>,
    /// Set when the last attempt failed (the data shown is from `fetched_at`).
    pub fetch_error: Option<String>,
}

pub struct BotState {
    pub store: Store,
    pub fetcher: Fetcher,
    pub interval: Duration,
    pub snapshot: RwLock<Option<Arc<Snapshot>>>,
    /// Guild -> image hash last uploaded to its widget message (in memory: a restart re-uploads once).
    uploaded: Mutex<HashMap<u64, u64>>,
    /// Serialises refreshes so a command and the loop don't fetch and render twice at once.
    refresh_lock: Mutex<()>,
}

pub type Data = Arc<BotState>;

impl BotState {
    pub fn new(store: Store, fetcher: Fetcher, interval: Duration) -> Self {
        Self {
            store,
            fetcher,
            interval,
            snapshot: RwLock::new(None),
            uploaded: Mutex::new(HashMap::new()),
            refresh_lock: Mutex::new(()),
        }
    }

    /// The current snapshot, fetching one first if the bot has none yet.
    pub async fn current(&self) -> anyhow::Result<Arc<Snapshot>> {
        if let Some(s) = self.snapshot.read().await.clone() {
            return Ok(s);
        }
        let (snap, _) = self.refresh().await;
        snap.ok_or_else(|| {
            anyhow::anyhow!("PortWatch VI could not be reached. Try again in a moment.")
        })
    }

    /// Fetches the tracker, re-renders the image if anything changed and stores the snapshot.
    /// Returns the snapshot (possibly stale) and whether this fetch succeeded.
    pub async fn refresh(&self) -> (Option<Arc<Snapshot>>, bool) {
        let _guard = self.refresh_lock.lock().await;
        let prev = self.snapshot.read().await.clone();
        let now = Utc::now();

        let result = async {
            let tracker = self.fetcher.fetch().await?;
            let overview = Overview::from_tracker(&tracker);
            let svg = render::build_svg(&overview);
            let mut h = DefaultHasher::new();
            svg.hash(&mut h);
            let image_hash = h.finish();

            let png = match &prev {
                Some(p) if p.image_hash == image_hash => p.png.clone(),
                _ => {
                    let png =
                        tokio::task::spawn_blocking(move || render::render_png(&svg)).await??;
                    info!(
                        bytes = png.len(),
                        leader = overview.score(),
                        "rendered new widget image"
                    );
                    Arc::new(png)
                }
            };
            anyhow::Ok(Snapshot {
                overview,
                png,
                image_hash,
                fetched_at: now,
                checked_at: now,
                fetch_error: None,
            })
        }
        .await;

        let (snap, ok) = match result {
            Ok(s) => (Some(Arc::new(s)), true),
            Err(e) => {
                warn!("refresh failed: {e:#}");
                let stale = prev.map(|p| {
                    Arc::new(Snapshot {
                        overview: p.overview.clone(),
                        png: p.png.clone(),
                        image_hash: p.image_hash,
                        fetched_at: p.fetched_at,
                        checked_at: now,
                        fetch_error: Some(format!("{e:#}")),
                    })
                });
                (stale, false)
            }
        };
        if let Some(s) = &snap {
            *self.snapshot.write().await = Some(s.clone());
        }
        (snap, ok)
    }

    pub async fn mark_uploaded(&self, guild: u64, hash: u64) {
        self.uploaded.lock().await.insert(guild, hash);
    }

    pub async fn forget_uploaded(&self, guild: u64) {
        self.uploaded.lock().await.remove(&guild);
    }
}

// ---------------------------------------------------------------------------------------------
// Widget message

pub fn site_button_row() -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new_link(SITE_URL).label("Open PortWatch VI"),
        CreateButton::new_link(format!("{SITE_URL}#race")).label("Full leaderboard"),
    ])
}

/// The embed around the image. `live` adds the "last checked" line used by the auto-updating widget.
pub fn progress_embed(snap: &Snapshot, interval: Duration, live: bool) -> CreateEmbed {
    let o = &snap.overview;
    let mut desc = String::new();
    match o.leader() {
        Some(l) => {
            desc.push_str(&format!(
                "**Leader:** [{name}]({SITE_URL}#effort-{id}) {by} · **{score}** · {ms}\n",
                name = l.effort.name,
                id = l.effort.id,
                by = effort_byline(o, &l.effort),
                score = format_pct(l.score.score),
                ms = l.score.reached.label(),
            ));
        }
        None => desc.push_str("No efforts tracked yet.\n"),
    }
    desc.push_str(&format!(
        "**Top-3 average:** {} · **Furthest milestone:** {}\n",
        format_pct(o.top3),
        o.furthest.label()
    ));
    if let Some(n) = &o.latest_news
        && let Some(title) = &n.title
    {
        desc.push_str(&format!("\n📰 **Latest:** [{title}]({})\n", news_url(n)));
    }
    if live {
        desc.push_str(&format!(
            "\n🔄 Last checked <t:{}:R> · refreshes every {}s",
            snap.checked_at.timestamp(),
            interval.as_secs()
        ));
        if snap.fetch_error.is_some() {
            desc.push_str(&format!(
                "\n⚠️ The site couldn't be reached on the last check. Showing data from <t:{}:R>.",
                snap.fetched_at.timestamp()
            ));
        }
    }

    CreateEmbed::new()
        .title(format!("Does GTA VI run on PC yet? {}", o.answer.text()))
        .url(SITE_URL)
        .color(BRAND_COLOR)
        .description(desc)
        .image(format!("attachment://{IMAGE_NAME}"))
        .footer(CreateEmbedFooter::new(
            "PortWatch VI · Live tracker of every PC port effort",
        ))
}

/// Link to an article on the site (same rule as the site's `articlePath`).
pub fn news_url(n: &NewsItem) -> String {
    let valid = !n.id.is_empty()
        && n.id.len() <= 81
        && n.id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !n.id.starts_with('-');
    if valid {
        format!("{SITE_URL}news/{}", n.id)
    } else {
        format!("{SITE_URL}#news")
    }
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}

/// Discord timestamp for a `YYYY-MM-DD` date, or the date as text.
fn day(date: Option<&str>) -> String {
    match date.and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
        Some(d) => format!(
            "<t:{}:D>",
            d.and_hms_opt(12, 0, 0)
                .unwrap_or_default()
                .and_utc()
                .timestamp()
        ),
        None => date.unwrap_or("").to_owned(),
    }
}

/// The optional second embed: the latest articles or changelog entries.
pub fn updates_embed(o: &Overview, mode: UpdatesMode, count: usize) -> Option<CreateEmbed> {
    let (title, url, lines): (&str, String, Vec<String>) = match mode {
        UpdatesMode::Off => return None,
        UpdatesMode::News => (
            "📰 Latest news",
            format!("{SITE_URL}#news"),
            o.news
                .iter()
                .take(count)
                .map(|n| {
                    let mut line = format!(
                        "**[{}]({})**",
                        cut(n.title.as_deref().unwrap_or("Untitled"), 200),
                        news_url(n)
                    );
                    let meta: Vec<String> = [n.category.clone(), Some(day(n.date.as_deref()))]
                        .into_iter()
                        .flatten()
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !meta.is_empty() {
                        line.push_str(&format!("\n-# {}", meta.join(" · ")));
                    }
                    if let Some(dek) = &n.dek {
                        line.push_str(&format!("\n{}", cut(dek, 280)));
                    }
                    line
                })
                .collect(),
        ),
        UpdatesMode::Changes => (
            "📋 Latest tracker changes",
            format!("{SITE_URL}#changes"),
            o.changes
                .iter()
                .take(count)
                .map(|c| {
                    let icon = match c.kind.as_deref() {
                        Some("up") => "📈",
                        Some("down") => "📉",
                        Some("new") => "🔎",
                        Some("claim") => "🗣️",
                        _ => "•",
                    };
                    format!(
                        "{icon} {}\n-# {}",
                        cut(&c.text, 400),
                        day(c.date.as_deref())
                    )
                })
                .collect(),
        ),
    };
    let desc = if lines.is_empty() {
        "Nothing yet.".to_owned()
    } else {
        lines.join("\n\n")
    };
    Some(
        CreateEmbed::new()
            .title(title)
            .url(url)
            .color(BRAND_COLOR)
            .description(cut(&desc, 4000)),
    )
}

/// All embeds of the live widget message for one server.
pub fn widget_embeds(snap: &Snapshot, interval: Duration, cfg: &GuildConfig) -> Vec<CreateEmbed> {
    let mut embeds = vec![progress_embed(snap, interval, true)];
    embeds.extend(updates_embed(
        &snap.overview,
        cfg.updates,
        cfg.updates_count(),
    ));
    embeds
}

pub fn image_attachment(snap: &Snapshot) -> CreateAttachment {
    CreateAttachment::bytes(snap.png.as_slice().to_vec(), IMAGE_NAME)
}

/// Posts a fresh widget message and returns its id.
pub async fn post_widget(
    http: &Http,
    state: &BotState,
    guild: u64,
    channel: ChannelId,
    snap: &Snapshot,
    cfg: &GuildConfig,
) -> serenity::Result<MessageId> {
    let msg = channel
        .send_message(
            http,
            CreateMessage::new()
                .embeds(widget_embeds(snap, state.interval, cfg))
                .components(vec![site_button_row()])
                .add_file(image_attachment(snap)),
        )
        .await?;
    state.mark_uploaded(guild, snap.image_hash).await;
    Ok(msg.id)
}

/// Discord's JSON error code, if this is an API error.
pub fn discord_code(e: &serenity::Error) -> Option<isize> {
    match e {
        serenity::Error::Http(serenity::HttpError::UnsuccessfulRequest(r)) => Some(r.error.code),
        _ => None,
    }
}

const UNKNOWN_CHANNEL: isize = 10003;
const UNKNOWN_MESSAGE: isize = 10008;
const MISSING_ACCESS: isize = 50001;
const MISSING_PERMISSIONS: isize = 50013;

async fn update_guild_widget(
    http: &Http,
    state: &BotState,
    guild: u64,
    cfg: GuildConfig,
    snap: &Snapshot,
) {
    let Some(channel) = cfg.widget_channel.map(ChannelId::new) else {
        return;
    };
    if cfg.widget_error.is_some() {
        return; // paused until an admin runs /widget setup again
    }

    let result = match cfg.widget_message.map(MessageId::new) {
        Some(message) => {
            let needs_upload = state.uploaded.lock().await.get(&guild) != Some(&snap.image_hash);
            let mut edit = EditMessage::new()
                .embeds(widget_embeds(snap, state.interval, &cfg))
                .components(vec![site_button_row()]);
            if needs_upload {
                edit = edit.new_attachment(image_attachment(snap));
            }
            match channel.edit_message(http, message, edit).await {
                Ok(_) => {
                    if needs_upload {
                        state.mark_uploaded(guild, snap.image_hash).await;
                    }
                    Ok(())
                }
                // Someone deleted the widget: post a new one.
                Err(e) if discord_code(&e) == Some(UNKNOWN_MESSAGE) => {
                    info!(guild, "widget message gone, posting a new one");
                    repost(http, state, guild, channel, snap, &cfg).await
                }
                Err(e) => Err(e),
            }
        }
        None => repost(http, state, guild, channel, snap, &cfg).await,
    };

    if let Err(e) = result {
        let code = discord_code(&e);
        warn!(guild, ?code, "widget update failed: {e}");
        let reason = match code {
            Some(UNKNOWN_CHANNEL) => Some("The widget channel no longer exists."),
            Some(MISSING_ACCESS) => Some("The bot can't see the widget channel."),
            Some(MISSING_PERMISSIONS) => Some(
                "The bot is missing permissions in the widget channel (needs Send Messages, Embed Links and Attach Files).",
            ),
            _ => None, // transient (network, 5xx): try again next tick
        };
        if let Some(reason) = reason {
            let _ = state
                .store
                .update(guild, |c| c.widget_error = Some(reason.to_owned()))
                .await;
        }
    }
}

async fn repost(
    http: &Http,
    state: &BotState,
    guild: u64,
    channel: ChannelId,
    snap: &Snapshot,
    cfg: &GuildConfig,
) -> serenity::Result<()> {
    let id = post_widget(http, state, guild, channel, snap, cfg).await?;
    if let Err(e) = state
        .store
        .update(guild, |c| {
            // Only if the widget wasn't moved or removed meanwhile.
            if c.widget_channel == Some(channel.get()) {
                c.widget_message = Some(id.get());
            }
        })
        .await
    {
        warn!(guild, "saving widget message id failed: {e:#}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Alerts

fn alert_state_of(o: &Overview) -> AlertState {
    AlertState {
        leader_id: o.leader().map(|l| l.effort.id.clone()),
        furthest: Some(o.furthest.key().to_owned()),
        answer: Some(o.answer.text().to_owned()),
        news_id: o.latest_news.as_ref().map(|n| n.id.clone()),
    }
}

fn milestone_index(key: Option<&str>) -> Option<usize> {
    key.and_then(|k| Milestone::ALL.iter().position(|m| m.key() == k))
}

/// Builds the alert lines for what changed between two states.
fn alert_lines(old: &AlertState, o: &Overview) -> Vec<String> {
    let new = alert_state_of(o);
    let mut lines = Vec::new();

    if let (Some(prev_idx), Some(new_idx)) = (
        milestone_index(old.furthest.as_deref()),
        milestone_index(new.furthest.as_deref()),
    ) && new_idx > prev_idx
    {
        lines.push(format!(
            "🚀 **New milestone reached:** an effort is now at **“{}”** ({}%).",
            o.furthest.label(),
            o.furthest.at() as u32
        ));
    }

    if old.leader_id.is_some()
        && new.leader_id != old.leader_id
        && let Some(l) = o.leader()
    {
        let prev_name = old
            .leader_id
            .as_deref()
            .and_then(|id| o.ranked.iter().find(|r| r.effort.id == id))
            .map(|r| {
                format!(
                    ", taking over from **{}** ({})",
                    r.effort.name,
                    format_pct(r.score.score)
                )
            })
            .unwrap_or_default();
        lines.push(format!(
            "👑 **New leader:** [{}]({SITE_URL}#effort-{}) {} at **{}** · {}{prev_name}.",
            l.effort.name,
            l.effort.id,
            effort_byline(o, &l.effort),
            format_pct(l.score.score),
            l.score.reached.label(),
        ));
    }

    if old.answer.is_some() && new.answer != old.answer {
        lines.push(format!(
            "❓ **Does GTA VI run on PC yet?** The answer is now **{}**",
            o.answer.text()
        ));
    }

    if old.news_id.is_some()
        && new.news_id != old.news_id
        && let Some(n) = &o.latest_news
    {
        let title = n.title.as_deref().unwrap_or("New article");
        let mut line = format!("📰 **New article:** [{title}]({})", news_url(n));
        if let Some(dek) = &n.dek {
            line.push_str(&format!("\n> {dek}"));
        }
        lines.push(line);
    }

    lines
}

async fn send_alerts(http: &Http, state: &BotState, o: &Overview) {
    let new_state = alert_state_of(o);
    let Some(old) = state.store.alert_state().await else {
        // First run: remember the current state without announcing it.
        let _ = state.store.set_alert_state(new_state).await;
        return;
    };
    let lines = alert_lines(&old, o);
    if let Err(e) = state.store.set_alert_state(new_state).await {
        warn!("saving alert state failed: {e:#}");
    }
    if lines.is_empty() {
        return;
    }
    info!(count = lines.len(), "sending alerts");

    let embed = CreateEmbed::new()
        .title("PortWatch VI update")
        .url(SITE_URL)
        .color(BRAND_COLOR)
        .description(lines.join("\n\n"))
        .timestamp(serenity::Timestamp::now());

    let guilds = state.store.all().await;
    futures::stream::iter(guilds)
        .for_each_concurrent(CONCURRENCY, |(guild, cfg)| {
            let embed = embed.clone();
            async move {
                let (Some(channel), None) = (cfg.alert_channel, cfg.alert_error.as_ref()) else {
                    return;
                };
                let mut msg = CreateMessage::new()
                    .embed(embed)
                    .components(vec![site_button_row()]);
                if let Some(role) = cfg.alert_role {
                    msg = msg.content(format!("<@&{role}>")).allowed_mentions(
                        serenity::CreateAllowedMentions::new().roles(vec![serenity::RoleId::new(role)]),
                    );
                }
                if let Err(e) = ChannelId::new(channel).send_message(http, msg).await {
                    let code = discord_code(&e);
                    warn!(guild, ?code, "alert failed: {e}");
                    if matches!(code, Some(UNKNOWN_CHANNEL | MISSING_ACCESS | MISSING_PERMISSIONS)) {
                        let _ = state
                            .store
                            .update(guild, |c| {
                                c.alert_error = Some(
                                    "The bot can't post in the alert channel (deleted, hidden or missing permissions)."
                                        .to_owned(),
                                )
                            })
                            .await;
                    }
                }
            }
        })
        .await;
}

// ---------------------------------------------------------------------------------------------
// Loop

pub async fn run_updater(http: Arc<Http>, state: Data) {
    let mut ticker = tokio::time::interval(state.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    info!(every = ?state.interval, "widget updater started");

    loop {
        ticker.tick().await;
        let (snap, ok) = state.refresh().await;
        let Some(snap) = snap else { continue };

        if ok {
            send_alerts(&http, &state, &snap.overview).await;
        }

        let guilds = state.store.all().await;
        futures::stream::iter(guilds)
            .for_each_concurrent(CONCURRENCY, |(guild, cfg)| {
                let (http, state, snap) = (&http, &state, &snap);
                async move { update_guild_widget(http, state, guild, cfg, snap).await }
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::Tracker;

    fn overview(efforts: serde_json::Value, news: serde_json::Value) -> Overview {
        let t: Tracker = serde_json::from_value(serde_json::json!({
            "updated": "2026-11-25T13:45:26.000Z", "efforts": efforts, "news": news
        }))
        .unwrap();
        Overview::from_tracker(&t)
    }

    #[test]
    fn updates_embed_modes() {
        let t: Tracker = serde_json::from_value(serde_json::json!({
            "efforts": [],
            "news": [
                { "id": "2026-11-25-a", "date": "2026-11-25", "category": "Port", "title": "A", "dek": "dek a" },
                { "id": "2026-11-24-b", "title": "B" },
                { "id": "Bad Id", "title": "C" },
                { "id": "2026-11-23-d", "title": "D" }
            ],
            "changes": [
                { "date": "2026-11-24", "kind": "new", "text": "oldest" },
                { "date": "2026-11-25", "kind": "up", "text": "newest" }
            ]
        }))
        .unwrap();
        let o = Overview::from_tracker(&t);
        assert!(updates_embed(&o, UpdatesMode::Off, 3).is_none());

        let news = serde_json::to_value(updates_embed(&o, UpdatesMode::News, 3).unwrap()).unwrap();
        let d = news["description"].as_str().unwrap();
        assert!(d.contains("(https://portwatch-vi.xyz/news/2026-11-25-a)"));
        assert!(
            d.contains("(https://portwatch-vi.xyz/#news)"),
            "invalid ids link to the list"
        );
        assert!(!d.contains("**[D]"), "only 3 entries");

        let changes =
            serde_json::to_value(updates_embed(&o, UpdatesMode::Changes, 3).unwrap()).unwrap();
        let d = changes["description"].as_str().unwrap();
        assert!(
            d.find("newest").unwrap() < d.find("oldest").unwrap(),
            "newest first"
        );
    }

    #[test]
    fn alerts_on_new_leader_and_milestone() {
        let o = overview(
            serde_json::json!([
                { "id": "a", "name": "A", "repo": "x/a", "tier": "tracked",
                  "gta": { "loads": 1, "boots": 1, "menu": 1, "world": 1, "playable": 1 } },
                { "id": "b", "name": "B", "repo": "x/b", "tier": "tracked", "gta": { "loads": 1 } }
            ]),
            serde_json::json!([{ "id": "n2", "title": "Big news" }]),
        );
        let old = AlertState {
            leader_id: Some("b".into()),
            furthest: Some("loads".into()),
            answer: Some("Not yet.".into()),
            news_id: Some("n1".into()),
        };
        let lines = alert_lines(&old, &o);
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(lines[0].contains("Playable"));
        assert!(lines[1].contains("New leader") && lines[1].contains("taking over from **B**"));
        assert!(lines[2].contains("Sort of."));
        assert!(lines[3].contains("Big news"));

        // Same state again: nothing to say.
        assert!(alert_lines(&alert_state_of(&o), &o).is_empty());
    }
}
