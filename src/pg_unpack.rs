//!
//! Unpack postgresql binaries
//!
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use archiver_rs::{Archive, Compressed};
use futures::TryFutureExt;

use crate::pg_errors::{PgEmbedError, PgEmbedErrorType};
use crate::pg_types::PgResult;

/// Name prefix of the folders inside the cache dir the tar is extracted into before its
/// entries are moved into place; each run's folder is `<prefix><pid>-<seq>`. The cache counts
/// as complete once `bin/initdb` exists, so extracting straight into the cache dir let a
/// process killed mid-extraction leave a partial tree every later start trusted. Inside the
/// cache dir it stays on the same volume (a rename is a metadata move) and adds no path
/// outside the folder pg-embed owns. The name is per run because two first acquisitions of
/// one cache dir can overlap in a process, and a shared folder let one run's cleanup delete
/// the other's half-written tree, which was then moved into place and trusted.
pub(crate) const UNPACK_STAGING_DIR_PREFIX: &str = ".unpack-staging-";

/// Source of the per-run part of a staging dir name; only uniqueness matters, so Relaxed
/// suffices. `usize` because 64-bit atomics are missing on some 32-bit targets.
static NEXT_UNPACK_STAGING_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// Folder inside a run's staging dir that the cache dir entries being replaced are renamed
/// into before they are deleted; see `move_staged_tree_into_place`.
const REPLACED_ENTRIES_DIR_NAME: &str = ".replaced-entries";

/// Serializes the move phase of overlapping runs in this process; see
/// `move_staged_tree_into_place`.
static MOVE_INTO_PLACE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

///
/// Path of a fresh staging dir for this run, unique within the process and across processes
///
fn new_unpack_staging_dir(cache_dir: &Path) -> PathBuf {
    let sequence = NEXT_UNPACK_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    cache_dir.join(format!(
        "{}{}-{}",
        UNPACK_STAGING_DIR_PREFIX,
        std::process::id(),
        sequence
    ))
}

///
/// The pid in a staging dir name, or `None` for any other name
///
/// Anything after the prefix that is not `<digits>-<rest>` is treated as not ours to judge.
///
fn staging_dir_owner_pid(entry_name: &std::ffi::OsStr) -> Option<u32> {
    let after_prefix = entry_name
        .to_str()?
        .strip_prefix(UNPACK_STAGING_DIR_PREFIX)?;
    let (pid, _) = after_prefix.split_once('-')?;
    if pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

///
/// Remove the staging dirs other processes left in the cache dir
///
/// A staging dir named with another pid can only come from a process that was killed: a
/// cancelled future cannot leave one, since extraction and the moves are synchronous, and a
/// failed run removes its own. One named with this process's pid belongs to a live peer
/// acquisition and is never touched. Overlapping acquisitions of one cache dir from two
/// processes stay unsupported, as upstream: they already share the zip, `.txz` and `.tar`
/// paths, and this would remove the live peer's staging dir. The tar extractor recreates
/// missing parent dirs, so that peer could then move a tree with holes into place, and it
/// would be trusted.
///
/// Returns `Ok(())` on success, otherwise returns an error.
///
pub(crate) fn remove_leftover_staging_dirs(cache_dir: &Path) -> PgResult<()> {
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(unpack_failure(
                error,
                format!("could not list {}", cache_dir.display()),
            ))
        }
    };
    let own_pid = std::process::id();
    for entry in entries {
        let entry = entry
            .map_err(|e| unpack_failure(e, format!("could not list {}", cache_dir.display())))?;
        match staging_dir_owner_pid(&entry.file_name()) {
            Some(owner_pid) if owner_pid != own_pid => remove_path_if_present(&entry.path())?,
            _ => {}
        }
    }
    Ok(())
}

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
/// An `UnpackFailure` carrying the io error and the path it concerns
///
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
    // initdb is the only presence marker, so `bin` goes in last: a kill at any earlier point
    // leaves no `cache_dir/bin/initdb` and the next start acquires again.
    let moves_bin = entry_names.iter().any(|name| name == "bin");
    entry_names.retain(|name| name != "bin");
    // Replaced entries are renamed into this run's staging dir and deleted there, because an
    // in-place delete is not atomic: a kill partway through could leave `bin/initdb` next to
    // already deleted binaries, and that tree would be trusted. A kill after the rename
    // leaves the old entry inside a staging dir, which the leftover cleanup removes.
    // `create_dir` fails if the archive itself has an entry of that name, so the two can
    // never mix.
    let replaced_entries_dir = staging_dir.join(REPLACED_ENTRIES_DIR_NAME);
    // No trusted partial tree when two runs in this process overlap: the lock keeps one run's
    // replacements from opening a hole in the tree the other has just completed with `bin`,
    // and the old `bin` goes first so initdb is absent while any entry is being replaced. A
    // failure anywhere before the `bin` rename is an UnpackFailure with initdb absent, so the
    // next start re-downloads. The lock is never held across an await. A poisoned lock only
    // means a peer panicked mid-move, which this run's full replacement repairs.
    let _move_guard = MOVE_INTO_PLACE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::fs::create_dir(&replaced_entries_dir).map_err(|e| {
        unpack_failure(
            e,
            format!("could not create {}", replaced_entries_dir.display()),
        )
    })?;
    if moves_bin {
        move_existing_entry_away(&cache_dir.join("bin"), &replaced_entries_dir.join("bin"))?;
    }
    for entry_name in entry_names {
        // Left by a run killed during this move, or by a pre-staging partial tree. A dir
        // rename onto an existing dir fails on Windows, and onto a non-empty one on Unix.
        move_existing_entry_away(
            &cache_dir.join(&entry_name),
            &replaced_entries_dir.join(&entry_name),
        )?;
        rename_entry(&staging_dir.join(&entry_name), &cache_dir.join(&entry_name))?;
    }
    remove_path_if_present(&replaced_entries_dir)?;
    if moves_bin {
        rename_entry(&staging_dir.join("bin"), &cache_dir.join("bin"))?;
    }
    std::fs::remove_dir(staging_dir)
        .map_err(|e| unpack_failure(e, format!("could not remove {}", staging_dir.display())))
}

