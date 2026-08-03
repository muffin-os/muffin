# Contributing to Muffin OS

Welcome to Muffin OS! This guide will help you get started with contributing to this hobby x86-64 operating system kernel written in Rust.

## Project Overview

**Muffin OS** is a bare-metal operating system kernel that boots using the Limine bootloader and runs on QEMU. The project is organized into kernel and userspace components.

- **Language:** Rust (Nightly)
- **Target:** x86_64-unknown-none
- **Bootloader:** Limine v12.5.2
- **Build System:** Bazel (bzlmod)

## Architecture

The project uses a modular Bazel build:

```
├── MODULE.bazel                 # External deps, Rust toolchain pin, crate specs
├── muffinos/                    # //muffinos runner, :iso, :disk
├── kernel/
│   ├── boot/                   # //kernel/boot, the bootable ELF + kernel lib
│   │   └── linker-x86_64.ld    # Custom linker script
│   └── <subsystem>/            # //kernel/vfs, //kernel/abi, ... crate kernel_<name>
├── userspace/                  # init, fbdemo, minilib, libs/gfx
├── tests/                      # QEMU integration tests, test-kernel, test bins
├── platforms/                  # The x86_64-unknown-none target platform
├── bazel/                      # Crate graph, image rules, external BUILD files
└── tools/miri/                 # Miri shim over a synthesized Cargo workspace
```

### Testability Philosophy

**The kernel crate itself cannot have standard Rust unit tests** because it uses a custom linker script for bare-metal targets. To maintain testability, we extract as much functionality as possible into separate crates (like `kernel_vfs`, `kernel_physical_memory`, etc.) which can be unit tested on the host system. When adding new kernel functionality, consider whether it can be implemented as a separate crate that can be tested independently.

## Prerequisites

Obviously, if you know better, do better. For example, there is no need to use bazelisk.
This is just to get someone started that maybe hasn't worked with bazel or other components yet.

### Required Tools

