//! Drawing. Every color is saturated or pure black/white so the screen
//! survives a phone camera.

use crate::app::{App, Image, Kind, Tone};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

const BLACK: Color = Color::Rgb(0, 0, 0);
const WHITE: Color = Color::Rgb(255, 255, 255);
const GREEN: Color = Color::Rgb(0, 230, 64);
const AMBER: Color = Color::Rgb(255, 176, 0);
const BLUE: Color = Color::Rgb(50, 110, 255);
const RED: Color = Color::Rgb(255, 40, 40);
const CYAN: Color = Color::Rgb(0, 220, 255);
const MAGENTA: Color = Color::Rgb(255, 60, 220);

/// Grid cell: 6 columns wide plus a gap, 2 rows tall plus a gap.
pub const CELL_W: u16 = 7;

/// (background, foreground) of an agent's cell.
fn colors(k: Kind) -> (Color, Color) {
    match k {
        Kind::Acting => (GREEN, BLACK),
        Kind::Waiting => (AMBER, BLACK),
        Kind::Parked => (BLUE, WHITE),
        Kind::Evicted => (MAGENTA, BLACK),
        Kind::Starting => (CYAN, BLACK),
        Kind::Finished => (WHITE, BLACK),
        Kind::Done => (WHITE, BLACK),
        Kind::Failed => (RED, WHITE),
        Kind::Queued => (BLACK, WHITE),
    }
}

fn mib(b: u64) -> String {
    let m = b as f64 / (1 << 20) as f64;
    if m >= 1024.0 {
        format!("{:.1}G", m / 1024.0)
    } else {
        format!("{m:.0}M")
    }
}

fn bold(s: impl Into<String>, fg: Color) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(fg).add_modifier(Modifier::BOLD))
}

fn plain(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(WHITE))
}

/// Cells per grid row for a grid this wide.
pub fn per_row(width: u16) -> usize {
    (width.saturating_sub(2) / CELL_W).max(1) as usize
}

pub fn draw(f: &mut Frame, app: &mut App) -> usize {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(BLACK)), area);
    let [head, body, tick] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(10),
        Constraint::Length(5),
    ])
    .areas(area);
    header(f, app, head);
    ticker(f, app, tick);
    if app.big {
        detail(f, app, body);
        per_row(body.width / 2)
    } else {
        let [g, d] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(body);
        let n = grid(f, app, g);
        detail(f, app, d);
        n
    }
}

fn header(f: &mut Frame, app: &App, r: Rect) {
    let fr = &app.frame;
    let mut counts = vec![bold(format!(" {} ", app.host_name), CYAN)];
    if app.replay {
        counts.push(Span::styled(" REPLAY ", Style::new().fg(BLACK).bg(MAGENTA)));
        counts.push(plain(" "));
    }
    for k in [
        Kind::Acting,
        Kind::Waiting,
        Kind::Parked,
        Kind::Starting,
        Kind::Done,
        Kind::Failed,
    ] {
        let n = app.count(k);
        let (bg, fg) = colors(k);
        counts.push(Span::styled(
            format!(" {n} {} ", k.label()),
            Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        ));
        counts.push(plain(" "));
    }
    let queue = app.count(Kind::Queued);
    counts.push(bold(format!("queue {queue}"), WHITE));
    // Agents whose model call waits out a 429 cooldown; resd shows them
    // frozen or parked like any other waiting agent.
    let limited = fr.rate_waiting;
    counts.push(bold(
        format!("   rate-limited {limited}"),
        if limited > 0 { AMBER } else { WHITE },
    ));
    if app.others > 0 {
        counts.push(plain(format!("   +{} other resd agents", app.others)));
    }
    if !app.resd_up {
        counts.push(plain("  "));
        counts.push(Span::styled(
            " resd unreachable ",
            Style::new().fg(WHITE).bg(RED),
        ));
    }

    let used = fr.mem_total.saturating_sub(fr.mem_available);
    let (zs, zd) = app.agents.iter().fold((0, 0), |(s, d), a| {
        (s + a.zswap.unwrap_or(0), d + a.zswapped.unwrap_or(0))
    });
    let ratio = if zs > 0 { zd as f64 / zs as f64 } else { 0.0 };
    let mut gauges = vec![
        bold(format!(" {:.1} steps/s", app.steps_per_s()), GREEN),
        plain(format!("  ({} steps)", app.steps_total)),
        plain("   RAM "),
        bold(format!("{}/{}", mib(used), mib(fr.mem_total)), WHITE),
        plain("   zswap "),
        bold(mib(app.zswap_pool.unwrap_or(zs)), BLUE),
    ];
    if zs > 0 {
        gauges.push(bold(format!(" {ratio:.1}:1"), BLUE));
    }
    gauges.push(plain("   CPU "));
    gauges.push(bold(format!("{:.0}%", fr.cpu * 100.0), AMBER));
    if let Some((u, m)) = fr.gpu {
        gauges.push(plain("   GPU "));
        gauges.push(bold(format!("{u:.0}% {:.1}G", m / 1024.0), MAGENTA));
    }
    let bar = meter(
        r.width.saturating_sub(2) as usize,
        fr.mem_total,
        used,
        app.zswap_pool.unwrap_or(zs),
    );
    let p = Paragraph::new(vec![Line::from(counts), Line::from(gauges), bar]).block(
        Block::new()
            .borders(Borders::BOTTOM)
            .border_style(Style::new().fg(WHITE)),
    );
    f.render_widget(p, r);
}

