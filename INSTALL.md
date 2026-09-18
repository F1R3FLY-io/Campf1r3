# Installing

This archive is the **complete workspace**, not an overlay. Unpack it over your
`Campf1r3` checkout:

```
tar -xzf campf1r3-complete.tar.gz
rsync -a Campf1r3/ /path/to/your/Campf1r3/     # keeps your .git
cd /path/to/your/Campf1r3
cargo test
```

Expect 170 tests to pass. `--test-threads=1` is not required.

One test, `a_detached_session_stops_advancing`, is the only one whose subject
is timing rather than logic: it checks that a session with no client attached
stops advancing. It is written as a margin test, because a few rounds always
slip through between a client closing its end and the server noticing, and how
many depends on how fast the machine is. Its signal is buffer exhaustion — a
session that ignored detachment would fill its 256-round buffer within a
millisecond or two and still have admission closed on reattach, so no round
would arrive at all. An earlier version asserted an exact tick count, which
passed on a slow machine and failed on a fast one.

The workspace contains all eleven crates, so it builds from a bare directory
too, but then you have no git history — prefer the rsync onto your clone.

## What changed against the previous state of the repository

Three crates are new, six files are edited, and nothing else is touched.
`k1ndl1ng-ast`, `k1ndl1ng-parse`, `campf1r3-core`, `k1ndl1ng-campf1r3` and
`b3ll0ws` are byte-identical to what was there before.

### New

```
crates/f1r3x-host/          sessions, pacing, vocabulary, frames.
                            No sockets, no threads, no clock.
crates/f1r3x-wasm/          a browser host, with a working visualiser in web/
crates/f1r3x-serve/         rewritten: transport only. Its former contents
                            are now f1r3x-host.
crates/k1ndl1ng-norm/src/json.rs
                            structural JSON for a normal form, beside the
                            normaliser where §11 of the k1ndl1ng spec puts
                            the editor and scene services
examples/replication.rho    replication from reflection
HOSTING.md                  what all of this is, and why it is arranged so
```

### Edited

| file | change |
| --- | --- |
| `Cargo.toml` | three new workspace members |
| `crates/k1ndl1ng-norm/src/lib.rs` | registers and re-exports `json` |
| `crates/f1r3x/src/scene.rs` | `Card` gains `term` and `chan_fp` |
| `crates/f1r3x-ffi/src/lib.rs` | `dispatch` delegates to `f1r3x-host` |
| `crates/f1r3x-ffi/Cargo.toml` | the dependency that follows from it |

Every existing test passes as written, including all 16 of `f1r3x-ffi`'s —
which is the check that moving its vocabulary downward changed nothing a
caller can see.

## The two source changes worth reviewing

**`Card` carries its term.** A consumer that wants a term's *shape* rather than
its label previously had to walk the machine a second time to recover the
terms, and the two walks agreeing was a property nobody could check. With the
term on the card, an id and a tree cannot describe different objects.

**`dispatch` moved down.** It is the command vocabulary, and three surfaces now
share one implementation rather than each carrying a copy. `f1r3x-ffi` keeps
its exact signature and behaviour; it is a delegation, not a rewrite.

## Quick check

```
cargo build --release
./target/release/f1r3x-serve --cores 8 --deploy examples/replication.rho
```

and, for the browser:

```
crates/f1r3x-wasm/build.sh
python3 -m http.server -d crates/f1r3x-wasm/web 8080
```
