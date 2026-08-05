"""Transitions the sysroot crates (core/alloc/compiler_builtins) to the bootstrap platform.

`rust_library` has no `platform` attribute, so a plain dependency on `@rust_src`
targets would build them under whatever platform the requesting target uses.
For the real muffin toolchain that is circular. Compiling core would need a
`rust_std`-bearing toolchain, and that toolchain's `rust_std` is these very
rlibs. The bootstrap platform's toolchain has an empty `rust_std`, breaking the
cycle.
"""

def _bootstrap_impl(_settings, _attr):
    return {"//command_line_option:platforms": str(Label("//platforms:x86_64-unknown-muffin-bootstrap"))}

_bootstrap = transition(
    implementation = _bootstrap_impl,
    inputs = [],
    outputs = ["//command_line_option:platforms"],
)

def _impl(ctx):
    # `rust_stdlib_filegroup` derives a `.a` symlink name from each rlib's path
    # relative to its owning package (utils.bzl's `make_static_lib_symlink`).
    # `@rust_src`'s BUILD file sits at that repo's root, an empty package name,
    # so the derived path collapses to something outside this rule's own
    # package and `declare_file` rejects it. Re-rooting the rlibs here first
    # makes their owner this target's package, which sidesteps that branch.
    outs = []
    for f in ctx.files.crates:
        out = ctx.actions.declare_file(f.basename)
        ctx.actions.symlink(output = out, target_file = f)
        outs.append(out)
    return [DefaultInfo(files = depset(outs))]

bootstrap_sysroot = rule(
    implementation = _impl,
    attrs = {
        "crates": attr.label_list(cfg = _bootstrap, allow_files = True, mandatory = True),
    },
)
