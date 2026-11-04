//!
//! Cache postgresql files, access to executables, clean up files
//!

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::TryFutureExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};

use crate::pg_enums::{OperationSystem, PgAcquisitionStatus};
use crate::pg_errors::{PgEmbedError, PgEmbedErrorType};
use crate::pg_fetch::PgFetchSettings;
use crate::pg_types::{PgCommandSync, PgResult};

lazy_static! {
    ///
    /// Stores the paths to the cache directories while acquiring the related postgres binaries
    ///
    /// Used to prevent simultaneous downloads and unpacking of the same binaries
    /// while executing multiple PgEmbed instances concurrently.
    ///
    static ref ACQUIRED_PG_BINS: Arc<Mutex<HashMap<PathBuf, PgAcquisitionStatus>>> =
    Arc::new(Mutex::new(HashMap::with_capacity(5)));
}

const PG_EMBED_CACHE_DIR_NAME: &'static str = "pg-embed";
const PG_VERSION_FILE_NAME: &'static str = "PG_VERSION";

///
/// Access to pg_ctl, initdb, database directory and cache directory
///
pub struct PgAccess {
    /// Cache directory path; in local mode, the local installation directory instead
    pub cache_dir: PathBuf,
    /// Database directory path
    pub database_dir: PathBuf,
    /// Postgresql pg_ctl executable path
    pub pg_ctl_exe: PathBuf,
    /// Postgresql initdb executable path
    pub init_db_exe: PathBuf,
    /// Password file path
    pub pw_file_path: PathBuf,
    /// Postgresql binaries zip file path; unused (never written) in local mode
    pub zip_file_path: PathBuf,
    /// Postgresql database version file
    /// used for internal checks
    pg_version_file: PathBuf,
    /// Existing postgresql installation used instead of the cache, if configured
    local_binaries_dir: Option<PathBuf>,
}

impl PgAccess {
    /// Whether an existing installation is used instead of the download cache.
    pub(crate) fn uses_local_binaries(&self) -> bool {
        self.local_binaries_dir.is_some()
    }

    ///
    /// Create a new instance
    ///
    /// Directory structure for cached postgresql binaries will be created
    ///
    pub async fn new(
        fetch_settings: &PgFetchSettings,
        database_dir: &PathBuf,
    ) -> Result<Self, PgEmbedError> {
        // Validate before any directory is created so a bad path has no side effects.
        if let Some(local_binaries_dir) = &fetch_settings.local_binaries_dir {
            Self::validate_local_binaries_dir(local_binaries_dir)?;
        }
        // Local mode never acquires, so it must not need (or populate) the user's real
        // pg-embed cache, which is unavailable on hosts without a cache dir; the install
        // directory is where the binaries live.
        let cache_dir = match &fetch_settings.local_binaries_dir {
            Some(local_binaries_dir) => local_binaries_dir.clone(),
            None => Self::create_cache_dir_structure(&fetch_settings).await?,
        };
        Self::create_db_dir_structure(database_dir).await?;
        // The executables come from the local installation when one is configured;
        // pg_executables_cached() then sees initdb and no download is attempted.
        // The exe suffix is included here (unlike the cache paths below) because
        // pg_executables_cached() opens the path as a file, which needs ".exe" on Windows.
        let (pg_ctl, init_db) = match &fetch_settings.local_binaries_dir {
            Some(local_binaries_dir) => (
                local_binaries_dir.join(format!("bin/pg_ctl{}", std::env::consts::EXE_SUFFIX)),
                local_binaries_dir.join(format!("bin/initdb{}", std::env::consts::EXE_SUFFIX)),
            ),
            None => {
                // pg_ctl executable
                let mut pg_ctl = cache_dir.clone();
                pg_ctl.push("bin/pg_ctl");
                // initdb executable
                let mut init_db = cache_dir.clone();
                init_db.push("bin/initdb");
                (pg_ctl, init_db)
            }
        };
        // postgres zip file
        let mut zip_file_path = cache_dir.clone();
        let platform = fetch_settings.platform();
        let file_name = format!("{}-{}.zip", platform, &fetch_settings.version.0);
        zip_file_path.push(file_name);
        // password file
        let mut pw_file = database_dir.clone();
        pw_file.set_extension("pwfile");
        // postgres version file
        let mut pg_version_file = database_dir.clone();
        pg_version_file.push(PG_VERSION_FILE_NAME);

        Ok(PgAccess {
            cache_dir,
            database_dir: database_dir.clone(),
            pg_ctl_exe: pg_ctl,
            init_db_exe: init_db,
            pw_file_path: pw_file,
            zip_file_path,
            pg_version_file,
            local_binaries_dir: fetch_settings.local_binaries_dir.clone(),
        })
    }

