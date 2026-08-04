"""Repo-wide clippy that follows every dependency edge.

`rust_clippy_aspect` declares no `attr_aspects`, so an `--aspects` build lints
only the targets named on the command line. Dependencies are configured but
never checked. That leaves two holes: `bazel build //kernel/boot:boot` checks
`main.rs` and none of the kernel behind it, and the kernel library is only ever
checked in the host configuration, because the `x86_64-none` instance exists
solely as a dependency of the kernel binary.

This wrapper propagates along every attribute and republishes the markers it
finds under `clippy_all`, so requesting that output group on any target checks
the target's whole graph in the configuration each crate really builds in.
Propagation is deliberately not limited to `deps`, since the kernel reaches the
ISO through `limine_iso.kernel` and the runner through `data`.

A separate output group name avoids colliding with the `clippy_checks` group
that `rust_clippy_aspect` publishes on the same targets.
"""

load("@rules_rust//rust:defs.bzl", "rust_clippy_aspect")

ClippyClosureInfo = provider(
    doc = "Clippy markers for a target and every target reachable from it.",
    fields = {
        "checks": "depset[File]: clippy markers for this target and its closure.",
    },
)

def _closure(attrs):
    """Returns the `checks` depsets of every dependency that carries the provider."""
    found = []
    for name in dir(attrs):
        if name.startswith("_"):
            continue
        value = getattr(attrs, name, None)
        candidates = value if type(value) == "list" else [value]
        for candidate in candidates:
            if type(candidate) == "Target" and ClippyClosureInfo in candidate:
                found.append(candidate[ClippyClosureInfo].checks)
    return found

def _clippy_closure_aspect_impl(target, ctx):
    direct = []
    if OutputGroupInfo in target:
        groups = target[OutputGroupInfo]
        if hasattr(groups, "clippy_checks"):
            direct.append(groups.clippy_checks)

    checks = depset(transitive = direct + _closure(ctx.rule.attr))
    return [
        ClippyClosureInfo(checks = checks),
        OutputGroupInfo(clippy_all = checks),
    ]

clippy_closure_aspect = aspect(
    implementation = _clippy_closure_aspect_impl,
    attr_aspects = ["*"],
    requires = [rust_clippy_aspect],
    provides = [ClippyClosureInfo],
    doc = "Collects clippy markers for a target and everything it reaches.",
)
