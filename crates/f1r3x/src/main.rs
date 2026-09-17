//! `f1r3x` — run a rho program from the command line.
//!
//! The headset is the real client; this binary exists so the same executive can
//! be driven without one, which is how the examples in `examples/` are checked.
//!
//!     f1r3x run prog.rho [--cores N] [--free-running] [--trace] [--json]
//!     f1r3x show prog.rho          normalise and print the normal form
//!     f1r3x hash prog.rho          print the content hash
//!
//! With `--cores N` and the default lockstep mode the program advances N
//! components per tick and the scene is printed after each tick, which is the
//! control panel in text form.

use std::io::Read;
use std::process::ExitCode;

use f1r3x::{json, Executive, Mode, Settings, Step};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        return ExitCode::from(2);
    }
    match args[0].as_str() {
        "run" => run(&args[1..]),
        "show" => show(&args[1..], false),
        "hash" => show(&args[1..], true),
        "-h" | "--help" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("f1r3x: unknown command `{other}`");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!(
        "f1r3x — a rho calculus executive

  f1r3x run  <file.rho|-> [--cores N] [--free-running] [--trace] [--json] [--ticks N]
  f1r3x show <file.rho|->
  f1r3x hash <file.rho|->

Options
  --cores N        components advanced per tick (default 1)
  --free-running   run to quiescence instead of one tick at a time
  --trace          print every step
  --json           print the scene as JSON
  --ticks N        stop after N ticks (default: until quiescent)"
    );
}

fn read(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| e.to_string())?;
        return Ok(s);
    }
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

struct Opts {
    path: String,
    cores: u32,
    mode: Mode,
    trace: bool,
    as_json: bool,
    ticks: Option<u64>,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        path: String::new(),
        cores: 1,
        mode: Mode::Lockstep,
        trace: false,
        as_json: false,
        ticks: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cores" => {
                i += 1;
                o.cores = args
                    .get(i)
                    .ok_or("--cores wants a number")?
                    .parse()
                    .map_err(|_| "--cores wants a number".to_string())?;
            }
            "--ticks" => {
                i += 1;
                o.ticks = Some(
                    args.get(i)
                        .ok_or("--ticks wants a number")?
                        .parse()
                        .map_err(|_| "--ticks wants a number".to_string())?,
                );
            }
            "--free-running" => o.mode = Mode::FreeRunning,
            "--trace" => o.trace = true,
            "--json" => o.as_json = true,
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option `{other}`"))
            }
            other => o.path = other.to_string(),
        }
        i += 1;
    }
    if o.path.is_empty() {
        return Err("give a file, or `-` for stdin".to_string());
    }
    Ok(o)
}

fn run(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("f1r3x: {e}");
            return ExitCode::from(2);
        }
    };
    let src = match read(&o.path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("f1r3x: {e}");
            return ExitCode::from(2);
        }
    };
    let mut x = Executive::new(Settings {
        cores: o.cores.max(1),
        mode: o.mode,
        ..Settings::default()
    });
    if let Err(e) = x.deploy(&src) {
        for d in &e.diags {
            eprintln!("{}:{}: {}: {}", d.lo, d.hi, d.code, d.message);
        }
        return ExitCode::from(1);
    }

    let mut ticks = 0u64;
    while !x.is_quiescent() {
        let steps = match x.tick() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("f1r3x: store error: {e:?}");
                return ExitCode::from(1);
            }
        };
        ticks += 1;
        if o.trace {
            for s in &steps {
                println!("{}", line(s));
            }
        }
        if let Some(limit) = o.ticks {
            if ticks >= limit {
                break;
            }
        }
        if steps.is_empty() {
            break;
        }
    }

    let sc = x.scene();
    if o.as_json {
        println!("{}", json::scene(&sc));
    } else {
        print_scene(&sc, ticks);
    }
    ExitCode::SUCCESS
}

fn line(s: &Step) -> String {
    match s {
        Step::Inert { term } => format!("inert    {}", term.short()),
        Step::Minted { count, first } => format!("mint     {count} from {}", first.short()),
        Step::Produced { chan, datum } => {
            format!("produce  {} <- {}", chan.short(), datum.short())
        }
        Step::Consumed { group, cont } => format!(
            "consume  {} on {}",
            cont.short(),
            group
                .iter()
                .map(|h| h.short())
                .collect::<Vec<_>>()
                .join(" & ")
        ),
        Step::Comm {
            chan,
            cont,
            spawned,
        } => format!(
            "comm     {} x {} -> {spawned}",
            chan.short(),
            cont.short()
        ),
        Step::Reissued { chan } => format!("reissue  {}", chan.short()),
    }
}

fn print_scene(sc: &f1r3x::Scene, ticks: u64) {
    println!(
        "after {ticks} tick(s): {} queued, {} datum(s), {} waiting, {} comm(s){}",
        sc.counts.0,
        sc.counts.1,
        sc.counts.2,
        sc.counts.3,
        if sc.quiescent { ", quiescent" } else { "" }
    );
    for c in &sc.rhobots {
        println!("  rhobot   {}  {}", &c.id[..8], c.label);
    }
    for c in &sc.tuples {
        println!("  tuple    {}  {}", &c.id[..8], c.label);
    }
    for c in &sc.listeners {
        println!("  listener {}  {}", &c.id[..8], c.label);
    }
}

fn show(args: &[String], hash_only: bool) -> ExitCode {
    let path = match args.iter().find(|a| !a.starts_with('-')) {
        Some(p) => p.clone(),
        None => {
            eprintln!("f1r3x: give a file, or `-` for stdin");
            return ExitCode::from(2);
        }
    };
    let src = match read(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("f1r3x: {e}");
            return ExitCode::from(2);
        }
    };
    let mut x = Executive::new(Settings::default());
    match x.deploy(&src) {
        Err(e) => {
            for d in &e.diags {
                eprintln!("{}:{}: {}: {}", d.lo, d.hi, d.code, d.message);
            }
            ExitCode::from(1)
        }
        Ok(id) => {
            let d = &x.deploys()[id as usize];
            if hash_only {
                println!("{}", d.term.hash().hex());
            } else {
                println!("{}", f1r3x::show(&d.term));
                if !d.free.is_empty() {
                    eprintln!("free names grounded: {}", d.free.join(", "));
                }
            }
            ExitCode::SUCCESS
        }
    }
}
