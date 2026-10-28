//! panoptes top: the fleet on one screen.
//!
//! ```text
//! header   counts by state, queue, rate-limited, steps/s, RAM, zswap, CPU, GPU
//! grid     one cell per agent, colored by state      | detail of the selected
//! ticker   park / wake / freeze / done events        | agent + its capture
//! ```

mod app;
mod source;
mod ui;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: panoptes-top [--resd SOCK] [--run DIR] [--record FILE]
       panoptes-top --demo-replay FILE [--speed X]

  --resd SOCK         panoptes-resd socket (default /run/panoptes/resd.sock)
  --run DIR           the loop's run directory: trace.jsonl, results.jsonl,
                      assigned.txt, model.json, agent-N/thumb.b64
  --record FILE       also append every frame to FILE for --demo-replay
  --demo-replay FILE  play a recording instead of reading the host; loops
  --speed X           replay speed (default 1)

keys: arrows or hjkl select, enter zooms the detail, q quits";

const TICK: Duration = Duration::from_millis(250);

enum Source {
    Live(source::Live),
    Replay(source::Replay),
}

fn parse() -> Result<(Source, String), String> {
    let mut it = std::env::args().skip(1);
    let mut resd = PathBuf::from("/run/panoptes/resd.sock");
    let mut run = None;
    let mut record = None;
    let mut replay = None;
    let mut speed = 1.0;
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--resd" => resd = PathBuf::from(val()?),
            "--run" => run = Some(PathBuf::from(val()?)),
            "--record" => record = Some(PathBuf::from(val()?)),
            "--demo-replay" => replay = Some(PathBuf::from(val()?)),
            "--speed" => speed = val()?.parse().map_err(|_| "--speed")?,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "panoptes".into());
    if let Some(p) = replay {
        return Ok((Source::Replay(source::Replay::open(&p, speed)?), host));
    }
    let record = match record {
        Some(p) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .map_err(|e| format!("{}: {e}", p.display()))?,
        ),
        None => None,
    };
    Ok((Source::Live(source::Live::new(resd, run, record)), host))
}

fn main() {
    let (mut src, host) = match parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut app = app::App::new(host, matches!(src, Source::Replay(_)));
    let mut term = ratatui::init();
    let mut per_row = 1;
    let mut next = Instant::now();
    let result: std::io::Result<()> = (|| loop {
        if Instant::now() >= next {
            next += TICK;
            let frame = match &mut src {
                Source::Live(l) => Some(l.poll(Some(app.selected), app.agents.len())),
                Source::Replay(r) => r.poll(),
            };
            if let Some(f) = frame {
                app.ingest(f);
            }
            term.draw(|f| per_row = ui::draw(f, &mut app))?;
        }
        let wait = next.saturating_duration_since(Instant::now());
        if !event::poll(wait)? {
            continue;
        }
        let Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Left | KeyCode::Char('h') => app.move_sel(-1, 0, per_row),
            KeyCode::Right | KeyCode::Char('l') => app.move_sel(1, 0, per_row),
            KeyCode::Up | KeyCode::Char('k') => app.move_sel(0, -1, per_row),
            KeyCode::Down | KeyCode::Char('j') => app.move_sel(0, 1, per_row),
            KeyCode::Enter => app.big = !app.big,
            _ => continue,
        }
        // Redraw on input without waiting for the next tick.
        term.draw(|f| per_row = ui::draw(f, &mut app))?;
    })();
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("panoptes-top: {e}");
        std::process::exit(1);
    }
}
