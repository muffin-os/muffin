#!/bin/sh
# Round-trips the patch series in support/ through a git tree.
#
#   patchdev extract <rust|rustc|rust-libc> <workdir>
#     Unpacks the pinned tarball into <workdir>, commits it as `base`, applies the
#     series directories (two for `rustc`) with `git am`. Stores the tree kind
#     in <workdir>/.patchdev.
#   patchdev export <workdir>
#     Rewrites the series directories and their series.bzl from base..HEAD.
#   patchdev sha
#     Prints `NAME SHA256` for every pinned archive, from a fresh download.
#
# The URL, strip-prefix and sha256 variables come from support/versions.bzl
# through the `env` of support/BUILD.bazel.
set -eu

REPO=${BUILD_WORKSPACE_DIRECTORY:?run through bazel run //support:patchdev}
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/muffin-support
LIBRARY=support/rust/patches/library
COMPILER=support/rust/patches/compiler
LIBC=support/rust-libc/patches

usage() {
    echo "usage: patchdev extract <rust|rustc|rust-libc> <workdir> | export <workdir> | sha" >&2
    exit 2
}

series_dirs() {
    case $1 in
    rust) echo "$LIBRARY" ;;
    rustc) echo "$LIBRARY $COMPILER" ;;
    rust-libc) echo "$LIBC" ;;
    *) echo "unknown tree: $1" >&2; exit 2 ;;
    esac
}

# Prints the cached archive path for the URL, after a sha256 check. A stale or
# partial file is downloaded again.
fetch() {
    url=$1 sha=$2
    file=$CACHE/$(basename "$url")
    mkdir -p "$CACHE"
    if ! echo "$sha  $file" | sha256sum -c --quiet >/dev/null 2>&1; then
        curl -fsSL -o "$file.part" "$url"
        echo "$sha  $file.part" | sha256sum -c --quiet >/dev/null
        mv "$file.part" "$file"
    fi
    echo "$file"
}

# Unpacks the archive as http_archive does: the strip prefix is dropped and
# entries outside it are skipped by tar.
unpack() {
    url=$1 sha=$2 prefix=$3 work=$4
    depth=$(printf %s "$prefix" | tr -cd / | wc -c)
    tar xf "$(fetch "$url" "$sha")" -C "$work" --strip-components=$((depth + 1))
}

apply_series() {
    work=$1 dir=$2
    for p in $(sed -n 's/^ *"\(.*\)",*$/\1/p' "$REPO/$dir/series.bzl"); do
        git -C "$work" am -q "$REPO/$dir/$p"
    done
}

extract() {
    kind=${1:-} work=${2:-}
    [ -n "$kind" ] && [ -n "$work" ] || usage
    dirs=$(series_dirs "$kind")
    mkdir -p "$work"
    case $kind in
    rust) unpack "$RUST_SRC_URL" "$RUST_SRC_SHA256" "$RUST_SRC_STRIP_PREFIX" "$work" ;;
    rustc) unpack "$RUSTC_SRC_URL" "$RUSTC_SRC_SHA256" "$RUSTC_SRC_STRIP_PREFIX" "$work" ;;
    rust-libc) unpack "$LIBC_URL" "$LIBC_SHA256" "$LIBC_STRIP_PREFIX" "$work" ;;
    esac
    git -C "$work" init -q
    git -C "$work" add -A
    git -C "$work" commit -qm base
    git -C "$work" tag base
    for dir in $dirs; do apply_series "$work" "$dir"; done
    echo "$kind" > "$work/.patchdev"
}

# rustc commits that touch only library/** belong to the library series.
series_for() {
    kind=$1 work=$2 sha=$3
    if [ "$kind" = rustc ] && git -C "$work" show --format= --name-only "$sha" | grep -qv '^library/'; then
        echo "$COMPILER"
    else
        series_dirs "$kind" | cut -d ' ' -f1
    fi
}

format_one() {
    work=$1 sha=$2 dest=$3
    n=$(find "$REPO/$dest" -name '*.patch' | wc -l)
    # --full-index: the abbreviated blob ids grow with the tree, so a rustc
    # export would rewrite every library patch.
    git -C "$work" format-patch -q --zero-commit --no-signature --no-numbered --no-stat \
        --full-index --start-number $((n + 1)) -o "$REPO/$dest" -1 "$sha"
}

write_series() {
    dir=$1
    names=$(find "$REPO/$dir" -name '*.patch' -exec basename {} \; | sort)
    if [ -z "$names" ]; then
        echo "PATCHES = []" > "$REPO/$dir/series.bzl"
        return
    fi
    {
        echo "PATCHES = ["
        for p in $names; do echo "    \"$p\","; done
        echo "]"
    } > "$REPO/$dir/series.bzl"
}

export_series() {
    work=${1:-}
    [ -n "$work" ] || usage
    kind=$(cat "$work/.patchdev")
    dirs=$(series_dirs "$kind")
    for dir in $dirs; do rm -f "$REPO/$dir"/*.patch; done
    for sha in $(git -C "$work" rev-list --reverse base..HEAD); do
        format_one "$work" "$sha" "$(series_for "$kind" "$work" "$sha")"
    done
    for dir in $dirs; do write_series "$dir"; done
}

print_sha() {
    echo "$1 $(curl -fsSL "$2" | sha256sum | cut -d ' ' -f1)"
}

case ${1:-} in
extract) extract "${2:-}" "${3:-}" ;;
export) export_series "${2:-}" ;;
sha)
    print_sha RUST_SRC_SHA256 "$RUST_SRC_URL"
    print_sha RUSTC_SRC_SHA256 "$RUSTC_SRC_URL"
    print_sha CI_LLVM_SHA256 "$CI_LLVM_URL"
    print_sha LIBC_SHA256 "$LIBC_URL"
    ;;
*) usage ;;
esac
