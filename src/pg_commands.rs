//!
//! Create postgres command executor
//!
//! Command executors for initdb, pg_ctl start, pg_ctl stop
//!
use std::path::PathBuf;

use crate::command_executor::{AsyncCommand, AsyncCommandExecutor};
use crate::pg_enums::{PgAuthMethod, PgProcessType, PgServerStatus};
use crate::pg_errors::PgEmbedError;
use crate::pg_types::PgResult;

///
/// Postgres command executors
///
pub struct PgCommand {}

impl PgCommand {
    ///
    /// Create initdb command
    ///
    pub fn init_db_executor(
        init_db_exe: &PathBuf,
        database_dir: &PathBuf,
        pw_file_path: &PathBuf,
        user: &str,
        auth_method: &PgAuthMethod,
    ) -> PgResult<AsyncCommandExecutor<PgServerStatus, PgEmbedError, PgProcessType>> {
        let init_db_executable = init_db_exe.as_os_str();
        let password_file_arg = format!("--pwfile={}", pw_file_path.to_str().unwrap());
        let auth_host = match auth_method {
            PgAuthMethod::Plain => "password",
            PgAuthMethod::MD5 => "md5",
            PgAuthMethod::ScramSha256 => "scram-sha-256",
        };
        let args = [
            "-A",
            auth_host,
            "-U",
            user,
            "-D",
            database_dir.to_str().unwrap(),
            &password_file_arg,
        ];

        let command_executor =
            AsyncCommandExecutor::<PgServerStatus, PgEmbedError, PgProcessType>::new(
                init_db_executable,
                args,
                PgProcessType::InitDb,
            )?;

        Ok(command_executor)
    }

    ///
    /// Create pg_ctl start command
    ///
    pub fn start_db_executor(
        pg_ctl_exe: &PathBuf,
        database_dir: &PathBuf,
        port: &u16,
    ) -> PgResult<AsyncCommandExecutor<PgServerStatus, PgEmbedError, PgProcessType>> {
        let pg_ctl_executable = pg_ctl_exe.as_os_str();
        let port_arg = Self::start_db_server_options(port);
        let args = [
            "-o",
            &port_arg,
            "start",
            "-w",
            "-D",
            database_dir.to_str().unwrap(),
        ];
        let command_executor =
            AsyncCommandExecutor::<PgServerStatus, PgEmbedError, PgProcessType>::new(
                pg_ctl_executable,
                args,
                PgProcessType::StartDb,
            )?;

        Ok(command_executor)
    }

    ///
    /// Server options passed to postgres through `pg_ctl start -o`
    ///
    /// Upstream passes `-F` here, which turns fsync off. A host that loses
    /// power with fsync off can be left with no valid checkpoint record and a
    /// cluster that never starts again (`PANIC: could not locate a valid
    /// checkpoint record`). Command-line options override postgresql.conf and
    /// ALTER SYSTEM, so callers could not turn fsync back on. Only the port is
    /// passed; callers that want faster writes can set `synchronous_commit =
    /// off` per connection, which risks losing the last few commits but never
    /// corrupts the cluster.
    ///
    fn start_db_server_options(port: &u16) -> String {
        format!("-p {}", port)
    }

    ///
    /// Create pg_ctl stop command
    ///
    pub fn stop_db_executor(
        pg_ctl_exe: &PathBuf,
        database_dir: &PathBuf,
    ) -> PgResult<AsyncCommandExecutor<PgServerStatus, PgEmbedError, PgProcessType>> {
        let pg_ctl_executable = pg_ctl_exe.as_os_str();
        let args = ["stop", "-w", "-D", database_dir.to_str().unwrap()];
        let command_executor =
            AsyncCommandExecutor::<PgServerStatus, PgEmbedError, PgProcessType>::new(
                pg_ctl_executable,
                args,
                PgProcessType::StopDb,
            )?;

        Ok(command_executor)
    }
}

#[cfg(test)]
mod tests {
    use super::PgCommand;

    #[test]
    fn test_start_options_do_not_disable_fsync() {
        let options = PgCommand::start_db_server_options(&15433);
        let tokens: Vec<&str> = options.split_whitespace().collect();
        assert!(
            !tokens.contains(&"-F"),
            "start options disable fsync: {}",
            options
        );
        assert!(
            !options.contains("fsync=off") && !options.contains("fsync=0"),
            "start options disable fsync: {}",
            options
        );
    }

    #[test]
    fn test_start_options_pass_port() {
        assert_eq!(PgCommand::start_db_server_options(&15433), "-p 15433");
    }
}
