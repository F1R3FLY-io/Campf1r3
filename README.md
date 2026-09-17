# f1r3-executive

A rho calculus executive in Rust: parser, normaliser, interpreter and rspace,
built to the specifications in `campf1r3/` of the F1R3FLY-io publications repo
(`campf1r3-spec.tex`, `k1ndl1ng-spec.tex`, `bytecode-or-normal-form.tex`).

It exists to be the executive behind the Rhobots Vision Pro interface: a person
writes rho with their hands, and this runs it, one visible step at a time or as
fast as the device allows.

## The crates

| crate | what it is |
| --- | --- |
| `k1ndl1ng-ast` | the surface tree: an arena, u32 indices, no recursion anywhere including `Drop` |
| `k1ndl1ng-parse` | lexer and an explicit-stack parser for K0/K1/K2; the parser has no opinion about binding |
| `k1ndl1ng-norm` | binder resolution, de Bruijn assignment, the normal form, its encoding, BLAKE2b-256 content hashing, substitution |
| `campf1r3-core` | the store: `D : C → Bag(datum)` and `W, I : C⁺ → Bag(waiting)`, with produce, consume and install as single cuts |
| `k1ndl1ng-campf1r3` | the glue that instantiates the store over normal forms, plus the kernel spatial matcher |
| `b3ll0ws` | the interpreter: executes the normal form directly, with a plan cache and a `Backend` seam |
| `f1r3x` | the executive: deploy path, scheduler, event stream, scene, CLI |
| `f1r3x-ffi` | the C ABI, for Swift on visionOS and for wasm without wasm-bindgen |

Each crate depends only on the ones above it. There are **no external
dependencies at all** — not even for hashing.

## Building

```
cargo build --offline
cargo test  --offline
```

Rust 1.75, edition 2021. Every crate but `f1r3x-ffi` carries
`#![forbid(unsafe_code)]`; the FFI cannot, because copying bytes into a
caller's buffer is its whole job, and its `unsafe` is confined to four
functions at the bottom of one file.

## Running something

```
f1r3x run examples/relay.rho --cores 1 --trace
f1r3x run examples/echo.rho  --free-running
f1r3x show examples/hello.rho      # the normal form
f1r3x hash examples/hello.rho      # its content hash
```

`--cores N` sets how many components advance per tick; `--free-running` runs to
quiescence. Turning that knob changes how many ticks a program takes and
changes nothing else: the queue is FIFO over encoding-ordered components and
the store is in its deterministic profile, so the final scene is identical at
1 core, at 64, and free-running. That is the property the demo rests on — a
person can slow the world to a single step and watch exactly the run they just
saw at speed.

## Three things worth knowing before reading the code

**Free names have to be grounded.** A K1ndl1ng encoding carries de Bruijn
levels and never identifiers, so two terms differing only by the naming of
their free names encode identically (spec Prop. 7.8). `stdout` in one deploy
and `stdout` in another are not the same channel until each is replaced by an
unforgeable derived from the identifier. The deploy path does this; nothing
downstream would work if it did not.

**An identifier inside a pattern is a binder.** If no inner scope binds it,
`for(@{d!(Nil)} <- c)` matches a send on *any* channel, because `d` is a
pattern variable. A pattern meant to be ground must name its channels by closed
quotes: `@{Nil}`, not `a`. See `examples/pattern.rho`.

**The re-issue contract is symmetric.** The store never stores an arriving
record that reacts, and an arriving *continuation* is an arriving record too.
A persistent send that reacts on arrival is re-issued by the caller until it
comes to rest — and so is a persistent receipt. Both loops live in `b3ll0ws`
and nowhere else.

## Layout

```
crates/            the eight crates above
examples/          rho programs, all runnable through the CLI
crates/f1r3x-ffi/include/f1r3x.h    the C header
```
