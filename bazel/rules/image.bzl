"""Bootable image assembly.

Both rules shell out to `mke2fs` and `xorriso`, which are built from source by
`@e2fsprogs` and `@xorriso` and reach the actions as declared tool inputs. No
host install of either is involved, so the action cache sees the tool the same
way it sees the kernel binary.
"""

# Runs a command, discarding its output unless it fails. Both streams are merged
# so a failure still shows the tool's own diagnostics in order.
_QUIET_FN = """quiet() {
  if ! _out=$("$@" 2>&1); then
    printf '%s\\n' "$_out" >&2
    exit 1
  fi
}"""

def _single_file(target):
    files = target.files.to_list()
    if len(files) != 1:
        fail("{} must resolve to exactly one file, got {}".format(target.label, len(files)))
    return files[0]

def _stage(dest, src_path):
    return [
        'mkdir -p "$(dirname "$root/{}")"'.format(dest),
        'cp {} "$root/{}"'.format(src_path, dest),
    ]

def _ext2_image_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".img")
    mke2fs = ctx.file._mke2fs

    inputs = [ctx.file._mke2fs_conf]

    cmds = [
        "set -eu",
        # Without this mke2fs reads the host's /etc/mke2fs.conf, so filesystem
        # defaults would track whatever e2fsprogs the machine has installed.
        'export MKE2FS_CONFIG="$PWD/{}"'.format(ctx.file._mke2fs_conf.path),
        'root="$(mktemp -d)"',
        "trap 'rm -rf \"$root\"' EXIT",
    ]

    for directory in ctx.attr.empty_dirs:
        cmds.append('mkdir -p "$root/{}"'.format(directory))

    for target, dest in ctx.attr.files.items():
        staged = _single_file(target)
        inputs.append(staged)
        cmds.extend(_stage(dest, staged.path))

    for dest, content in ctx.attr.contents.items():
        staged = ctx.actions.declare_file("{}.contents/{}".format(ctx.label.name, dest))
        ctx.actions.write(staged, content)
        inputs.append(staged)
        cmds.extend(_stage(dest, staged.path))

    cmds.append('{} -q -d "$root" -m 5 -t ext2 {} {}'.format(mke2fs.path, out.path, ctx.attr.image_size))

    ctx.actions.run_shell(
        outputs = [out],
        inputs = inputs,
        tools = [mke2fs],
        command = "\n".join(cmds),
        mnemonic = "Ext2Image",
        progress_message = "Building ext2 image %{output}",
    )

    return [DefaultInfo(files = depset([out]))]

ext2_image = rule(
    implementation = _ext2_image_impl,
    doc = "Builds an ext2 filesystem image from staged files, literal contents, and empty dirs.",
    attrs = {
        "contents": attr.string_dict(
            doc = "Destination path inside the image to literal file content.",
        ),
        "empty_dirs": attr.string_list(
            doc = "Directories to create inside the image with no contents.",
        ),
        "files": attr.label_keyed_string_dict(
            allow_files = True,
            doc = "Single-file target to its destination path inside the image.",
        ),
        # Not `size`: Bazel reserves that attribute name for test targets.
        "image_size": attr.string(
            default = "64M",
            doc = "Filesystem size passed to mke2fs.",
        ),
        "_mke2fs": attr.label(
            allow_single_file = True,
            cfg = "exec",
            default = "@e2fsprogs//:mke2fs",
        ),
        "_mke2fs_conf": attr.label(
            allow_single_file = True,
            default = "@e2fsprogs//:mke2fs_conf",
        ),
    },
)

def _limine_iso_impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.out or ctx.label.name + ".iso")
    limine = ctx.executable._limine
    xorriso = ctx.file._xorriso

    cmds = [
        "set -eu",
        # xorriso and limine narrate every step on success, and Bazel surfaces an
        # action's whole output even when it exits 0. Hold the transcript and only
        # emit it if the step actually fails.
        _QUIET_FN,
        'root="$(mktemp -d)"',
        "trap 'rm -rf \"$root\"' EXIT",
        'mkdir -p "$root/boot/limine" "$root/EFI/BOOT"',
        # Limine looks up its config by this exact name, so the input's own
        # basename must not leak into the image.
        'cp {} "$root/boot/limine/limine.conf"'.format(ctx.file.limine_conf.path),
        # limine.conf points kernel_path at boot():/boot/kernel.
        'cp {} "$root/boot/kernel"'.format(ctx.file.kernel.path),
    ]

    for f in ctx.files._bios:
        cmds.append('cp {} "$root/boot/limine/{}"'.format(f.path, f.basename))
    for f in ctx.files._efi:
        cmds.append('cp {} "$root/EFI/BOOT/{}"'.format(f.path, f.basename))

    cmds.append(" ".join([
        "quiet {} -as mkisofs".format(xorriso.path),
        "-b boot/limine/limine-bios-cd.bin",
        "-no-emul-boot",
        "-boot-load-size 4",
        "-boot-info-table",
        "--efi-boot boot/limine/limine-uefi-cd.bin",
        "-efi-boot-part",
        "--efi-boot-image",
        "--protective-msdos-label",
        '"$root"',
        "-o",
        out.path,
    ]))

    # Stamps the BIOS stage into the ISO's protective MBR. Without this the
    # image only boots through UEFI.
    cmds.append("quiet {} bios-install {}".format(limine.path, out.path))

    ctx.actions.run_shell(
        outputs = [out],
        inputs = [ctx.file.kernel, ctx.file.limine_conf] + ctx.files._bios + ctx.files._efi,
        tools = [limine, xorriso],
        command = "\n".join(cmds),
        mnemonic = "LimineIso",
        progress_message = "Building bootable ISO %{output}",
    )

    return [DefaultInfo(files = depset([out]))]

limine_iso = rule(
    implementation = _limine_iso_impl,
    doc = "Builds a hybrid BIOS/UEFI bootable Limine ISO around a kernel ELF.",
    attrs = {
        "kernel": attr.label(
            allow_single_file = True,
            mandatory = True,
            doc = "Kernel ELF placed at boot/kernel inside the image.",
        ),
        "limine_conf": attr.label(
            allow_single_file = True,
            default = "//:limine.conf",
        ),
        # The target name is often a role rather than a filename, so a release
        # artifact can be named independently of it.
        "out": attr.string(
            doc = "Output filename. Defaults to <name>.iso.",
        ),
        "_bios": attr.label(default = "@limine//:bios_files"),
        "_efi": attr.label(default = "@limine//:efi_files"),
        "_limine": attr.label(
            cfg = "exec",
            default = "@limine//:limine",
            executable = True,
        ),
        "_xorriso": attr.label(
            allow_single_file = True,
            cfg = "exec",
            default = "@xorriso//:xorriso",
        ),
    },
)
