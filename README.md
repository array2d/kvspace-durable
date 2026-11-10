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
- KV read/write: `kvspaceGet` (borrow), `kvspaceGetPart`, `kvspaceGetHead`, synchronous `kvspaceSetValue`, `kvspaceSetPart`, `kvspaceWriteInPlace`, `kvspaceWriteNewPlace`
- enumerate / delete / copy: `kvspaceListLen`, `kvspaceListAt`, `kvspaceDel`, `kvspaceDelTree`, `kvspaceCp`, `kvspaceCpTree`, `kvspaceCpList`
- directories / extindex: `kvspaceMkindex`, `kvspaceMkindexExt`, `kvspaceRmindexExt`
- watch / clear: `kvspaceWatch`, `kvspaceClear`
- XValue constructors (caller `free()`s; same signatures and bytes as the frontend and `kvspace-c`, `langtype` is written verbatim): `kvspaceNewPtr`, `kvspaceNewChar`, `kvspaceNewBool`, `kvspaceNewInt64`, `kvspaceNewFloat64`
- shm-only views, always `-2` here (callers fall back to the calls above): `kvspaceXvalueBodyView`, `kvspaceView*`

The same ABI is implemented by `kvspace-c` (`shm://`), so a consumer (e.g. the kvlang layout) switches backends by DSN only, with no code change.

## XValue

The wire format is byte-identical across this backend, `kvspace-c`, and the `kvspace` frontend (`kvspace/xvalue_head.h`), and a value is always one key:

```
[bodylen:u64 LE @0][attrslen:u16 LE @8][padding:6 @10-15]
[body: bodylen bytes @16][attr: attrslen bytes]
attr   = record...                                  (no length prefix; attrslen is in the head)
record = [tag:u8][len:u16 LE][data]                 tag 1 = langtype, 2+ reserved
```

- `padding` must be zero; non-zero is rejected.
- The head carries **no semantics**: the langtype (in the attr region) says what the value is — `*T` pointer, `@T` extension, `[d0,d1,…]T` tensor.
- `bodylen` is **always the effective byte count**. Capacity is a physical-allocation attribute and does not appear in the wire; a backend that needs it keeps it in its own allocation metadata.
- An empty langtype means `None` for inline values. Map members and directory children are enumerated from physical key prefixes.

## Tutorial

```bash
python3 tutorial/test.py
```

The shell cases cover links, extensions, directory/value coexistence, type variety, and bulk operations. The codec tests compare the wire format across implementations.