///
/// Rename `source` to `destination`, as an `UnpackFailure` naming both on error
///
fn rename_entry(source: &Path, destination: &Path) -> PgResult<()> {
    std::fs::rename(source, destination).map_err(|e| {
        unpack_failure(
            e,
            format!(
                "could not move {} to {}",
                source.display(),
                destination.display()
            ),
        )
    })
}

///
/// Rename an existing cache dir entry into the replaced-entries dir, if it exists
///
fn move_existing_entry_away(existing_entry: &Path, replaced_entry: &Path) -> PgResult<()> {
    match std::fs::symlink_metadata(existing_entry) {
        Ok(_) => rename_entry(existing_entry, replaced_entry),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(unpack_failure(
            error,
            format!("could not inspect {}", existing_entry.display()),
        )),
    }
}

///
/// Remove this run's staging dir after its extraction or move failed
///
/// The leftover cleanup never touches a staging dir of this process, so without this a
/// failed run would hold its partial tree on disk until the process exits and is restarted.
/// A failure is logged rather than returned: the caller needs the original error.
///
fn remove_own_staging_dir(staging_dir: &Path) {
    if let Err(error) = remove_path_if_present(staging_dir) {
        log::warn!(
            "could not remove the staging dir after a failed unpack: {}",
            error
        );
    }
}

