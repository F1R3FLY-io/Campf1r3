// A thin wrapper over the module's exports.
//
// The whole of the boundary is here: strings in and out of linear memory, and
// a loop that polls. Everything else -- the vocabulary, the pacing, the scene
// trees -- is the same `f1r3x-host` a socket client talks to, so a page and a
// headset are reading identical frames.

export class Executive {
  static async load(url = "f1r3x_wasm.wasm", coresMax = 1) {
    const { instance } = await WebAssembly.instantiateStreaming(fetch(url), {});
    return new Executive(instance.exports, coresMax);
  }

  constructor(x, coresMax) {
    this.x = x;
    this.wire = x.f1r3x_version();
    this.handle = x.f1r3x_new(coresMax);
    this.round = 0;
  }

  get memory() { return new Uint8Array(this.x.memory.buffer); }

  // Copy a JS string into the module's memory, run `fn`, free the copy.
  _withString(s, fn) {
    const bytes = new TextEncoder().encode(s);
    const ptr = this.x.f1r3x_alloc(bytes.length);
    this.memory.set(bytes, ptr);
    try { return fn(ptr, bytes.length); }
    finally { this.x.f1r3x_dealloc(ptr, bytes.length); }
  }

  // Read a parked buffer out and release it.
  _take(id) {
    const n = this.x.f1r3x_take_len(id);
    if (n === 0) { this.x.f1r3x_release(id); return ""; }
    const ptr = this.x.f1r3x_alloc(n);
    const got = this.x.f1r3x_take_read(id, ptr, n);
    const text = new TextDecoder().decode(this.memory.slice(ptr, ptr + got));
    this.x.f1r3x_dealloc(ptr, n);
    return text;
  }

  ready(name = "page") {
    return JSON.parse(this._withString(name, (p, n) =>
      this._take(this.x.f1r3x_ready(this.handle, p, n))));
  }

  request(command) {
    return JSON.parse(this._withString(command, (p, n) =>
      this._take(this.x.f1r3x_request(this.handle, p, n))));
  }

  scene() { return JSON.parse(this._take(this.x.f1r3x_scene(this.handle))); }

  // One turn of the loop. `now` is the page's clock, because the host has none.
  poll(now = performance.now()) {
    return JSON.parse(this._take(this.x.f1r3x_poll(this.handle, Math.floor(now))));
  }

  // Drive the session, acknowledging each round once it has been drawn.
  //
  // This is the whole pacing contract as a page sees it. Acknowledging here,
  // after `onFrame` has returned, is what makes lockstep mean lockstep: an ack
  // sent on arrival would turn it into free-running and leave the drawing
  // silently behind the evaluator.
  run(onFrame) {
    const tick = () => {
      for (const frame of this.poll()) {
        onFrame(frame);
        if (frame.t === "round") {
          this.round = frame.round;
          this.request(`ack ${frame.round}`);
        }
      }
      this._raf = requestAnimationFrame(tick);
    };
    tick();
  }

  stop() { if (this._raf) cancelAnimationFrame(this._raf); }
}
