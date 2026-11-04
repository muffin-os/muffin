"""Upstream pins for the Rust std port.

`.github/workflows/nightly-upgrade.yml` rewrites `RUST_NIGHTLY_DATE`, `RUST_COMMIT`
and every sha256 here together with `rust-toolchain.toml` and `MODULE.bazel`;
bump `LIBC_VERSION` by hand. `bazel run //support:patchdev -- sha` prints the
sha256 values.
"""

RUST_NIGHTLY_DATE = "2026-07-12"

# The rust-lang/rust commit the nightly was built from (`rustc -vV`).
RUST_COMMIT = "be8e82435eb04fbe75ed5286b52735366e160bed"

LIBC_VERSION = "0.2.185"

_DIST = "https://static.rust-lang.org/dist/" + RUST_NIGHTLY_DATE + "/"

RUST_SRC_URL = _DIST + "rust-src-nightly.tar.xz"

RUST_SRC_STRIP_PREFIX = "rust-src-nightly/rust-src/lib/rustlib/src/rust"

RUST_SRC_SHA256 = "99c110f203f38294e3b1565950809a45a5b0a91749244fa3a2ea94dd77e5be5b"

RUSTC_SRC_URL = _DIST + "rustc-nightly-src.tar.xz"

RUSTC_SRC_STRIP_PREFIX = "rustc-nightly-src"

RUSTC_SRC_SHA256 = "e2f5d93dc43e4b3275817fd2897aad41deb64c15995b5ceee0ae347ee0673c88"

# dist/<date>/ has no rust-dev tarball; only the CI artifact store does.
CI_LLVM_URL = "https://ci-artifacts.rust-lang.org/rustc-builds/" + RUST_COMMIT + "/rust-dev-nightly-x86_64-unknown-linux-gnu.tar.xz"

CI_LLVM_STRIP_PREFIX = "rust-dev-nightly-x86_64-unknown-linux-gnu"

CI_LLVM_SHA256 = "581b628a48e08111016201d0e6d1bddd3de5d23b82b6b5c0183e9afd99d46bfc"

LIBC_URL = "https://static.crates.io/crates/libc/libc-" + LIBC_VERSION + ".crate"

LIBC_STRIP_PREFIX = "libc-" + LIBC_VERSION

LIBC_SHA256 = "52ff2c0fe9bc6cb6b14a0592c2ff4fa9ceb83eea9db979b0487cd054946a2b8f"
