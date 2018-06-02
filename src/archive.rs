use tar;
use flate2::bufread::GzDecoder;
use bzip2::bufread::BzDecoder;
use xz2::bufread::XzDecoder;
use tempfile;

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use config::Config;
use package::{BuildFile, PackageError};
use util::path_to_string;

#[derive(Debug, Fail)]
pub enum ArchiveError {
    #[fail(display = "could not create temporary file: {}", _0)]
    TempFile(#[cause] io::Error),

    #[fail(display = "could not open '{}': {}", _0, _1)]
    OpenFile(String, #[cause] io::Error),

    #[fail(display = "could not decompress file: {}", _0)]
    Decompress(#[cause] io::Error),

    #[fail(display = "could not extract contents of '{}': {}", _0, _1)]
    Extract(String, #[cause] io::Error),

    #[fail(display = "could not remove previously extracted files at '{}': {}", _0, _1)]
    RemoveDir(String, #[cause] io::Error),

    #[fail(display = "{}", _0)]
    Package(#[cause] PackageError),
}

pub struct Archiver {

}

// XXX: when given source with .tar.gz/.tgz files, try to extract them to srcdir
// XXX: if we try to build in a container, maybe extract to separately writable dirs or something?

trait CompDecoder<R: BufRead>: Sized + Read {
    fn create(reader: R) -> Self;
}

impl<R: BufRead> CompDecoder<R> for GzDecoder<R> {
    fn create(reader: R) -> Self {
        Self::new(reader)
    }
}

impl<R: BufRead> CompDecoder<R> for BzDecoder<R> {
    fn create(reader: R) -> Self {
        Self::new(reader)
    }
}

impl<R: BufRead> CompDecoder<R> for XzDecoder<R> {
    fn create(reader: R) -> Self {
        Self::new(reader)
    }
}

impl Archiver {
    pub fn new() -> Self {
        Self { }
    }

    pub(crate) fn extract(&self, config: &Config, pkg: &BuildFile) -> Result<(), ArchiveError> {
        for src in pkg.source() {
            let build_path = pkg.file_build_path(config, src).map_err(|e| ArchiveError::Package(e))?;

            if let Some(filename) = build_path.file_name() {
                const IS_TAR: &[&[u8]] = &[b".tar.gz", b".tar.bz2", b".tar.xz", b".tgz", b".tbz", b".txz"];
                const IS_GZ: &[&[u8]] = &[b".gz", b".tgz"];
                const IS_BZIP2: &[&[u8]] = &[b".bz2", b".tbz"];
                const IS_XZ: &[&[u8]] = &[b".xz", b".txz"];

                let filename = filename.as_bytes();

                let res = self.try_extraction(filename, &build_path, IS_GZ, |path| self.decompress::<GzDecoder<_>>(path))
                    .or_else(|| self.try_extraction(filename, &build_path, IS_BZIP2, |path| self.decompress::<BzDecoder<_>>(path)))
                    .or_else(|| self.try_extraction(filename, &build_path, IS_XZ, |path| self.decompress::<XzDecoder<_>>(path)));

                // should probably use .transpose() when that is stable
                let file = match res {
                    Some(Ok(file)) => Some(file),
                    Some(Err(f)) => return Err(f),
                    None => None,
                };

                if let Some(res) = self.try_extraction(filename, &build_path, IS_TAR, |path| self.extract_tar(config, pkg, path, file)) {
                    res?;
                }
            }
        }

        Ok(())
    }

    fn try_extraction<F: FnOnce(&Path) -> Result<File, ArchiveError>>(&self, filename: &[u8], build_path: &Path, exts: &[&[u8]], action: F) -> Option<Result<File, ArchiveError>> {
        for ext in exts {
            if filename.ends_with(ext) {
                return Some(action(build_path));
            }
        }
        None
    }

    fn extract_tar(&self, config: &Config, pkg: &BuildFile, build_path: &Path, file: Option<File>) -> Result<File, ArchiveError> {
        let mut file = match file {
            Some(file) => file,
            None => File::open(&build_path).map_err(|e| ArchiveError::OpenFile(path_to_string(&build_path), e))?
        };
        {
            let reader = BufReader::new(&mut file);

            let mut archive = tar::Archive::new(reader);
            // FIXME: extract to correct path (it might make sense to extract to a directory based on the name of the file
            //        in case two files that were downloaded conflict, but this seems unlikely to occur)
            let target_path = pkg.archive_out_dir(config);
            if target_path.exists() {
                fs::remove_dir_all(&target_path).map_err(|e| ArchiveError::RemoveDir(path_to_string(&target_path), e))?;
            }
            archive.unpack(&target_path).map_err(|e| ArchiveError::Extract(path_to_string(&target_path), e))?;
        }

        Ok(file)
    }

    fn decompress<T: CompDecoder<BufReader<File>>>(&self, build_path: &Path) -> Result<File, ArchiveError> {
        let mut file = tempfile::tempfile().map_err(|e| ArchiveError::TempFile(e))?;
        {
            let mut writer = BufWriter::new(&mut file);

            let input = File::open(build_path).map_err(|e| ArchiveError::OpenFile(path_to_string(build_path), e))?;
            let reader = BufReader::new(input);

            let mut decoder = T::create(reader);

            io::copy(&mut decoder, &mut writer).map_err(|e| ArchiveError::Decompress(e))?;
        }

        file.seek(SeekFrom::Start(0)).map_err(|e| ArchiveError::Decompress(e))?;

        Ok(file)
    }
}
