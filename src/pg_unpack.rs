//!
//! Unpack postgresql binaries
//!
use std::path::PathBuf;

use archiver_rs::{Archive, Compressed};
use futures::TryFutureExt;

use crate::pg_errors::{PgEmbedError, PgEmbedErrorType};
use crate::pg_types::PgResult;

///
/// Unzip the postgresql txz file
///
/// Returns `Ok(PathBuf(txz_file_path))` file path of the txz archive on success, otherwise returns an error.
///
fn unzip_txz(zip_file_path: &PathBuf, cache_dir: &PathBuf) -> Result<PathBuf, PgEmbedError> {
    let mut zip = archiver_rs::Zip::open(zip_file_path.as_path()).map_err(|e| PgEmbedError {
        error_type: PgEmbedErrorType::ReadFileError,
        source: Some(Box::new(e)),
        message: None,
    })?;
    let file_name = zip
        .files()
        .map_err(|e| PgEmbedError {
            error_type: PgEmbedErrorType::UnpackFailure,
            source: Some(Box::new(e)),
            message: None,
        })?
        .into_iter()
        .find(|name| name.ends_with(".txz"));
    match file_name {
        Some(file_name) => {
            // decompress zip
            let mut target_path = cache_dir.clone();
            target_path.push(&file_name);
            zip.extract_single(&target_path.as_path(), file_name.clone())
                .map_err(|e| PgEmbedError {
                    error_type: PgEmbedErrorType::UnpackFailure,
                    source: Some(Box::new(e)),
                    message: None,
                })?;
            Ok(target_path)
        }
        None => Err(PgEmbedError {
            error_type: PgEmbedErrorType::InvalidPgPackage,
            source: None,
            message: Some(String::from("no postgresql txz in zip")),
        }),
    }
}

///
/// Decompress the postgresql txz file
///
/// Returns `Ok(PathBuf(tar_file_path))` (*the file path to the postgresql tar file*) on success, otherwise returns an error.
///
fn decompress_xz(file_path: &PathBuf) -> Result<PathBuf, PgEmbedError> {
    let mut xz = archiver_rs::Xz::open(file_path.as_path()).map_err(|e| PgEmbedError {
        error_type: PgEmbedErrorType::ReadFileError,
        source: Some(Box::new(e)),
        message: None,
    })?;
    // rename file path suffix from .txz to .tar
    let target_path = file_path.with_extension(".tar");
    xz.decompress(&target_path.as_path())
        .map_err(|e| PgEmbedError {
            error_type: PgEmbedErrorType::UnpackFailure,
            source: Some(Box::new(e)),
            message: None,
        })?;
    Ok(target_path)
}

///
/// Unpack the postgresql tar file
///
/// Returns `Ok(())` on success, otherwise returns an error.
///
fn decompress_tar(file_path: &PathBuf, cache_dir: &PathBuf) -> Result<(), PgEmbedError> {
    let mut tar = archiver_rs::Tar::open(&file_path.as_path()).map_err(|e| PgEmbedError {
        error_type: PgEmbedErrorType::ReadFileError,
        source: Some(Box::new(e)),
        message: None,
    })?;

    tar.extract(cache_dir.as_path()).map_err(|e| PgEmbedError {
        error_type: PgEmbedErrorType::UnpackFailure,
        source: Some(Box::new(e)),
        message: None,
    })?;

    Ok(())
}

///
/// Unpack the postgresql executables
///
/// Returns `Ok(())` on success, otherwise returns an error.
///
pub async fn unpack_postgres(zip_file_path: &PathBuf, cache_dir: &PathBuf) -> PgResult<()> {
    let txz_file_path = unzip_txz(&zip_file_path, &cache_dir)?;
    let tar_file_path = decompress_xz(&txz_file_path)?;
    tokio::fs::remove_file(txz_file_path)
        .map_err(|e| PgEmbedError {
            error_type: PgEmbedErrorType::PgCleanUpFailure,
            source: Some(Box::new(e)),
            message: None,
        })
        .await?;
    let extraction_result = decompress_tar(&tar_file_path, &cache_dir);
    // The tar is removed whatever the extraction result, so a failure does not leave a
    // large intermediate behind.
    let cleanup_result = tokio::fs::remove_file(tar_file_path)
        .map_err(|e| PgEmbedError {
            error_type: PgEmbedErrorType::PgCleanUpFailure,
            source: Some(Box::new(e)),
            message: None,
        })
        .await;
    match (extraction_result, cleanup_result) {
        (Err(extraction_error), cleanup_result) => {
            // The extraction error is the root cause; a cleanup error must not mask it.
            if let Err(cleanup_error) = cleanup_result {
                log::warn!(
                    "could not remove the postgres tar file after a failed extraction: {}",
                    cleanup_error
                );
            }
            Err(extraction_error)
        }
        (Ok(()), cleanup_result) => cleanup_result,
    }
}

