# rhoviz

Draw a rho term as a coloured figure in three dimensions and animate its
reduction.

```
cargo run --release -p rhoviz -- examples/replication.rho -n 6 -o replication.html
cargo run --release -p rhoviz -- -e 'for(y <- @Nil){ *y } | @Nil!(Nil)'
```

| option | meaning |
| --- | --- |
| `<file.rho>` or `-` | the term, from a file or standard input |
| `-e TERM` | the term, from the command line |
| `-n, --steps N` | stop after N comm events (default 10); `0` draws the term only |
| `-o, --out FILE` | the animation, one self-contained HTML file (default `rho.html`) |

Open the HTML file in any browser. Drag to rotate, pinch or scroll to zoom.

## What is drawn

The *chromatic realisation* of the syntax tree: the constructor of a node is its
colour, and the position of an argument is one of three orthogonal axes. A node
whose largest child has radius `r` puts its children at distance
`sqrt(2) * r + 1` along the axes, which keeps sibling figures in disjoint balls;
no two spokes meet except at a shared hub. The crate's tests check that on a
running example.

| node | children along axes 1, 2, 3 |
| --- | --- |
| `P \| Q` | left half, right half (balanced; `\|` is associative) |
| `x!(P)` | channel, payload |
| `for(y <- x){P}` | pattern, channel, body |
| `for(.. & ..){P}` | pattern, channel, then the next bind or the body |
| `@P`, `*x`, `new n in {P}` | the one argument |
| `a, b, ..` | head, rest (polyadic payloads and patterns) |

Bound names are de Bruijn indices, shown as labelled leaves. Binding itself is
not drawn.

## What is animated

The term is deployed to the `f1r3x` executive at one core in lockstep. After
every tick the world is read back as one parallel composition: queued
components, data at rest (as sends) and continuations at rest (as receipts). A
frame is recorded when that composition changes, so minting by `new` is a frame
of its own and a send coming to rest is not. `-n` counts comms only.

A component is identified by its content hash, so one that takes no part in a
step is the same object in the next frame and glides to its new place. A red
ring marks a component about to react; a green ring marks one just produced.

## Limits

- Sizes grow by about 2.4 per level of nesting, so deep subterms are small at
  the default zoom.
- A component that reacts and is re-formed identically, as the listener in
  `examples/replication.rho` is, keeps its identity and is not ringed.
- The store does not expose whether a datum at rest is persistent, so a
  persistent send at rest is drawn as an ordinary send.
