//! `f1r3x-serve` — run a rho executive on a Mac for the Rhobots client.
//!
//! ```text
//! f1r3x-serve [--addr HOST:PORT] [--cores N] [--ttl SECONDS]
//!             [--no-bonjour] [--verbose] [--deploy FILE]
//! ```
//!
//! The defaults are the ones a demo wants: listen on every interface on 9099,
//! advertise over Bonjour so the headset finds the Mac without anybody typing
//! an address, and keep a detached session's world for fifteen minutes.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use f1r3x_host::available_parallelism;
use f1r3x_serve::{Config, Server};

fn main() {
    let mut cfg = Config::default();
    let mut bonjour = true;
    let mut deploy: Option<String> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let next = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match a {
            "--addr" | "-a" => match next(&mut i) {
                Some(v) => cfg.addr = normalise_addr(&v),
                None => die("--addr needs HOST:PORT or a bare port"),
            },
            "--cores" | "-c" => match next(&mut i).and_then(|v| v.parse::<u32>().ok()) {
                Some(v) if v >= 1 => cfg.cores_max = v,
                _ => die("--cores needs a positive integer"),
            },
            "--ttl" => match next(&mut i).and_then(|v| v.parse::<u64>().ok()) {
                Some(v) => cfg.session_ttl = Duration::from_secs(v),
                None => die("--ttl needs a number of seconds"),
            },
            "--deploy" | "-d" => match next(&mut i) {
                Some(v) => deploy = Some(v),
                None => die("--deploy needs a path to a .rho file"),
            },
            "--no-bonjour" => bonjour = false,
            "--verbose" | "-v" => cfg.verbose = true,
            "--help" | "-h" => {
                usage();
                return;
            }
            other => die(&format!("unknown option `{other}`")),
        }
        i += 1;
    }

    let server = match Server::bind(&cfg) {
        Ok(s) => Arc::new(s),
        Err(e) => die(&format!("cannot bind {}: {e}", cfg.addr)),
    };
    let addr = server
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| cfg.addr.clone());
    let port = server.local_addr().map(|a| a.port()).unwrap_or(9099);

    // A term given on the command line is deployed into the session the first
    // client attaches to under the well-known id, so a demo can start with
    // something already standing on the live plane.
    if let Some(path) = &deploy {
        match std::fs::read_to_string(path) {
            Ok(src) => match server.preload("default", &src) {
                Ok(id) => println!("deployed {path} into session `default` as deploy {id}"),
                Err(diags) => {
                    for d in &diags {
                        eprintln!("{path}:{}..{}: {}: {}", d.lo, d.hi, d.code, d.message);
                    }
                    die("the term given to --deploy did not parse");
                }
            },
            Err(e) => die(&format!("cannot read {path}: {e}")),
        }
    }

    println!("f1r3x-serve listening on ws://{addr}/s/<session>");
    println!("  cores slider maximum : {}", cfg.cores_max);
    println!(
        "  session lifetime     : {}s after the last client detaches",
        cfg.session_ttl.as_secs()
    );
    for ip in local_addresses() {
        println!("  reachable at         : ws://{ip}:{port}/s/default");
    }

    // Bonjour, so the headset's browser finds this without anyone reading an
    // IP address aloud. `dns-sd` ships with macOS; if it is missing or fails,
    // that is not fatal — the client can still be given an address by hand.
    let _advert = if bonjour {
        match advertise(port) {
            Some(child) => {
                println!("  advertising          : _f1r3x._tcp on port {port}");
                Some(Guard(child))
            }
            None => {
                println!("  advertising          : unavailable (enter the address by hand)");
                None
            }
        }
    } else {
        None
    };

    println!("\nready. Ctrl-C to stop.");
    if let Err(e) = server.serve() {
        eprintln!("server stopped: {e}");
    }
}

/// Kills the Bonjour advertisement when the server exits.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn advertise(port: u16) -> Option<Child> {
    Command::new("dns-sd")
        .args([
            "-R",
            "f1r3x",
            "_f1r3x._tcp",
            "local",
            &port.to_string(),
            "path=/s",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Addresses a headset on the same network could reach, read from `ifconfig`
/// so that nothing has to be linked for it. Best-effort: the listing is a
/// convenience, and the server works without it.
fn local_addresses() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(mut child) = Command::new("ifconfig")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return out;
    };
    let mut text = String::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_string(&mut text);
    }
    let _ = child.wait();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("inet ") else {
            continue;
        };
        let Some(ip) = rest.split_whitespace().next() else {
            continue;
        };
        if ip.starts_with("127.") || ip.contains(':') {
            continue;
        }
        out.push(ip.to_string());
    }
    out
}

/// Accept `9099`, `:9099`, `0.0.0.0:9099` and `localhost:9099` alike.
fn normalise_addr(v: &str) -> String {
    if let Ok(port) = v.parse::<u16>() {
        return format!("0.0.0.0:{port}");
    }
    if let Some(port) = v.strip_prefix(':') {
        return format!("0.0.0.0:{port}");
    }
    v.to_string()
}

fn usage() {
    println!(
        "f1r3x-serve — a rho executive on a Mac, served to the Rhobots Vision Pro client

USAGE
    f1r3x-serve [OPTIONS]

OPTIONS
    -a, --addr HOST:PORT   where to listen            [default 0.0.0.0:9099]
    -c, --cores N          cores slider maximum       [default {}]
        --ttl SECONDS      detached session lifetime  [default 900]
    -d, --deploy FILE      deploy a .rho file into session `default` at start
        --no-bonjour       do not advertise over _f1r3x._tcp
    -v, --verbose          log requests
    -h, --help             this

A client connects to  ws://<host>:<port>/s/<session-name>  and speaks the
vocabulary defined in `f1r3x-host`: deploy, tick, run, scene, trace, log,
cores, mode, status, show, plus play, pause, step, pacing, buffer, ack,
treedepth, reset, ping and bye. The same vocabulary is what the C ABI and a
browser host accept, because all three dispatch through one crate.",
        available_parallelism()
    );
}

fn die(msg: &str) -> ! {
    eprintln!("f1r3x-serve: {msg}");
    eprintln!("try --help");
    std::process::exit(2);
}
