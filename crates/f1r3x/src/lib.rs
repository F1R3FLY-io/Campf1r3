//! `f1r3x` — the executive.
//!
//! This is what the Vision Pro talks to. It owns the deploy path (parse,
//! normalise, ground the free names, spawn), the scheduler the control panel
//! drives, the event stream the animation reads, and the scene the renderer
//! draws.
//!
//! Two knobs, both asked for by the Rhobots design: **cores**, how many
//! components may be advanced in one tick, and **mode**, whether the machine
//! runs freely or waits for the next tick. Neither changes the *order* of
//! reduction — the queue is FIFO over encoding-ordered components and the
//! store is in its deterministic profile — so turning the cores knob changes
//! how fast the rhobots move, never what they do. That is the property the
//! demo needs: a person can slow the world to one step and see exactly the run
//! they just watched at full speed.

#![forbid(unsafe_code)]

pub mod json;
pub mod scene;

use b3ll0ws::{Direct, Machine, Sequential};
use campf1r3_core::{Config, SpaceError};
use k1ndl1ng_norm::{normalise, show_pretty, Norm, Options as NOpts};
use k1ndl1ng_parse::{parse, Level, Options as POpts};

pub use b3ll0ws::{Job, Step};
pub use campf1r3_core::Profile;
pub use scene::{Card, Scene};

/// A deploy failed before it ever reached the machine.
#[derive(Clone, Debug)]
pub struct DeployError {
    pub diags: Vec<Diag>,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub lo: u32,
    pub hi: u32,
    pub code: String,
    pub message: String,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Advance at most `cores` components per tick, then stop and wait.
    Lockstep,
    /// Keep going until the machine is quiescent or the budget runs out.
    FreeRunning,
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// 1 .. device maximum. The panel's slider.
    pub cores: u32,
    pub mode: Mode,
    /// Ceiling on components advanced in one `tick`, whatever the mode.
    pub budget: u64,
    pub profile: Profile,
    pub level: Level,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            cores: 1,
            mode: Mode::Lockstep,
            budget: 100_000,
            profile: Profile::Deterministic,
            level: Level::K1,
        }
    }
}

/// One deploy, as the headset submitted it.
#[derive(Clone, Debug)]
pub struct Deploy {
    pub id: u32,
    pub source: String,
    pub term: Norm,
    /// The free names the deploy mentioned, in level order, now grounded.
    pub free: Vec<String>,
}

pub struct Executive {
    machine: Machine<Direct, Sequential>,
    settings: Settings,
    deploys: Vec<Deploy>,
    tick: u64,
    trace: Vec<Step>,
    keep_trace: usize,
}

impl Executive {
    pub fn new(settings: Settings) -> Executive {
        let cfg = Config {
            profile: settings.profile,
            ..Config::default()
        };
        Executive {
            machine: Machine::new(cfg),
            settings,
            deploys: Vec::new(),
            tick: 0,
            trace: Vec::new(),
            keep_trace: 4096,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Turn the cores knob. Clamped to at least one; the caller supplies the
    /// device maximum.
    pub fn set_cores(&mut self, cores: u32, device_max: u32) {
        self.settings.cores = cores.clamp(1, device_max.max(1));
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.settings.mode = mode;
    }

    pub fn deploys(&self) -> &[Deploy] {
        &self.deploys
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    pub fn machine(&self) -> &Machine<Direct, Sequential> {
        &self.machine
    }

    /// The deploy path: parse, normalise, ground the free names, spawn.
    ///
    /// Grounding is not optional. A K1ndl1ng encoding carries de Bruijn levels
    /// and never identifiers, so two terms that differ only by the naming of
    /// their free names encode identically (spec Prop. 7.8). Until a free name
    /// is replaced by an unforgeable derived from its identifier, `stdout` in
    /// one deploy and `stdout` in another are not the same channel.
    pub fn deploy(&mut self, source: &str) -> Result<u32, DeployError> {
        let popts = POpts {
            level: self.settings.level,
            ..POpts::default()
        };
        let p = parse(source, &popts);
        if !p.ok() {
            return Err(DeployError {
                diags: p
                    .diags
                    .iter()
                    .map(|d| Diag {
                        lo: d.span.lo,
                        hi: d.span.hi,
                        code: d.code.to_string(),
                        message: d.message.clone(),
                    })
                    .collect(),
            });
        }
        let n = normalise(&p.tree, &NOpts::default()).map_err(|ds| DeployError {
            diags: ds
                .iter()
                .map(|d| Diag {
                    lo: d.span.lo,
                    hi: d.span.hi,
                    code: d.code.to_string(),
                    message: d.message.clone(),
                })
                .collect(),
        })?;
        let free: Vec<String> = n.free.iter().map(|f| f.ident.clone()).collect();
        let term = n.term.ground_free_by(&free);
        let id = self.deploys.len() as u32;
        self.deploys.push(Deploy {
            id,
            source: source.to_string(),
            term: term.clone(),
            free,
        });
        self.machine.spawn(term);
        Ok(id)
    }

    /// One tick of the control panel. Returns the steps taken, in order.
    pub fn tick(&mut self) -> Result<Vec<Step>, SpaceError> {
        self.tick += 1;
        let budget = match self.settings.mode {
            Mode::Lockstep => self.settings.cores as u64,
            Mode::FreeRunning => self.settings.budget,
        };
        let steps = self.machine.run(budget)?;
        self.trace.extend(steps.iter().cloned());
        // The trace is for the animation, not for the record; the store's own
        // event log is the record.
        if self.trace.len() > self.keep_trace {
            let drop = self.trace.len() - self.keep_trace;
            self.trace.drain(..drop);
        }
        Ok(steps)
    }

    /// Run to quiescence, whatever the mode.
    pub fn run_to_rest(&mut self) -> Result<Vec<Step>, SpaceError> {
        let mut all = Vec::new();
        while !self.machine.is_quiescent() {
            let before = all.len();
            all.extend(self.machine.run(self.settings.budget)?);
            if all.len() == before && !self.machine.is_quiescent() {
                break;
            }
        }
        self.trace.extend(all.iter().cloned());
        Ok(all)
    }

    pub fn is_quiescent(&self) -> bool {
        self.machine.is_quiescent()
    }

    pub fn trace(&self) -> &[Step] {
        &self.trace
    }

    /// What the store recorded. Draining, like the store's own log.
    pub fn take_log(&self) -> Vec<campf1r3_core::Event> {
        self.machine.space().take_log()
    }

    /// The world, as the renderer should draw it now.
    pub fn scene(&self) -> Scene {
        scene::build(&self.machine, self.tick)
    }
}

/// Pretty-print a term the way the panel shows it.
pub fn show(term: &Norm) -> String {
    show_pretty(term)
}

#[cfg(test)]
mod tests;
