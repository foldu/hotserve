#!/usr/bin/env run-cargo-script
use std::{
    fs, io,
    os::unix::fs::{symlink, MetadataExt},
    path::{Path, PathBuf},
    process::{exit, Command},
};

fn is_same_file<P, Q>(a: P, b: Q) -> io::Result<bool>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let (a, b) = (fs::metadata(a)?, fs::metadata(b)?);
    let ino_dev = |meta: fs::Metadata| (meta.ino(), meta.dev());

    Ok(ino_dev(a) == ino_dev(b))
}

fn main() {
    let root = String::from_utf8(
        Command::new("git")
            .arg("rev-parse")
            .arg("--show-toplevel")
            .output()
            .expect("Git not installed")
            .stdout,
    ).unwrap()
    .trim()
    .to_owned();

    if !is_same_file(root, ".").unwrap() {
        eprintln!("Not in git root");
        exit(1);
    }

    fs::read_dir("hooks")
        .expect("No hook dir found")
        .filter_map(|hook| hook.ok())
        .map(|ent| ent.path())
        .filter(|path| !path.ends_with("install.rs"))
        .for_each(|path| {
            let target = PathBuf::from(".git/hooks").join(path.file_stem().unwrap());
            let relative_path = Path::new("../../").join(&path);
            if let Err(e) = symlink(&relative_path, &target) {
                match e.kind() {
                    io::ErrorKind::AlreadyExists => {}
                    _ => {
                        eprintln!("Can't symlink {:?} to {:?}: {}", path, target, e);
                    }
                }
            }
            println!("{:?} -> {:?}", path, target);
        });
}