    ///
    /// Check that a local postgresql installation contains the executables pg-embed runs
    /// and the `share/` directory initdb needs
    ///
    /// Reports everything missing at once so the caller can fix it in one go.
    ///
    fn validate_local_binaries_dir(dir: &Path) -> PgResult<()> {
        // stop_db_command_sync runs from Drop and unwraps the pg_ctl path to a str,
        // so a non-UTF-8 user-supplied path would panic there.
        if dir.to_str().is_none() {
            return Err(PgEmbedError {
                error_type: PgEmbedErrorType::InvalidPgPackage,
                source: None,
                message: Some(format!(
                    "local binaries directory {} must be a valid UTF-8 path",
                    dir.display()
                )),
            });
        }
        if !dir.is_dir() {
            return Err(PgEmbedError {
                error_type: PgEmbedErrorType::InvalidPgPackage,
                source: None,
                message: Some(format!(
                    "local binaries directory {} does not exist or is not a directory",
                    dir.display()
                )),
            });
        }
        let mut missing: Vec<String> = ["initdb", "pg_ctl", "postgres"]
            .iter()
            .map(|name| format!("bin/{}{}", name, std::env::consts::EXE_SUFFIX))
            .filter(|relative| !dir.join(relative).is_file())
            .collect();
        // initdb reads its template files from share/; the layout inside differs
        // between distributions, so only the directory itself is checked.
        if !dir.join("share").is_dir() {
            missing.push("share/".to_string());
        }
        if missing.is_empty() {
            return Ok(());
        }
        Err(PgEmbedError {
            error_type: PgEmbedErrorType::InvalidPgPackage,
            source: None,
            message: Some(format!(
                "local binaries directory {} is missing required entries: {}",
                dir.display(),
                missing.join(", ")
            )),
        })
    }

    ///
    /// Create directory structure for cached postgresql executables
    ///
    /// Returns PathBuf(cache_directory) on success, an error otherwise
    ///
    async fn create_cache_dir_structure(fetch_settings: &PgFetchSettings) -> PgResult<PathBuf> {
        let cache_dir = dirs::cache_dir().ok_or_else(|| PgEmbedError {
            error_type: PgEmbedErrorType::InvalidPgUrl,
            source: None,
            message: None,
        })?;
        let os_string = match fetch_settings.operating_system {
            OperationSystem::Darwin | OperationSystem::Windows | OperationSystem::Linux => {
                fetch_settings.operating_system.to_string()
            }
            OperationSystem::AlpineLinux => {
                format!("arch_{}", fetch_settings.operating_system.to_string())
            }
        };
        let pg_path = format!(
            "{}/{}/{}/{}",
            PG_EMBED_CACHE_DIR_NAME,
            os_string,
            fetch_settings.architecture.to_string(),
            fetch_settings.version.0
        );
        let mut cache_pg_embed = cache_dir.clone();
        cache_pg_embed.push(pg_path);
        tokio::fs::create_dir_all(&cache_pg_embed)
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::DirCreationError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        Ok(cache_pg_embed)
    }

    async fn create_db_dir_structure(db_dir: &PathBuf) -> PgResult<()> {
        tokio::fs::create_dir_all(db_dir)
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::DirCreationError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await
    }

    ///
    /// Check if postgresql executables are already cached
    ///
    pub async fn pg_executables_cached(&self) -> PgResult<bool> {
        Self::path_exists(self.init_db_exe.as_path()).await
    }

