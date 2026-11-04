"""Shared config for the muffin `rust_toolchain` set.

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

`stage2_toolchain` uses the stage-2 rustc of `//support/rust:stage2`, which has
`x86_64-unknown-muffin` built in. It needs no JSON file, so the name rule does
not apply to it.
"""

load("@rules_rust//rust:toolchain.bzl", "rust_toolchain")
load("//support:versions.bzl", "RUST_NIGHTLY_DATE")
load(":spec.bzl", "X86_64_UNKNOWN_MUFFIN")

TOOLS = "@rust_linux_x86_64__x86_64-unknown-none__nightly_tools"

TARGET_JSON = json.encode(X86_64_UNKNOWN_MUFFIN)

# extra_rustc_flags_triples in MODULE.bazel keys on the triple STRING
# "x86_64-unknown-none", so none of those flags fire for the muffin target. They
# are restated here. The panic strategy and -Crelocation-model=static live in the
# spec itself so bootstrap and real toolchain cannot disagree.
_CODEGEN_FLAGS = [
    "-Clink-arg=-z",
    "-Clink-arg=nostart-stop-gc",
    "-Cforce-frame-pointers=yes",
    "-Cforce-unwind-tables=yes",
    "-Cdebuginfo=2",
]

# rustc destabilised JSON target specs in rust-lang/rust#150151. Loading one
# now requires -Zunstable-options.
EXTRA_RUSTC_FLAGS = ["-Zunstable-options"] + _CODEGEN_FLAGS

def _rust_toolchain(name, rust_std, **kwargs):
    rust_toolchain(
        name = name,
        binary_ext = "",
        channel = "nightly",
        clippy_driver = TOOLS + "//:clippy_driver_bin",
        default_edition = "2024",
        dylib_ext = ".so",
        exec_triple = "x86_64-unknown-linux-gnu",
        iso_date = RUST_NIGHTLY_DATE,
        linker = TOOLS + "//:rust-lld",
        linker_type = "direct",
        rust_doc = TOOLS + "//:rustdoc",
        rust_objcopy = TOOLS + "//:rust-objcopy",
        rust_std = rust_std,
        staticlib_ext = ".a",
        stdlib_linkflags = [],
        version = "1.99.0",
        visibility = ["//visibility:public"],
        **kwargs
    )

def muffin_toolchain(rust_std):
    """Declares the `impl` rust_toolchain of a package with the given `rust_std`."""
    _rust_toolchain(
        name = "impl",
        extra_rustc_flags = EXTRA_RUSTC_FLAGS,
        rust_std = rust_std,
        rustc = TOOLS + "//:rustc",
        rustc_lib = TOOLS + "//:rustc_lib",
        target_json = TARGET_JSON,
    )

def stage2_toolchain(name, rust_std):
    """Declares a rust_toolchain that compiles with the stage-2 rustc.

    The toolchain is `manual`: `bazel build //...` must not build the stage-2 rustc.
    clippy and rustdoc stay the stock ones, which do not know the built-in target.
    """
    _rust_toolchain(
        name = name,
        extra_rustc_flags = _CODEGEN_FLAGS,
        rust_std = rust_std,
        rustc = "//support/rust:stage2_rustc",
        rustc_lib = "//support/rust:stage2_lib",
        tags = ["manual"],
        target_triple = "x86_64-unknown-muffin",
    )
