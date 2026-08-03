# Muffin OS 🧁

[![Rust](https://github.com/muffin-os/muffin/actions/workflows/build.yml/badge.svg)](https://github.com/muffin-os/muffin/actions/workflows/build.yml)

A hobby x86-64 operating system kernel written in Rust, designed to be a general-purpose OS with POSIX.1-2024 compliance as a goal.

## Overview

Muffin OS is a bare-metal operating system kernel that boots using the Limine bootloader and runs on QEMU.
The project is structured as a modular Bazel build with a kernel and userspace components, all written in Rust.

## I'm in the fast lane, how do I try this?

1. Install `bazel`
2. Install `xorriso`, `e2fsprogs` and `qemu-system-x86`
3. Run `bazel run //muffinos`

## Key Features

- **Multi-threading support** - Cooperative and preemptive multitasking with process and thread management
- **VirtIO drivers** - Support for VirtIO block devices and GPU with PCI device discovery
- **Virtual filesystem (VFS)** - Abstraction layer with ext2 filesystem support and devfs
- **Memory management** - Physical and virtual memory allocators with custom address space management
- **POSIX system interface** - Eventually POSIX-compatible system interface with support for file operations, threading primitives (pthread), memory management, and more (work in progress)
- **ACPI support** - Power management and hardware discovery via ACPI tables
- **ELF loader** - Dynamic ELF binary loading for userspace programs
- **Userspace foundation** - Init process and minimal C library (minilib) for userspace development
- **Stack unwinding** - Kernel panic backtraces for debugging

## POSIX Compliance

Muffin OS aims for basic POSIX.1-2024 compliance, implementing standard system functions to support portable POSIX-compliant applications. The kernel provides POSIX-compatible interfaces for file operations, process management, threading, and memory management.

## Building and Running

### Prerequisites

`bazel`, `xorriso`, `e2fsprogs` and `qemu-system-x86`.

I would like to use cargo workspaces, but [cargo#10444](https://github.com/rust-lang/cargo/issues/10444) makes that impossible right now.

### Quick Start

```bash
# Build and run in QEMU
bazel run //muffinos

# Run without GUI
bazel run //muffinos -- --headless

# Run with debugging support (GDB on localhost:1234)
bazel run //muffinos -- --debug

# Other options
bazel run //muffinos -- --help
```

The kernel log level is baked into `limine.conf` as a `cmdline` entry rather than
read from the host environment.

### Building

```bash
# Build everything
bazel build //...
```

```
# Build optimized
bazel build -c opt //...
```

`bazel build //muffinos:iso` produces the bootable ISO at
`bazel-bin/muffinos/muffinos.iso`, and `bazel build //muffinos:disk` produces the
ext2 disk image at `bazel-bin/muffinos/disk.img`.

### Testing

```bash
# Host unit tests plus the QEMU integration tests
bazel test //...
```

The integration tests under `//tests` boot the real kernel under QEMU against
Bazel-built images, so they need all of the required components that were mentioned above.

**Note:** The kernel binary itself uses a custom linker script for bare-metal execution and cannot run standard unit tests.
Testable functionality is extracted into separate crates that can be tested on the host.

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on how to build, test, and submit changes.

## License

Muffin OS is dual-licensed under Apache-2.0 OR MIT. See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT) for details.