    ///
    /// Check if database files exist
    ///
    pub async fn db_files_exist(&self) -> PgResult<bool> {
        Self::path_exists(self.pg_version_file.as_path()).await
    }

    ///
    /// Check if database version file exists
    ///
    pub async fn pg_version_file_exists(db_dir: &PathBuf) -> PgResult<bool> {
        let mut pg_version_file = db_dir.clone();
        pg_version_file.push(PG_VERSION_FILE_NAME);
        let file_exists = if let Ok(_) = tokio::fs::File::open(pg_version_file.as_path()).await {
            true
        } else {
            false
        };
        Ok(file_exists)
    }

    ///
    /// Check if file path exists
    ///
    async fn path_exists(file: &Path) -> PgResult<bool> {
        if let Ok(_) = tokio::fs::File::open(file).await {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    ///
    /// Mark postgresql binaries acquisition in progress
    ///
    /// Used while acquiring postgresql binaries, so that no two instances
    /// of PgEmbed try to acquire the same resources
    ///
    pub async fn mark_acquisition_in_progress(&self) -> PgResult<()> {
        let mut lock = ACQUIRED_PG_BINS.lock().await;
        lock.insert(self.cache_dir.clone(), PgAcquisitionStatus::InProgress);
        Ok(())
    }

    ///
    /// Mark postgresql binaries acquisition finished
    ///
    /// Used when acquiring postgresql has finished, so that other instances
    /// of PgEmbed don't try to reacquire resources
    ///
    pub async fn mark_acquisition_finished(&self) -> PgResult<()> {
        let mut lock = ACQUIRED_PG_BINS.lock().await;
        lock.insert(self.cache_dir.clone(), PgAcquisitionStatus::Finished);
        Ok(())
    }

    ///
    /// Check postgresql acquisition status
    ///
    pub async fn acquisition_status(&self) -> PgAcquisitionStatus {
        let lock = ACQUIRED_PG_BINS.lock().await;
        let acquisition_status = lock.get(&self.cache_dir);
        match acquisition_status {
            None => PgAcquisitionStatus::Undefined,
            Some(status) => *status,
        }
    }

    ///
    /// Determine if postgresql binaries acquisition is needed
    ///
    pub async fn acquisition_needed(&self) -> PgResult<bool> {
        // Downloading into the cache would not help: the executables are run from the
        // local directory, so a bundle that vanished after `new` must surface as an error.
        if let Some(local_binaries_dir) = &self.local_binaries_dir {
            // The few blocking stat calls are deliberate: validate_local_binaries_dir is
            // shared with the sync constructor path and their cost is negligible.
            Self::validate_local_binaries_dir(local_binaries_dir)?;
            return Ok(false);
        }
        if !self.pg_executables_cached().await? {
            match self.acquisition_status().await {
                PgAcquisitionStatus::InProgress => {
                    let mut interval = interval(Duration::from_millis(100));
                    while self.acquisition_status().await == PgAcquisitionStatus::InProgress {
                        interval.tick().await;
                    }
                    Ok(false)
                }
                PgAcquisitionStatus::Finished => Ok(false),
                PgAcquisitionStatus::Undefined => Ok(true),
            }
        } else {
            Ok(false)
        }
    }

    ///
    /// Write pg binaries zip to postgresql cache directory
    ///
    pub async fn write_pg_zip(&self, bytes: &[u8]) -> PgResult<()> {
        // In local mode cache_dir is the user's own installation; never drop a zip into it.
        if self.local_binaries_dir.is_some() {
            return Err(PgEmbedError {
                error_type: PgEmbedErrorType::InvalidPgPackage,
                source: None,
                message: Some(
                    "write_pg_zip is not available when local_binaries_dir is set; \
                     pg-embed never downloads in local mode"
                        .to_string(),
                ),
            });
        }
        let mut file: tokio::fs::File = tokio::fs::File::create(&self.zip_file_path.as_path())
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::WriteFileError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        file.write_all(&bytes)
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::WriteFileError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        Ok(())
    }

    ///
    /// Clean up created files and directories.
    ///
    /// Remove created directories containing the database and the password file.
    ///
    pub fn clean(&self) -> PgResult<()> {
        // not using tokio::fs async methods because clean() is called on drop
        std::fs::remove_dir_all(self.database_dir.as_path()).map_err(|e| PgEmbedError {
            error_type: PgEmbedErrorType::PgCleanUpFailure,
            source: Some(Box::new(e)),
            message: None,
        })?;
        std::fs::remove_file(self.pw_file_path.as_path()).map_err(|e| PgEmbedError {
            error_type: PgEmbedErrorType::PgCleanUpFailure,
            source: Some(Box::new(e)),
            message: None,
        })?;
        Ok(())
    }

    ///
    /// Purge postgresql executables
    ///
    /// Remove all cached postgresql executables
    ///
    pub async fn purge() -> PgResult<()> {
        let mut cache_dir = dirs::cache_dir().ok_or_else(|| PgEmbedError {
            error_type: PgEmbedErrorType::ReadFileError,
            source: None,
            message: Some(String::from("cache dir error")),
        })?;
        cache_dir.push(PG_EMBED_CACHE_DIR_NAME);
        let _ = tokio::fs::remove_dir_all(cache_dir.as_path())
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::PgPurgeFailure,
                source: Some(Box::new(e)),
                message: None,
            })
            .await;
        Ok(())
    }

