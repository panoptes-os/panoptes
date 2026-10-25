//! panoptesctl: Panoptes command line.
//!
//! ```text
//! panoptesctl                          print this process's cgroup
//! panoptesctl desk SOCK TOOL [ARGS]    call one desk tool, print the reply line
//! panoptesctl resd SOCK JSON           send one request to panoptes-resd
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => whoami(),
        Some("desk") if (3..=4).contains(&args.len()) => {
            desk(&args[1], &args[2], args.get(3).map_or("{}", String::as_str))
        }
        Some("resd") if args.len() == 3 => line(&args[1], &args[2]),
        _ => {
            eprintln!("usage: panoptesctl [desk SOCK TOOL [ARGS_JSON] | resd SOCK JSON]");
            2
        }
    };
    std::process::exit(code);
}

fn whoami() -> i32 {
    match panoptes_cgroup::Cgroup::current() {
        Ok(cg) => {
            println!("panoptesctl {}: running in {}", env!("CARGO_PKG_VERSION"), cg.path().display());
            0
        }
        Err(e) => {
            eprintln!("panoptesctl: {e}");
            1
        }
    }
}

/// One request on the desk's internal protocol. ARGS is passed through as
/// raw JSON; the desk rejects it if it does not parse.
fn desk(sock: &str, tool: &str, args: &str) -> i32 {
    let tool = tool.replace('\\', "\\\\").replace('"', "\\\"");
    line(sock, &format!(r#"{{"id":1,"tool":"{tool}","args":{args}}}"#))
}

/// Send one request line and print the reply; exit 0 when it says ok.
fn line(sock: &str, req: &str) -> i32 {
    let run = || -> std::io::Result<String> {
        let mut s = UnixStream::connect(sock)?;
        writeln!(s, "{req}")?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line)?;
        Ok(line)
    };
    match run() {
        Ok(line) if line.contains(r#""ok":true"#) => {
            print!("{line}");
            0
        }
        Ok(line) => {
            print!("{line}");
            1
        }
        Err(e) => {
            eprintln!("panoptesctl: {sock}: {e}");
            1
        }
    }
}
