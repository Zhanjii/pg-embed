//!
//! Postgresql server
//!
//! Start, stop, initialize the postgresql server.
//! Create database clusters and databases.
//!
use std::io::BufRead;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::TryFutureExt;
use log::{error, info};
use tokio::sync::Mutex;

#[cfg(feature = "rt_tokio_migrate")]
use sqlx_tokio::migrate::{MigrateDatabase, Migrator};
#[cfg(feature = "rt_tokio_migrate")]
use sqlx_tokio::postgres::PgPoolOptions;
#[cfg(feature = "rt_tokio_migrate")]
use sqlx_tokio::Postgres;

use crate::command_executor::AsyncCommand;
use crate::pg_access::PgAccess;
use crate::pg_commands::PgCommand;
use crate::pg_enums::{PgAuthMethod, PgServerStatus};
use crate::pg_errors::{PgEmbedError, PgEmbedErrorType};
use crate::pg_types::PgResult;
use crate::{pg_fetch, pg_unpack};

///
/// Database settings
///
pub struct PgSettings {
    /// postgresql database directory
    pub database_dir: PathBuf,
    /// postgresql port
    pub port: u16,
    /// postgresql user name
    pub user: String,
    /// postgresql password
    pub password: String,
    /// authentication
    pub auth_method: PgAuthMethod,
    /// persist database
    pub persistent: bool,
    /// duration to wait before terminating process execution
    /// pg_ctl start/stop and initdb timeout
    pub timeout: Option<Duration>,
    /// migrations folder
    /// sql script files to execute on migrate
    pub migration_dir: Option<PathBuf>,
}

///
/// Embedded postgresql database
///
/// If the PgEmbed instance is dropped / goes out of scope and postgresql is still
/// running, the postgresql process will be killed and depending on the [PgSettings::persistent] setting,
/// file and directories will be cleaned up.
///
pub struct PgEmbed {
    /// Postgresql settings
    pub pg_settings: PgSettings,
    /// Download settings
    pub fetch_settings: pg_fetch::PgFetchSettings,
    /// Database uri `postgres://{username}:{password}@localhost:{port}`
    pub db_uri: String,
    /// Postgres server status
    pub server_status: Arc<Mutex<PgServerStatus>>,
    pub shutting_down: bool,
    /// Postgres files access
    pub pg_access: PgAccess,
}

impl Drop for PgEmbed {
    fn drop(&mut self) {
        if !self.shutting_down {
            let _ = self.stop_db_sync();
        }
        if !&self.pg_settings.persistent {
            let _ = &self.pg_access.clean();
        }
    }
}

impl PgEmbed {
    ///
    /// Create a new PgEmbed instance
    ///
    pub async fn new(
        pg_settings: PgSettings,
        fetch_settings: pg_fetch::PgFetchSettings,
    ) -> PgResult<Self> {
        let password: &str = &pg_settings.password;
        let db_uri = format!(
            "postgres://{}:{}@localhost:{}",
            &pg_settings.user,
            &password,
            pg_settings.port.to_string()
        );
        let pg_access = PgAccess::new(&fetch_settings, &pg_settings.database_dir).await?;
        Ok(PgEmbed {
            pg_settings,
            fetch_settings,
            db_uri,
            server_status: Arc::new(Mutex::new(PgServerStatus::Uninitialized)),
            shutting_down: false,
            pg_access,
        })
    }

    ///
    /// Setup postgresql for execution
    ///
    /// Download, unpack, create password file and database
    ///
    pub async fn setup(&mut self) -> PgResult<()> {
        if self.pg_access.acquisition_needed().await? {
            self.acquire_postgres().await?;
        }
        self.pg_access
            .create_password_file(self.pg_settings.password.as_bytes())
            .await?;
        if self.pg_access.db_files_exist().await? {
            let mut server_status = self.server_status.lock().await;
            *server_status = PgServerStatus::Initialized;
        } else {
            &self.init_db().await?;
        }
        Ok(())
    }

