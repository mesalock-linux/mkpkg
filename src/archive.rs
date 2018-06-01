use tar;
use libflate::gzip::Decoder;
use bzip2::bufread::BzDecoder;
use tempfile;

use std::fs::File;
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

    #[fail(display = "could not create decoder: {}", _0)]
    Decoder(#[cause] io::Error),

    #[fail(display = "could not decompress file: {}", _0)]
    Decompress(#[cause] io::Error),

    #[fail(display = "could not extract contents of '{}': {}", _0, _1)]
    Extract(String, #[cause] io::Error),

    #[fail(display = "{}", _0)]
    Package(#[cause] PackageError),
}

pub struct Archiver {

}

// XXX: when given source with .tar.gz/.tgz files, try to extract them to srcdir
// XXX: if we try to build in a container, maybe extract to separately writable dirs or something?

trait CompDecoder<R: BufRead>: Sized + Read {
    fn create(reader: R) -> Result<Self, ArchiveError>;
}

impl<R: BufRead> CompDecoder<R> for Decoder<R> {
    fn create(reader: R) -> Result<Self, ArchiveError> {
        Self::new(reader).map_err(|e| ArchiveError::Decoder(e))
    }
}

impl<R: BufRead> CompDecoder<R> for BzDecoder<R> {
    fn create(reader: R) -> Result<Self, ArchiveError> {
        Ok(Self::new(reader))
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
                let mut slice = filename.as_bytes();
                let (is_tar, is_gz, is_bzip2) = {
                    if slice.ends_with(b".tgz") {
                        (true, true, false)
                    } else if slice.ends_with(b".tbz") {
                        (true, false, true)
                    } else {
                        let (gz, bzip2) = if slice.ends_with(b".gz") {
                            slice = &slice[0..slice.len() - 3];
                            (true, false)
                        } else if slice.ends_with(b".bz2") {
                            slice = &slice[0..slice.len() - 4];
                            (false, true)
                        } else {
                            (false, false)
                        };

                        (slice.ends_with(b".tar"), gz, bzip2)
                    }
                };

                let decompressed = if is_gz {
                    // decompress using gzip (libflate)
                    // FIXME: libflate is very slow compared to the system gzip
                    Some(self.decompress::<Decoder<_>>(&build_path)?)
                } else if is_bzip2 {
                    // decompress using bzip2
                    Some(self.decompress::<BzDecoder<_>>(&build_path)?)
                } else {
                    None
                };

                if is_tar {
                    // extract everything with tar
                    let file = match decompressed {
                        Some(file) => file,
                        None => File::open(&build_path).map_err(|e| ArchiveError::OpenFile(path_to_string(&build_path), e))?,
                    };
                    let mut reader = BufReader::new(file);
                    
                    let mut archive = tar::Archive::new(reader);
                    // FIXME: extract to correct path
                    archive.unpack("test").map_err(|e| ArchiveError::Extract(path_to_string(&build_path), e))?;
                }
            }
        }

        Ok(())
    }

    fn decompress<T: CompDecoder<BufReader<File>>>(&self, build_path: &Path) -> Result<File, ArchiveError> {
        let mut file = tempfile::tempfile().map_err(|e| ArchiveError::TempFile(e))?;
        {
            let mut writer = BufWriter::new(&mut file);

            let input = File::open(build_path).map_err(|e| ArchiveError::OpenFile(path_to_string(build_path), e))?;
            let reader = BufReader::new(input);

            let mut decoder = T::create(reader)?;

            io::copy(&mut decoder, &mut writer).map_err(|e| ArchiveError::Decompress(e))?;
        }

        file.seek(SeekFrom::Start(0)).map_err(|e| ArchiveError::Decompress(e))?;

        Ok(file)
    }
}
