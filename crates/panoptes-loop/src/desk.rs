//! One agent's panoptes-desk: start it, talk its internal line protocol,
//! stop it.
//!
//! ```text
//! > {"id":1,"tool":"click","args":{"node_id":42}}
//! < {"id":1,"ok":true,"result":{...},"timing":{"action_ms":3.1,"total_ms":3.2}}
//! ```

use crate::config::{DeskLaunch, expand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::Command;

pub struct Desk {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
    next_id: u64,
    stop: Vec<String>,
}

/// One tool call's reply.
#[derive(Debug)]
pub struct Reply {
    pub ok: bool,
    pub result: Value,
    pub error: Option<String>,
    /// The desk's own time for the call.
    pub total_ms: f64,
}

async fn run(cmd: &[String]) -> Result<(), String> {
    let (prog, args) = cmd.split_first().ok_or("empty command")?;
    let st = Command::new(prog)
        .args(args)
        .status()
        .await
        .map_err(|e| format!("{prog}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("{prog} exited with {st}"))
    }
}

impl Desk {
    /// Start agent `i`'s desk in `run_dir` and connect to its socket.
    pub async fn start(cfg: &DeskLaunch, i: usize, run_dir: &Path) -> Result<Self, String> {
        let dir = run_dir.to_string_lossy().into_owned();
        let stop = expand(&cfg.stop, i, &dir);
        let _ = run(&stop).await;
        let _ = std::fs::remove_dir_all(run_dir);
        std::fs::create_dir_all(run_dir).map_err(|e| format!("{dir}: {e}"))?;
        run(&expand(&cfg.launch, i, &dir)).await?;
        let sock = PathBuf::from(cfg.socket.replace("{run}", &dir));
        let deadline = Instant::now() + Duration::from_secs(cfg.start_timeout_s);
        let stream = loop {
            match UnixStream::connect(&sock).await {
                Ok(s) => break s,
                Err(e) if Instant::now() > deadline => {
                    return Err(format!("{}: {e}", sock.display()));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        };
        let (read, write) = stream.into_split();
        Ok(Desk {
            read: BufReader::new(read),
            write,
            next_id: 1,
            stop,
        })
    }

    pub async fn call(&mut self, tool: &str, args: &Value) -> Result<Reply, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({ "id": id, "tool": tool, "args": args }).to_string();
        line.push('\n');
        self.write
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("desk write: {e}"))?;
        let mut buf = String::new();
        if self
            .read
            .read_line(&mut buf)
            .await
            .map_err(|e| format!("desk read: {e}"))?
            == 0
        {
            return Err("desk closed its socket".into());
        }
        let mut v: Value = serde_json::from_str(&buf).map_err(|e| format!("desk reply: {e}"))?;
        Ok(Reply {
            ok: v["ok"].as_bool().unwrap_or(false),
            result: v["result"].take(),
            error: v["error"].as_str().map(str::to_owned),
            total_ms: v["timing"]["total_ms"].as_f64().unwrap_or(0.0),
        })
    }

    pub async fn stop(self) {
        drop(self.write);
        let _ = run(&self.stop).await;
    }
}