/// In-memory fixtures shared with the postgres.rs tests: a zip holding a `.txz` holding a tar.
#[cfg(test)]
pub(crate) mod test_archives {
    use std::io::Write;

    /// Size of the second tar entry's claimed data; large enough that cutting the tar inside
    /// it cannot land on an entry boundary (which the tar crate reads as a clean end).
    const SECOND_ENTRY_CLAIMED_BYTES: usize = 100_000;

    fn append_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, contents: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, path, contents).unwrap();
    }

    /// `corrupt` cuts the tar off mid-data of the second entry, after `bin/initdb` was
    /// completely written, mimicking an extraction that fails part way through.
    pub(crate) fn postgres_zip(corrupt: bool) -> Vec<u8> {
        let initdb_path = format!("bin/initdb{}", std::env::consts::EXE_SUFFIX);
        let mut builder = tar::Builder::new(Vec::new());
        append_file(&mut builder, &initdb_path, b"fake initdb");
        let second_entry = vec![7u8; SECOND_ENTRY_CLAIMED_BYTES];
        append_file(&mut builder, "lib/libfake", &second_entry);
        let mut tar_bytes = builder.into_inner().unwrap();
        if corrupt {
            // Two 512-byte blocks for the first entry, one header block, then a sliver of data.
            tar_bytes.truncate(512 * 3 + 1000);
        }

        let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
        encoder.write_all(&tar_bytes).unwrap();
        let txz_bytes = encoder.finish().unwrap();

        let mut zip_writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip_writer
            .start_file(
                "postgres-darwin-x86_64.txz",
                zip::write::FileOptions::default(),
            )
            .unwrap();
        zip_writer.write_all(&txz_bytes).unwrap();
        zip_writer.finish().unwrap().into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files_with_extension(dir: &std::path::Path, extension: &str) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().ends_with(extension))
            .collect()
    }

    #[tokio::test]
    async fn unpack_postgres_reports_tar_extraction_failure_and_removes_tar() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        let zip_file_path = cache_dir.join("binaries.zip");
        std::fs::write(&zip_file_path, test_archives::postgres_zip(true)).unwrap();

        let error = unpack_postgres(&zip_file_path, &cache_dir)
            .await
            .unwrap_err();
        assert_eq!(
            error.error_type,
            PgEmbedErrorType::UnpackFailure,
            "{:?}",
            error
        );
        // The premise of the postgres.rs test: the failure leaves a partial tree holding initdb.
        assert!(cache_dir
            .join("bin")
            .join(format!("initdb{}", std::env::consts::EXE_SUFFIX))
            .exists());
        // decompress_xz names the intermediate `<name>..tar` (with_extension(".tar")).
        assert!(files_with_extension(&cache_dir, ".tar").is_empty());
        assert!(files_with_extension(&cache_dir, ".txz").is_empty());
    }

    #[tokio::test]
    async fn unpack_postgres_unpacks_a_good_archive_and_removes_intermediates() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        let zip_file_path = cache_dir.join("binaries.zip");
        std::fs::write(&zip_file_path, test_archives::postgres_zip(false)).unwrap();

        unpack_postgres(&zip_file_path, &cache_dir).await.unwrap();
        let initdb = cache_dir
            .join("bin")
            .join(format!("initdb{}", std::env::consts::EXE_SUFFIX));
        assert!(initdb.exists());
        assert!(cache_dir.join("lib").join("libfake").exists());
        assert!(files_with_extension(&cache_dir, ".tar").is_empty());
        assert!(files_with_extension(&cache_dir, ".txz").is_empty());
    }
}
