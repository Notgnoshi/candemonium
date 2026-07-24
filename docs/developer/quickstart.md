# Developer quickstart

## MSRV

This is a Cargo virtual workspace project. All crates are versioned and released together. The MSRV
is Rust 1.89.

The minimum supported target environment is Linux 6.1+ with a 4-core ~1Ghz ARM CPU with ~1GB of
memory. Many of the design choices reflect the constraints of this environment.

## Build, and lint

Building and testing is the usual:

```sh
cargo build
cargo clippy --all-targets
```

This project uses custom rustfmt options that make resolving merge conflicts on module imports much
easier to resolve:

```sh
cargo fmt -- --config group_imports=StdExternalCrate,imports_granularity=Module
```

There are examples you can run with `cargo run --example=dump`. You likely need to create at least
one vcan network on your development host:

```sh
sudo ip link add dev can0 type vcan
sudo ip link set up can0
```

## Test dependencies

Just building the project doesn't have any dependencies other than Cargo and a C toolchain. But
testing this project requires the `vcan` kernel module, and the ability to enter a user namespace.

### Ubuntu 24.04

```sh
sudo apt-get install -y linux-modules-extra-"$(uname -r)"
sudo modprobe vcan
sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0
```

### Ubuntu 26.04

The `vcan` kernel module has been merged back into `linux-modules`, so there's no extra packages to
install. But in order to use the [vcan-fixture](/vcan-fixture/src/lib.rs) crate, you still need to

```sh
sudo modprobe vcan
sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0
```

### Fedora 44

```sh
sudo modprobe vcan
```

## Tests

Tests may be run either with `cargo test` or <https://nexte.st>:

```sh
cargo test
cargo nextest run
```

### Test fixtures

There are test fixtures provided by the [vcan-fixture](/vcan-fixture/src/lib.rs) crate. This
provides several features:

* `enter_namespace()` - enter a process namespace that allows creating vcan networks, which would
  otherwise require additional permissions outside the namespace.
* `VcanHarness::new(num)` - create a number of unique vcan interfaces - this is thread safe, and is
  intended for use in tests.
* `bench::getrusage_thread()` and `getrusage_self()` - get resource usages for the current thread or
  process. This measures user and system time, as well as context switches. Other resources could be
  added in the future.
* `bench::pin_to_cores(n)` - pin the current process to the first `n` CPU cores
* `bench::start_cpu_load(num, percent)` - starts `num` threads doing a PWM-like busyloop to hit
  `percent` CPU usage

In CI, we attempt to install the vcan module, but can skip the vcan-dependent tests with a warning
if it's not available.

### ASAN

As this project uses quite a bit of `unsafe` Rust to interact with `libc`, it's important to run
with ASAN. You can do this with:

```sh
# tests
RUSTFLAGS="$RUSTFLAGS -Zsanitizer=address" cargo +nightly nextest run -Zbuild-std --target x86_64-unknown-linux-gnu
# example
RUSTFLAGS="$RUSTFLAGS -Zsanitizer=address" cargo +nightly run -Zbuild-std --target x86_64-unknown-linux-gnu --example=dump
```

## Benchmarks

This project includes several benchmarks. Some of them depend on
[gungraun](https://gungraun.github.io/gungraun/latest/html/index.html):

```sh
cargo install gungraun-runner
cargo bench
```

## Release process

This project isn't released to <https://crates.io>, but there is still a GitHub release workflow.
Here's the release checklist:

* [ ] Use SemVer to pick an appropriate version number
* [ ] Edit the workspace [Cargo.toml](/Cargo.toml)'s `workspace.package.version`
* [ ] Ensure the [CHANGELOG.md](/CHANGELOG.md) has a heading for the new version
* [ ] Check the changelog entry. Did anything get forgotten? Is it formatted well? Spelling,
      phrasing, grammar, etc.
* [ ] Merge a PR including the Cargo.toml and CHANGELOG.md changes.
  * [ ] A Git tag will be generated
  * [ ] The contents of the CHANGELOG.md will be used to create a GitHub release
