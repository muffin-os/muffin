"""Single source of truth for the kernel subsystem crate graph.

Package directory and Rust crate name deliberately differ: //kernel/devfs
builds crate `kernel_devfs`, because that is what the sources refer to each
other by. Renaming a directory must not change a crate name.
"""

load("@rules_rust//rust:defs.bzl", "rust_doc_test", "rust_library", "rust_test")

# crate_universe exposes registry packages under their Cargo package name.
# Everything below keys off Cargo package names because the Miri shim in
# //tools/miri writes them into real manifests.

def crate_label(package):
    """Returns the @crates label for a Cargo package name."""
    return "@crates//" + package

# `deps` are sibling kernel crates named by their package name, `crates` are
# external packages in @crates.
KERNEL_CRATES = {
    "abi": struct(deps = [], crates = ["bitflags"]),
    "devfs": struct(
        deps = ["abi", "device", "vfs"],
        crates = ["spin", "thiserror"],
    ),
    "device": struct(deps = [], crates = ["spin", "thiserror", "x86_64"]),
    "elfloader": struct(deps = [], crates = ["thiserror", "zerocopy"]),
    "ext2": struct(deps = ["device"], crates = ["bitflags"]),
    "log": struct(deps = [], crates = ["conquer-once", "spin", "tracing"]),
    "memapi": struct(deps = [], crates = ["x86_64"]),
    "park": struct(deps = [], crates = ["thiserror"]),
    "pci": struct(deps = ["memapi"], crates = ["spin", "thiserror", "x86_64"]),
    "physical_memory": struct(deps = [], crates = ["thiserror", "x86_64"]),
    "syscall": struct(
        deps = ["abi", "vfs"],
        crates = ["spin", "thiserror", "tracing", "x86_64"],
    ),
    "vfs": struct(deps = ["abi"], crates = ["spin", "thiserror"]),
    "virtual_memory": struct(deps = [], crates = ["thiserror", "tracing", "x86_64"]),
}

def kernel_crate_name(name):
    """Returns the Rust crate name for a kernel subsystem package name."""
    return "kernel_" + name

def kernel_crate_label(name):
    """Returns the Bazel label for a kernel subsystem package name."""
    return "//kernel/" + name

def kernel_crate(name):
    """Declares one kernel subsystem crate, its host tests, and its sources.

    The sources filegroup exists for //tools/miri, which cannot glob across a
    package boundary.
    """
    spec = KERNEL_CRATES[name]
    srcs = native.glob(["src/**/*.rs"])

    rust_library(
        name = name,
        srcs = srcs,
        crate_name = kernel_crate_name(name),
        crate_root = "src/lib.rs",
        edition = "2024",
        deps = [kernel_crate_label(d) for d in spec.deps] +
               [crate_label(c) for c in spec.crates],
    )

    # Pure host-side unit and doc tests, no emulator and no I/O. They finish in
    # milliseconds, so the default medium size just reserves resources nothing
    # needs and trips Bazel's timeout-range warning.
    rust_test(
        name = "test",
        crate = ":" + name,
        size = "small",
    )

    rust_doc_test(
        name = "doc_test",
        crate = ":" + name,
        size = "small",
    )

    native.filegroup(
        name = "srcs",
        srcs = srcs,
    )

def kernel_crate_closure(name):
    """Returns `name` plus every kernel crate it transitively depends on.

    The Miri shim needs this because Cargo resolves a whole workspace, so every
    path dependency of the crate under test has to exist on disk.
    """
    closure = [name]
    frontier = list(KERNEL_CRATES[name].deps)

    # Starlark forbids recursion. The graph is a DAG, so one pass per node is
    # an upper bound on its depth.
    for _ in range(len(KERNEL_CRATES)):
        if not frontier:
            break
        next_frontier = []
        for crate in frontier:
            if crate not in closure:
                closure.append(crate)
                next_frontier.extend(KERNEL_CRATES[crate].deps)
        frontier = next_frontier

    if frontier:
        fail("kernel crate graph is deeper than the number of crates, so it has a cycle")

    return sorted(closure)

# The `[workspace.dependencies]` table the Miri shim writes into its synthesized
# Cargo workspace. Versions and feature lists must stay in sync with the
# crate.spec tags in MODULE.bazel. Nothing checks that automatically, and a
# divergence means Miri validates different code than Bazel builds.
MIRI_WORKSPACE_DEPENDENCIES = """\
acpi = "5.2"
addr2line = { version = "0.26", default-features = false, features = [
  "fallible-iterator",
  "rustc-demangle",
] }
bitfield = "0.19"
bitflags = "2.11"
conquer-once = { version = "0.4", default-features = false }
cordyceps = { version = "0.3", default-features = false, features = ["alloc"] }
raw-cpuid = "11"
elf = { version = "0.7", default-features = false, features = ["nightly"] }
itertools = { version = "0.14.0", default-features = false, features = [
  "use_alloc",
] }
jiff = { version = "0.2", default-features = false, features = ["alloc"] }
limine = "0.5"
linked_list_allocator = "0.10"
linkme = "0.3"
rustc-demangle = "0.1"
sha3 = { version = "0.11.0-rc.8", default-features = false }
spin = "0.10"
thiserror = { version = "2.0", default-features = false }
tracing = { version = "0.1", default-features = false, features = ["attributes"] }
uart_16550 = "0.4"
unwinding = { version = "0.2.10", default-features = false, features = [
  "dwarf-expr",
  "fde-custom",
  "hide-trace",
  "panic",
  "personality",
  "unwinder",
] }
virtio-drivers = "0.13"
volatile = { version = "0.6", features = ["derive"] }
x2apic = "0.5"
x86_64 = "0.15"
zerocopy = { version = "0.9.0-alpha.0", features = ["alloc", "derive"] }
"""