/// RAM as a solid bar: used (white), of which zswap pool (blue), free (black).
fn meter(w: usize, total: u64, used: u64, pool: u64) -> Line<'static> {
    if total == 0 || w == 0 {
        return Line::default();
    }
    let cells = |b: u64| ((b as f64 / total as f64) * w as f64).round() as usize;
    let p = cells(pool).min(w);
    let u = cells(used).clamp(p, w);
    Line::from(vec![
        plain(" "),
        Span::styled("█".repeat(u - p), Style::new().fg(WHITE)),
        Span::styled("█".repeat(p), Style::new().fg(BLUE)),
        Span::styled("─".repeat(w - u), Style::new().fg(WHITE)),
    ])
}

fn grid(f: &mut Frame, app: &App, r: Rect) -> usize {
    let block = Block::new()
        .borders(Borders::RIGHT)
        .border_style(Style::new().fg(WHITE))
        .title(bold(format!(" agents {} ", app.agents.len()), WHITE));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let n = per_row(inner.width);
    let mut lines = vec![Line::default()];
    for row in app.agents.chunks(n).enumerate() {
        let (ri, agents) = row;
        let mut top = vec![plain(" ")];
        let mut bot = vec![plain(" ")];
        for (ci, a) in agents.iter().enumerate() {
            let i = ri * n + ci;
            let (mut bg, mut fg) = colors(a.kind);
            if i == app.selected {
                (bg, fg) = (fg, bg);
            }
            let st = Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD);
            let id = format!("{i:04}");
            let mark = match a.kind {
                Kind::Done => " ✓".to_owned(),
                Kind::Failed => " ✗".to_owned(),
                Kind::Queued => " ·".to_owned(),
                _ => format!("{:>2}", a.steps.min(99)),
            };
            // Brackets mark the selection even without color.
            let (l, r) = if i == app.selected {
                ("[", "]")
            } else {
                (" ", " ")
            };
            top.push(Span::styled(format!("{l}{id:<4}{r}"), st));
            bot.push(Span::styled(format!("   {mark} "), st));
            top.push(plain(" "));
            bot.push(plain(" "));
        }
        lines.push(Line::from(top));
        lines.push(Line::from(bot));
        lines.push(Line::default());
    }
    // Legend under the grid.
    let mut legend = vec![plain(" ")];
    for k in [
        Kind::Acting,
        Kind::Waiting,
        Kind::Parked,
        Kind::Starting,
        Kind::Finished,
        Kind::Done,
        Kind::Failed,
        Kind::Queued,
    ] {
        let (bg, fg) = colors(k);
        legend.push(Span::styled(
            format!(" {} ", k.label()),
            Style::new().fg(fg).bg(bg),
        ));
        legend.push(plain(" "));
    }
    lines.push(Line::from(legend));
    lines.push(Line::from(plain(" lower number: steps taken")));
    f.render_widget(Paragraph::new(lines), inner);
    n
}

