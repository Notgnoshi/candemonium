# Testing strategy

## Status

**DRAFT**

## Scope

This document specifies how candumpr (and other tools in this workspace) are tested, given that they
depend on Linux socketcan interfaces that require either real hardware or elevated permissions to
create.

## Problem

candumpr interacts directly with CAN sockets. Testing requires CAN interfaces, but:

* Real CAN hardware is not available in CI.
* Virtual CAN (vcan) interfaces require `CAP_NET_ADMIN` to create.
* vcan interfaces are system-global resources, so parallel tests using shared interfaces cause
  interference.
* Tests must run in CI (GitHub Actions) and locally without requiring root.

## Solution: user + network namespaces

Each test process enters its own isolated Linux network namespace using
`unshare(CLONE_NEWUSER | CLONE_NEWNET)`. Inside the namespace, the process has `CAP_NET_ADMIN`
without real root privileges, vcan interfaces are private and isolated, and everything is cleaned up
when the process exits. See the [vcan-fixture](../../vcan-fixture/) crate for the implementation.

Constraint: `unshare(CLONE_NEWUSER)` requires a single-threaded process. The Rust test harness is
multi-threaded, so namespace entry happens in a `ctor` constructor before `main()`.

## Test tiers

### Unit tests

No sockets, no namespaces. Config parsing, filter compilation, output formatting, filename template
expansion, duration/size parsing.

### Integration tests

Run inside user + network namespaces with vcan interfaces. Socket binding, filter application,
multi-interface capture, file rotation, ZSTD streaming, address claim, device resilience.

### End-to-end tests

Run the actual binary inside a network namespace. Launch candumpr, send frames with cangenr, verify
output files, signal handling, config file loading.

## CI

Tests that require vcan use `#[cfg_attr(feature = "ci", ignore = "requires vcan")]`. In CI,
`--all-features` enables the `ci` feature, making them `#[ignore]`. They are then run as a separate
step gated on whether vcan setup succeeded:

A separate canary job (`vcan-available`) with `continue-on-error: true` shows yellow when the vcan
module is unavailable on the runner, rather than silently skipping the tests.

See [lint.yml](/.github/workflows/lint.yml) for the implementation.

## Benchmarking

Benchmarks compare candumpr against candump on 4 vcan interfaces with J1939 traffic.

### Metrics

* **Frame loss** (primary): frames sent vs. frames in output
* **Throughput ceiling**: send rate at which frames start dropping
* **CPU usage**: total CPU time (user + system)
* **Memory usage**: peak RSS

### Simulating the target environment

The target is a ~4 core ~1 GHz ARM CPU. Use `taskset` to pin benchmarks to 4 cores:

```sh
taskset -c 0-3 cargo bench
```

Core count is the important variable for comparing architecture options (dedicated thread pairs vs.
shared threads). Clock speed matters less for relative comparison. Final validation must happen on
real target hardware.

### Acceptance criteria

candumpr must not drop frames at the realistic J1939 rate (2000 frames/s per interface, 8000
frames/s aggregate). At higher rates, candumpr should drop fewer frames than candump.
