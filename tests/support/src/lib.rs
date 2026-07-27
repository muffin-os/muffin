//! Shared helpers for assembling bootable QEMU images (a Limine ISO and an
//! ext2 disk) out of prebuilt kernel/init binaries.
//!
//! The root `build.rs` uses these to build the production image. Integration
//! tests under `tests/` use the same functions to assemble a dedicated image
//! around a test-specific kernel and init binary, so test-only boot
//! sequences never have to be hacked into the production kernel or init.

use std::fs;
use std::fs::{copy, create_dir_all, remove_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub mod harness;

pub use harness::{HostEnv, KernelTest, Outcome, RunReport, SpawnedProcess, qemu_available};

/// A single file to place on the ext2 disk image, keyed by its path
/// relative to the disk root (e.g. `"bin/init"`).
pub struct DiskFile {
    pub path: &'static str,
    pub content: Vec<u8>,
}

/// Builds an ext2 disk image containing `files` at `out_dir/disk.img`.
///
/// # Panics
/// Panics if `mke2fs` is not on `PATH` or exits unsuccessfully.
#[must_use]
pub fn build_disk_image(files: &[DiskFile], out_dir: &Path) -> PathBuf {
    let disk_dir = out_dir.join("disk");
    let _ = fs::remove_dir_all(&disk_dir); // fresh contents each build
    create_dir_all(&disk_dir).expect("should be able to create disk directory");

    for file in files {
        let dest = disk_dir.join(file.path);
        if let Some(parent) = dest.parent() {
            create_dir_all(parent).expect("should be able to create disk subdirectory");
        }
        fs::write(&dest, &file.content).expect("should be able to write disk file");
    }

    let disk_image = out_dir.join("disk.img");
    let _ = remove_file(&disk_image); // if this fails, doesn't matter

    let status = Command::new("mke2fs")
        .arg("-d")
        .arg(
            disk_dir
                .to_str()
                .expect("disk_dir path should be valid UTF-8"),
        )
        .arg("-m")
        .arg("5")
        .arg("-t")
        .arg("ext2")
        .arg(
            disk_image
                .to_str()
                .expect("disk_image path should be valid UTF-8"),
        )
        .arg("10M")
        .status()
        .expect("mke2fs command should execute");
    assert!(status.success(), "mke2fs should exit successfully");

    disk_image
}

/// Builds a bootable Limine ISO at `out_dir/muffin.iso` that boots
/// `kernel_binary`.
///
/// `limine_dir` is an already fetched/built Limine checkout (binary tools
/// plus boot shims). `limine_conf` is the `limine.conf` to embed.
///
/// # Panics
/// Panics if `xorriso`, the Limine checkout, or its `limine` tool are
/// missing, or if any step exits unsuccessfully.
#[must_use]
pub fn build_iso(
    limine_dir: &Path,
    limine_conf: &Path,
    kernel_binary: &Path,
    out_dir: &Path,
) -> PathBuf {
    let iso_dir = out_dir.join("iso_root");
    let boot_dir = iso_dir.join("boot");
    let limine_out_dir = boot_dir.join("limine");
    create_dir_all(&limine_out_dir).expect("should be able to create limine directory");
    let efi_boot_dir = iso_dir.join("EFI/BOOT");
    create_dir_all(&efi_boot_dir).expect("should be able to create EFI boot directory");

    let limine_conf_name = limine_conf
        .file_name()
        .expect("limine_conf should have a file name");
    copy(limine_conf, limine_out_dir.join(limine_conf_name))
        .expect("should be able to copy limine.conf");

    // copy the kernel binary to the location that is specified in limine.conf
    copy(kernel_binary, boot_dir.join("kernel")).expect("should be able to copy kernel binary");

    // the following is x86_64 specific

    for path in [
        "limine-bios.sys",
        "limine-bios-cd.bin",
        "limine-uefi-cd.bin",
    ] {
        let from = limine_dir.join(path);
        let to = limine_out_dir.join(path);
        copy(&from, &to).unwrap_or_else(|_| {
            panic!(
                "should be able to copy {} to {}",
                from.display(),
                to.display()
            )
        });
    }

    for path in ["BOOTX64.EFI", "BOOTIA32.EFI"] {
        let from = limine_dir.join(path);
        let to = efi_boot_dir.join(path);
        copy(from, to).expect("should be able to copy EFI boot files");
    }

    let output_iso = out_dir.join("muffin.iso");

    let status = Command::new("xorriso")
        .arg("-as")
        .arg("mkisofs")
        .arg("-b")
        .arg(
            limine_out_dir
                .join("limine-bios-cd.bin")
                .strip_prefix(&iso_dir)
                .expect("limine-bios-cd.bin path should be within iso_dir"),
        )
        .arg("-no-emul-boot")
        .arg("-boot-load-size")
        .arg("4")
        .arg("-boot-info-table")
        .arg("--efi-boot")
        .arg(
            limine_out_dir
                .join("limine-uefi-cd.bin")
                .strip_prefix(&iso_dir)
                .expect("limine-uefi-cd.bin path should be within iso_dir"),
        )
        .arg("-efi-boot-part")
        .arg("--efi-boot-image")
        .arg("--protective-msdos-label")
        .arg(&iso_dir)
        .arg("-o")
        .arg(&output_iso)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("xorriso command should execute");
    assert!(status.success());

    let status = Command::new(limine_dir.join("limine"))
        .arg("bios-install")
        .arg(&output_iso)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("limine bios-install command should execute");
    assert!(status.success());

    output_iso
}