    ///
    /// Clean up database directory and password file
    ///
    pub async fn clean_up(database_dir: PathBuf, pw_file: PathBuf) -> PgResult<()> {
        tokio::fs::remove_dir_all(database_dir.as_path())
            .await
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::PgCleanUpFailure,
                source: Some(Box::new(e)),
                message: None,
            })?;

        tokio::fs::remove_file(pw_file.as_path())
            .await
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::PgCleanUpFailure,
                source: Some(Box::new(e)),
                message: None,
            })
    }

    ///
    /// Create a database password file
    ///
    /// Returns `Ok(())` on success, otherwise returns an error.
    ///
    pub async fn create_password_file(&self, password: &[u8]) -> PgResult<()> {
        let mut file: tokio::fs::File = tokio::fs::File::create(self.pw_file_path.as_path())
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::WriteFileError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        let _ = file
            .write(password)
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::WriteFileError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        Ok(())
    }

    ///
    /// Create synchronous pg_ctl stop command
    ///
    pub fn stop_db_command_sync(&self, database_dir: &PathBuf) -> PgCommandSync {
        let pg_ctl_executable = self.pg_ctl_exe.to_str().unwrap();
        let mut command = Box::new(Cell::new(std::process::Command::new(pg_ctl_executable)));
        command
            .get_mut()
            .args(&["stop", "-w", "-D", database_dir.to_str().unwrap()]);

        // Match the async spawn path in command_executor::generate_command and
        // suppress the console window Windows would allocate for pg_ctl on a
        // GUI-subsystem parent. See that function for why we use
        // CREATE_NO_WINDOW alone (not combined with DETACHED_PROCESS) — pg_ctl
        // spawns postgres via `cmd /C` and needs a hidden console to inherit.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.get_mut().creation_flags(CREATE_NO_WINDOW);
        }

        command
    }
}

