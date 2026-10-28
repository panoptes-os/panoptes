//! A task line's `check`: a shell command whose exit status verifies an
//! agent's done claim. Once the agent claims done and its desk has stopped,
//! the loop runs
//!
//! ```text
//! sh -c CHECK
//!   cwd     the agent's run directory, out_dir/run_name/agent-N/; what the
//!           browser downloaded is in downloads/
//!   env     the loop's, plus
//!           PANOPTES_URL   URL of the page the model saw last
//!           PANOPTES_NOTE  the model's last note, where it gives an answer
//!   stdin   /dev/null; stdout and stderr go to check.log in the cwd
//! ```
//!
//! in its own process group, and kills the group after [`TIMEOUT`]. Exit 0
//! is verified. Any other exit, a signal, a timeout, or a failure to start
//! is not.

use serde_json::{Value, json};
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::process::Command;

pub const TIMEOUT: Duration = Duration::from_secs(60);

/// `{verified, check_exit, check_error}`: the exit code when the command
/// exited, and otherwise why it has none.
pub async fn run(check: &str, dir: &Path, url: &str, note: &str, timeout: Duration) -> Value {
    let (exit, error) = match status(check, dir, url, note, timeout).await {
        Ok(st) => (st.code(), st.code().is_none().then(|| st.to_string())),
        Err(e) => (None, Some(e)),
    };
    json!({ "verified": exit == Some(0), "check_exit": exit, "check_error": error })
}

async fn status(
    check: &str,
    dir: &Path,
    url: &str,
    note: &str,
    timeout: Duration,
) -> Result<ExitStatus, String> {
    let log =
        std::fs::File::create(dir.join("check.log")).map_err(|e| format!("check.log: {e}"))?;
    let err = log.try_clone().map_err(|e| format!("check.log: {e}"))?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(check)
        .current_dir(dir)
        .env("PANOPTES_URL", url)
        .env("PANOPTES_NOTE", note)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("sh: {e}"))?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(st) => st.map_err(|e| format!("sh: {e}")),
        Err(_) => {
            // A pipeline's commands are the shell's children; the group
            // holds them all.
            let group = child
                .id()
                .and_then(|p| rustix::process::Pid::from_raw(p as i32));
            if let Some(g) = group {
                let _ = rustix::process::kill_process_group(g, rustix::process::Signal::KILL);
            }
            let _ = child.wait().await;
            Err(format!("timed out after {timeout:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Run `cmd` in a fresh directory named for the test; returns the
    /// result, the directory, and check.log.
    async fn check(name: &str, cmd: &str, timeout: Duration) -> (Value, PathBuf, String) {
        let dir =
            std::env::temp_dir().join(format!("panoptes-check-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let v = run(cmd, &dir, "https://a.example/done", "price 51.77", timeout).await;
        let log = std::fs::read_to_string(dir.join("check.log")).unwrap_or_default();
        (v, dir, log)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exit_status_verifies_with_the_url_and_note_in_its_env() {
        let pass = "case \"$PANOPTES_NOTE\" in *51.77*) ;; *) exit 1 ;; esac; \
                    test \"$PANOPTES_URL\" = https://a.example/done; echo checked; pwd";
        let (v, dir, log) = check("pass", pass, TIMEOUT).await;
        assert_eq!(
            v,
            json!({ "verified": true, "check_exit": 0, "check_error": null })
        );
        assert_eq!(log, format!("checked\n{}\n", dir.display()));

        let (v, _, log) = check("fail", "echo nope >&2; exit 3", TIMEOUT).await;
        assert_eq!(
            v,
            json!({ "verified": false, "check_exit": 3, "check_error": null })
        );
        assert_eq!(log, "nope\n");

        let (v, _, _) = check("signal", "kill -TERM $$", TIMEOUT).await;
        assert_eq!(v["verified"], false);
        assert_eq!(v["check_exit"], Value::Null);
        assert!(
            v["check_error"].as_str().unwrap().contains("signal: 15"),
            "{v}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_timeout_kills_the_shells_children_too() {
        let t = std::time::Instant::now();
        let cmd = "sleep 30 & echo $! > sleep.pid; wait";
        let (v, dir, _) = check("timeout", cmd, Duration::from_millis(300)).await;
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert_eq!(
            v,
            json!({ "verified": false, "check_exit": null, "check_error": "timed out after 300ms" })
        );
        // The killed sleep belongs to init now; give it a moment to be
        // reaped, then it must be gone.
        let pid: i32 = std::fs::read_to_string(dir.join("sleep.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            rustix::process::test_kill_process(pid).is_err()
        });
        assert!(gone, "the check's background sleep outlived its timeout");
    }
}
