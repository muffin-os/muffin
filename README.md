# Muffin OS 🧁

[![Rust](https://github.com/muffin-os/muffin/actions/workflows/build.yml/badge.svg)](https://github.com/muffin-os/muffin/actions/workflows/build.yml)

A hobby x86-64 operating system kernel written in Rust, designed to be a general-purpose OS with POSIX.1-2024 compliance as a goal.

## Overview

Muffin OS is a bare-metal operating system kernel that boots using the Limine bootloader and runs on QEMU. The project is structured as a modular workspace with a kernel and userspace components, all written in Rust.

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

Muffin OS is designed to be easy to build with minimal dependencies:

```bash
# System dependencies (xorriso for ISO creation, e2fsprogs for filesystem)
sudo apt update && sudo apt install -y xorriso e2fsprogs

# QEMU for running the OS (optional, only needed to run)
sudo apt install -y qemu-system
```

Rust toolchain is automatically configured via `rust-toolchain.toml` (nightly channel with required components).

### Quick Start

```bash
# Build and run in QEMU
cargo run

# Run without GUI
cargo run -- --headless

# Run with debugging support (GDB on localhost:1234)
cargo run -- --debug

# Customize resources
cargo run -- --smp 4 --mem 512M
```

### Building

```bash
# Build all workspace components
cargo build

# Build in release mode
cargo build --release
```

This creates a bootable ISO image (`muffin.iso`) and ext2 disk image.

### Testing

```bash
# Run tests on workspace crates
cargo test
```

**Note:** The kernel binary itself uses a custom linker script for bare-metal execution and cannot run standard unit tests. Testable functionality is extracted into separate crates (like `kernel_vfs`, `kernel_physical_memory`, etc.) that can be tested on the host.

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on how to build, test, and submit changes.

## License

Muffin OS is dual-licensed under Apache-2.0 OR MIT. See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT) for details.
