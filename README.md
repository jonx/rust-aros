# rust-aros — the Rust standard library, ported to AROS

This repo is a **vendored `rust-src`** (the Rust standard library source) with
an **AROS platform layer** added. AROS here means *hosted* AROS — the
open-source AmigaOS re-implementation running as a normal process on
Apple-Silicon macOS (`darwin-aarch64`).

## What this is (and is *not*)

**It is:** the `library/` tree from a stock nightly Rust (std, core, alloc,
and the sysroot-support crates) plus the small `src/llvm-project/libunwind`
that std references, snapshotted from **`nightly-2026-06-27` (rustc
`ce9954c0c`)** — see the first commit. On top of that snapshot, **13
platform-abstraction-layer (pal) modules** implement std's OS surface for
AROS over `posixc` and the Amiga libraries.

**It is *not* a rustc compiler fork.** There is no `compiler/` directory and
no codegen changes. That is the whole point of the design: **AROS needs zero
changes to the Rust compiler.** Support lives entirely in (1) this std pal
and (2) a target-spec JSON.

> So if you're asking "is this the real rust-aros, or just the std lib?" —
> the std lib *is* rust-aros. There is no bigger/fuller checkout. This plus a
> JSON file plus a stock nightly is the complete toolchain.

## The three pieces of "Rust on AROS"

Building a Rust program for AROS uses three things, and **only this one is a
git repo you edit**:

| # | Piece | Where | Role |
|---|-------|-------|------|
| 1 | **This repo** (`rust-aros`) | `~/Source/rust-aros` | The std pal — the OS-specific half of the standard library. |
| 2 | **Target spec JSON** | `aros-aarch64/hosted/rust/aarch64-unknown-aros.json` | Defines the `aarch64-unknown-aros` target (aarch64 ELF, `mcmodel=large`, x18 reserved, static, `panic=abort`). |
| 3 | **Stock nightly rustc** | `rustup … nightly-2026-06-27` | Unmodified. Does all codegen. |

The target is built as a **custom-JSON `-Zbuild-std` target**: rustc compiles
*this repo's* std (wired in as the toolchain's `rust-src`) for the JSON
target. No rustc source is touched.

## How it's wired into the toolchain

`-Zbuild-std` compiles whatever std sits at the nightly's `rust-src`
component. Point that at this repo with a symlink:

```sh
rustup component add rust-src --toolchain nightly-2026-06-27   # once
SYSROOT=$(rustc +nightly-2026-06-27 --print sysroot)
cd "$SYSROOT/lib/rustlib/src"
mv rust rust.orig                 # keep the stock copy
ln -s ~/Source/rust-aros rust     # build-std now uses this
```

Then any AROS build gets this std:

```sh
cargo +nightly-2026-06-27 build \
  --target /path/to/aarch64-unknown-aros.json \
  -Zjson-target-spec -Zbuild-std=std,panic_abort
```

> **Gotcha:** cargo caches the built std. After editing a pal module, if your
> change doesn't take, nuke the cached artifacts:
> `rm -rf target/aarch64-unknown-aros/*/​.fingerprint/std-* target/aarch64-unknown-aros/*/deps/libstd-*`.

## The AROS platform layer

All AROS-specific code is under `library/std/src/sys/*/aros.rs`. Each module
calls into `posixc` / an Amiga library — most via a thin C glue
(`aros_*` externs) that lives in **`aros-aarch64/hosted/rust/*.c`** and is
linked at final-link time by `collect-aros`, not here.

| Module | Backend | Status |
|--------|---------|--------|
| `sys/alloc` | posixc `malloc` / `posix_memalign` | ✅ |
| `sys/args` | reads the C harness `aros_argc`/`aros_argv` globals | ✅ |
| `sys/env` | posixc `getenv`/`setenv`/`unsetenv` | ✅ |
| `sys/fs` | posixc `open`/`read`/`write`/`lseek`/`close`/`unlink`/`mkdir`/`rmdir`; `stat`/`lstat`/`fstat`; `opendir`/`readdir`/`closedir`; `getcwd`/`chdir`/temp_dir; unix→AROS path translation | ✅ |
| `sys/io/error` | real errno via `__stdc_geterrnoptr()` + `strerror`, NetBSD `ErrorKind` map | ✅ |
| `sys/net` | `TcpStream`/`TcpListener`/`UdpSocket` over `bsdsocket.library` LVOs | ✅ |
| `sys/paths` | `$HOME` / posixc paths | ✅ |
| `sys/process` | `Command::output`/`status` via dos `SystemTagList` | ✅ |
| `sys/random` | posixc `arc4random_buf` (host-backed CSPRNG on the hosted port) | ✅ |
| `sys/stdio` | fd 0/1/2 via posixc `read`/`write` → dos | ✅ |
| `sys/thread` | `pthread.library` (`aros_thr_*` glue) + full sync core (Mutex/Condvar/RwLock/Parker) | ✅ |
| `sys/thread_local/key` | pthread-key TLS | ✅ |
| `sys/time` | posixc clocks | ✅ |

`build.rs` lists `aros` as a known target so std is **not** `restricted_std`
— i.e. the full std builds and links, not a stub.

The proof these work end-to-end lives in the **`aros-aarch64`** project:
`graft/rust-smoke` (no_std + alloc) and `graft/bench-run C:RustStd` (a std
probe exercising Vec/HashMap/fs/env/threads/time/process/random on booted
AROS). Both pass.

## Rebasing onto a newer nightly

This is a *snapshot*, not a live fork, so moving to a newer Rust means
re-vendoring:

1. Pick the new nightly; note its date + rustc hash.
2. Replace `library/` + `src/llvm-project` with that nightly's `rust-src`.
3. Re-apply the 13 `sys/*/aros.rs` modules and the `build.rs` known-target
   line (the pal is self-contained; upstream churn is usually in the `sys`
   module *dispatch* — `sys/*/mod.rs` `cfg_if!` arms — which is a small
   re-thread).
4. Update the target JSON + `.cargo/config` in the consuming projects if the
   nightly changed unstable-flag spellings (e.g. `-Zjson-target-spec` was a
   requirement added around this era).

## Relationship to the rest of the port

This is one of five checkouts. The full map, the from-scratch build guide,
and the troubleshooting ledger are in **Feraille**'s
`docs/features/aros-building.md` (branch `aros-port`). In short:

- **Feraille** (the app being ported) → **gpui** (a zed fork, `zed-aros`,
  with the `gpui_aros` CPU backend) → this **std** → **AROS**.
- The C glue the pal calls, the target JSON, and the run harness all live in
  **`aros-aarch64`**.

## License

Inherited from upstream Rust: dual **MIT OR Apache-2.0** (see the `LICENSE-*`
files carried in `library/`). The AROS pal modules are contributed under the
same terms.