    ///
    /// Download and unpack postgres binaries
    ///
    /// Returns `InvalidPgPackage` when `local_binaries_dir` is set: local mode never downloads.
    /// Marks the acquisition finished on success and resets it on failure or cancellation,
    /// which also releases a retry claim handed over by `PgAccess::acquisition_needed`.
    ///
    pub async fn acquire_postgres(&self) -> PgResult<()> {
        // In local mode cache_dir is the user's own installation; downloading would write
        // a zip into it and unpack over it, so refuse before touching anything. Guard on
        // PgAccess's copy of the setting: every path was built from it, whereas the pub
        // fetch_settings field can be mutated after construction.
        if self.pg_access.uses_local_binaries() {
            return Err(PgEmbedError {
                error_type: PgEmbedErrorType::InvalidPgPackage,
                source: None,
                message: Some(
                    "acquire_postgres is not available when local_binaries_dir is set; \
                     pg-embed never downloads in local mode"
                        .to_string(),
                ),
            });
        }
        // The guard only covers cancellation (this future dropped mid-await). Completed runs,
        // successful or failed, set the status explicitly below and then release it, so the
        // reset happens once and failure keeps its awaited, error-reporting path.
        // It is armed before marking in progress because on the retry path the claim is
        // already held, and that await contends with polling waiters for the status lock;
        // a cancellation there would otherwise leave the status in progress forever. If
        // marking itself fails, the unreleased guard clears the entry, which is right since
        // this instance never acquired anything and waiters must be free to retry. The guard
        // only clears an entry this instance set, so a cancellation before marking on the
        // first-acquisition path cannot clear a peer's live claim.
        let mut claim_guard = self.pg_access.acquisition_claim_guard();
        self.pg_access.mark_acquisition_in_progress().await?;
        let result = match self.acquire_postgres_steps().await {
            Ok(()) => Ok(()),
            Err(error) => {
                // Other instances wait while the status is in progress; reset it so they
                // stop waiting and retry. The original error is what the caller needs.
                if let Err(reset_error) = self.pg_access.mark_acquisition_failed().await {
                    log::warn!(
                        "could not reset the postgres acquisition status after a failed acquisition: {}",
                        reset_error
                    );
                }
                Err(error)
            }
        };
        claim_guard.release();
        result
    }

    async fn acquire_postgres_steps(&self) -> PgResult<()> {
        // Before the download, so a staging dir left by a killed unpack (hundreds of MB) is
        // freed even when the zip write then fails, as it can on every start on a nearly full
        // disk. Only logged: the presence check never looks inside a staging dir, so a
        // leftover costs disk space, never correctness, and failing here would block an
        // acquisition that may well succeed.
        if let Err(error) = pg_unpack::remove_leftover_staging_dirs(&self.pg_access.cache_dir) {
            log::warn!(
                "could not remove a staging dir left by a killed unpack: {}",
                error
            );
        }
        let pg_bin_data = &self.fetch_settings.fetch_postgres().await?;
        self.pg_access.write_pg_zip(&pg_bin_data).await?;
        if let Err(unpack_error) =
            pg_unpack::unpack_postgres(&self.pg_access.zip_file_path, &self.pg_access.cache_dir)
                .await
        {
            // "Binaries cached" is decided by the presence of initdb alone, so a tree that
            // failed part way through must not keep it or every later start would trust it.
            // unpack_postgres confines a failed extraction to its own staging dir and removes
            // it, and moves `bin` in last, so initdb is normally absent here; this stays as
            // defence in depth (e.g. a failed move next to a `bin` left by an older pg-embed's
            // partial tree).
            // A cleanup failure means the tree is untouched or complete, so it is left alone.
            // Synchronous, with no await since the failed extraction, so a cancellation
            // cannot skip it.
            if unpack_error.error_type != PgEmbedErrorType::PgCleanUpFailure {
                if let Err(remove_error) = std::fs::remove_file(&self.pg_access.init_db_exe) {
                    if remove_error.kind() != std::io::ErrorKind::NotFound {
                        log::warn!(
                            "could not remove {} after a failed unpack: {}",
                            self.pg_access.init_db_exe.display(),
                            remove_error
                        );
                    }
                }
            }
            return Err(unpack_error);
        }
        self.pg_access.mark_acquisition_finished().await?;
        Ok(())
    }

