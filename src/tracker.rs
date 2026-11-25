//! Data model for `data/tracker.json` and a faithful port of the site's `assets/js/scoring.js`.
//!
//! The site computes every number it shows in the browser from `tracker.json`, so the bot
//! does the same: fetch the JSON, score each effort, and build an [`Overview`].

use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Map, Value};

pub const SITE_URL: &str = "https://portwatch-vi.xyz/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Milestone {
    Groundwork,
    Loads,
    Boots,
    Menu,
    World,
    Playable,
    Complete,
}

impl Milestone {
    pub const ALL: [Milestone; 7] = [
        Milestone::Groundwork,
        Milestone::Loads,
        Milestone::Boots,
        Milestone::Menu,
        Milestone::World,
        Milestone::Playable,
        Milestone::Complete,
    ];

    /// The ladder steps between groundwork and playable, in order.
    const LADDER: [Milestone; 5] = [
        Milestone::Loads,
        Milestone::Boots,
        Milestone::Menu,
        Milestone::World,
        Milestone::Playable,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Milestone::Groundwork => "groundwork",
            Milestone::Loads => "loads",
            Milestone::Boots => "boots",
            Milestone::Menu => "menu",
            Milestone::World => "world",
            Milestone::Playable => "playable",
            Milestone::Complete => "complete",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Milestone::Groundwork => "Groundwork",
            Milestone::Loads => "Exe runs",
            Milestone::Boots => "First frame",
            Milestone::Menu => "Main menu",
            Milestone::World => "In the world",
            Milestone::Playable => "Playable",
            Milestone::Complete => "Complete",
        }
    }

    /// Position on the 0-100 scale.
    pub fn at(self) -> f64 {
        match self {
            Milestone::Groundwork => 0.0,
            Milestone::Loads => 10.0,
            Milestone::Boots => 20.0,
            Milestone::Menu => 30.0,
            Milestone::World => 40.0,
            Milestone::Playable => 50.0,
            Milestone::Complete => 100.0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Raw JSON

#[derive(Debug, Clone, Deserialize)]
pub struct Tracker {
    pub updated: Option<String>,
    #[serde(default)]
    pub efforts: Vec<Effort>,
    #[serde(default)]
    pub news: Vec<NewsItem>,
    /// The site's changelog, oldest first.
    #[serde(default)]
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Change {
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Effort {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub gta: Option<Map<String, Value>>,
    #[serde(default)]
    pub groundwork: Option<Map<String, Value>>,
    #[serde(default)]
    pub nuked: Option<Value>,
    #[serde(default, rename = "manualScore")]
    pub manual_score: Option<Value>,
    #[serde(default, rename = "basedOn")]
    pub based_on: Option<Vec<String>>,
    #[serde(default)]
    pub stats: Option<Stats>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Stats {
    #[serde(default)]
    pub stars: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewsItem {
    pub id: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub dek: Option<String>,
}

impl Effort {
    /// JS truthiness of `effort.nuked`.
    pub fn is_nuked(&self) -> bool {
        match &self.nuked {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
            Some(Value::String(s)) => !s.is_empty(),
            Some(_) => true,
        }
    }

    /// A private build has no public code (`repo` is not `owner/name`).
    pub fn is_private(&self) -> bool {
        !self.repo.as_deref().is_some_and(|r| r.contains('/'))
    }

    pub fn is_tracked(&self) -> bool {
        self.tier.as_deref() == Some("tracked")
    }

    pub fn owner(&self) -> Option<&str> {
        self.repo.as_deref().and_then(|r| r.split('/').next())
    }

    pub fn stars(&self) -> f64 {
        self.stats.as_ref().and_then(|s| s.stars).unwrap_or(0.0)
    }

    fn gta(&self, key: &str) -> Option<&Value> {
        self.gta.as_ref().and_then(|m| m.get(key))
    }
}

// ---------------------------------------------------------------------------------------------
// Scoring (port of scoring.js)

/// JS `+v` followed by a finiteness check; anything unparsable becomes `None`.
fn js_number(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::String(s) if s.trim().is_empty() => 0.0,
        Value::String(s) => s.trim().parse().ok()?,
        Value::Null => 0.0,
        _ => return None,
    };
    n.is_finite().then_some(n)
}

fn clamp01(v: Option<&Value>) -> f64 {
    v.and_then(js_number).map_or(0.0, |n| n.clamp(0.0, 1.0))
}

/// Ladder values are 0 (no), 0.5 (partially) or 1 (yes).
fn ladder_value(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Bool(true)) => 1.0,
        None | Some(Value::Bool(false)) | Some(Value::Null) => 0.0,
        other => clamp01(other),
    }
}

/// JS `Math.round(x * 10) / 10`.
fn round1(x: f64) -> f64 {
    (x * 10.0 + 0.5).floor() / 10.0
}

const GROUNDWORK_WEIGHTS: [(&str, f64); 4] = [
    ("loader", 0.2),
    ("shaders", 0.28),
    ("graphics", 0.32),
    ("proven", 0.2),
];

pub const TRACKS: [(&str, &str, f64); 4] = [
    ("story", "Story", 0.35),
    ("render", "Rendering", 0.25),
    ("perf", "Performance", 0.2),
    ("stability", "Stability", 0.2),
];

fn stability_label_value(label: &str) -> f64 {
    match label {
        "often" => 0.25,
        "sometimes" => 0.55,
        "rarely" => 0.8,
        "stable" => 1.0,
        _ => 0.0, // unknown, constant, anything else
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Tracks {
    pub story: f64,
    pub render: f64,
    pub perf: f64,
    pub stability: f64,
}

impl Tracks {
    fn of(e: &Effort) -> Self {
        let fps = e.gta("fps").and_then(js_number).unwrap_or(0.0);
        let stability = match e.gta("stability") {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0).clamp(0.0, 1.0),
            Some(Value::String(s)) => stability_label_value(s),
            _ => 0.0,
        };
        Tracks {
            story: clamp01(e.gta("story")),
            render: clamp01(e.gta("render")),
            perf: (fps / 30.0).clamp(0.0, 1.0),
            stability,
        }
    }

    pub fn get(&self, key: &str) -> f64 {
        match key {
            "story" => self.story,
            "render" => self.render,
            "perf" => self.perf,
            "stability" => self.stability,
            _ => 0.0,
        }
    }

    fn quality(&self) -> f64 {
        TRACKS.iter().map(|(k, _, w)| w * self.get(k)).sum()
    }
}

#[derive(Debug, Clone)]
pub struct Score {
    pub score: f64,
    pub reached: Milestone,
}

pub fn score_effort(e: &Effort) -> Score {
    let gw: f64 = GROUNDWORK_WEIGHTS
        .iter()
        .map(|(k, w)| w * clamp01(e.groundwork.as_ref().and_then(|m| m.get(*k))))
        .sum();

    let mut score = Milestone::Loads.at() * gw;
    let mut reached = Milestone::Groundwork;
    let mut prev_tick = score;

    for step in Milestone::LADDER {
        let v = ladder_value(e.gta(step.key()));
        if v >= 1.0 {
            score = step.at();
            prev_tick = score;
            reached = step;
            continue;
        }
        if v > 0.0 {
            score = score.max(prev_tick + v * (step.at() - prev_tick));
        }
        break;
    }

    let tracks = Tracks::of(e);
    if reached == Milestone::Playable {
        let quality = tracks.quality();
        score = Milestone::Playable.at()
            + quality * (Milestone::Complete.at() - Milestone::Playable.at());
        if quality >= 1.0 {
            reached = Milestone::Complete;
        }
    }

    if let Some(manual) = e.manual_score.as_ref().and_then(js_number) {
        score = manual.clamp(0.0, 100.0);
    }

    Score {
        score: round1(score),
        reached,
    }
}

// ---------------------------------------------------------------------------------------------
// Overview: everything the widget shows

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    NotYet,
    SortOf,
    Mostly,
    Yes,
}

impl Answer {
    pub fn text(self) -> &'static str {
        match self {
            Answer::NotYet => "Not yet.",
            Answer::SortOf => "Sort of.",
            Answer::Mostly => "Mostly.",
            Answer::Yes => "Yes.",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Ranked {
    pub effort: Effort,
    pub score: Score,
}

#[derive(Debug, Clone)]
pub struct Overview {
    pub updated: Option<String>,
    /// All non-nuked efforts, best first (private builds included, like the site's headline).
    pub ranked: Vec<Ranked>,
    pub top3: f64,
    pub furthest: Milestone,
    pub booted: usize,
    pub total: usize,
    pub answer: Answer,
    pub latest_news: Option<NewsItem>,
    /// Newest articles first (up to [`RECENT`]).
    pub news: Vec<NewsItem>,
    /// Newest changelog entries first (up to [`RECENT`]).
    pub changes: Vec<Change>,
}

/// How many recent articles / changelog entries an [`Overview`] keeps.
pub const RECENT: usize = 5;

impl Overview {
    pub fn from_tracker(t: &Tracker) -> Self {
        let mut ranked: Vec<Ranked> = t
            .efforts
            .iter()
            .filter(|e| !e.is_nuked())
            .map(|e| Ranked {
                effort: e.clone(),
                score: score_effort(e),
            })
            .collect();
        // Stable sort, same tie-break as the site: score desc, then GitHub stars desc.
        ranked.sort_by(|a, b| {
            b.score
                .score
                .total_cmp(&a.score.score)
                .then(b.effort.stars().total_cmp(&a.effort.stars()))
        });

        let top = &ranked[..ranked.len().min(3)];
        let top3 = if top.is_empty() {
            0.0
        } else {
            round1(top.iter().map(|r| r.score.score).sum::<f64>() / top.len() as f64)
        };
        let furthest = ranked
            .iter()
            .map(|r| r.score.reached)
            .max()
            .unwrap_or(Milestone::Groundwork);
        let booted = t
            .efforts
            .iter()
            .filter(|e| !e.is_nuked() && ladder_value(e.gta("boots")) >= 1.0)
            .count();
        let total = ranked.len();
        let lead = ranked.first().map_or(0.0, |r| r.score.score);

        let answer = if furthest == Milestone::Complete {
            Answer::Yes
        } else if lead >= 75.0 {
            Answer::Mostly
        } else if furthest == Milestone::Playable {
            Answer::SortOf
        } else {
            Answer::NotYet
        };

        Overview {
            updated: t.updated.clone(),
            ranked,
            top3,
            furthest,
            booted,
            total,
            answer,
            latest_news: t.news.first().cloned(),
            news: t.news.iter().take(RECENT).cloned().collect(),
            changes: t.changes.iter().rev().take(RECENT).cloned().collect(),
        }
    }

    pub fn leader(&self) -> Option<&Ranked> {
        self.ranked.first()
    }

    pub fn score(&self) -> f64 {
        self.leader().map_or(0.0, |r| r.score.score)
    }

    /// Tracked efforts for the leaderboard (the site's race list plus private builds, marked).
    pub fn board(&self) -> impl Iterator<Item = &Ranked> {
        self.ranked.iter().filter(|r| r.effort.is_tracked())
    }
}

/// Text that says who is behind an effort: `by owner` or `private build on X`.
pub fn effort_byline(o: &Overview, e: &Effort) -> String {
    if let Some(owner) = e.owner().filter(|_| !e.is_private()) {
        return format!("by {owner}");
    }
    match e.based_on.as_deref() {
        Some(ids) if !ids.is_empty() => {
            let names: Vec<&str> = ids
                .iter()
                .map(|id| {
                    o.ranked
                        .iter()
                        .find(|r| &r.effort.id == id)
                        .map_or(id.as_str(), |r| r.effort.name.as_str())
                })
                .collect();
            format!("private build on {}", names.join(", "))
        }
        _ => "private build".to_owned(),
    }
}

pub fn format_pct(v: f64) -> String {
    format!("{v:.1}%")
}

// ---------------------------------------------------------------------------------------------
// Fetching

pub struct Fetcher {
    client: reqwest::Client,
    url: String,
}

impl Fetcher {
    pub fn new(url: String) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                "PortWatcher-DiscordBot/",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self { client, url })
    }

    pub async fn fetch(&self) -> anyhow::Result<Tracker> {
        // Same cache-buster as the site: changes once a minute.
        let minute = chrono::Utc::now().timestamp() / 60;
        let sep = if self.url.contains('?') { '&' } else { '?' };
        let url = format!("{}{sep}ts={minute}", self.url);
        let tracker = self
            .client
            .get(&url)
            .header("Cache-Control", "no-cache")
            .send()
            .await
            .context("requesting tracker.json")?
            .error_for_status()
            .context("tracker.json returned an error status")?
            .json::<Tracker>()
            .await
            .context("parsing tracker.json")?;
        Ok(tracker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effort(gta: Value) -> Effort {
        serde_json::from_value(serde_json::json!({
            "id": "x", "name": "X", "repo": "a/b", "tier": "tracked", "gta": gta
        }))
        .unwrap()
    }

    #[test]
    fn belobug_scores_like_the_site() {
        // Values from tracker.json on 2026-11-25; the site shows 77.4%.
        let e = effort(serde_json::json!({
            "loads": 1, "boots": 1, "menu": 1, "world": 1, "playable": 1,
            "story": 0.5, "render": 0.85, "fps": 3, "stability": 0.7
        }));
        let s = score_effort(&e);
        assert_eq!(s.reached, Milestone::Playable);
        assert_eq!(s.score, 77.4);
    }

    #[test]
    fn partial_ladder_step() {
        let e = effort(serde_json::json!({ "loads": true, "boots": 1, "menu": 0.5 }));
        let s = score_effort(&e);
        assert_eq!(s.reached, Milestone::Boots);
        assert_eq!(s.score, 25.0);
    }

    #[test]
    fn stability_labels_and_manual_score() {
        let mut e = effort(serde_json::json!({
            "loads": 1, "boots": 1, "menu": 1, "world": 1, "playable": 1, "stability": "rarely"
        }));
        assert!((Tracks::of(&e).stability - 0.8).abs() < 1e-9);
        e.manual_score = Some(serde_json::json!(42.123));
        assert_eq!(score_effort(&e).score, 42.1);
    }

    #[test]
    fn nuked_truthiness() {
        let mut e = effort(serde_json::json!({}));
        assert!(!e.is_nuked());
        e.nuked = Some(serde_json::json!({ "at": "2026-11-25" }));
        assert!(e.is_nuked());
        e.nuked = Some(Value::Bool(false));
        assert!(!e.is_nuked());
    }
}
