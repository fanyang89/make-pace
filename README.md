# make-pace

An experimental Linux resource-aware build wrapper. It detects host and visible
cgroup v1/v2 constraints and adjusts a GNU-compatible FIFO jobserver while a build
runs. No CPU or memory budget needs to be supplied.

## Usage

```sh
cargo build --release
./target/release/make-pace -- cargo build --release
./target/release/make-pace -- make
cmake -S . -B build -G Ninja
./target/release/make-pace -- cmake --build build
```

Requires Linux, Cargo with FIFO jobserver support, GNU Make **4.4+**, or Ninja
**1.13+**. CMake Ninja, Ninja Multi-Config and Unix Makefiles generators are
supported. CMake configuration is performed separately. Nested build tools must
also support and propagate the jobserver protocol.

Do not specify `-j`, `--jobs`, `--parallel`, `CARGO_BUILD_JOBS`, or
`CMAKE_BUILD_PARALLEL_LEVEL`. Avoid parallelism overrides in tool configuration or
nested commands as well. Tools that ignore the protocol cannot be controlled.

## Resource discovery

- CPU: online CPUs, process affinity, cpuset and visible ancestor CPU quotas,
  including fractional quotas.
- Memory: host `MemAvailable`, cgroup limit minus usage at every visible ancestor,
  and v2 `memory.high` as an additional operational boundary. v1 unlimited values
  and v2 `max` are recognized. Shared parent usage includes competing processes.
- `/proc/self/cgroup` and `/proc/self/mountinfo` locate separate v1 controllers,
  unified v2 controllers, or hybrid layouts. Limits are re-read every second.
- Memory PSI is optional. Without it, memory headroom and its trend remain active.
- No root access, cgroup creation or changes to host limits are required.

Only mounted, readable constraints can be observed. Limits hidden by a cgroup
namespace cannot be discovered. Missing optional accounting/PSI files degrade
monitoring; unreadable usage for a known finite memory limit prevents expansion.
The wrapper uses conservative raw cgroup usage, without assuming page cache is
immediately reclaimable. Swap is not counted as extra build capacity.

## Control policy

The first build owns one implicit job slot; all additional slots use FIFO tokens.
The controller samples every second, starts at one job, and adds one job after
three healthy samples. CPU saturation stops expansion. Memory pressure, estimated
headroom exhaustion within five seconds, or CPU throttling triggers multiplicative
reduction (25%) with a cooldown. A smoothed memory growth rate and safety reserve
reduce overshoot. Exploration is bounded at twice the effective CPU capacity
(rounded up), capped at 4096 jobs; this is a heuristic, not a resource guarantee.

Contraction drains available/returned tokens every 50 ms and **does not revoke
running work**. The implementation owns a private FIFO instead of relying on an
anonymous-pipe jobserver, since Ninja on POSIX requires FIFO authentication.

An inherited parent jobserver is preserved unchanged: adaptive control is disabled
in this mode rather than minting tokens beyond the parent's allocation. The parent
must provide a valid, compatible jobserver. Exit status is propagated; INT, TERM
and HUP are forwarded to the build process group, with a five-second kill deadline.

## Limitations

- This is admission control, not an OOM prevention guarantee or hard resource cap.
  A single compiler/linker can exhaust memory, and running jobs may grow suddenly.
- One token does not equal one OS thread. Internally threaded tools can oversubscribe.
- CPU accounting is approximate: host utilization, leaf cgroup usage when available,
  and ancestor throttling do not provide exact per-build utilization. Multiple
  independent wrappers may compete; shared jobserver coordination is preferable.
- Starting at one job favors safety and can slow short builds. The default gains
  are initial heuristics, not benchmark-tuned parameters. Speedups are not established.
- SIGKILL cannot be cleaned up; it can leave temporary FIFO directories or children.
- Restrictive ancestors outside the visible namespace, abnormal token loss, and
  tools that override/ignore jobserver settings remain limitations.

## Validation

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build
python3 tests/integration.py
```

Unit tests cover resource parsing, visible ancestor traversal, v1/v2 memory and CPU
constraints, control behavior, and in-flight token contraction. Integration tests
use real Cargo, recursive Make, Ninja and CMake builds, exercise inherited
jobservers, explicit-parallelism rejection, exit codes and signal forwarding.
Real cgroup v1 and constrained-container tests require an appropriate external
runtime; fixture coverage is not a substitute for those tests. Benchmark clean and
incremental builds against fixed `-j` settings before using this in production.