Install Bazel through [Bazelisk](https://github.com/bazelbuild/bazelisk) so the
version in `.bazelversion` is honored. Bazel downloads the Rust toolchain itself
from the pin in `MODULE.bazel`, so a `rustup` install is not needed to build or
test.

The toolchain is the only thing Bazel provides for you. `xorriso` and
`e2fsprogs` are still host prerequisites, because `//muffinos:iso` and every
`ext2_image` target shell out to `xorriso` and `mke2fs`. `qemu-system-x86` is
needed to run the OS and to run the `//tests` integration tests.

```bash
sudo apt update && sudo apt install -y bazelisk xorriso e2fsprogs qemu-system-x86
```

`rustup` with the `miri` component is required only for the Miri targets, which
drive Cargo rather than Bazel.

### Optional Tools

- **GDB or LLDB:** For debugging with `--debug` flag

## Building

### Quick Build

To build the project:

```bash
# Build everything
bazel build //...

# Build optimized
bazel build -c opt //...
```

### Full System Build

To build the complete bootable ISO:

```bash
# Requires xorriso and e2fsprogs to be installed
bazel build -c opt //muffinos:iso //muffinos:disk
```

This creates:
- Kernel binary (`bazel build //kernel/boot`)
- Bootable ISO image (`bazel-bin/muffinos/muffinos.iso`)
- Disk image (`bazel-bin/muffinos/disk.img`)

The build process automatically:
1. Fetches the pinned Limine bootloader release and builds its `limine` tool
2. Fetches the pinned OVMF firmware for UEFI support
3. Compiles the kernel for bare-metal x86-64
4. Creates a bootable ISO with xorriso
5. Builds an ext2 filesystem image with mke2fs

### Updating External Crates

External Rust dependencies are declared as `crate.spec` tags in `MODULE.bazel`.
After editing them, repin the lock files:

```bash
CARGO_BAZEL_REPIN=1 bazel mod deps
```

Commit the resulting `bazel/Cargo.Bazel.lock` and `bazel/cargo-bazel-lock.json`.

### IDE Support

```bash
bazel run @rules_rust//tools/rust_analyzer:gen_rust_project
```

This writes a gitignored `rust-project.json` that rust-analyzer picks up.

## Testing

### Running Tests

```bash
# Host unit tests, doc tests, and the QEMU integration tests
bazel test //...

# Test one package, no need to know its target names
bazel test //kernel/vfs/...
```

The integration tests under `//tests` boot the real kernel under QEMU against
Bazel-built images. Many crates may have no tests yet (0 tests is normal).

**Note:** The kernel binary itself cannot be tested with standard unit tests.

### Miri Tests (Undefined Behavior Detection)

Miri is used to detect undefined behavior in unsafe code. These targets are
tagged `manual`, so `bazel test //...` skips them and they must be named
explicitly. Each one synthesizes a throwaway Cargo workspace and runs
`cargo miri test` against it, so it needs `rustup` with the `miri` component.

```bash
bazel test --config=miri //tools/miri:kernel_abi
bazel test --config=miri //tools/miri:kernel_vfs
```

## Development Workflow

### Code Quality Standards

The project uses rustfmt with custom configuration (`rustfmt.toml`) and enforces all clippy warnings as errors in CI.

### Before Submitting a PR

Run these commands in order to validate your changes:

```bash
# 1. Format check (fastest)
bazel build --config=rustfmt //...

# 2. Apply formatting if the check failed
bazel run @rules_rust//tools/rustfmt

# 3. Lint check
bazel build --config=clippy //...

# 4. Build and test
bazel test //...

# 5. (Optional) Miri tests if you changed kernel crates
bazel test --config=miri //tools/miri:vfs

# 6. (Optional) Optimized build
bazel test -c opt //...
```

### CI Pipeline

GitHub Actions runs on every push with these jobs:

1. **Lint:** Runs the rustfmt and clippy aspects with `-D clippy::all`
2. **Test:** Runs `bazel test //...` in both `-c dbg` and `-c opt`
3. **Miri:** Tests each kernel crate with Miri for undefined behavior
4. **Build:** Creates the bootable ISO and uploads artifacts

The CI also runs twice daily on a schedule.

## Running the OS

To build and run Muffin OS in QEMU:

```bash
# Run with default settings
bazel run //muffinos

# Run without GUI
bazel run //muffinos -- --headless

# Run with GDB debugging (connects on localhost:1234)
bazel run //muffinos -- --debug

# Customize CPU cores and memory
bazel run //muffinos -- --smp 4 --mem 512M

# Build the images without booting
bazel run //muffinos -- --no-run
```

`bazel run` boots QEMU in the foreground and does not exit on its own, since the
kernel idles once boot completes. Stop it with Ctrl-C. It is a manual smoke test,
not an automated one. Automated coverage lives in `//tests`.

## Project Guidelines

### Code Style

- Follow Rust naming conventions and idioms
- Keep functions focused and modular
- Document public APIs with doc comments
- Use descriptive variable names
- Prefer safe Rust; justify all `unsafe` blocks with safety comments

### Commit Messages

- Use clear, descriptive commit messages
- Start with a verb in present tense (e.g., "Add", "Fix", "Update")
- Reference issue numbers when applicable

### Pull Requests

- Keep PRs focused on a single feature or fix
- Update documentation for user-facing changes
- Ensure all CI checks pass
- Add tests when adding testable functionality to crates

## License

Muffin OS is dual-licensed under Apache-2.0 OR MIT. All contributions must be compatible with this licensing.

## Getting Help

- Check existing issues for similar problems
- Review the CI logs for detailed error messages
- Ask questions in issue discussions

## Additional Notes

### Known Limitations

- The kernel binary uses a custom linker script and cannot run standard Rust tests

### Performance Tips

- Bazel caches actions across builds, so iteration after the first build is fast
- The first build downloads the Rust toolchain, Limine, OVMF, and every crate

---

Thank you for contributing to Muffin OS! 🧁
