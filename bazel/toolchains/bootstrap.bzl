"""Transitions sysroot crates to a bootstrap platform and collects their rlibs.

`rust_library` has no `platform` attribute, so a plain dependency on `@rust_src`
targets would build them under whatever platform the requesting target uses.
For the real muffin toolchain that is circular. Compiling core would need a
`rust_std`-bearing toolchain, and that toolchain's `rust_std` is these very
rlibs. The `platform` attribute selects the sysroot layer (platforms/BUILD.bazel)
that breaks the cycle.

The output holds every rlib that `crates` pull in transitively, so a sysroot
layer is complete for the crates that link against it.
"""

load("@rules_rust//rust:rust_common.bzl", "CrateInfo", "DepInfo")

def _bootstrap_impl(_settings, attr):
    return {"//command_line_option:platforms": str(attr.platform)}

_bootstrap = transition(
    implementation = _bootstrap_impl,
    inputs = [],
    outputs = ["//command_line_option:platforms"],
)

def _impl(ctx):
    files = depset(
        [t[CrateInfo].output for t in ctx.attr.crates],
        transitive = [t[DepInfo].transitive_crate_outputs for t in ctx.attr.crates],
    )

    # `rust_stdlib_filegroup` derives a `.a` symlink name from each rlib's path
    # relative to its owning package (utils.bzl's `make_static_lib_symlink`).
    # `@rust_src`'s BUILD file sits at that repo's root, an empty package name,
    # so the derived path collapses to something outside this rule's own
    # package and `declare_file` rejects it. Re-rooting the rlibs here first
    # makes their owner this target's package, which sidesteps that branch.
    outs = []
    for f in files.to_list():
        if f.extension != "rlib":
            continue
        out = ctx.actions.declare_file(f.basename)
        ctx.actions.symlink(output = out, target_file = f)
        outs.append(out)
    return [DefaultInfo(files = depset(outs))]

bootstrap_sysroot = rule(
    implementation = _impl,
    attrs = {
        "crates": attr.label_list(cfg = _bootstrap, providers = [CrateInfo, DepInfo], mandatory = True),
        "platform": attr.label(mandatory = True),
    },
)
