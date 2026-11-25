//! Renders the progress widget as a PNG: an SVG is built as a string and rasterised with resvg.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, anyhow};
use resvg::{tiny_skia, usvg};

use crate::tracker::{Milestone, Overview, TRACKS, effort_byline, format_pct};

const W: f64 = 1200.0;
const H: f64 = 724.0;

const BG: &str = "#0f0b24";
const PANEL: &str = "#1a1538";
const PANEL_LINE: &str = "#2c2552";
const TEXT: &str = "#f4f2ff";
const MUTED: &str = "#a39cc8";
const DIM: &str = "#6c6496";
const TRACK_BG: &str = "#2a2350";

static FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Black.ttf"),
];

fn options() -> &'static usvg::Options<'static> {
    static OPTS: OnceLock<usvg::Options<'static>> = OnceLock::new();
    OPTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        for font in FONTS {
            db.load_font_data(font.to_vec());
        }
        usvg::Options {
            font_family: "Inter".to_owned(),
            fontdb: Arc::new(db),
            ..Default::default()
        }
    })
}

/// XML-escape text for SVG.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// Cuts text to `max` characters with an ellipsis.
fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn updated_text(o: &Overview) -> String {
    o.updated
        .as_deref()
        .and_then(|u| chrono::DateTime::parse_from_rfc3339(u).ok())
        .map(|d| {
            d.with_timezone(&chrono::Utc)
                .format("Site data updated %b %-d, %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|| "Site data update time unknown".to_owned())
}

/// Builds the widget SVG. Deterministic for the same data, so it doubles as a change key.
pub fn build_svg(o: &Overview) -> String {
    let mut s = String::with_capacity(16 * 1024);
    let score = o.score();

    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="Inter">
<defs>
  <linearGradient id="brand" x1="0" y1="0" x2="1" y2="0">
    <stop offset="0" stop-color="#7a5cff"/><stop offset=".55" stop-color="#ff2e7e"/><stop offset="1" stop-color="#ff8a3d"/>
  </linearGradient>
  <linearGradient id="brandv" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#7a5cff"/><stop offset=".55" stop-color="#ff2e7e"/><stop offset="1" stop-color="#ff8a3d"/>
  </linearGradient>
  <linearGradient id="sky" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#1b1145"/><stop offset=".6" stop-color="{BG}"/><stop offset="1" stop-color="{BG}"/>
  </linearGradient>
  <radialGradient id="glow" cx=".85" cy="0" r=".7">
    <stop offset="0" stop-color="#ff2e7e" stop-opacity=".28"/><stop offset="1" stop-color="#ff2e7e" stop-opacity="0"/>
  </radialGradient>
</defs>
<rect width="{W}" height="{H}" fill="url(#sky)"/>
<rect width="{W}" height="{H}" fill="url(#glow)"/>
<rect width="{W}" height="6" fill="url(#brand)"/>
"##
    );

    // ---- Header: logo, title, answer pill
    let _ = write!(
        s,
        r##"<g transform="translate(48 34)">
  <circle cx="22" cy="22" r="17" fill="none" stroke="#3a3366" stroke-width="7"/>
  <circle cx="22" cy="22" r="17" fill="none" stroke="url(#brandv)" stroke-width="7" stroke-linecap="round"
          stroke-dasharray="{dash:.1} 200" transform="rotate(-90 22 22)"/>
</g>
<text x="104" y="58" font-size="30" font-weight="900" fill="{TEXT}">PortWatch<tspan fill="#ff2e7e">VI</tspan></text>
<text x="104" y="84" font-size="17" fill="{MUTED}">Does GTA VI run on PC yet?</text>
"##,
        dash = (score / 100.0 * 2.0 * std::f64::consts::PI * 17.0).max(0.1),
    );
    let answer = o.answer.text();
    let pill_w = 40.0 + answer.chars().count() as f64 * 17.0;
    let _ = write!(
        s,
        r##"<rect x="{x}" y="36" width="{pill_w}" height="52" rx="26" fill="url(#brand)"/>
<text x="{cx}" y="71" font-size="28" font-weight="900" fill="#ffffff" text-anchor="middle">{answer}</text>
"##,
        x = W - 48.0 - pill_w,
        cx = W - 48.0 - pill_w / 2.0,
        answer = esc(answer),
    );

    // ---- Headline score + leader
    let _ = write!(
        s,
        r##"<text x="48" y="150" font-size="15" font-weight="600" fill="{MUTED}" letter-spacing="1.5">OVERALL PROGRESS · CLOSEST EFFORT TO PLAYABLE</text>
<text x="44" y="238" font-size="96" font-weight="900" fill="url(#brand)">{score}</text>
"##,
        score = esc(&format_pct(score)),
    );
    let leader_line = match o.leader() {
        Some(l) => {
            let reached = if l.score.reached == Milestone::Groundwork {
                "still groundwork".to_owned()
            } else {
                format!("reached “{}”", l.score.reached.label())
            };
            format!(
                r##"<text x="48" y="276" font-size="20" fill="{MUTED}">Leader: <tspan font-weight="700" fill="{TEXT}">{}</tspan> {} · {}</text>"##,
                esc(&trunc(&l.effort.name, 28)),
                esc(&trunc(&effort_byline(o, &l.effort), 40)),
                esc(&reached),
            )
        }
        None => format!(
            r##"<text x="48" y="276" font-size="20" fill="{MUTED}">No efforts tracked yet.</text>"##
        ),
    };
    s.push_str(&leader_line);
    s.push('\n');

    // ---- Stat tiles (2 x 2) on the right
    let tiles = [
        ("Furthest milestone", o.furthest.label().to_owned()),
        ("Top-3 average", format_pct(o.top3)),
        ("Boot GTA VI", format!("{} / {}", o.booted, o.total)),
        ("Efforts tracked", o.total.to_string()),
    ];
    let (tx, ty, tw, th, gap) = (700.0, 120.0, 216.0, 74.0, 12.0);
    for (i, (label, value)) in tiles.iter().enumerate() {
        let x = tx + (i % 2) as f64 * (tw + gap);
        let y = ty + (i / 2) as f64 * (th + gap);
        let _ = write!(
            s,
            r##"<rect x="{x}" y="{y}" width="{tw}" height="{th}" rx="14" fill="{PANEL}" stroke="{PANEL_LINE}"/>
<text x="{lx}" y="{ly}" font-size="14" fill="{MUTED}">{label}</text>
<text x="{lx}" y="{vy}" font-size="26" font-weight="800" fill="{TEXT}">{value}</text>
"##,
            lx = x + 18.0,
            ly = y + 28.0,
            vy = y + 60.0,
            label = esc(label),
            value = esc(value),
        );
    }

    // ---- Mega progress bar with milestone ticks
    let (bx, by, bw, bh) = (48.0, 318.0, W - 96.0, 26.0);
    let fill_w = (bw * score / 100.0).clamp(0.0, bw);
    let _ = write!(
        s,
        r##"<rect x="{bx}" y="{by}" width="{bw}" height="{bh}" rx="13" fill="{TRACK_BG}"/>
"##
    );
    if fill_w > 0.0 {
        let _ = write!(
            s,
            r##"<rect x="{bx}" y="{by}" width="{w}" height="{bh}" rx="13" fill="url(#brand)"/>
"##,
            w = fill_w.max(bh),
        );
    }
    for m in &Milestone::ALL[1..Milestone::ALL.len() - 1] {
        let x = bx + bw * m.at() / 100.0;
        let _ = write!(
            s,
            r##"<rect x="{x0}" y="{by}" width="2" height="{bh}" fill="{BG}" opacity=".55"/>
"##,
            x0 = x - 1.0,
        );
    }
    for m in Milestone::ALL {
        let x = bx + bw * m.at() / 100.0;
        let reached = m <= o.furthest;
        let next = !reached
            && Milestone::ALL.iter().position(|&k| k == m)
                == Milestone::ALL
                    .iter()
                    .position(|&k| k == o.furthest)
                    .map(|i| i + 1);
        let (color, weight) = if reached {
            (TEXT, 700)
        } else if next {
            ("#ff8fb6", 700)
        } else {
            (DIM, 500)
        };
        let anchor = match m {
            Milestone::Groundwork => "start",
            Milestone::Complete => "end",
            _ => "middle",
        };
        let _ = write!(
            s,
            r##"<text x="{x}" y="{y1}" font-size="13" font-weight="700" fill="{color}" text-anchor="{anchor}">{at}%</text>
<text x="{x}" y="{y2}" font-size="13" font-weight="{weight}" fill="{color}" text-anchor="{anchor}">{label}</text>
"##,
            y1 = by + bh + 22.0,
            y2 = by + bh + 41.0,
            at = m.at() as u32,
            label = esc(m.label()),
        );
    }
    let tracks_text = TRACKS
        .iter()
        .map(|(_, l, _)| *l)
        .collect::<Vec<_>>()
        .join(" · ");
    let _ = write!(
        s,
        r##"<text x="{x}" y="{y}" font-size="13" fill="{DIM}" text-anchor="middle">{t}</text>
"##,
        x = bx + bw * 0.75,
        y = by + bh + 41.0,
        t = esc(&tracks_text),
    );

    // ---- Leaderboard
    let (lx, ly, lw) = (48.0, 418.0, W - 96.0);
    let rows: Vec<_> = o.board().take(5).collect();
    let row_h = 40.0;
    let panel_h = 48.0 + row_h * rows.len().max(1) as f64;
    let _ = write!(
        s,
        r##"<rect x="{lx}" y="{ly}" width="{lw}" height="{panel_h}" rx="16" fill="{PANEL}" stroke="{PANEL_LINE}"/>
<text x="{tx}" y="{ty}" font-size="14" font-weight="600" fill="{MUTED}" letter-spacing="1.2">LEADERBOARD</text>
<text x="{rx}" y="{ty}" font-size="13" fill="{DIM}" text-anchor="end">milestone · score</text>
"##,
        tx = lx + 22.0,
        ty = ly + 30.0,
        rx = lx + lw - 22.0,
    );
    if rows.is_empty() {
        let _ = write!(
            s,
            r##"<text x="{x}" y="{y}" font-size="16" fill="{MUTED}">No efforts tracked yet.</text>
"##,
            x = lx + 22.0,
            y = ly + 70.0,
        );
    }
    for (i, r) in rows.iter().enumerate() {
        let y = ly + 48.0 + row_h * i as f64;
        let mid = y + row_h / 2.0;
        if i > 0 {
            let _ = write!(
                s,
                r##"<rect x="{x}" y="{y}" width="{w}" height="1" fill="{PANEL_LINE}"/>
"##,
                x = lx + 16.0,
                w = lw - 32.0,
            );
        }
        let rank_fill = if i == 0 { "url(#brand)" } else { "#2f2860" };
        let bar_x = lx + 640.0;
        let bar_w = 250.0;
        let _ = write!(
            s,
            r##"<rect x="{rx}" y="{ry}" width="28" height="28" rx="8" fill="{rank_fill}"/>
<text x="{rcx}" y="{rty}" font-size="15" font-weight="800" fill="#ffffff" text-anchor="middle">{rank}</text>
<text x="{nx}" y="{rty}" font-size="18" font-weight="700" fill="{TEXT}">{name} <tspan font-size="14" font-weight="400" fill="{MUTED}">{by}</tspan></text>
<rect x="{bar_x}" y="{bary}" width="{bar_w}" height="10" rx="5" fill="{TRACK_BG}"/>
<rect x="{bar_x}" y="{bary}" width="{fw}" height="10" rx="5" fill="url(#brand)"/>
<text x="{msx}" y="{rty}" font-size="14" fill="{MUTED}" text-anchor="end">{ms}</text>
<text x="{sx}" y="{rty}" font-size="18" font-weight="800" fill="{TEXT}" text-anchor="end">{score}</text>
"##,
            rx = lx + 20.0,
            ry = mid - 14.0,
            rcx = lx + 34.0,
            rty = mid + 6.0,
            rank = i + 1,
            nx = lx + 62.0,
            name = esc(&trunc(&r.effort.name, 24)),
            by = esc(&trunc(&effort_byline(o, &r.effort), 38)),
            bary = mid - 5.0,
            fw = (bar_w * r.score.score / 100.0).clamp(10.0, bar_w),
            msx = lx + lw - 110.0,
            ms = esc(r.score.reached.label()),
            sx = lx + lw - 22.0,
            score = esc(&format_pct(r.score.score)),
        );
    }

    // ---- Footer
    let _ = write!(
        s,
        r##"<text x="48" y="{fy}" font-size="14" fill="{DIM}">portwatch-vi.xyz · {upd}</text>
<text x="{rx}" y="{fy}" font-size="14" fill="{DIM}" text-anchor="end">Live tracker of every PC port effort</text>
</svg>"##,
        fy = H - 22.0,
        rx = W - 48.0,
        upd = esc(&updated_text(o)),
    );

    s
}

/// Rasterises the widget SVG to PNG bytes. CPU-bound: call from `spawn_blocking`.
pub fn render_png(svg: &str) -> anyhow::Result<Vec<u8>> {
    let tree = usvg::Tree::from_str(svg, options()).context("parsing widget SVG")?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| anyhow!("could not allocate pixmap"))?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.encode_png().context("encoding PNG")
}
