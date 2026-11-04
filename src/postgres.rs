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
        // this instance never acquired anything and waiters must be free to retry.
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
        let pg_bin_data = &self.fetch_settings.fetch_postgres().await?;
        self.pg_access.write_pg_zip(&pg_bin_data).await?;
        pg_unpack::unpack_postgres(&self.pg_access.zip_file_path, &self.pg_access.cache_dir)
            .await?;
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
}
