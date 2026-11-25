//! Per-server settings, persisted as one JSON file (written atomically).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuildConfig {
    /// Channel holding the live widget.
    pub widget_channel: Option<u64>,
    /// The widget message the bot keeps editing.
    pub widget_message: Option<u64>,
    /// Set when the widget can't be updated (missing access, channel gone…); cleared by `/widget setup`.
    pub widget_error: Option<String>,
    /// Channel for change alerts (new leader, new milestone, new article).
    pub alert_channel: Option<u64>,
    /// Role to ping with alerts.
    pub alert_role: Option<u64>,
    pub alert_error: Option<String>,
    /// Second embed under the widget with recent updates.
    #[serde(default)]
    pub updates: UpdatesMode,
    /// How many entries that embed shows (1-5).
    #[serde(default)]
    pub updates_count: Option<u8>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, poise::ChoiceParameter,
)]
#[serde(rename_all = "lowercase")]
pub enum UpdatesMode {
    /// No updates embed.
    #[name = "Off"]
    Off,
    /// The site's latest news articles.
    #[default]
    #[name = "News articles"]
    News,
    /// The site's changelog (score changes, new efforts spotted, claims).
    #[name = "Tracker changelog"]
    Changes,
}

pub const DEFAULT_UPDATES_COUNT: u8 = 3;

impl GuildConfig {
    pub fn updates_count(&self) -> usize {
        usize::from(
            self.updates_count
                .unwrap_or(DEFAULT_UPDATES_COUNT)
                .clamp(1, 5),
        )
    }
}

impl GuildConfig {
    fn is_empty(&self) -> bool {
        self.widget_channel.is_none() && self.alert_channel.is_none()
    }
}

/// What the alerts compare against, persisted so a restart doesn't re-announce everything.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AlertState {
    pub leader_id: Option<String>,
    pub furthest: Option<String>,
    pub answer: Option<String>,
    pub news_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileData {
    #[serde(default)]
    guilds: HashMap<u64, GuildConfig>,
    #[serde(default)]
    alert_state: Option<AlertState>,
}

pub struct Store {
    path: PathBuf,
    data: RwLock<FileData>,
}

impl Store {
    pub async fn load(path: PathBuf) -> anyhow::Result<Self> {
        let data = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileData::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path,
            data: RwLock::new(data),
        })
    }

    async fn save(&self, data: &FileData) -> anyhow::Result<()> {
        let json = serde_json::to_vec_pretty(data)?;
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(dir).await?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    pub async fn get(&self, guild: u64) -> GuildConfig {
        self.data
            .read()
            .await
            .guilds
            .get(&guild)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn all(&self) -> Vec<(u64, GuildConfig)> {
        self.data
            .read()
            .await
            .guilds
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    /// Changes one server's settings and saves. The write lock is held across the save,
    /// so concurrent updates can't overwrite each other.
    pub async fn update<R>(
        &self,
        guild: u64,
        f: impl FnOnce(&mut GuildConfig) -> R,
    ) -> anyhow::Result<R> {
        let mut data = self.data.write().await;
        let cfg = data.guilds.entry(guild).or_default();
        let result = f(cfg);
        let empty = cfg.is_empty();
        if empty {
            data.guilds.remove(&guild);
        }
        self.save(&data).await?;
        Ok(result)
    }

    pub async fn remove_guild(&self, guild: u64) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        if data.guilds.remove(&guild).is_some() {
            self.save(&data).await?;
        }
        Ok(())
    }

    pub async fn alert_state(&self) -> Option<AlertState> {
        self.data.read().await.alert_state.clone()
    }

    pub async fn set_alert_state(&self, state: AlertState) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        if data.alert_state.as_ref() != Some(&state) {
            data.alert_state = Some(state);
            self.save(&data).await?;
        }
        Ok(())
    }
}
