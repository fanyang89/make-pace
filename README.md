# make-pace

**Resource-aware parallel builds for Linux.**

make-pace adapts build concurrency to available CPU and memory, including cgroup
v1/v2 limits. It coordinates Cargo, GNU Make, and Ninja through a shared FIFO
jobserver—no manual job count required.

- **Adaptive concurrency** — increases parallelism when resources are available and
  reduces it under pressure.
- **Container-aware** — detects visible host and cgroup constraints without root.
- **Works with your build tools** — supports Cargo, Make, Ninja, and CMake builds.

This is experimental admission control, not a hard resource cap or OOM guarantee.
Running jobs are not interrupted when concurrency is reduced.

## Install

Requires **Linux** and **Rust 1.85+**. Build tools must support FIFO jobservers:
Cargo, GNU Make **4.4+**, or Ninja **1.13+**.

```sh
cargo install --git https://github.com/fanyang89/make-pace.git --locked
```

Ensure `~/.cargo/bin` (or your configured Cargo binary directory) is on `PATH`.

From a local checkout:

```sh
cargo install --path . --locked
```

## Quick start

Prefix your build command with `make-pace --`:

```sh
make-pace -- cargo build --release
make-pace -- make
make-pace -- ninja -C build

# Configure CMake separately, then wrap the build.
cmake -S . -B build -G Ninja
make-pace -- cmake --build build
```

Do not set `-j`, `--jobs`, `--parallel`, `CARGO_BUILD_JOBS`, or
`CMAKE_BUILD_PARALLEL_LEVEL`; make-pace manages concurrency for you. An inherited
jobserver is preserved, with adaptive control disabled.

## License

[MIT](LICENSE)