/// Creates an installation tree with empty stand-in executables, minus `omit`.
/// Shared with the postgres.rs tests.
#[cfg(test)]
pub(crate) fn fake_installation(omit: &[&str]) -> tempfile::TempDir {
    let installation = tempfile::tempdir().unwrap();
    let bin_dir = installation.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    if !omit.contains(&"share") {
        std::fs::create_dir_all(installation.path().join("share")).unwrap();
    }
    for name in ["initdb", "pg_ctl", "postgres"] {
        if omit.contains(&name) {
            continue;
        }
        let executable = bin_dir.join(format!("{}{}", name, std::env::consts::EXE_SUFFIX));
        std::fs::write(&executable, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    installation
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_settings(dir: &Path) -> PgFetchSettings {
        PgFetchSettings {
            local_binaries_dir: Some(dir.to_path_buf()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn write_pg_zip_is_refused_in_local_mode() {
        let installation = fake_installation(&[]);
        let database = tempfile::tempdir().unwrap();
        let access = PgAccess::new(
            &local_settings(installation.path()),
            &database.path().join("db"),
        )
        .await
        .unwrap();
        let error = access.write_pg_zip(b"x").await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::InvalidPgPackage);
        let zip_files = std::fs::read_dir(installation.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension() == Some(std::ffi::OsStr::new("zip")))
            .count();
        assert_eq!(zip_files, 0);
    }

    #[tokio::test]
    async fn valid_local_binaries_dir_is_used_without_acquisition() {
        let installation = fake_installation(&[]);
        let database = tempfile::tempdir().unwrap();
        let access = PgAccess::new(
            &local_settings(installation.path()),
            &database.path().join("db"),
        )
        .await
        .unwrap();
        assert!(access.pg_ctl_exe.starts_with(installation.path()));
        assert!(access.init_db_exe.starts_with(installation.path()));
        assert!(!access.acquisition_needed().await.unwrap());
    }

    #[tokio::test]
    async fn local_mode_uses_installation_dir_instead_of_cache_dir() {
        let installation = fake_installation(&[]);
        let database = tempfile::tempdir().unwrap();
        let access = PgAccess::new(
            &local_settings(installation.path()),
            &database.path().join("db"),
        )
        .await
        .unwrap();
        assert_eq!(access.cache_dir, installation.path());
    }

    #[tokio::test]
    async fn missing_postgres_executable_is_reported() {
        let installation = fake_installation(&["postgres"]);
        let database = tempfile::tempdir().unwrap();
        let error = PgAccess::new(
            &local_settings(installation.path()),
            &database.path().join("db"),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.error_type, PgEmbedErrorType::InvalidPgPackage);
        let message = error.message.unwrap();
        assert!(message.contains("bin/postgres"), "{}", message);
        assert!(!message.contains("initdb"), "{}", message);
    }

    #[tokio::test]
    async fn vanished_local_bundle_is_an_error_not_a_download() {
        let installation = fake_installation(&[]);
        let database = tempfile::tempdir().unwrap();
        let access = PgAccess::new(
            &local_settings(installation.path()),
            &database.path().join("db"),
        )
        .await
        .unwrap();
        std::fs::remove_file(
            installation
                .path()
                .join(format!("bin/initdb{}", std::env::consts::EXE_SUFFIX)),
        )
        .unwrap();
        let error = access.acquisition_needed().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::InvalidPgPackage);
    }

    #[test]
    fn all_missing_executables_are_reported_together() {
        let installation = fake_installation(&["initdb", "pg_ctl"]);
        let message = PgAccess::validate_local_binaries_dir(installation.path())
            .unwrap_err()
            .message
            .unwrap();
        assert!(message.contains("initdb"), "{}", message);
        assert!(message.contains("pg_ctl"), "{}", message);
    }

    #[test]
    fn missing_share_directory_is_reported() {
        let installation = fake_installation(&["share"]);
        let error = PgAccess::validate_local_binaries_dir(installation.path()).unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::InvalidPgPackage);
        let message = error.message.unwrap();
        assert!(message.contains("share"), "{}", message);
        assert!(!message.contains("initdb"), "{}", message);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_local_binaries_dir_is_rejected() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff"));
        let error = PgAccess::validate_local_binaries_dir(path).unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::InvalidPgPackage);
        assert!(error.message.unwrap().contains("UTF-8"));
    }

    #[tokio::test]
    async fn nonexistent_local_binaries_dir_is_rejected() {
        let parent = tempfile::tempdir().unwrap();
        let database = tempfile::tempdir().unwrap();
        let error = PgAccess::new(
            &local_settings(&parent.path().join("does-not-exist")),
            &database.path().join("db"),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.error_type, PgEmbedErrorType::InvalidPgPackage);
        assert!(error.message.unwrap().contains("does not exist"));
    }
}
