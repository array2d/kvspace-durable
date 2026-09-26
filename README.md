# kvspace-durable

[![CI](https://github.com/array2d/kvspace-durable/actions/workflows/ci.yml/badge.svg)](https://github.com/array2d/kvspace-durable/actions/workflows/ci.yml)

Rust implementation of the **KVSpace** used by kvlang — the filesystem-style key-value store that serves as kvlang's unified addressing and memory space (keys are paths, values are XValues).

This is one of two standard implementations of the KVSpace contract; the other is [kvspace-c](../kvspace-c). Both expose the same C ABI and the same XValue kindexpr format, so a consumer (the kvlang layout/runtime) switches between them by DSN only.

Backends: `redis://` (default when no scheme is given), `fs://` — selected by DSN scheme in `conn("redis://127.0.0.1:6379")` / `conn("fs:///tmp/kvspace")`.

## Build

```bash
make build       # cargo build --release → libkvspace_durable.so
make test        # build + tutorial/test.py
```

Crate types: `rlib`, `staticlib`, `cdylib` (`libkvspace_durable.so`).

> The `kvspace` CLI is **not** part of this crate — it lives in the [kvspace](../kvspace) repo
> (`cli/`, drives the dispatch front end so the backend is chosen by DSN).

## ABI

C ABI exported from the cdylib (`src/ffi.rs`):

- lifecycle: `kvspaceConnect`, `kvspaceClose`
- KV read/write: `kvspaceGet` (borrow), synchronous `kvspaceSetValue`, `kvspaceWriteInPlace`, `kvspaceWriteNewPlace`
- enumerate / delete / copy: `kvspaceListLen`, `kvspaceListAt`, `kvspaceDel`, `kvspaceDelTree`, `kvspaceCp`, `kvspaceCpTree`
- directories / extindex: `kvspaceMkindex`, `kvspaceMkindexExt`, `kvspaceRmindexExt`
- watch / clear: `kvspaceWatch`, `kvspaceClear`
- XValue codec (frontend malloc, caller `free()`s): `kvspaceTlvEncode`, `kvspaceTlvEncodeMode`, `kvspaceDecodeHead`, `kvspaceNewPtr`, `kvspaceNewChar`, `kvspaceNewBool`, `kvspaceNewInt64`, `kvspaceNewFloat64`

The same ABI is implemented by `kvspace-c` (`shm://`), so a consumer (e.g. the kvlang layout) switches backends by DSN only, with no code change.

## XValue

The headlenpow wire format is byte-identical across this backend, `kvspace-c`, and the `kvspace` frontend. Its 18-byte prefix is `[headlenpow:u8][flags:u8][a:u64 LE][b:u64 LE]`. The UTF-8 langtype occupies the rest of the `2^headlenpow` head, followed by the body. `flags` uses its low two bits for the storage class and bit 2 for a pointer.

- Class 0 stores short fixed values, `None`, maps, structs, and directory markers. `None` has an empty langtype; map members and directory children are enumerated from physical key prefixes.
- Class 1 stores a byte length and reserved capacity (`a`, `b`) for strings and code slots. UTF-8 records the code point count in its langtype and the byte count in `a`.
- Class 2 stores tensor element count and width. Its langtype carries the dimensions.
- Class 3 stores an external locator, with the target's actual langtype. Pointer values use class 1 with the pointer bit set and a target key in the body.

`ro` and `vid` live at `/.kvspace-meta/<hex key>` as a separate XValue. Writes and copies preserve the sidecar; its absence means `ro=false, vid=0`.

## Tutorial

```bash
python3 tutorial/test.py
```

The shell cases cover links, extensions, directory/value coexistence, type variety, and bulk operations. The codec tests compare the wire format across implementations.
