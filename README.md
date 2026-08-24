# kvspace-durable

[![CI](https://github.com/array2d/kvspace-durable/actions/workflows/ci.yml/badge.svg)](https://github.com/array2d/kvspace-durable/actions/workflows/ci.yml)

Rust implementation of the **KVSpace** used by kvlang — the filesystem-style key-value store that serves as kvlang's unified addressing and memory space (keys are paths, values are XValues).

This is one of two standard implementations of the KVSpace contract; the other is [kvspace-c](../kvspace-c). Both expose the same C ABI and the same XValue kindexpr format, so a consumer (the kvlang layout/runtime) switches between them by DSN only.

Backends: `redis://` (default when no scheme is given), `fs://` — selected by DSN scheme in `conn("redis://127.0.0.1:6379")` / `conn("fs:///tmp/kvspace")`.

## Build

```bash
make build       # cargo build --release --bin kvspace → ~/.local/bin/kvspace (CLI)
make test        # build + tutorial/test.py
```

Crate types: `rlib`, `staticlib`, `cdylib` (`libkvspace_durable.so`).

## ABI

C ABI exported from the cdylib (`src/ffi.rs`):

- lifecycle: `kvspaceConnect`, `kvspaceFree`, `kvspaceBytesFree`, `kvspaceDisconnect`
- KV ops: `kvspaceGet`, `kvspaceGetBatch`, `kvspaceSet`, `kvspaceList`, `kvspaceDel`, `kvspaceDelTree`
- directories / extindex: `kvspaceMkindex`, `kvspaceMkindexExt`, `kvspaceRmindexExt`
- watch / clear: `kvspaceWatch`, `kvspaceClear`
- XValue codec: `kvspaceTlvEncode`, `kvspaceTlvEncodePtr`, `kvspaceTlvEncodeMode`, `kvspaceDecodeHead`, `kvspaceNewPtr`, `kvspaceNewChar`, `kvspaceNewCharByte`, `kvspaceNewBool`, `kvspaceNewInt64`, `kvspaceNewFloat64`

The same ABI is implemented by `kvspace-c` (`shm://`), so a consumer (e.g. the kvlang layout) switches backends by DSN only, with no code change.

## XValue

kindexpr TLV head, byte-identical to `kvspace-c`:

```
[1B kindexprlen][kindexpr + 0x00 pad][1B ro][4B vid LE][4B raw_len LE][raw]
```

- kindexpr first byte: `*` = soft link (raw = target path), `@` = ext handle, otherwise inline.
- `[d0,d1]kind` carries ndim+dims; bare `kind` is a scalar; `char/*` is always a 1-D sequence (`[n]`).
- `None` is encoded as NULL / length 0.

Kinds: `bool`, `int8..int64`, `uint8..uint64`, `float32/64`, `char/utf32|utf8|ascii`, `objindex`, `strkeymapindex`, `index`, `extindex`, `rwir`, `rwfunc`, `defrwir`, `defrwfunc`, `scope`, `time`, `duration`.

## Tutorial

```bash
python3 tutorial/test.py
```

Fourteen shell cases (`01-basic.sh` … `14-head-perm.sh`) covering link, extindex, unlink, dir/value coexistence, type variety, edge cases, bulk ops, and head permission. The harness also cross-validates that `kvspace-c` and `kvspace-durable` produce byte-identical head encoding (ro/vid).
