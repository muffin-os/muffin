use std::fs;
use std::fs::{copy, create_dir, create_dir_all, exists, remove_dir_all, remove_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use file_structure::{Dir, Kind};
use ovmf_prebuilt::{Arch, FileType, Prebuilt, Source};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=limine.conf");

    let limine_dir = limine();

    let kernel = PathBuf::from(std::env::var_os("CARGO_BIN_FILE_KERNEL_kernel").unwrap());
    println!("cargo:rustc-env=KERNEL_BINARY={}", kernel.display());

    let iso = build_iso(&limine_dir, &kernel);
    println!("cargo:rustc-env=BOOTABLE_ISO={}", iso.display());

    let ovmf = ovmf();
    println!(
        "cargo:rustc-env=OVMF_X86_64_CODE={}",
        ovmf.get_file(Arch::X64, FileType::Code).display()
    );
    println!(
        "cargo:rustc-env=OVMF_X86_64_VARS={}",
        ovmf.get_file(Arch::X64, FileType::Vars).display()
    );

    let disk_image = build_os_disk_image();
    println!("cargo:rustc-env=DISK_IMAGE={}", disk_image.display());
}

fn build_os_disk_image() -> PathBuf {
    let disk_dir = build_os_disk_dir();
    let disk_image = disk_dir.with_extension("img");

    let _ = remove_file(&disk_image); // if this fails, doesn't matter

    // works on my machine. TODO: use the mkfs-ext2 crate once it's ready
    let mut cmd = Command::new("mke2fs");
    cmd.arg("-d").arg(disk_dir.to_str().unwrap());
    cmd.arg("-m").arg("5");
    cmd.arg("-t").arg("ext2");
    cmd.arg(disk_image.to_str().unwrap());
    cmd.arg("10M");

    let rc = cmd.status().unwrap();
    assert_eq!(0, rc.code().unwrap(), "process should exit successfully");

    disk_image
}

fn build_os_disk_dir() -> PathBuf {
    let disk = out_dir().join("disk");
    let _ = remove_dir_all(&disk);
    create_dir(&disk).unwrap();

    build_dir(&disk, &file_structure::STRUCTURE);

    fs::write(disk.join("var/hello.txt"), "Hello, Muffin OS!\n").unwrap();

    disk
}

fn build_dir(current_path: &Path, current_dir: &Dir<'_>) {
    for file in current_dir.files {
        let file_path = current_path.join(file.name);
        match file.kind {
            Kind::Executable => {
                let env_var = format!("CARGO_BIN_FILE_{}_{}", file.name.to_uppercase(), file.name);
                let bindep = std::env::var_os(&env_var).unwrap_or_else(|| {
                    panic!("could not find the bindep {env_var} in the environment variables")
                });
                copy(&bindep, &file_path).unwrap();
            }
            Kind::Resource => {
                todo!("copy resource into the disk image");
            }
        }
    }

    for subdir in current_dir.subdirs {
        let subdir_path = current_path.join(subdir.name);
        create_dir(&subdir_path).unwrap();

        build_dir(&subdir_path, subdir);
    }
}

fn ovmf() -> Prebuilt {
    Prebuilt::fetch(Source::LATEST, PathBuf::from("target/ovmf")).unwrap()
}

fn build_iso(limine_checkout: impl AsRef<Path>, kernel_binary: impl AsRef<Path>) -> PathBuf {
    let limine_checkout = limine_checkout.as_ref();
    let kernel_binary = kernel_binary.as_ref();

    let out_dir = out_dir();

    let iso_dir = out_dir.join("iso_root");
    let boot_dir = iso_dir.join("boot");
    let limine_dir = boot_dir.join("limine");
    create_dir_all(&limine_dir).unwrap();
    let efi_boot_dir = iso_dir.join("EFI/BOOT");
    create_dir_all(&efi_boot_dir).unwrap();

    let project_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    let limine_conf_name = "limine.conf";
    let limine_conf = project_dir.join(limine_conf_name);

    copy(limine_conf, limine_dir.join(limine_conf_name)).unwrap();

    // copy the kernel binary to the location that is specified in limine.conf
    copy(kernel_binary, boot_dir.join("kernel")).unwrap();

    // the following is x86_64 specific

    for path in [
        "limine-bios.sys",
        "limine-bios-cd.bin",
        "limine-uefi-cd.bin",
    ] {
        let from = limine_checkout.join(path);
        let to = limine_dir.join(path);
        copy(&from, &to).unwrap_or_else(|_| {
            panic!(
                "should be able to copy {} to {}",
                from.display(),
                to.display()
            )
        });
    }

    for path in ["BOOTX64.EFI", "BOOTIA32.EFI"] {
        let from = limine_checkout.join(path);
        let to = efi_boot_dir.join(path);
        copy(from, to).unwrap();
    }

    let output_iso = out_dir.join("muffin.iso");

    let status = std::process::Command::new("xorriso")
        .arg("-as")
        .arg("mkisofs")
        .arg("-b")
        .arg(
            limine_dir
                .join("limine-bios-cd.bin")
                .strip_prefix(&iso_dir)
                .unwrap(),
        )
        .arg("-no-emul-boot")
        .arg("-boot-load-size")
        .arg("4")
        .arg("-boot-info-table")
        .arg("--efi-boot")
        .arg(
            limine_dir
                .join("limine-uefi-cd.bin")
                .strip_prefix(&iso_dir)
                .unwrap(),
        )
        .arg("-efi-boot-part")
        .arg("--efi-boot-image")
        .arg("--protective-msdos-label")
        .arg(iso_dir)
        .arg("-o")
        .arg(&output_iso)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .unwrap();
    assert!(status.success());

    let status = std::process::Command::new(limine_checkout.join("limine"))
        .arg("bios-install")
        .arg(&output_iso)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .unwrap();
    assert!(status.success());

    output_iso
}

fn limine() -> PathBuf {
    let limine_dir = PathBuf::from("target/limine");

    // check whether we've already checked it out
    if exists(&limine_dir).unwrap() {
        return limine_dir;
    }

    // check out
    let status = std::process::Command::new("git")
        .arg("clone")
        .arg("https://github.com/limine-bootloader/limine.git")
        .arg("--branch=v9.x-binary")
        .arg("--depth=1")
        .arg(&limine_dir)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .unwrap();
    assert!(status.success());

    // build
    let status = std::process::Command::new("make")
        .current_dir(&limine_dir)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .unwrap();
    assert!(status.success());

    limine_dir
}

fn out_dir() -> PathBuf {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    PathBuf::from(out_dir)
}
