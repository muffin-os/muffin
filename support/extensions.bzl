"""Fetches the pinned upstream Rust sources and applies this repo's patch series.

Every archive is an `http_archive` pinned by `versions.bzl`. The patch series
are `git format-patch` output listed in order by a `series.bzl` next to each
patch directory. `bazel run //support:patchdev` round-trips the series.
"""

load("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive")
load("//support/rust-libc/patches:series.bzl", LIBC_PATCHES = "PATCHES")
load("//support/rust/patches/compiler:series.bzl", COMPILER_PATCHES = "PATCHES")
load("//support/rust/patches/library:series.bzl", LIBRARY_PATCHES = "PATCHES")
load(
    ":versions.bzl",
    "CI_LLVM_SHA256",
    "CI_LLVM_STRIP_PREFIX",
    "CI_LLVM_URL",
    "LIBC_SHA256",
    "LIBC_STRIP_PREFIX",
    "LIBC_URL",
    "RUSTC_SRC_SHA256",
    "RUSTC_SRC_STRIP_PREFIX",
    "RUSTC_SRC_URL",
    "RUST_SRC_SHA256",
    "RUST_SRC_STRIP_PREFIX",
    "RUST_SRC_URL",
)

def _patch_labels(package, patches):
    return [package + ":" + p for p in patches]

def _rust_support_impl(_ctx):
    http_archive(
        name = "rust_src",
        build_file = "//support/rust:rust_src.BUILD.bazel",
        patch_args = ["-p1"],
        patches = _patch_labels("//support/rust/patches/library", LIBRARY_PATCHES),
        sha256 = RUST_SRC_SHA256,
        strip_prefix = RUST_SRC_STRIP_PREFIX,
        urls = [RUST_SRC_URL],
    )
    http_archive(
        name = "rustc_src",
        build_file = "//support/rust:rustc_src.BUILD.bazel",
        patch_args = ["-p1"],
        patches = _patch_labels("//support/rust/patches/library", LIBRARY_PATCHES) +
                  _patch_labels("//support/rust/patches/compiler", COMPILER_PATCHES),
        sha256 = RUSTC_SRC_SHA256,
        strip_prefix = RUSTC_SRC_STRIP_PREFIX,
        urls = [RUSTC_SRC_URL],
    )
    http_archive(
        name = "ci_llvm",
        build_file = "//support/rust:ci_llvm.BUILD.bazel",
        sha256 = CI_LLVM_SHA256,
        strip_prefix = CI_LLVM_STRIP_PREFIX,
        urls = [CI_LLVM_URL],
    )
    http_archive(
        name = "rust_libc",
        build_file = "//support/rust-libc:rust_libc.BUILD.bazel",
        patch_args = ["-p1"],
        patches = _patch_labels("//support/rust-libc/patches", LIBC_PATCHES),
        sha256 = LIBC_SHA256,
        strip_prefix = LIBC_STRIP_PREFIX,
        type = "tar.gz",
        urls = [LIBC_URL],
    )

rust_support = module_extension(implementation = _rust_support_impl)
