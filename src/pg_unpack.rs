//!
//! Unpack postgresql binaries
//!
use std::path::{Path, PathBuf};

use archiver_rs::{Archive, Compressed};
use futures::TryFutureExt;

use crate::pg_errors::{PgEmbedError, PgEmbedErrorType};
use crate::pg_types::PgResult;

/// Folder inside the cache dir the tar is extracted into before its entries are moved into
/// place. The cache counts as complete once `bin/initdb` exists, so extracting straight into
/// the cache dir let a process killed mid-extraction leave a partial tree every later start
/// trusted. Inside the cache dir it stays on the same volume (a rename is a metadata move)
/// and adds no path outside the folder pg-embed owns.
pub(crate) const UNPACK_STAGING_DIR_NAME: &str = ".unpack-staging";

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

fn unpack_failure(error: std::io::Error, message: String) -> PgEmbedError {
    PgEmbedError {
        error_type: PgEmbedErrorType::UnpackFailure,
        source: Some(Box::new(error)),
        message: Some(message),
    }
}

///
/// Remove a file or directory tree, treating an already absent path as removed
///
/// Uses `symlink_metadata` so a symlink is removed itself and never followed.
///
fn remove_path_if_present(path: &Path) -> PgResult<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(unpack_failure(
                error,
                format!("could not inspect {}", path.display()),
            ))
        }
    };
    let removal = if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removal {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(unpack_failure(
            error,
            format!("could not remove {}", path.display()),
        )),
        _ => Ok(()),
    }
}

///
/// Move every top-level entry of the extracted staging tree into the cache dir, then remove
/// the empty staging dir
///
/// Returns `Ok(())` on success, otherwise returns an error.
///
fn move_staged_tree_into_place(staging_dir: &Path, cache_dir: &Path) -> PgResult<()> {
    let mut entry_names = Vec::new();
    for entry in std::fs::read_dir(staging_dir)
        .map_err(|e| unpack_failure(e, format!("could not list {}", staging_dir.display())))?
    {
        let entry = entry
            .map_err(|e| unpack_failure(e, format!("could not list {}", staging_dir.display())))?;
        entry_names.push(entry.file_name());
    }
    // initdb is the only presence marker, so `bin` goes last: a kill at any earlier point
    // leaves no `cache_dir/bin/initdb` and the next start acquires again.
    entry_names.sort_by_key(|name| name == "bin");
    for entry_name in entry_names {
        let source = staging_dir.join(&entry_name);
        let destination = cache_dir.join(&entry_name);
        // Left by a run killed during this move, or by a pre-staging partial tree. A dir
        // rename onto an existing dir fails on Windows, and onto a non-empty one on Unix.
        remove_path_if_present(&destination)?;
        std::fs::rename(&source, &destination).map_err(|e| {
            unpack_failure(
                e,
                format!(
                    "could not move {} to {}",
                    source.display(),
                    destination.display()
                ),
            )
        })?;
    }
    std::fs::remove_dir(staging_dir)
        .map_err(|e| unpack_failure(e, format!("could not remove {}", staging_dir.display())))
}