///
/// Unpack the postgresql executables
///
/// Returns `Ok(())` on success, otherwise returns an error.
///
pub async fn unpack_postgres(zip_file_path: &PathBuf, cache_dir: &PathBuf) -> PgResult<()> {
    // The tar is extracted into a staging dir, out of the presence check's sight, so a process
    // killed mid-extraction leaves nothing a later start trusts. acquire_postgres has already
    // cleared leftovers before downloading; this repeat covers direct callers of this pub fn.
    // A failure only costs disk space (see acquire_postgres_steps), so it is logged.
    if let Err(error) = remove_leftover_staging_dirs(cache_dir) {
        log::warn!(
            "could not remove a staging dir left by a killed unpack: {}",
            error
        );
    }
    let staging_dir = new_unpack_staging_dir(cache_dir);
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
            // The extraction error is the root cause; a cleanup error must not mask it.
            if let Err(cleanup_error) = cleanup_result {
                log::warn!(
                    "could not remove the postgres tar file after a failed extraction: {}",
                    cleanup_error
                );
            }
            remove_own_staging_dir(&staging_dir);
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
                remove_own_staging_dir(&staging_dir);
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

    /// Name of a staging dir left by a killed process. u32::MAX is never a real pid here
    /// (Linux caps pids at 2^22, macOS below 100000, Windows uses multiples of 4), so it can
    /// never be this process's pid.
    pub(crate) fn killed_process_staging_dir_name() -> String {
        assert_ne!(std::process::id(), u32::MAX);
        format!("{}{}-0", super::UNPACK_STAGING_DIR_PREFIX, u32::MAX)
    }

    /// Top-level entries of `cache_dir` named like a staging dir, of any pid.
    pub(crate) fn staging_dir_names(cache_dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(cache_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(super::UNPACK_STAGING_DIR_PREFIX))
            .collect()
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
        // The partial tree (initdb included) was extracted into the staging dir, out of the
        // presence check, and removed with it.
        assert!(!initdb_path(&cache_dir).exists());
        // decompress_xz names the intermediate `<name>..tar` (with_extension(".tar")).
        assert!(files_with_extension(&cache_dir, ".tar").is_empty());
        assert!(files_with_extension(&cache_dir, ".txz").is_empty());
    }

    // A live process never runs the leftover cleanup on its own staging dirs, so a failed run
    // that kept its dir would leak hundreds of MB for as long as the process lives.
    #[tokio::test]
    async fn failed_unpack_postgres_removes_its_own_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        let zip_file_path = cache_dir.join("binaries.zip");
        std::fs::write(&zip_file_path, test_archives::postgres_zip(true)).unwrap();

        unpack_postgres(&zip_file_path, &cache_dir)
            .await
            .unwrap_err();
        let own_prefix = format!("{}{}-", UNPACK_STAGING_DIR_PREFIX, std::process::id());
        let own_staging_dirs: Vec<String> = test_archives::staging_dir_names(&cache_dir)
            .into_iter()
            .filter(|name| name.starts_with(&own_prefix))
            .collect();
        assert!(own_staging_dirs.is_empty(), "{:?}", own_staging_dirs);
    }

    #[test]
    fn leftover_cleanup_removes_other_pid_staging_dirs_only() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        let killed_staging_dir = cache_dir.join(test_archives::killed_process_staging_dir_name());
        std::fs::create_dir_all(killed_staging_dir.join("bin")).unwrap();
        let live_peer_staging_dir = cache_dir.join(format!(
            "{}{}-999999",
            UNPACK_STAGING_DIR_PREFIX,
            std::process::id()
        ));
        std::fs::create_dir_all(live_peer_staging_dir.join("bin")).unwrap();
        let malformed_staging_dir = cache_dir.join(format!("{}abc-1", UNPACK_STAGING_DIR_PREFIX));
        std::fs::create_dir_all(&malformed_staging_dir).unwrap();
        std::fs::create_dir_all(cache_dir.join("bin")).unwrap();

        remove_leftover_staging_dirs(cache_dir).unwrap();
        assert!(!killed_staging_dir.exists());
        assert!(live_peer_staging_dir.join("bin").exists());
        assert!(malformed_staging_dir.exists());
        assert!(cache_dir.join("bin").exists());
    }

    #[test]
    fn leftover_cleanup_of_a_missing_cache_dir_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        remove_leftover_staging_dirs(&dir.path().join("absent")).unwrap();
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
        let staging_dir = cache_dir.join(test_archives::killed_process_staging_dir_name());
        std::fs::create_dir_all(staging_dir.join("bin")).unwrap();
        std::fs::write(initdb_path(&staging_dir), b"partial initdb").unwrap();
        std::fs::write(staging_dir.join("stale-file"), b"stale").unwrap();
        let zip_file_path = write_good_zip(&cache_dir);

        unpack_postgres(&zip_file_path, &cache_dir).await.unwrap();
        assert_complete_tree(&cache_dir);
        assert!(!cache_dir.join("stale-file").exists());
    }

    // A process killed during the move phase has already moved `lib` and left `bin` (always
    // last) in its staging dir.
    #[tokio::test]
    async fn unpack_postgres_replaces_entries_left_by_a_killed_move() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        std::fs::create_dir_all(cache_dir.join("lib")).unwrap();
        std::fs::write(cache_dir.join("lib").join("stale-file"), b"stale").unwrap();
        let staging_dir = cache_dir.join(test_archives::killed_process_staging_dir_name());
        std::fs::create_dir_all(staging_dir.join("bin")).unwrap();
        std::fs::write(initdb_path(&staging_dir), b"fake initdb").unwrap();
        let zip_file_path = write_good_zip(&cache_dir);

        unpack_postgres(&zip_file_path, &cache_dir).await.unwrap();
        assert_complete_tree(&cache_dir);
        assert!(!cache_dir.join("lib").join("stale-file").exists());
    }

    // A complete cache from a prior run being replaced: the old `bin` is renamed away whole
    // rather than deleted in place, so no kill can leave its initdb next to missing binaries.
    #[tokio::test]
    async fn unpack_postgres_replaces_a_complete_bin_from_a_prior_run() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().to_path_buf();
        std::fs::create_dir_all(cache_dir.join("bin")).unwrap();
        std::fs::write(initdb_path(&cache_dir), b"old initdb").unwrap();
        std::fs::write(cache_dir.join("bin").join("stale-binary"), b"stale").unwrap();
        let zip_file_path = write_good_zip(&cache_dir);

        unpack_postgres(&zip_file_path, &cache_dir).await.unwrap();
        assert_complete_tree(&cache_dir);
        assert!(!cache_dir.join("bin").join("stale-binary").exists());
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
        let staging_dir_names = test_archives::staging_dir_names(cache_dir);
        assert!(staging_dir_names.is_empty(), "{:?}", staging_dir_names);
    }
}
