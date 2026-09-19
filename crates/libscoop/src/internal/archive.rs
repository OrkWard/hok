#![allow(dead_code)]

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, Fallible};

#[derive(Debug)]
pub enum Format {
    /// .bz2
    Bz2,
    /// .gz
    Gzip,
    /// .rar
    Rar,
    /// .7z, .xz
    XZip,
    /// .tar
    Tar,
    /// .zip
    Zip,
    /// .zstd
    Zst,
}

/// Extract a ZIP archive into `destination`.
///
/// Entry paths are validated before extraction. When `strip_dir` is supplied,
/// only entries below that directory are extracted and the prefix is removed.
pub fn extract_zip<P, Q>(archive: P, destination: Q, strip_dir: Option<&str>) -> Fallible<()>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let file = File::open(archive.as_ref())?;
    let mut archive = zip::ZipArchive::new(file).map_err(zip_error)?;
    let destination = destination.as_ref();
    let strip_dir = strip_dir.map(PathBuf::from);

    std::fs::create_dir_all(destination)?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        let enclosed = entry.enclosed_name().ok_or_else(|| {
            Error::Custom(format!("unsafe path '{}' in ZIP archive", entry.name()))
        })?;

        let relative = match strip_dir.as_ref() {
            Some(prefix) => match enclosed.strip_prefix(prefix) {
                Ok(path) if !path.as_os_str().is_empty() => path,
                _ => continue,
            },
            None => enclosed,
        };
        let output = destination.join(relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&output)?;
            continue;
        }

        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = File::create(output)?;
        io::copy(&mut entry, &mut file)?;
    }

    Ok(())
}

/// Extract a 7z archive into `destination`.
pub fn extract_tar_lzma<P, Q>(archive: P, destination: Q, strip_dir: Option<&str>) -> Fallible<()>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let input = File::open(archive.as_ref())?;
    let decoder = lzma_rust2::LzmaReader::new_mem_limit(input, u32::MAX, None)?;
    extract_tar(decoder, destination.as_ref(), strip_dir)
}

fn extract_tar<R: io::Read>(
    reader: R,
    destination: &Path,
    strip_dir: Option<&str>,
) -> Fallible<()> {
    std::fs::create_dir_all(destination)?;

    if strip_dir.is_none() {
        tar::Archive::new(reader).unpack(destination)?;
        return Ok(());
    }

    let temporary = destination.join(".hok-tar-extract");
    if temporary.exists() {
        std::fs::remove_dir_all(&temporary)?;
    }
    std::fs::create_dir_all(&temporary)?;

    let result = (|| {
        tar::Archive::new(reader).unpack(&temporary)?;
        move_archive_subdirectory(&temporary, destination, strip_dir.unwrap())
    })();

    let _ = std::fs::remove_dir_all(temporary);
    result
}

pub fn extract_7z<P, Q>(archive: P, destination: Q, strip_dir: Option<&str>) -> Fallible<()>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let destination = destination.as_ref();
    std::fs::create_dir_all(destination)?;

    if strip_dir.is_none() {
        return sevenz_rust2::decompress_file(archive, destination).map_err(sevenz_error);
    }

    let temporary = destination.join(".hok-7z-extract");
    if temporary.exists() {
        std::fs::remove_dir_all(&temporary)?;
    }
    std::fs::create_dir_all(&temporary)?;

    let result = (|| {
        sevenz_rust2::decompress_file(archive, &temporary).map_err(sevenz_error)?;
        move_archive_subdirectory(&temporary, destination, strip_dir.unwrap())
    })();

    let _ = std::fs::remove_dir_all(temporary);
    result
}

fn move_archive_subdirectory(base: &Path, destination: &Path, relative: &str) -> Fallible<()> {
    let source = safe_archive_subdirectory(base, relative)?;
    if !source.is_dir() {
        return Err(Error::Custom(format!(
            "archive directory '{relative}' does not exist"
        )));
    }

    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if target.exists() {
            if target.is_dir() {
                std::fs::remove_dir_all(&target)?;
            } else {
                std::fs::remove_file(&target)?;
            }
        }
        std::fs::rename(entry.path(), target)?;
    }
    Ok(())
}

fn safe_archive_subdirectory(base: &Path, relative: &str) -> Fallible<PathBuf> {
    use std::path::Component;

    let relative = Path::new(relative);
    if relative.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err(Error::Custom(format!(
            "unsafe archive directory '{}'",
            relative.display()
        )));
    }
    Ok(base.join(relative))
}

fn zip_error(error: zip::result::ZipError) -> Error {
    Error::Custom(format!("ZIP error: {error}"))
}

fn sevenz_error(error: sevenz_rust2::Error) -> Error {
    Error::Custom(format!("7z error: {error}"))
}
