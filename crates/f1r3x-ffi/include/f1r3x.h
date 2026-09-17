/* f1r3x.h — the C ABI of the rho executive.
 *
 * Two clients are served by this one header: Swift on visionOS, via a bridging
 * header over the static library, and wasm32-unknown-unknown with no
 * wasm-bindgen, where the same exports are called through the module's own
 * linear memory.
 *
 * Nothing here returns a string. A wasm export cannot, so every call returns a
 * small integer and the caller pulls the bytes out afterwards. Executives live
 * behind u32 handles inside the library, so no Rust pointer crosses the wall.
 *
 * Requests are a one-line command, optionally followed by a newline and a
 * payload:
 *
 *     deploy\n<rho source>     submit a program
 *     tick                     one tick of the control panel
 *     run                      run to quiescence
 *     scene                    the world as the renderer should draw it
 *     trace                    the steps since the last drain
 *     log                      the store's event log (draining)
 *     cores <n> [max]          set the cores knob, clamped to max
 *     mode lockstep|free       set the scheduler mode
 *     status                   counters and settings
 *     show                     the normal form of the last deploy
 *
 * Every response is JSON: {"ok":true,"result":...} or
 * {"ok":false,"diags":[...]}.
 *
 * Typical use:
 *
 *     uint32_t x  = f1r3x_new();
 *     uint32_t r  = f1r3x_call(x, req, req_len);
 *     size_t   n  = f1r3x_response_len(r);
 *     char    *b  = malloc(n + 1);
 *     size_t   got = f1r3x_response_read(r, (uint8_t *)b, n);
 *     b[got] = 0;
 *     ...
 *     free(b);
 *     f1r3x_free(x);
 */

#ifndef F1R3X_H
#define F1R3X_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Protocol version. Check this before anything else; it is bumped when a
 * client must notice a change. */
uint32_t f1r3x_version(void);

/* Create an executive. Returns a handle, never 0. */
uint32_t f1r3x_new(void);

/* Destroy an executive. Returns 1 if it existed, 0 otherwise. */
uint32_t f1r3x_free(uint32_t handle);

/* Run a request. `ptr`/`len` are UTF-8 bytes, which need not be
 * NUL-terminated. Returns a response id.
 *
 * A call on a handle that does not exist still returns a readable response;
 * it just says so in its diagnostics. */
uint32_t f1r3x_call(uint32_t handle, const uint8_t *ptr, size_t len);

/* Bytes the response occupies, so the caller can size a buffer. 0 means there
 * is no such response, or it has already been read. */
size_t f1r3x_response_len(uint32_t response);

/* Copy a response into `out` and release it. Returns the number of bytes
 * written, or 0 if `cap` was too small — in which case the response is left in
 * place, so the caller can ask for its length again and retry. */
size_t f1r3x_response_read(uint32_t response, uint8_t *out, size_t cap);

/* Discard a response the caller decided not to read. Returns 1 if it existed. */
uint32_t f1r3x_response_release(uint32_t response);

/* Allocate `len` bytes inside the library, for a caller with no other way to
 * hand over a string — which is to say, for wasm. Native callers can pass
 * their own pointer to f1r3x_call and ignore these two. */
uint8_t *f1r3x_alloc(size_t len);

/* Release a buffer from f1r3x_alloc. `len` must be the length it was made
 * with. */
void f1r3x_dealloc(uint8_t *ptr, size_t len);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* F1R3X_H */
