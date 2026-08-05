"""Shared config for the muffin/bootstrap `rust_toolchain` pair.

rustc's JSON custom-target crate metadata check keys the target's identity on
the basename of the `--target=<path>.json` argument, not its content or
directory. Verified against nightly-2026-07-12. Two files with identical
content but different basenames make rustc report `E0461: couldn't find crate
... with expected target triple <other-basename>`. rules_rust always names
that generated file `<rust_toolchain rule name>.target.json`
(rust/private/toolchain.bzl), so the muffin and bootstrap toolchains live in
separate packages but both are named `impl`, giving them the same generated
basename `impl.target.json` and therefore the same rustc target identity. Any
future toolchain sharing this sysroot MUST keep that rule name.
"""

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