fn detail(f: &mut Frame, app: &mut App, r: Rect) {
    let Some(a) = app.agents.get(app.selected).cloned() else {
        f.render_widget(Paragraph::new(plain(" no agents yet")), r);
        return;
    };
    let (bg, fg) = colors(a.kind);
    let title = Line::from(vec![
        bold(format!(" {} ", a.name.trim_end_matches(".service")), WHITE),
        Span::styled(
            format!(" {} {:.1}s ", a.kind.label(), a.for_ms as f64 / 1000.0),
            Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        ),
    ]);
    let mut lines = vec![
        title,
        Line::from(vec![
            plain(format!(" {} ", a.task.as_ref().map_or("task", |t| t.0))),
            bold(a.task.as_ref().map_or("-".into(), |t| t.1.clone()), CYAN),
            plain(format!("   steps {}", a.steps)),
        ]),
    ];
    let mut cg = vec![plain(" memory.current ")];
    cg.push(bold(a.mem.map_or("-".into(), mib), WHITE));
    cg.push(plain("  zswap "));
    cg.push(bold(a.zswap.map_or("-".into(), mib), BLUE));
    cg.push(plain(" holding "));
    cg.push(bold(a.zswapped.map_or("-".into(), mib), BLUE));
    let frozen = matches!(a.kind, Kind::Waiting | Kind::Parked);
    cg.push(plain("  frozen "));
    cg.push(bold(
        if frozen { "yes" } else { "no" },
        if frozen { AMBER } else { GREEN },
    ));
    lines.push(Line::from(cg));
    if let Some(p) = &a.last_park {
        lines.push(Line::from(vec![
            plain(" last park "),
            bold(
                format!(
                    "{}→{} in {:.0}ms",
                    mib(p["before"].as_u64().unwrap_or(0)),
                    mib(p["after"].as_u64().unwrap_or(0)),
                    p["ms"].as_f64().unwrap_or(0.0)
                ),
                BLUE,
            ),
        ]));
    }
    lines.push(Line::from(bold(
        " step  model  action settle observe  total  calls",
        WHITE,
    )));
    let ms = |t: &serde_json::Value, k: &str| {
        t[k].as_f64()
            .map_or("    -".into(), |v| format!("{v:>5.0}"))
    };
    for t in a.recent.iter().rev() {
        let acts = t["actions"]
            .as_u64()
            .map_or(String::new(), |n| format!("{n:>4}"));
        let err = t["error"].as_str().map(|e| format!(" ERR {e}"));
        let mut row = vec![
            plain(format!(" {:>4}", t["step"].as_u64().unwrap_or(0))),
            bold(format!("  {}", ms(t, "model_ms")), MAGENTA),
            bold(format!("  {}", ms(t, "action_ms")), GREEN),
            plain(format!("  {}", ms(t, "settle_ms"))),
            plain(format!("  {}", ms(t, "observe_ms"))),
            bold(format!("  {}", ms(t, "step_ms")), WHITE),
            plain(format!("  {acts}")),
        ];
        if let Some(e) = err {
            row.push(bold(truncate(&e, 40), RED));
        }
        lines.push(Line::from(row));
    }
    let text_h = lines.len() as u16 + 1;
    let [t, img] = Layout::vertical([Constraint::Length(text_h), Constraint::Min(0)]).areas(r);
    f.render_widget(Paragraph::new(lines), t);
    if img.height >= 4 {
        match app.thumb() {
            Some(im) => thumbnail(f, im, img),
            None => f.render_widget(Paragraph::new(plain(" no capture yet")), img),
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

/// The capture as `▀` cells: foreground is the upper pixel, background the
/// lower. Nearest-neighbor, aspect-preserving, left-aligned.
fn thumbnail(f: &mut Frame, im: &Image, r: Rect) {
    if im.w == 0 || im.h == 0 {
        return;
    }
    let max_w = r.width.saturating_sub(2) as usize;
    let max_h = r.height as usize * 2;
    let (mut w, mut h) = (max_w, max_w * im.h / im.w);
    if h > max_h {
        h = max_h;
        w = max_h * im.w / im.h;
    }
    let px = |x: usize, y: usize| {
        let c = im.rgb[(y * im.h / h).min(im.h - 1) * im.w + (x * im.w / w).min(im.w - 1)];
        Color::Rgb(c[0], c[1], c[2])
    };
    let buf = f.buffer_mut();
    for cy in 0..h.div_ceil(2) {
        for x in 0..w {
            let top = px(x, cy * 2);
            let bot = if cy * 2 + 1 < h {
                px(x, cy * 2 + 1)
            } else {
                BLACK
            };
            if let Some(cell) = buf.cell_mut((r.x + 1 + x as u16, r.y + cy as u16)) {
                cell.set_symbol("▀").set_fg(top).set_bg(bot);
            }
        }
    }
}

fn ticker(f: &mut Frame, app: &App, r: Rect) {
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(WHITE))
        .title(bold(" events ", WHITE))
        .title_bottom(plain(" ←↓↑→/hjkl select   enter zoom   q quit "));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines = Vec::new();
    let mut line = vec![plain(" ")];
    let mut used = 1usize;
    for (text, tone) in &app.events {
        let c = match tone {
            Tone::Park => BLUE,
            Tone::Wake => GREEN,
            Tone::Freeze => AMBER,
            Tone::Done => WHITE,
            Tone::Fail => RED,
        };
        let w = text.chars().count() + 3;
        if used + w > inner.width as usize {
            lines.push(Line::from(std::mem::take(&mut line)));
            if lines.len() as u16 >= inner.height {
                break;
            }
            line = vec![plain(" ")];
            used = 1;
        }
        line.push(bold(text.clone(), c));
        line.push(plain("   "));
        used += w;
    }
    if lines.len() < inner.height as usize {
        lines.push(Line::from(line));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
