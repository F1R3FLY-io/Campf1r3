# Hosting a `CampF1R3` executive

`f1r3x` decides *what* reduces. This layer decides *when* a decided commit is
released to whoever is watching, and packages the result as frames. There are
three hosts and they differ only in what they wire.

```
k1ndl1ng-ast · k1ndl1ng-parse · k1ndl1ng-norm        pure, portable
campf1r3-core · k1ndl1ng-campf1r3 · b3ll0ws · f1r3x
        │
        ├── f1r3x-host     sessions, pacing, vocabulary, frames.
        │                  No sockets, no threads, no clock.
        │
        ├── f1r3x-serve    TcpListener + threads + WebSocket   a networked host
        ├── f1r3x-wasm     the page's own event loop           a browser tab
        └── f1r3x-ffi      whatever loop the caller has        linked in-process
```

No application appears anywhere in this tree. A headset, a browser visualiser
and a command-line driver are all clients of the same vocabulary, and the one
that motivated it — the Rhobots Vision Pro interface — lives in its own
repository and contains no Rust at all.

## `f1r3x-host`

Two decisions make the rest possible, and both were mistakes in the first
version of this code, which was written as a server and only afterwards asked
to be anything else.

**The driver is a state machine, not a thread.** `Session::poll(now_ms)`
returns the frames that fell out. A native host calls it from a thread; a page
calls it from `requestAnimationFrame`. The admission logic does not change
between them, because it was always a pure predicate over the control state —
it was only ever the *waiting* that needed a thread.

**The clock is injected.** `std::time::Instant::now` panics on
`wasm32-unknown-unknown`, which is one of the reasons `CampF1R3` exists; a host
layer that called it would have put the hazard straight back. Time arrives as a
`u64` of milliseconds from whatever the host has. Building with
`--no-default-features` removes `SystemClock` altogether, so a wasm host cannot
reach it by accident.

### Pacing

How a client and the evaluator are paced against each other is the client's
choice:

- **Lockstep.** Admit a round of at most `cores` commits, then withhold
  admission of the next until the client acknowledges that it has finished with
  this one.
- **Free-running.** Admit without waiting. Rounds feed a bounded buffer that
  the client drains; when it is full, admission pauses until an entry is
  drained, so the evaluator never runs further ahead than the bound and *every
  commit is seen*.

The two are one rule with different bounds — lockstep is free-running with a
buffer of one — and `Control::admission_open` is the only place either is
decided. In process this was a semaphore; across any boundary at all it has to
be an acknowledgment.

A client acknowledges when it has finished *showing* a round, not when the
round arrives. Acknowledging on arrival turns lockstep into free-running and
free-running into an unbounded queue, and the client falls silently behind the
evaluator while looking perfectly healthy.

### The vocabulary

One line protocol, one implementation, three surfaces. A request may begin
`#<n> ` to carry a correlation number, which comes back on the reply.

| needs | verbs |
| --- | --- |
| an executive (`proto::executive_command`) | `deploy` `tick` `run` `scene` `trace` `log` `cores` `mode` `status` `show` |
| a session (`Session::handle`) | `play` `pause` `step` `pacing` `buffer` `ack` `treedepth` `reset` `ping` `bye` |

`f1r3x-ffi` calls the first directly, which is why its C ABI is unchanged.

Frames out are tagged objects: `ready`, `reply`, `round`, `scene`, `stats`,
`quiescent`, `error`. A `round` carries the commit log *and* the scene that
results from it, so a client animates the log and then has the world to apply —
no diffing, and no state the executive already holds reconstructed elsewhere.

`WIRE_VERSION` is the one number a client checks. It arrives on `ready`.

## `k1ndl1ng-norm::json`

A normal form has a printed form and an encoded form, and neither is what a
renderer wants. A printer gives text a person reads; the encoding gives bytes
the store keys by. `json` gives the *shape*: constructors, children, and enough
identity on each to follow an object across frames.

It sits beside the normaliser for the reason §11 of the k1ndl1ng spec gives for
putting editor and scene services there rather than in any client — so that a
text editor, an LSP server and a headset share one implementation. Nothing in
it knows what a rhobot is.

Every node carries `fp`, the content hash. Two names that agree there are
equivalent, so a client never decides name equivalence itself. Elision happens
here, against a depth the client asks for, and an elided node keeps its
fingerprint — which is what makes an elided name still comparable, and what
stops one deep term from sending megabytes to a consumer that would draw three
levels of it.

## `f1r3x-serve`

```
cargo build --release
./target/release/f1r3x-serve --cores 8 --deploy examples/replication.rho
```

```
f1r3x-serve [--addr HOST:PORT] [--cores N] [--ttl SECONDS]
            [--no-bonjour] [--verbose] [--deploy FILE]
```

Clients connect to `ws://<host>:<port>/s/<session-name>`. The server prints the
addresses it can be reached on and advertises `_f1r3x._tcp` through `dns-sd`,
so nobody has to read an IP address aloud.

**Sessions outlive sockets.** Clients go away: a headset sleeps, a tab is
reloaded, Wi-Fi roams. If that destroyed the executive, a person would lose a
running world by putting a device down for a minute. Sessions are named by the
client, in the request path, and the name lives in the client's own storage
rather than in a cookie this server issued — so a resume works across a process
the system has restarted. An unknown name creates the session, so a first run
and a resume are the same call. A second attach to a live session evicts the
first and says so: two clients acknowledging into one executive would each see
pacing neither asked for.

A detached session does not advance. A round nobody can see would never be
acknowledged, and this is also what makes a reconnect pick up where it stopped
rather than skip ahead.

## `f1r3x-wasm`

```
crates/f1r3x-wasm/build.sh
python3 -m http.server -d crates/f1r3x-wasm/web 8080
```

No server, no network in the loop: the executive is compiled into the page and
the transport is a function call. `web/index.html` is a working visualiser in
about a hundred lines — deploy, run, step, pacing, round width, the commit log,
and the scene tree rendered back to source-shaped text.

Because it is the same host, a page and a headset run the same term and receive
identical frames with identical fingerprints, which makes one a check on the
other.

There is no `wasm-bindgen` and no toolchain beyond cargo. A wasm export cannot
return a string, so every call returns a small integer and the caller pulls the
bytes out afterwards — the shape `f1r3x-ffi` already uses.

## One departure worth knowing

`b3ll0ws` is a single-threaded deterministic machine, so `cores` is the **width
of an admitted round**, not a count of racing threads. Up to `cores`
communications are in progress at once and channel contention still flattens
the throughput readout, but the plateau is a property of the term rather than
of a lock.

The upside is the property this workspace's README already claims, now visible
over a wire: the final scene is identical at width 1, at width 64, and
free-running. A person can slow a world to a single step and watch exactly the
run they saw at speed. `f1r3x-host` tests it.

## Tests

```
cargo test
```

170 across the workspace. The hosting layer's are worth noting for where they
are: the pacing contract, the vocabulary and the frames are 27 tests in
`f1r3x-host` that need no socket, no thread and no sleep, because the driver is
a state machine. `f1r3x-serve` keeps 12 that only a socket can show — the
handshake, the framing, and the lifecycle. Before the split these were all
socket tests, and four of them asserted that *nothing* arrived within a window;
the suite ran in 21 seconds and was flaky under load. It now runs in under one.
