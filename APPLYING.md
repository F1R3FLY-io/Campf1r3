# Applying this to the CampF1R3 checkout

This archive is an overlay: copy it over the repository root. It adds three
crates and replaces five files.

```
rsync -a campf1r3-hosting/ /path/to/Campf1r3/
cd /path/to/Campf1r3
cargo test
```

Expect 170 tests to pass.

## New

```
crates/f1r3x-host/          sessions, pacing, vocabulary, frames.
                            No sockets, no threads, no clock.
crates/f1r3x-wasm/          a browser host, with a working visualiser in web/
crates/k1ndl1ng-norm/src/json.rs
                            structural JSON for a normal form, beside the
                            normaliser where the spec's §11 puts the editor
                            and scene services
examples/replication.rho    replication from reflection
HOSTING.md                  what all of this is, and why it is arranged so
```

## Replaced

| file | change |
| --- | --- |
| `Cargo.toml` | three new workspace members |
| `crates/k1ndl1ng-norm/src/lib.rs` | registers and re-exports `json` |
| `crates/f1r3x/src/scene.rs` | `Card` gains `term` and `chan_fp` |
| `crates/f1r3x-ffi/src/lib.rs` | `dispatch` delegates to `f1r3x-host` |
| `crates/f1r3x-ffi/Cargo.toml` | the dependency that follows from it |
| `crates/f1r3x-serve/*` | reduced to transport; its former contents are now `f1r3x-host` |

Nothing else is touched. `k1ndl1ng-ast`, `k1ndl1ng-parse`, `campf1r3-core`,
`k1ndl1ng-campf1r3` and `b3ll0ws` are unchanged, and every existing test passes
as written — including all 16 of `f1r3x-ffi`'s, which is the check that moving
its vocabulary downward changed nothing a caller can see.

## The two source changes worth reviewing

**`Card` carries its term.** A consumer that wants a term's *shape* rather than
its label previously had to walk the machine a second time to get the terms
back, and the two walks agreeing was a property nobody could check. With the
term on the card, an id and a tree cannot describe different objects.

**`dispatch` moved down.** It is the command vocabulary, and three surfaces now
share one implementation rather than each carrying a copy. `f1r3x-ffi` keeps
its exact signature and behaviour; it is a delegation, not a rewrite.
