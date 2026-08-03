#!/bin/sh
# Runs `cargo miri test` for one kernel crate inside a throwaway Cargo workspace.
#
# $1  Cargo package name of the crate under test, for example kernel_abi
# $2  workspace-root Cargo.toml
# $3+ one "<cargo package>=<manifest>=<source package dir>" per crate in the
#     closure. The Cargo package name and the source directory differ, so both
#     are passed explicitly.
set -eu

crate="$1"
workspace_manifest="$2"
shift 2

ws="$TEST_TMPDIR/ws"
mkdir -p "$ws"
cp "$workspace_manifest" "$ws/Cargo.toml"

# Pins the synthesized workspace to the same nightly Bazel uses, so the date
# lives in one place.
cp rust-toolchain.toml "$ws/rust-toolchain.toml"

for spec in "$@"; do
    member=$(printf '%s' "$spec" | cut -d= -f1)
    manifest=$(printf '%s' "$spec" | cut -d= -f2)
    srcdir=$(printf '%s' "$spec" | cut -d= -f3)
    mkdir -p "$ws/$member"
    cp "$manifest" "$ws/$member/Cargo.toml"
    cp -R "$srcdir/src" "$ws/$member/src"
done

# Runfiles are read-only, so the copies are too. Cargo only writes to target/,
# but Miri's own caches want a writable tree.
chmod -R u+w "$ws"

cd "$ws"
exec cargo miri test -p "$crate"
