"""Target spec for muffin userspace, consumed as `target_json = json.encode(X86_64_UNKNOWN_MUFFIN)`.

`rust_toolchain.target_json` is `attr.string`, not a label, and Starlark cannot read
a file at load time, so this spec has to be Starlark.

Nothing here is derived from the pinned nightly date, so a bump alone does not make
this stale. It goes stale only when rustc changes the stock target it is derived
from, or changes the spec format. To re-derive it, run

    rustc +nightly-<date> -Zunstable-options --print target-spec-json --target x86_64-unknown-none

and reapply these deltas to the printed JSON:

1. `features` becomes `-mmx,+fxsr,+sse,+sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2`. AVX stays off
   because the kernel saves FPU state with `fxsave`, which does not cover YMM.
2. Remove `rustc-abi` entirely. That field, not `features`, selects the integer-register float ABI on
   this nightly.
3. Set `position-independent-executables` and `static-position-independent-executables` to `False`,
   and `relocation-model` to `"static"`. The loader only accepts `ET_EXEC`.
4. Drop `is-builtin` if present. Set `os` to `"muffin"`, which is what gives userspace a
   `cfg(target_os = "muffin")` distinct from bare metal. The key must be present at all,
   because rules_rust dereferences `toolchain.target_os` unguarded.
5. Restate `metadata.description`. rustc prints the stock target's wording, which names
   the float ABI this spec exists to change.

The Bazel platform deliberately keeps `@platforms//os:none` and does not follow this
field. Every `@crates` target gates `target_compatible_with` on
`@rules_rust//rust/platform:x86_64-unknown-none`, which resolves through
`@platforms//os:none`, and crate_universe cannot be given a JSON triple to generate a
`muffin` condition from. Moving the platform onto a `muffin` OS constraint makes all of
userspace resolve to `@platforms//:incompatible`.

rustc rejects unknown JSON target spec keys. A stale or hand-edited field therefore
fails loudly at toolchain resolution, not as a silent miscompile.
"""

X86_64_UNKNOWN_MUFFIN = {
    "arch": "x86_64",
    "code-model": "kernel",
    "cpu": "x86-64",
    "crt-objects-fallback": "false",
    "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
    "disable-redzone": True,
    "features": "-mmx,+fxsr,+sse,+sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2",
    "linker": "rust-lld",
    "linker-flavor": "gnu-lld",
    "llvm-target": "x86_64-unknown-none-elf",
    "max-atomic-width": 64,
    "metadata": {
        "description": "muffin userspace, x86_64 with SSE2 and the SSE float ABI",
        "host_tools": False,
        "std": False,
        "tier": 2,
    },
    "os": "muffin",
    "panic-strategy": "abort",
    "plt-by-default": False,
    "position-independent-executables": False,
    "relocation-model": "static",
    "relro-level": "full",
    "stack-probes": {
        "kind": "inline",
    },
    "static-position-independent-executables": False,
    "supported-sanitizers": [
        "kcfi",
        "kernel-address",
    ],
    "target-pointer-width": 64,
}
