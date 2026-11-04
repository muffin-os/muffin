"""Builds a stage-2 rustc from the patched `@rustc_src` tree with x.py."""

RustcStage2Info = provider(
    doc = "The stage-2 rustc, its clippy-driver, and the shared libraries both load from `../lib`.",
    fields = {
        "clippy_driver": "File: must sit in `bin/` beside the `lib/` that holds `lib`.",
        "lib": "list[File]: librustc_driver.so and libLLVM.so.",
        "rustc": "File: must sit in `bin/` beside the `lib/` that holds `lib`.",
    },
)

# x.py writes into its source tree, so the action copies the tree into a scratch
# dir first. The scratch dir holds the source copy and the x.py build dir (about
# 8.5 GB together) and is deleted on exit.
#
# x.py treats an `llvm-config` LLVM as a system LLVM: it neither finds libLLVM.so
# at run time nor copies it into the sysroot. The action puts the library on the
# loader path and copies it next to librustc_driver itself.
#
# rules_rust links every `rustc_lib` file into its sysroot one by one, so these
# must be files with names known at analysis time, not a directory. The action
# drops the hash from the librustc_driver file name and patches the NEEDED entry
# of rustc and clippy-driver to match.
_BUILD = """
set -eu
llvm_config=$PWD/$LLVM_CONFIG
llvm_lib=$PWD/$(dirname "$LLVM_SO")
scratch=$PWD/$SCRATCH
rm -rf "$scratch"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/src"
cp -RL "$SRC/." "$scratch/src"
sed "s|@LLVM_CONFIG@|$llvm_config|" "$CONFIG" > "$scratch/src/bootstrap.toml"
(
    cd "$scratch/src"
    env -u RUSTUP_TOOLCHAIN LD_LIBRARY_PATH="$llvm_lib" CARGO_HOME="$scratch/cargo" \
        python3 x.py build --stage 2 compiler clippy --build-dir "$scratch/build"
)
built_sysroot=$scratch/build/x86_64-unknown-linux-gnu/stage2
built_driver=$(cd "$built_sysroot/lib" && echo librustc_driver-*.so)
cp "$built_sysroot/bin/rustc" "$OUT_RUSTC"
cp "$built_sysroot/bin/clippy-driver" "$OUT_CLIPPY"
cp "$built_sysroot/lib/$built_driver" "$OUT_DRIVER"
cp -L "$LLVM_SO" "$OUT_LIBLLVM"
patchelf --replace-needed "$built_driver" "$(basename "$OUT_DRIVER")" "$OUT_RUSTC" "$OUT_CLIPPY"
"""

def _rustc_stage2_impl(ctx):
    rustc = ctx.actions.declare_file(ctx.label.name + "/bin/rustc")
    clippy = ctx.actions.declare_file(ctx.label.name + "/bin/clippy-driver")
    driver = ctx.actions.declare_file(ctx.label.name + "/lib/librustc_driver.so")
    libllvm = ctx.actions.declare_file(ctx.label.name + "/lib/" + ctx.file.libllvm.basename)
    ctx.actions.run_shell(
        inputs = ctx.files.srcs + ctx.files.llvm + [ctx.file.config],
        outputs = [rustc, clippy, driver, libllvm],
        command = _BUILD,
        env = {
            "CONFIG": ctx.file.config.path,
            "LLVM_CONFIG": ctx.file.llvm_config.path,
            "LLVM_SO": ctx.file.libllvm.path,
            "OUT_CLIPPY": clippy.path,
            "OUT_DRIVER": driver.path,
            "OUT_LIBLLVM": libllvm.path,
            "OUT_RUSTC": rustc.path,
            "SCRATCH": "{}/{}/{}.scratch".format(ctx.bin_dir.path, ctx.label.package, ctx.label.name),
            "SRC": ctx.attr.srcs.label.workspace_root,
            # PATH tools: python3 runs x.py; curl lets x.py fetch its stage-0
            # compiler; a C++ compiler builds rustc_llvm; the linker links
            # rustc; patchelf rewrites the NEEDED entries of rustc and
            # clippy-driver. A fixed PATH keeps the client environment out of
            # the action key, so a disk cache hit survives a changed shell
            # PATH, e.g. a CI runner image update.
            "PATH": "/usr/local/bin:/usr/bin:/bin",
        },
        execution_requirements = {
            "no-remote": "1",
            "no-sandbox": "1",
            "requires-network": "1",
        },
        mnemonic = "RustcStage2",
        progress_message = "Building the stage-2 rustc with x.py",
    )
    return [
        DefaultInfo(files = depset([rustc, clippy, driver, libllvm])),
        OutputGroupInfo(
            clippy = depset([clippy]),
            lib = depset([driver, libllvm]),
            rustc = depset([rustc]),
        ),
        RustcStage2Info(clippy_driver = clippy, lib = [driver, libllvm], rustc = rustc),
    ]

rustc_stage2 = rule(
    implementation = _rustc_stage2_impl,
    attrs = {
        "config": attr.label(allow_single_file = True, mandatory = True),
        "libllvm": attr.label(allow_single_file = True, mandatory = True),
        "llvm": attr.label(mandatory = True),
        "llvm_config": attr.label(allow_single_file = True, mandatory = True),
        "srcs": attr.label(mandatory = True),
    },
)

def _rustc_stage2_exec_impl(ctx):
    info = ctx.attr.build[RustcStage2Info]
    rustc = ctx.actions.declare_file(ctx.label.name + "/bin/rustc")
    ctx.actions.symlink(output = rustc, target_file = info.rustc, is_executable = True)
    clippy = ctx.actions.declare_file(ctx.label.name + "/bin/clippy-driver")
    ctx.actions.symlink(output = clippy, target_file = info.clippy_driver, is_executable = True)
    libs = []
    for built in info.lib:
        lib = ctx.actions.declare_file(ctx.label.name + "/lib/" + built.basename)
        ctx.actions.symlink(output = lib, target_file = built)
        libs.append(lib)
    return [DefaultInfo(files = depset([rustc, clippy] + libs))]

# The toolchains take rustc with `cfg = "exec"`. Building `build` in the exec
# configuration here too makes `bazel build` of this target and a toolchain
# user share one x.py run.
rustc_stage2_exec = rule(
    implementation = _rustc_stage2_exec_impl,
    attrs = {
        "build": attr.label(cfg = "exec", mandatory = True, providers = [RustcStage2Info]),
    },
)
