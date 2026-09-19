#![allow(dead_code)]

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
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

/// Extract a tar.lzma archive into `destination`.
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
        return decompress_7z(archive, destination);
    }

    let temporary = destination.join(".hok-7z-extract");
    if temporary.exists() {
        std::fs::remove_dir_all(&temporary)?;
    }
    std::fs::create_dir_all(&temporary)?;

    let result = (|| {
        decompress_7z(archive, &temporary)?;
        move_archive_subdirectory(&temporary, destination, strip_dir.unwrap())
    })();

    let _ = std::fs::remove_dir_all(temporary);
    result
}

/// Decompress a plain 7z file or a self-extracting executable containing a 7z payload.
fn decompress_7z<P, Q>(archive: P, destination: Q) -> Fallible<()>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let mut file = File::open(archive.as_ref())?;
    let offset = find_7z_signature(&mut file)?.ok_or_else(|| {
        Error::Custom(format!(
            "7z signature not found in '{}'",
            archive.as_ref().display()
        ))
    })?;
    let reader = OffsetReader::new(file, offset)?;
    sevenz_rust2::decompress(reader, destination).map_err(sevenz_error)
}

const SEVEN_Z_SIGNATURE: &[u8] = &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];

fn find_7z_signature<R: Read + Seek>(reader: &mut R) -> io::Result<Option<u64>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut buffer = [0u8; 64 * 1024];
    let mut carry = Vec::new();
    let mut consumed = 0u64;

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(None);
        }

        let mut candidate = Vec::with_capacity(carry.len() + count);
        candidate.extend_from_slice(&carry);
        candidate.extend_from_slice(&buffer[..count]);
        if let Some(index) = candidate
            .windows(SEVEN_Z_SIGNATURE.len())
            .position(|window| window == SEVEN_Z_SIGNATURE)
        {
            return Ok(Some(consumed - carry.len() as u64 + index as u64));
        }

        consumed += count as u64;
        let carry_len = (SEVEN_Z_SIGNATURE.len() - 1).min(candidate.len());
        carry.clear();
        carry.extend_from_slice(&candidate[candidate.len() - carry_len..]);
    }
}

struct OffsetReader<R> {
    inner: R,
    offset: u64,
}

impl<R: Seek> OffsetReader<R> {
    fn new(mut inner: R, offset: u64) -> io::Result<Self> {
        inner.seek(SeekFrom::Start(offset))?;
        Ok(Self { inner, offset })
    }
}

impl<R: Read> Read for OffsetReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl<R: Seek> Seek for OffsetReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let absolute = match position {
            SeekFrom::Start(position) => {
                self.inner.seek(SeekFrom::Start(self.offset + position))?
            }
            SeekFrom::End(position) => self.inner.seek(SeekFrom::End(position))?,
            SeekFrom::Current(position) => self.inner.seek(SeekFrom::Current(position))?,
        };
        absolute.checked_sub(self.offset).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "attempted to seek before embedded 7z payload",
            )
        })
    }
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

#[cfg(test)]
mod tests {
    use super::{find_7z_signature, OffsetReader, SEVEN_Z_SIGNATURE};
    use std::io::{Cursor, Read, Seek, SeekFrom};

    #[test]
    fn finds_embedded_7z_signature() {
        let mut bytes = vec![0x4D, 0x5A, 0x90, 0x00];
        bytes.extend_from_slice(SEVEN_Z_SIGNATURE);
        let mut reader = Cursor::new(bytes);
        assert_eq!(find_7z_signature(&mut reader).unwrap(), Some(4));
    }

    #[test]
    fn offset_reader_maps_archive_positions() {
        let mut reader = OffsetReader::new(Cursor::new(b"stubpayload".to_vec()), 4).unwrap();
        let mut payload = String::new();
        reader.read_to_string(&mut payload).unwrap();
        assert_eq!(payload, "payload");
        assert_eq!(reader.seek(SeekFrom::Start(0)).unwrap(), 0);
    }
}