    ///
    /// Initialize postgresql database
    ///
    /// Returns `Ok(())` on success, otherwise returns an error.
    ///
    pub async fn init_db(&mut self) -> PgResult<()> {
        {
            let mut server_status = self.server_status.lock().await;
            *server_status = PgServerStatus::Initializing;
        }

        let mut executor = PgCommand::init_db_executor(
            &self.pg_access.init_db_exe,
            &self.pg_access.database_dir,
            &self.pg_access.pw_file_path,
            &self.pg_settings.user,
            &self.pg_settings.auth_method,
        )?;
        let exit_status = executor.execute(self.pg_settings.timeout).await?;
        let mut server_status = self.server_status.lock().await;
        *server_status = exit_status;
        Ok(())
    }

    ///
    /// Start postgresql database
    ///
    /// Returns `Ok(())` on success, otherwise returns an error.
    ///
    pub async fn start_db(&mut self) -> PgResult<()> {
        {
            let mut server_status = self.server_status.lock().await;
            *server_status = PgServerStatus::Starting;
        }
        self.shutting_down = false;
        let mut executor = PgCommand::start_db_executor(
            &self.pg_access.pg_ctl_exe,
            &self.pg_access.database_dir,
            &self.pg_settings.port,
        )?;
        let exit_status = executor.execute(self.pg_settings.timeout).await?;
        let mut server_status = self.server_status.lock().await;
        *server_status = exit_status;
        Ok(())
    }

    ///
    /// Stop postgresql database
    ///
    /// Returns `Ok(())` on success, otherwise returns an error.
    ///
    pub async fn stop_db(&mut self) -> PgResult<()> {
        {
            let mut server_status = self.server_status.lock().await;
            *server_status = PgServerStatus::Stopping;
        }
        self.shutting_down = true;
        let mut executor =
            PgCommand::stop_db_executor(&self.pg_access.pg_ctl_exe, &self.pg_access.database_dir)?;
        let exit_status = executor.execute(self.pg_settings.timeout).await?;
        let mut server_status = self.server_status.lock().await;
        *server_status = exit_status;
        Ok(())
    }

