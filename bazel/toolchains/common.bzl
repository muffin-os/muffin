"""Shared config for the muffin/bootstrap `rust_toolchain` set.

rustc's JSON custom-target crate metadata check keys the target's identity on
the basename of the `--target=<path>.json` argument, not its content or
directory. Verified against nightly-2026-07-12. Two files with identical
content but different basenames make rustc report `E0461: couldn't find crate
... with expected target triple <other-basename>`. rules_rust always names
that generated file `<rust_toolchain rule name>.target.json`
(rust/private/toolchain.bzl), so each toolchain lives in its own package but
all are named `impl`, giving them the same generated basename `impl.target.json`
and therefore the same rustc target identity. `muffin_toolchain` enforces that
rule name; the only per-layer difference is `rust_std`.
"""

load("@rules_rust//rust:toolchain.bzl", "rust_toolchain")
load("//support:versions.bzl", "RUST_NIGHTLY_DATE")
load(":spec.bzl", "X86_64_UNKNOWN_MUFFIN")

TOOLS = "@rust_linux_x86_64__x86_64-unknown-none__nightly_tools"

TARGET_JSON = json.encode(X86_64_UNKNOWN_MUFFIN)

# extra_rustc_flags_triples in MODULE.bazel keys on the triple STRING
# "x86_64-unknown-none", so none of those flags fire for a JSON target. They are
# restated here. The panic strategy and -Crelocation-model=static live in the
# spec itself so bootstrap and real toolchain cannot disagree.
EXTRA_RUSTC_FLAGS = [
    # rustc destabilised JSON target specs in rust-lang/rust#150151. Loading one
    # now requires -Zunstable-options.
    "-Zunstable-options",
    "-Clink-arg=-z",
    "-Clink-arg=nostart-stop-gc",
    "-Cforce-frame-pointers=yes",
    "-Cforce-unwind-tables=yes",
    "-Cdebuginfo=2",
]

def muffin_toolchain(rust_std):
    """Declares the `impl` rust_toolchain of a package with the given `rust_std`."""
    rust_toolchain(
        name = "impl",
        binary_ext = "",
        channel = "nightly",
        clippy_driver = TOOLS + "//:clippy_driver_bin",
        default_edition = "2024",
        dylib_ext = ".so",
        exec_triple = "x86_64-unknown-linux-gnu",
        extra_rustc_flags = EXTRA_RUSTC_FLAGS,
        iso_date = RUST_NIGHTLY_DATE,
        linker = TOOLS + "//:rust-lld",
        linker_type = "direct",
        rust_doc = TOOLS + "//:rustdoc",
        rust_objcopy = TOOLS + "//:rust-objcopy",
        rust_std = rust_std,
        rustc = TOOLS + "//:rustc",
        rustc_lib = TOOLS + "//:rustc_lib",
        staticlib_ext = ".a",
        stdlib_linkflags = [],
        target_json = TARGET_JSON,
        version = "1.99.0",
        visibility = ["//visibility:public"],
    )
