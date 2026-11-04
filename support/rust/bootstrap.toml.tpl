# x.py config for //support/rust:stage2. stage2.bzl replaces @LLVM_CONFIG@ with
# the absolute path of the ci-llvm `llvm-config`.

# The dist profile builds an optimized compiler without debug assertions or
# incremental state, as the nightly release does.
profile = "dist"
# A nightly bump would make a pinned change id stale; x.py only warns about it.
change-id = "ignore"

[build]
build = "x86_64-unknown-linux-gnu"
host = ["x86_64-unknown-linux-gnu"]
target = ["x86_64-unknown-linux-gnu"]
extended = false
docs = false
# cargo builds with `--frozen` from the source tarball's vendor/, so it never
# goes online. x.py also detects this, but an explicit value fails loudly when
# vendor/ is missing.
vendor = true
# x.py appends "built from a source tarball" to the version by default. rustc
# then rejects the proc macros that the stock exec toolchain builds.
description = ""

[llvm]
# The ci-llvm tarball has only libLLVM.so, no static LLVM libraries.
link-shared = true

[rust]
# The dist profile builds this linker for GPU targets, which muffin does not use.
llvm-bitcode-linker = false
# The ci-llvm tarball ships no llvm-strip, so the llvm-tools copy fails.
llvm-tools = false

[target.x86_64-unknown-linux-gnu]
llvm-config = "@LLVM_CONFIG@"