    ///
    /// Stop postgresql database synchronous
    ///
    /// Returns `Ok(())` on success, otherwise returns an error.
    ///
    pub fn stop_db_sync(&mut self) -> PgResult<()> {
        self.shutting_down = true;
        let mut stop_db_command = self
            .pg_access
            .stop_db_command_sync(&self.pg_settings.database_dir);
        let process = stop_db_command
            .get_mut()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::PgError,
                source: Some(Box::new(e)),
                message: None,
            })?;

        self.handle_process_io_sync(process)
    }

    ///
    /// Handle process logging synchronous
    ///
    pub fn handle_process_io_sync(&self, mut process: std::process::Child) -> PgResult<()> {
        let reader_out = std::io::BufReader::new(process.stdout.take().unwrap()).lines();
        let reader_err = std::io::BufReader::new(process.stderr.take().unwrap()).lines();
        reader_out.for_each(|line| info!("{}", line.unwrap()));
        reader_err.for_each(|line| error!("{}", line.unwrap()));
        Ok(())
    }

    ///
    /// Create a database
    ///
    #[cfg(any(
        feature = "rt_tokio_migrate",
        feature = "rt_async_std_migrate",
        feature = "rt_actix_migrate"
    ))]
    pub async fn create_database(&self, db_name: &str) -> PgResult<()> {
        Postgres::create_database(&self.full_db_uri(db_name))
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::PgTaskJoinError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        Ok(())
    }

    ///
    /// Drop a database
    ///
    #[cfg(any(
        feature = "rt_tokio_migrate",
        feature = "rt_async_std_migrate",
        feature = "rt_actix_migrate"
    ))]
    pub async fn drop_database(&self, db_name: &str) -> PgResult<()> {
        Postgres::drop_database(&self.full_db_uri(db_name))
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::PgTaskJoinError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        Ok(())
    }

    ///
    /// Check database existence
    ///
    #[cfg(any(
        feature = "rt_tokio_migrate",
        feature = "rt_async_std_migrate",
        feature = "rt_actix_migrate"
    ))]
    pub async fn database_exists(&self, db_name: &str) -> PgResult<bool> {
        let result = Postgres::database_exists(&self.full_db_uri(db_name))
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::PgTaskJoinError,
                source: Some(Box::new(e)),
                message: None,
            })
            .await?;
        Ok(result)
    }

    ///
    /// The full database uri
    ///
    /// (*postgres://{username}:{password}@localhost:{port}/{db_name}*)
    ///
    pub fn full_db_uri(&self, db_name: &str) -> String {
        format!("{}/{}", &self.db_uri, db_name)
    }

    ///
    /// Run migrations
    ///
    #[cfg(any(
        feature = "rt_tokio_migrate",
        feature = "rt_async_std_migrate",
        feature = "rt_actix_migrate"
    ))]
    pub async fn migrate(&self, db_name: &str) -> PgResult<()> {
        if let Some(migration_dir) = &self.pg_settings.migration_dir {
            let m = Migrator::new(migration_dir.as_path())
                .map_err(|e| PgEmbedError {
                    error_type: PgEmbedErrorType::MigrationError,
                    source: Some(Box::new(e)),
                    message: None,
                })
                .await?;
            let pool = PgPoolOptions::new()
                .connect(&self.full_db_uri(db_name))
                .map_err(|e| PgEmbedError {
                    error_type: PgEmbedErrorType::SqlQueryError,
                    source: Some(Box::new(e)),
                    message: None,
                })
                .await?;
            m.run(&pool)
                .map_err(|e| PgEmbedError {
                    error_type: PgEmbedErrorType::MigrationError,
                    source: Some(Box::new(e)),
                    message: None,
                })
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg_access::fake_installation;
    use crate::pg_fetch::{PgFetchSettings, PG_V15};

    fn settings_for(database_dir: PathBuf) -> PgSettings {
        PgSettings {
            database_dir,
            port: 5432,
            user: "postgres".to_string(),
            password: "password".to_string(),
            auth_method: PgAuthMethod::ScramSha256,
            persistent: true,
            timeout: Some(Duration::from_secs(30)),
            migration_dir: None,
        }
    }

    /// Exists to guard compilation of both literal shapes callers use: `PgFetchSettings`
    /// with `..Default::default()` and a fully spelled-out `PgSettings`. Adding fields
    /// must not break either.
    #[test]
    fn default_fetch_settings_struct_update_syntax_compiles() {
        let fetch_settings = PgFetchSettings {
            version: PG_V15,
            ..Default::default()
        };
        let _pg_settings = settings_for(PathBuf::from("unused"));
        assert!(fetch_settings.local_binaries_dir.is_none());
        assert_eq!(fetch_settings.host, "https://repo1.maven.org");
    }

    #[tokio::test]
    async fn local_binaries_dir_flows_through_pg_embed_new() {
        // Empty stand-in executables: the Drop impl must never run anything real.
        let installation = fake_installation(&[]);
        let database = tempfile::tempdir().unwrap();
        let mut pg = PgEmbed::new(
            settings_for(database.path().join("db")),
            PgFetchSettings {
                local_binaries_dir: Some(installation.path().to_path_buf()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Drop would otherwise spawn the fake pg_ctl via stop_db_sync.
        pg.shutting_down = true;
        assert!(!pg.pg_access.acquisition_needed().await.unwrap());
        assert!(pg.pg_access.pg_ctl_exe.starts_with(installation.path()));
        assert!(pg.pg_access.init_db_exe.starts_with(installation.path()));
    }

    #[tokio::test]
    async fn acquire_postgres_is_refused_in_local_mode() {
        let installation = fake_installation(&[]);
        let database = tempfile::tempdir().unwrap();
        let mut pg = PgEmbed::new(
            settings_for(database.path().join("db")),
            PgFetchSettings {
                local_binaries_dir: Some(installation.path().to_path_buf()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        pg.shutting_down = true;
        let error = pg.acquire_postgres().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::InvalidPgPackage);
        let zip_files = std::fs::read_dir(installation.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension() == Some(std::ffi::OsStr::new("zip")))
            .count();
        assert_eq!(zip_files, 0);
    }

    fn zip_files_under(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut zip_files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                zip_files.extend(zip_files_under(&path));
            } else if path.extension() == Some(std::ffi::OsStr::new("zip")) {
                zip_files.push(path);
            }
        }
        zip_files
    }

    #[tokio::test]
    async fn failed_download_resets_acquisition_status_and_writes_no_zip() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let cache_dir = temporary_directory.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let host = crate::pg_fetch::serve_once("404 Not Found", b"not here".to_vec()).await;
        // A struct literal: PgEmbed::new in remote mode would create the real user cache.
        let mut pg = PgEmbed {
            pg_settings: settings_for(temporary_directory.path().join("db")),
            fetch_settings: PgFetchSettings {
                host,
                ..Default::default()
            },
            db_uri: "postgres://postgres:password@localhost:5432".to_string(),
            server_status: Arc::new(Mutex::new(PgServerStatus::Uninitialized)),
            shutting_down: false,
            pg_access: crate::pg_access::access_for(&cache_dir),
        };
        // Drop must neither run pg_ctl nor clean anything up.
        pg.shutting_down = true;
        pg.pg_settings.persistent = true;

        let error = pg.acquire_postgres().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::DownloadFailure);
        let message = error.message.clone().unwrap();
        assert!(message.contains("404"), "{}", message);
        assert!(
            pg.pg_access.acquisition_status().await
                == crate::pg_enums::PgAcquisitionStatus::Undefined
        );
        assert!(zip_files_under(&cache_dir).is_empty());
    }

    #[tokio::test]
    async fn failed_unpack_invalidates_partial_tree_and_resets_status() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let cache_dir = temporary_directory.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let host = crate::pg_fetch::serve_once(
            "200 OK",
            crate::pg_unpack::test_archives::postgres_zip(true),
        )
        .await;
        // A struct literal: PgEmbed::new in remote mode would create the real user cache.
        let mut pg = PgEmbed {
            pg_settings: settings_for(temporary_directory.path().join("db")),
            fetch_settings: PgFetchSettings {
                host,
                ..Default::default()
            },
            db_uri: "postgres://postgres:password@localhost:5432".to_string(),
            server_status: Arc::new(Mutex::new(PgServerStatus::Uninitialized)),
            shutting_down: false,
            pg_access: crate::pg_access::access_for(&cache_dir),
        };
        // Drop must neither run pg_ctl nor clean anything up.
        pg.shutting_down = true;
        pg.pg_settings.persistent = true;

        let error = pg.acquire_postgres().await.unwrap_err();
        assert_eq!(
            error.error_type,
            PgEmbedErrorType::UnpackFailure,
            "{:?}",
            error
        );
        // The first tar entry (initdb) was fully extracted before the failure, into the
        // staging dir, so it never reached the cache dir.
        assert!(!pg.pg_access.init_db_exe.exists());
        assert!(!pg.pg_access.pg_executables_cached().await.unwrap());
        assert!(
            pg.pg_access.acquisition_status().await
                == crate::pg_enums::PgAcquisitionStatus::Undefined
        );
        assert!(pg.pg_access.acquisition_needed().await.unwrap());
    }

    // A process killed mid-extraction leaves its partial tree in the staging dir: the next
    // start must not trust it, and its acquisition must clean it up and complete the cache.
    #[tokio::test]
    async fn leftover_staging_tree_is_not_trusted_and_next_acquire_completes_cache() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let cache_dir = temporary_directory.path().join("cache");
        let staging_bin = cache_dir
            .join(crate::pg_unpack::test_archives::killed_process_staging_dir_name())
            .join("bin");
        std::fs::create_dir_all(&staging_bin).unwrap();
        std::fs::write(
            staging_bin.join(format!("initdb{}", std::env::consts::EXE_SUFFIX)),
            b"partial initdb",
        )
        .unwrap();
        let host = crate::pg_fetch::serve_once(
            "200 OK",
            crate::pg_unpack::test_archives::postgres_zip(false),
        )
        .await;
        // A struct literal: PgEmbed::new in remote mode would create the real user cache.
        let mut pg = PgEmbed {
            pg_settings: settings_for(temporary_directory.path().join("db")),
            fetch_settings: PgFetchSettings {
                host,
                ..Default::default()
            },
            db_uri: "postgres://postgres:password@localhost:5432".to_string(),
            server_status: Arc::new(Mutex::new(PgServerStatus::Uninitialized)),
            shutting_down: false,
            pg_access: crate::pg_access::access_for(&cache_dir),
        };
        // Drop must neither run pg_ctl nor clean anything up.
        pg.shutting_down = true;
        pg.pg_settings.persistent = true;

        assert!(!pg.pg_access.pg_executables_cached().await.unwrap());
        assert!(pg.pg_access.acquisition_needed().await.unwrap());

        pg.acquire_postgres().await.unwrap();
        assert!(pg.pg_access.init_db_exe.exists());
        let staging_dir_names = crate::pg_unpack::test_archives::staging_dir_names(&cache_dir);
        assert!(staging_dir_names.is_empty(), "{:?}", staging_dir_names);
        assert!(
            pg.pg_access.acquisition_status().await
                == crate::pg_enums::PgAcquisitionStatus::Finished
        );
    }

    // On a nearly full disk the zip write can fail on every start; the leftover, which may be
    // what fills the disk, must still be freed.
    #[tokio::test]
    async fn leftover_staging_dir_is_removed_even_when_the_download_fails() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let cache_dir = temporary_directory.path().join("cache");
        let leftover_staging_dir =
            cache_dir.join(crate::pg_unpack::test_archives::killed_process_staging_dir_name());
        std::fs::create_dir_all(leftover_staging_dir.join("bin")).unwrap();
        let host = crate::pg_fetch::serve_once("404 Not Found", b"not here".to_vec()).await;
        // A struct literal: PgEmbed::new in remote mode would create the real user cache.
        let mut pg = PgEmbed {
            pg_settings: settings_for(temporary_directory.path().join("db")),
            fetch_settings: PgFetchSettings {
                host,
                ..Default::default()
            },
            db_uri: "postgres://postgres:password@localhost:5432".to_string(),
            server_status: Arc::new(Mutex::new(PgServerStatus::Uninitialized)),
            shutting_down: false,
            pg_access: crate::pg_access::access_for(&cache_dir),
        };
        // Drop must neither run pg_ctl nor clean anything up.
        pg.shutting_down = true;
        pg.pg_settings.persistent = true;

        let error = pg.acquire_postgres().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::DownloadFailure);
        assert!(!leftover_staging_dir.exists());
    }

    // End to end for the claim guard: a timeout around acquisition (the typical way callers
    // bound setup()) must not leave other instances waiting on an in-progress status forever.
    #[tokio::test]
    async fn cancelled_download_resets_acquisition_status_and_writes_no_zip() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let cache_dir = temporary_directory.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        // Accepts the request and never answers, so the download is still in flight when the
        // acquisition future is dropped.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted_sender, mut accepted_receiver) = tokio::sync::oneshot::channel::<()>();
        let stalled_server = tokio::spawn(async move {
            let (_held_socket, _) = listener.accept().await.unwrap();
            let _ = accepted_sender.send(());
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        // A struct literal: PgEmbed::new in remote mode would create the real user cache.
        let mut pg = PgEmbed {
            pg_settings: settings_for(temporary_directory.path().join("db")),
            fetch_settings: PgFetchSettings {
                host: format!("http://127.0.0.1:{}", port),
                ..Default::default()
            },
            db_uri: "postgres://postgres:password@localhost:5432".to_string(),
            server_status: Arc::new(Mutex::new(PgServerStatus::Uninitialized)),
            shutting_down: false,
            pg_access: crate::pg_access::access_for(&cache_dir),
        };
        // Drop must neither run pg_ctl nor clean anything up.
        pg.shutting_down = true;
        pg.pg_settings.persistent = true;

        // Cancelling on the server's accept signal, not after a fixed delay, guarantees the
        // download is in flight when the future is dropped, however slow the test machine.
        {
            let acquire = pg.acquire_postgres();
            tokio::pin!(acquire);
            // Bounded so a broken test fails instead of hanging.
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::select! {
                    outcome = &mut acquire => {
                        panic!("the stalled download must not complete: {:?}", outcome.err())
                    }
                    accepted = &mut accepted_receiver => {
                        accepted.expect("the stalled server must signal the accepted request")
                    }
                }
            })
            .await
            .expect("the request must reach the stalled server");
            // Leaving this scope drops the pinned acquisition mid-download.
        }
        // A contended status lock makes the guard reset through a spawned task, so poll.
        tokio::time::timeout(Duration::from_secs(2), async {
            while pg.pg_access.acquisition_status().await
                != crate::pg_enums::PgAcquisitionStatus::Undefined
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a cancelled acquisition must reset the status");
        assert!(zip_files_under(&cache_dir).is_empty());
        stalled_server.abort();
    }
}