///
/// Unpack the postgresql executables
///
/// Returns `Ok(())` on success, otherwise returns an error.
///
pub async fn unpack_postgres(zip_file_path: &PathBuf, cache_dir: &PathBuf) -> PgResult<()> {
    // The tar is extracted into a staging dir, out of the presence check's sight, so a process
    // killed mid-extraction leaves nothing a later start trusts. A leftover from such a kill
    // is cleared here, before it could mix with this extraction.
    let staging_dir = cache_dir.join(UNPACK_STAGING_DIR_NAME);
    remove_path_if_present(&staging_dir)?;
    let txz_file_path = unzip_txz(&zip_file_path, &cache_dir)?;
    let tar_file_path = decompress_xz(&txz_file_path)?;
    tokio::fs::remove_file(txz_file_path)
        .map_err(|e| PgEmbedError {
            error_type: PgEmbedErrorType::PgCleanUpFailure,
            source: Some(Box::new(e)),
            message: None,
        })
        .await?;
    let extraction_result = decompress_tar(&tar_file_path, &staging_dir);
    // The tar is removed whatever the extraction result, so a failure does not leave a
    // large intermediate behind. Synchronously, like the extraction and the moves: an await
    // here would let a cancellation (e.g. a timeout around setup()) drop the future between a
    // failed extraction and the caller's invalidation of the partial tree.
    let cleanup_result = std::fs::remove_file(tar_file_path).map_err(|e| PgEmbedError {
        error_type: PgEmbedErrorType::PgCleanUpFailure,
        source: Some(Box::new(e)),
        message: None,
    });
    match (extraction_result, cleanup_result) {
        (Err(extraction_error), cleanup_result) => {
            // The extraction error is the root cause; a cleanup error must not mask it. The
            // partial tree stays in the staging dir for the next acquisition to clear.
            if let Err(cleanup_error) = cleanup_result {
                log::warn!(
                    "could not remove the postgres tar file after a failed extraction: {}",
                    cleanup_error
                );
            }
            Err(extraction_error)
        }
        (Ok(()), cleanup_result) => {
            // A leftover tar does not make the extracted tree any less complete, so it is
            // moved into place even then: a PgCleanUpFailure must mean the cache dir is
            // complete, which is why the caller skips invalidating it.
            if let Err(move_error) = move_staged_tree_into_place(&staging_dir, &cache_dir) {
                if let Err(cleanup_error) = cleanup_result {
                    log::warn!(
                        "could not remove the postgres tar file after a successful extraction: {}",
                        cleanup_error
                    );
                }
                return Err(move_error);
            }
            cleanup_result
        }
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
        // The partial tree (initdb included) stays in the staging dir, out of the presence check.
        assert!(!initdb_path(&cache_dir).exists());
        // decompress_xz names the intermediate `<name>..tar` (with_extension(".tar")).
        assert!(files_with_extension(&cache_dir, ".tar").is_empty());
        assert!(files_with_extension(&cache_dir, ".txz").is_empty());
    }

    #[tokio::test]
    async fn unpack_postgres_unpacks_a_good_archive_and_removes_intermediates() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        let zip_file_path = write_good_zip(&cache_dir);

        unpack_postgres(&zip_file_path, &cache_dir).await.unwrap();
        assert_complete_tree(&cache_dir);
        assert!(files_with_extension(&cache_dir, ".tar").is_empty());
        assert!(files_with_extension(&cache_dir, ".txz").is_empty());
    }

    #[tokio::test]
    async fn unpack_postgres_clears_a_staging_dir_left_by_a_killed_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        let staging_dir = cache_dir.join(UNPACK_STAGING_DIR_NAME);
        std::fs::create_dir_all(staging_dir.join("bin")).unwrap();
        std::fs::write(initdb_path(&staging_dir), b"partial initdb").unwrap();
        std::fs::write(staging_dir.join("stale-file"), b"stale").unwrap();
        let zip_file_path = write_good_zip(&cache_dir);

        unpack_postgres(&zip_file_path, &cache_dir).await.unwrap();
        assert_complete_tree(&cache_dir);
        assert!(!cache_dir.join("stale-file").exists());
    }

    #[tokio::test]
    async fn unpack_postgres_replaces_entries_left_by_a_killed_move() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        std::fs::create_dir_all(cache_dir.join("lib")).unwrap();
        std::fs::write(cache_dir.join("lib").join("stale-file"), b"stale").unwrap();
        let staging_dir = cache_dir.join(UNPACK_STAGING_DIR_NAME);
        std::fs::create_dir_all(staging_dir.join("bin")).unwrap();
        std::fs::write(initdb_path(&staging_dir), b"fake initdb").unwrap();
        let zip_file_path = write_good_zip(&cache_dir);

        unpack_postgres(&zip_file_path, &cache_dir).await.unwrap();
        assert_complete_tree(&cache_dir);
        assert!(!cache_dir.join("lib").join("stale-file").exists());
    }

    fn initdb_path(root: &std::path::Path) -> PathBuf {
        root.join("bin")
            .join(format!("initdb{}", std::env::consts::EXE_SUFFIX))
    }

    fn write_good_zip(cache_dir: &Path) -> PathBuf {
        let zip_file_path = cache_dir.join("binaries.zip");
        std::fs::write(&zip_file_path, test_archives::postgres_zip(false)).unwrap();
        zip_file_path
    }

    fn assert_complete_tree(cache_dir: &Path) {
        assert_eq!(
            std::fs::read(initdb_path(cache_dir)).unwrap(),
            b"fake initdb"
        );
        let libfake = std::fs::metadata(cache_dir.join("lib").join("libfake")).unwrap();
        assert_eq!(libfake.len(), 100_000);
        assert!(!cache_dir.join(UNPACK_STAGING_DIR_NAME).exists());
    }
}
