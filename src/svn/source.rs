use std::ffi::OsString;
use std::io::{Read as _, Seek as _};
use std::path::PathBuf;

use crate::pipe;

#[derive(Debug)]
pub(crate) enum OpenError {
    MetadataFetchError {
        path: PathBuf,
        error: std::io::Error,
    },
    FileOpenError {
        path: PathBuf,
        error: std::io::Error,
    },
    FileReadError {
        path: PathBuf,
        error: std::io::Error,
    },
    FileSeekError {
        path: PathBuf,
        error: std::io::Error,
    },
    SpawnProcessError {
        arg0: OsString,
        error: std::io::Error,
    },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MetadataFetchError { path, error } => {
                write!(f, "failed to fetch metadata for {path:?}: {error}")
            }
            Self::FileOpenError { path, error } => {
                write!(f, "failed to open file {path:?}: {error}")
            }
            Self::FileReadError { path, error } => {
                write!(f, "failed to read file {path:?}: {error}")
            }
            Self::FileSeekError { path, error } => {
                write!(f, "failed to seek file {path:?}: {error}")
            }
            Self::SpawnProcessError { arg0, error } => {
                write!(f, "failed to spawn process {arg0:?}: {error}")
            }
        }
    }
}

pub(crate) enum DumpSource {
    ThreadPipe(
        std::thread::JoinHandle<Result<(), std::io::Error>>,
        std::io::BufReader<pipe::PipeReader>,
    ),
    Command(
        std::process::Child,
        std::io::BufReader<std::process::ChildStdout>,
    ),
}

impl DumpSource {
    pub(crate) fn open(path: &std::path::Path) -> Result<Self, OpenError> {
        let path_meta = std::fs::metadata(path).map_err(|e| OpenError::MetadataFetchError {
            path: path.to_path_buf(),
            error: e,
        })?;
        if path_meta.file_type().is_dir() {
            let mut child = std::process::Command::new("svnadmin")
                .arg("dump")
                .arg(path)
                .arg("-q")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .map_err(|e| OpenError::SpawnProcessError {
                    arg0: "svnadmin".into(),
                    error: e,
                })?;
            let stdout = child.stdout.take().unwrap();
            Ok(Self::Command(child, std::io::BufReader::new(stdout)))
        } else {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(|e| OpenError::FileOpenError {
                    path: path.to_path_buf(),
                    error: e,
                })?;

            const ZSTD_MAGIC: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
            const GZIP_MAGIC: &[u8] = &[0x1F, 0x8B];
            const BZIP2_MAGIC: &[u8] = b"BZh";
            const XZ_MAGIC: &[u8] = &[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00];
            const LZ4_MAGIC: &[u8] = &[0x04, 0x22, 0x4D, 0x18];

            const HEADER_SIZE: usize = 6;

            let mut header = Vec::<u8>::with_capacity(HEADER_SIZE);
            while header.len() < HEADER_SIZE {
                let mut buf = [0; HEADER_SIZE];
                match file.read(&mut buf[..(HEADER_SIZE - header.len())]) {
                    Ok(0) => break,
                    Ok(n) => header.extend(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        return Err(OpenError::FileReadError {
                            path: path.to_path_buf(),
                            error: e,
                        });
                    }
                }
            }

            file.seek(std::io::SeekFrom::Start(0))
                .map_err(|e| OpenError::FileSeekError {
                    path: path.to_path_buf(),
                    error: e,
                })?;

            let (pipe_read, mut pipe_write) = pipe::create();

            let joiner = std::thread::Builder::new()
                .name("svn source".into())
                .spawn(move || {
                    if header.starts_with(ZSTD_MAGIC) {
                        zstd::stream::copy_decode(&file, &mut pipe_write)?;
                    } else if header.starts_with(GZIP_MAGIC) {
                        let mut decoder = flate2::read::GzDecoder::new(&file);
                        std::io::copy(&mut decoder, &mut pipe_write)?;
                    } else if header.starts_with(BZIP2_MAGIC) {
                        let mut decoder = bzip2::read::BzDecoder::new(&file);
                        std::io::copy(&mut decoder, &mut pipe_write)?;
                    } else if header.starts_with(XZ_MAGIC) {
                        liblzma::copy_decode(&file, &mut pipe_write)?;
                    } else if header.starts_with(LZ4_MAGIC) {
                        let mut decoder = lz4_flex::frame::FrameDecoder::new(&file);
                        std::io::copy(&mut decoder, &mut pipe_write)?;
                    } else {
                        std::io::copy(&mut file, &mut pipe_write)?;
                    }
                    Ok(())
                })
                .expect("failed to spawn thread");

            Ok(Self::ThreadPipe(joiner, std::io::BufReader::new(pipe_read)))
        }
    }

    pub(crate) fn close(self) -> Result<(), std::io::Error> {
        match self {
            Self::ThreadPipe(joiner, pipe) => {
                drop(pipe);
                match joiner.join() {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(e) => {
                        std::panic::resume_unwind(e);
                    }
                }
            }
            Self::Command(mut child, _) => {
                let exit_code = child.wait()?;
                if exit_code.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other(format!(
                        "process finished code {exit_code}"
                    )))
                }
            }
        }
    }

    pub(crate) fn stream(&mut self) -> &mut dyn std::io::BufRead {
        match self {
            Self::ThreadPipe(_, pipe) => pipe,
            Self::Command(_, stdout) => stdout,
        }
    }
}
