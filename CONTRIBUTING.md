# Contributing to Muffin OS

Welcome to Muffin OS! This guide will help you get started with contributing to this hobby x86-64 operating system kernel written in Rust.

## Project Overview

**Muffin OS** is a bare-metal operating system kernel that boots using the Limine bootloader and runs on QEMU. The project consists of ~109 Rust source files organized into a kernel and userspace components.

- **Language:** Rust (Nightly)
- **Target:** x86_64-unknown-none
- **Bootloader:** Limine v9.x
- **Build System:** Cargo with custom build scripts

## Architecture

The project uses a modular workspace structure:

```
├── kernel/                      # Main kernel crate (bare-metal)
│   ├── crates/                 # Kernel subsystem crates (testable on host)
│   │   └── kernel_*            #   - VFS, memory management, device drivers, etc.
│   ├── src/                    # Kernel source code
│   └── linker-x86_64.ld        # Custom linker script
├── userspace/                  # User-space components
├── src/main.rs                 # QEMU runner
└── build.rs                    # Build orchestration
```

### Testability Philosophy

**The kernel crate itself cannot have standard Rust unit tests** because it uses a custom linker script for bare-metal targets. To maintain testability, we extract as much functionality as possible into separate crates (like `kernel_vfs`, `kernel_physical_memory`, etc.) which can be unit tested on the host system. When adding new kernel functionality, consider whether it can be implemented as a separate crate that can be tested independently.

## Prerequisites

### Required Tools

Assumes Rust is installed through [rustup](https://rustup.rs/). The project uses Rust nightly with required components configured in `rust-toolchain.toml`, which will be automatically set up by rustup.

```bash
# Install system dependencies for ISO creation and running the OS
sudo apt update && sudo apt install -y xorriso qemu-system
```

### Optional Tools

- **GDB or LLDB:** For debugging with `--debug` flag

## Building

### Quick Build

To build the project:

```bash
# Build all workspace components
cargo build

# Build in release mode
cargo build --release
```

### Full System Build

To build the complete bootable ISO:

```bash
# Requires xorriso to be installed
cargo build --release
```

This creates:
- Kernel binary
- Bootable ISO image (`target/release/build/**/out/muffin.iso`)
- Disk image (`disk.img`)

The build process automatically:
1. Clones the Limine bootloader (cached after first build)
2. Downloads OVMF firmware for UEFI support
3. Compiles the kernel for bare-metal x86-64
4. Creates a bootable ISO with xorriso
5. Builds an ext2 filesystem image

## Testing

### Running Tests

Due to the bare-metal nature of the kernel, testing is done at the crate level:

```bash
# Run all tests (automatically tests only testable crates)
cargo test

# Test individual crates
cargo test -p kernel_abi
cargo test -p kernel_vfs
```

**Note:** The kernel binary itself cannot be tested with standard unit tests. Many crates may have no tests yet (0 tests is normal).

### Miri Tests (Undefined Behavior Detection)

Miri is used to detect undefined behavior in unsafe code:

```bash
# Setup Miri (first time only)
cargo miri setup

# Run Miri on specific crates
cargo miri test -p kernel_abi
cargo miri test -p kernel_vfs
```

## Development Workflow

### Code Quality Standards

The project uses rustfmt with custom configuration (`rustfmt.toml`) and enforces all clippy warnings as errors in CI.

### Before Submitting a PR

Run these commands in order to validate your changes:

```bash
# 1. Format check (fastest)
cargo fmt -- --check

# 2. Lint check
cargo clippy -- -D clippy::all

# 3. Build check
cargo build

# 4. Test
cargo test

# 5. (Optional) Miri tests if you changed kernel crates
cargo miri setup
cargo miri test -p <modified_crate>

# 6. (Optional) Full build
cargo build --release
```

### CI Pipeline

GitHub Actions runs on every push with these jobs:

1. **Lint:** Checks formatting and runs clippy with `-D clippy::all`
2. **Test:** Runs tests in both debug and release modes
3. **Miri:** Tests each kernel crate with Miri for undefined behavior
4. **Build:** Creates the bootable ISO and uploads artifacts

The CI also runs twice daily on a schedule.

## Running the OS

To build and run Muffin OS in QEMU:

```bash
# Run with default settings
cargo run

# Run without GUI
cargo run -- --headless

# Run with GDB debugging (connects on localhost:1234)
cargo run -- --debug

# Customize CPU cores and memory
cargo run -- --smp 4 --mem 512M

# Build ISO without running
cargo run -- --no-run
```

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

- Use incremental builds (default) for faster iteration
- First build takes longer due to downloading dependencies
- Subsequent builds are much faster

---

Thank you for contributing to Muffin OS! 🧁
