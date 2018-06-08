use bzip2::bufread::BzDecoder;
use flate2::bufread::GzDecoder;
use tar;
use tempfile;
use xz2::bufread::{XzDecoder, XzEncoder};

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use config::Config;
use package::{BuildFile, PackageError};
use util::{self, path_to_string, UtilError};

#[derive(Debug, Fail)]
pub enum ArchiveError {
    #[fail(display = "could not create temporary file: {}", _0)]
    TempFile(#[cause] io::Error),

    #[fail(display = "could not open '{}': {}", _0, _1)]
    OpenFile(String, #[cause] io::Error),

    #[fail(display = "could not seek to beginning of '{}': {}", _0, _1)]
    Seek(String, #[cause] io::Error),

    #[fail(display = "could not create file '{}': {}", _0, _1)]
    CreateFile(String, #[cause] io::Error),

    #[fail(display = "could not decompress file: {}", _0)]
    Decompress(#[cause] io::Error),

    #[fail(display = "could not compress file: {}", _0)]
    Compress(#[cause] io::Error),

    #[fail(display = "could not extract contents of '{}': {}", _0, _1)]
    Extract(String, #[cause] io::Error),

    #[fail(display = "could not archive '{}': {}", _0, _1)]
    Archive(String, #[cause] io::Error),

    #[fail(display = "could not create directory '{}': {}", _0, _1)]
    CreateDir(String, #[cause] io::Error),

    #[fail(display = "could not remove previously extracted files at '{}': {}", _0, _1)]
    RemoveDir(String, #[cause] io::Error),

    #[fail(display = "could not remove intermediate file at '{}': {}", _0, _1)]
    RemoveFile(String, #[cause] io::Error),

    #[fail(display = "{}", _0)]
    Util(#[cause] UtilError),

    #[fail(display = "{}", _0)]
    Package(#[cause] PackageError),
}

pub struct Archiver {}

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
    const XZ_LEVEL: u32 = 6;

    pub fn new() -> Self {
        Self {}
    }

    pub fn package(&self, config: &Config, pkg: &BuildFile) -> Result<(), ArchiveError> {
        let tar_path = pkg.base_dir(config)
            .join(format!("{}-{}.tar", pkg.name(), pkg.version()));
        let mut tar_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tar_path)
            .map_err(|e| ArchiveError::OpenFile(path_to_string(&tar_path), e))?;

        {
            let mut builder = tar::Builder::new(BufWriter::new(&mut tar_file));
            builder.follow_symlinks(false);
            // XXX: set header mode?
            // XXX: do we care what type of tar file?  (default is GNU)

            let pkgdir = pkg.pkg_dir(config);
            builder
                .append_dir_all(".", &pkgdir)
                .and_then(|_| builder.finish())
                .map_err(|e| ArchiveError::Archive(path_to_string(&pkgdir), e))?;
        }

        // now compress the archive
        tar_file
            .seek(SeekFrom::Start(0))
            .map_err(|e| ArchiveError::Seek(path_to_string(&tar_path), e))?;

        let package_path =
            pkg.base_dir(config)
                .join(format!("{}-{}.tar.xz", pkg.name(), pkg.version()));
        let package_file = File::create(&package_path)
            .map_err(|e| ArchiveError::CreateFile(path_to_string(&package_path), e))?;

        io::copy(
            &mut XzEncoder::new(BufReader::new(tar_file), Self::XZ_LEVEL),
            &mut BufWriter::new(package_file),
        ).map_err(|e| ArchiveError::Compress(e))?;

        fs::remove_file(&tar_path)
            .map_err(|e| ArchiveError::RemoveFile(path_to_string(&tar_path), e))?;

        Ok(())
    }

    // XXX: maybe should just create all necessary directories up-front (like a
    //      pkg.init_dirs(config) before calling download, build, etc.)
    pub fn extract(&self, config: &Config, pkg: &BuildFile) -> Result<(), ArchiveError> {
        let target_path = pkg.archive_out_dir(config);
        if target_path.exists() {
            fs::remove_dir_all(&target_path)
                .map_err(|e| ArchiveError::RemoveDir(path_to_string(&target_path), e))?;
        }
        fs::create_dir(&target_path)
            .map_err(|e| ArchiveError::CreateDir(path_to_string(&target_path), e))?;

        if pkg.skip_extract() {
            return Ok(());
        }

        for src in pkg.source() {
            let build_path = pkg.file_download_path(config, src)
                .map_err(|e| ArchiveError::Package(e))?;

            if let Some(filename) = build_path.file_name() {
                const IS_TAR: &[&[u8]] = &[
                    b".tar.gz",
                    b".tar.bz2",
                    b".tar.xz",
                    b".tgz",
                    b".tbz",
                    b".txz",
                ];
                const IS_GZ: &[&[u8]] = &[b".gz", b".tgz"];
                const IS_BZIP2: &[&[u8]] = &[b".bz2", b".tbz"];
                const IS_XZ: &[&[u8]] = &[b".xz", b".txz"];

                let filename = filename.as_bytes();
                let mut found = false;

                let res = self.try_extraction(filename, &build_path, IS_GZ, |path| {
                    self.decompress::<GzDecoder<_>>(path)
                }).or_else(|| {
                        self.try_extraction(filename, &build_path, IS_BZIP2, |path| {
                            self.decompress::<BzDecoder<_>>(path)
                        })
                    })
                    .or_else(|| {
                        self.try_extraction(filename, &build_path, IS_XZ, |path| {
                            self.decompress::<XzDecoder<_>>(path)
                        })
                    });

                // should probably use .transpose() when that is stable
                let file = match res {
                    Some(Ok(file)) => {
                        found = true;
                        Some(file)
                    }
                    Some(Err(f)) => return Err(f),
                    None => None,
                };

                if let Some(res) = self.try_extraction(filename, &build_path, IS_TAR, |path| {
                    self.extract_tar(config, pkg, path, file)
                }) {
                    res?;
                    found = true;
                }

                if !found {
                    // move the file/directory into place even though it wasn't extracted
                    util::copy_dir(&build_path, &pkg.archive_out_dir(config))
                        .map_err(|e| ArchiveError::Util(e))?
                }
            }
        }

        Ok(())
    }

    fn try_extraction<F: FnOnce(&Path) -> Result<File, ArchiveError>>(
        &self,
        filename: &[u8],
        build_path: &Path,
        exts: &[&[u8]],
        action: F,
    ) -> Option<Result<File, ArchiveError>> {
        for ext in exts {
            if filename.ends_with(ext) {
                return Some(action(build_path));
            }
        }
        None
    }

    fn extract_tar(
        &self,
        config: &Config,
        pkg: &BuildFile,
        build_path: &Path,
        file: Option<File>,
    ) -> Result<File, ArchiveError> {
        let mut file = match file {
            Some(file) => file,
            None => File::open(build_path)
                .map_err(|e| ArchiveError::OpenFile(path_to_string(&build_path), e))?,
        };
        {
            let reader = BufReader::new(&mut file);

            let mut archive = tar::Archive::new(reader);
            // FIXME: extract to correct path (it might make sense to extract to a directory based on the name of the file
            //        in case two files that were downloaded conflict, but this seems unlikely to occur)
            let target_path = pkg.archive_out_dir(config);

            // XXX: do we care about permissions here?  most likely we only care when we are installing for real
            archive.set_preserve_permissions(true);
            archive.set_unpack_xattrs(true);
            archive
                .unpack(&target_path)
                .map_err(|e| ArchiveError::Extract(path_to_string(build_path), e))?;
        }

        Ok(file)
    }

    fn decompress<T>(&self, build_path: &Path) -> Result<File, ArchiveError>
    where
        T: CompDecoder<BufReader<File>>,
    {
        let mut file = tempfile::tempfile().map_err(|e| ArchiveError::TempFile(e))?;
        {
            let mut writer = BufWriter::new(&mut file);

            let input = File::open(build_path)
                .map_err(|e| ArchiveError::OpenFile(path_to_string(build_path), e))?;
            let reader = BufReader::new(input);

            let mut decoder = T::create(reader);

            io::copy(&mut decoder, &mut writer).map_err(|e| ArchiveError::Decompress(e))?;
        }

        file.seek(SeekFrom::Start(0))
            .map_err(|e| ArchiveError::Decompress(e))?;

        Ok(file)
    }
}
