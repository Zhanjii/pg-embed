//!
//! Fetch postgresql binaries
//!
//! Download and unpack postgresql binaries
//!

use std::path::PathBuf;

use bytes::Bytes;
use futures::TryFutureExt;
use reqwest::Response;

use crate::pg_enums::{Architecture, OperationSystem};
use crate::pg_errors::{PgEmbedError, PgEmbedErrorType};
use crate::pg_types::PgResult;

/// Postgresql version struct (simple version wrapper)
pub struct PostgresVersion(pub &'static str);
/// Latest postgres version 15
pub const PG_V15: PostgresVersion = PostgresVersion("15.1.0");
/// Latest postgres version 14
pub const PG_V14: PostgresVersion = PostgresVersion("14.6.0");
/// Latest postgres version 13
pub const PG_V13: PostgresVersion = PostgresVersion("13.9.0");
/// Latest postgres version 12
pub const PG_V12: PostgresVersion = PostgresVersion("12.13.0");
/// Latest pstgres version 11
pub const PG_V11: PostgresVersion = PostgresVersion("11.18.0");
/// Latest postgres version 10
pub const PG_V10: PostgresVersion = PostgresVersion("10.23.0");
/// Latest postgres version 9
pub const PG_V9: PostgresVersion = PostgresVersion("9.6.24");

/// Maximum number of bytes of an error response body that is logged
///
/// 512 is enough of an HTML error page to diagnose a bad mirror, while bounding
/// what is buffered and logged.
const MAX_LOGGED_BODY_BYTES: usize = 512;

/// Settings that determine the postgres binary to be fetched
pub struct PgFetchSettings {
    /// The repository base url (scheme, host and optional path prefix, e.g.
    /// `https://artifactory.example.com/artifactory/maven-remote`)
    ///
    /// `/maven2/io/zonky/...` is always appended, so a mirror must serve the Maven layout
    /// `<host>/maven2/io/zonky/test/postgres/embedded-postgres-binaries-<platform>/<version>/embedded-postgres-binaries-<platform>-<version>.jar`
    /// under that base.
    /// Credentials in the host (`https://user:token@mirror`) are sent as basic auth
    /// but redacted from logs and errors.
    /// Only the userinfo part (`user:password@`) is redacted; tokens in the path or query
    /// string are logged as-is, so pass credentials as userinfo instead.
    pub host: String,
    /// The operation system
    pub operating_system: OperationSystem,
    /// The cpu architecture
    pub architecture: Architecture,
    /// The postgresql version
    pub version: PostgresVersion,
    /// Directory of a complete existing postgresql installation (`bin/`, `lib/`, `share/`)
    ///
    /// `bin/` must hold `initdb`, `pg_ctl` and `postgres`, and `share/` must exist;
    /// only `bin/` and `share/` are checked. `share/` must sit in the same directory as
    /// `bin/`; split distro layouts (Debian/Ubuntu) are rejected.
    ///
    /// Must be the same major version as `version`; this is not checked.
    /// When set, pg-embed never downloads binaries and uses these executables instead.
    pub local_binaries_dir: Option<PathBuf>,
}

impl Default for PgFetchSettings {
    fn default() -> Self {
        PgFetchSettings {
            host: "https://repo1.maven.org".to_string(),
            operating_system: OperationSystem::default(),
            architecture: Architecture::default(),
            version: PG_V13,
            local_binaries_dir: None,
        }
    }
}

impl PgFetchSettings {
    /// The platform string (*needed to determine the download path*)
    pub fn platform(&self) -> String {
        let os = self.operating_system.to_string();
        let arch = if self.operating_system == OperationSystem::AlpineLinux {
            format!("{}-{}", self.architecture.to_string(), "alpine")
        } else {
            self.architecture.to_string()
        };
        format!("{}-{}", os, arch)
    }

    ///
    /// The url the postgres binaries are downloaded from
    ///
    pub fn download_url(&self) -> String {
        let platform = self.platform();
        let version = self.version.0;
        // A trailing slash on a mirror host would otherwise produce `//maven2`.
        let host = self.host.trim_end_matches('/');
        format!(
            "{}/maven2/io/zonky/test/postgres/embedded-postgres-binaries-{}/{}/embedded-postgres-binaries-{}-{}.jar",
            host, platform, version, platform, version
        )
    }

    ///
    /// Fetch postgres binaries
    ///
    /// Returns the data of the downloaded binary in an `Ok([u8])` on success, otherwise returns an error.
    ///
    pub async fn fetch_postgres(&self) -> PgResult<Box<Bytes>> {
        let download_url = self.download_url();
        // The host may carry basic-auth credentials; only the redacted form is ever logged or put in errors.
        let loggable_url = redact_url_credentials(&download_url);
        log::debug!("downloading PostgreSQL binaries from {}", loggable_url);
        let mut response: Response = reqwest::get(&download_url)
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::DownloadFailure,
                source: Some(Box::new(e)),
                message: Some(format!(
                    "failed to download PostgreSQL binaries from {}",
                    loggable_url
                )),
            })
            .await?;

        let status = response.status();
        if !status.is_success() {
            // Without this check an HTML error page would be written to disk and unpacked as a zip.
            // The body is only read for the log; failing to read it must not hide the status error.
            // Reading stops at the log limit so a huge error page is not buffered.
            let mut body_prefix: Vec<u8> = Vec::new();
            let mut body_read_error: Option<reqwest::Error> = None;
            while body_prefix.len() < MAX_LOGGED_BODY_BYTES {
                match response.chunk().await {
                    Ok(Some(chunk)) => body_prefix.extend_from_slice(&chunk),
                    Ok(None) => break,
                    Err(error) => {
                        body_read_error = Some(error);
                        break;
                    }
                }
            }
            let body_for_log = match body_read_error {
                None => truncate_for_log(&body_prefix),
                Some(error) => format!("<body unreadable: {}>", error),
            };
            log::warn!(
                "downloading PostgreSQL binaries from {} failed with HTTP status {}",
                loggable_url,
                status
            );
            // Some proxies (Artifactory, Nexus) echo request headers in error pages, which
            // would include the Authorization header built from credentials in `host`, so the
            // body is never logged for a credentialed request, not even at debug.
            if url_has_credentials(&download_url) {
                log::debug!(
                    "error response body from {} withheld because the request carried credentials, {} bytes",
                    loggable_url,
                    body_prefix.len()
                );
            } else {
                log::debug!(
                    "error response body from {}: {}",
                    loggable_url,
                    body_for_log
                );
            }
            return Err(PgEmbedError {
                error_type: PgEmbedErrorType::DownloadFailure,
                source: None,
                message: Some(format!(
                    "downloading PostgreSQL binaries from {} failed with HTTP status {}",
                    loggable_url, status
                )),
            });
        }

        let content: Bytes = response
            .bytes()
            .map_err(|e| PgEmbedError {
                error_type: PgEmbedErrorType::ConversionFailure,
                source: Some(Box::new(e)),
                message: Some(format!(
                    "failed to read the response body from {}",
                    loggable_url
                )),
            })
            .await?;

        Ok(Box::new(content))
    }
}

/// Strip basic-auth credentials from a url so it is safe to log or embed in an error
fn redact_url_credentials(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            if !parsed.username().is_empty() || parsed.password().is_some() {
                // These only fail for cannot-be-a-base urls, which cannot carry credentials.
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
            }
            parsed.to_string()
        }
        // An unparseable url may still embed credentials; reqwest fails on it too and its error source is attached.
        Err(_) => "<unparseable download url>".to_string(),
    }
}

/// Whether a url carries basic-auth userinfo, i.e. the request sends an Authorization header
fn url_has_credentials(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(parsed) => !parsed.username().is_empty() || parsed.password().is_some(),
        // Callers use this to decide whether logging is safe; an unparseable url may still
        // embed credentials, so it is treated as credentialed.
        Err(_) => true,
    }
}

/// Render an error response body for logging, cut to about `MAX_LOGGED_BODY_BYTES` (plus a `...` marker)
fn truncate_for_log(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if text.len() <= MAX_LOGGED_BODY_BYTES {
        return text.into_owned();
    }
    // Cutting inside a multi-byte character would panic on the slice.
    let mut end = MAX_LOGGED_BODY_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn fixed_settings(host: &str) -> PgFetchSettings {
        PgFetchSettings {
            host: host.to_string(),
            operating_system: OperationSystem::Linux,
            architecture: Architecture::Amd64,
            version: PG_V15,
            local_binaries_dir: None,
        }
    }

    #[test]
    fn download_url_for_default_host() {
        let settings = PgFetchSettings {
            operating_system: OperationSystem::Linux,
            architecture: Architecture::Amd64,
            version: PG_V15,
            ..Default::default()
        };
        assert_eq!(
            settings.download_url(),
            "https://repo1.maven.org/maven2/io/zonky/test/postgres/embedded-postgres-binaries-linux-amd64/15.1.0/embedded-postgres-binaries-linux-amd64-15.1.0.jar"
        );
    }

    #[test]
    fn download_url_for_custom_host() {
        let settings = fixed_settings("https://mirror.example.com");
        assert_eq!(
            settings.download_url(),
            "https://mirror.example.com/maven2/io/zonky/test/postgres/embedded-postgres-binaries-linux-amd64/15.1.0/embedded-postgres-binaries-linux-amd64-15.1.0.jar"
        );
    }

    #[test]
    fn download_url_trims_trailing_slash_of_host() {
        let settings = fixed_settings("https://mirror.example.com/");
        let download_url = settings.download_url();
        assert!(!download_url.contains("//maven2"));
        assert!(download_url.starts_with("https://mirror.example.com/maven2/"));
    }

    #[test]
    fn download_url_keeps_path_prefix_of_host() {
        let settings = fixed_settings("https://artifactory.example.com/artifactory/maven-remote/");
        assert!(settings.download_url().starts_with(
            "https://artifactory.example.com/artifactory/maven-remote/maven2/io/zonky/"
        ));
    }

    #[test]
    fn truncate_for_log_respects_char_boundaries() {
        let body = "é".repeat(400);
        let truncated = truncate_for_log(body.as_bytes());
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= MAX_LOGGED_BODY_BYTES + 3);
    }

    // reqwest's default client honours HTTP_PROXY/HTTPS_PROXY; set NO_PROXY=127.0.0.1 if
    // these tests fail behind a proxy.
    /// Serves exactly one canned HTTP response on a local port and returns the host url.
    async fn serve_once(status_line: &'static str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let head = format!(
                "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                status_line,
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        format!("http://127.0.0.1:{}", port)
    }

    #[tokio::test]
    async fn fetch_postgres_maps_404_to_download_failure() {
        let host = serve_once("404 Not Found", b"<html>not here</html>".to_vec()).await;
        let settings = fixed_settings(&host);
        let error = settings.fetch_postgres().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::DownloadFailure);
        let message = error.message.unwrap();
        assert!(message.contains(&settings.download_url()), "{}", message);
        assert!(message.contains("404"), "{}", message);
    }

    #[test]
    fn redact_url_credentials_removes_user_and_password() {
        let redacted =
            redact_url_credentials("https://user:secret@mirror.internal/maven2/some/file.jar");
        assert!(!redacted.contains("user"), "{}", redacted);
        assert!(!redacted.contains("secret"), "{}", redacted);
        assert!(redacted.contains("mirror.internal"), "{}", redacted);
        assert!(redacted.contains("/maven2/some/file.jar"), "{}", redacted);
    }

    #[test]
    fn redact_url_credentials_hides_unparseable_url() {
        assert_eq!(
            redact_url_credentials("not a url user:secret@"),
            "<unparseable download url>"
        );
    }

    #[test]
    fn url_has_credentials_detects_userinfo() {
        assert!(url_has_credentials(
            "https://user:secret@mirror.internal/x.jar"
        ));
        assert!(url_has_credentials("https://user@mirror.internal/x.jar"));
        assert!(url_has_credentials("https://:secret@mirror.internal/x.jar"));
    }

    #[test]
    fn url_has_credentials_is_false_without_userinfo() {
        assert!(!url_has_credentials("https://mirror.internal/x.jar"));
    }

    #[test]
    fn url_has_credentials_treats_unparseable_url_as_credentialed() {
        assert!(url_has_credentials("not a url user:secret@"));
    }

    #[tokio::test]
    async fn fetch_postgres_error_does_not_leak_host_credentials() {
        let host = serve_once("404 Not Found", b"nope".to_vec()).await;
        let host_with_credentials = host.replace("http://", "http://user:secret@");
        let settings = fixed_settings(&host_with_credentials);
        let error = settings.fetch_postgres().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::DownloadFailure);
        let message = error.message.unwrap();
        assert!(!message.contains("secret"), "{}", message);
        assert!(message.contains("404"), "{}", message);
    }

    #[tokio::test]
    async fn fetch_postgres_transport_error_does_not_leak_host_credentials() {
        // The error source is the raw reqwest error; this pins that reqwest keeps stripping
        // userinfo from its own url so an upgrade cannot silently start leaking credentials.
        // The listener stays bound (a freed port could be reused by a parallel test) and the
        // connection is closed without a response, which reqwest reports as a transport error.
        // reqwest's default client honours HTTP_PROXY/HTTPS_PROXY; set NO_PROXY=127.0.0.1 if
        // this test fails behind a proxy.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
        });
        let settings = fixed_settings(&format!("http://user:secret@127.0.0.1:{}", port));
        let error = settings.fetch_postgres().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::DownloadFailure);
        let message = error.message.unwrap();
        assert!(message.contains("failed to download"), "{}", message);
        assert!(!message.contains("secret"), "{}", message);
        let source = error.source.unwrap();
        assert!(!format!("{:?}", source).contains("secret"), "{:?}", source);
        assert!(!source.to_string().contains("secret"), "{}", source);
    }

    #[tokio::test]
    async fn fetch_postgres_maps_500_to_download_failure() {
        let host = serve_once("500 Internal Server Error", b"boom".to_vec()).await;
        let settings = fixed_settings(&host);
        let error = settings.fetch_postgres().await.unwrap_err();
        assert_eq!(error.error_type, PgEmbedErrorType::DownloadFailure);
        let message = error.message.unwrap();
        assert!(message.contains(&settings.download_url()), "{}", message);
        assert!(message.contains("500"), "{}", message);
    }

    #[tokio::test]
    async fn fetch_postgres_returns_body_on_200() {
        let body = b"PK fake jar bytes".to_vec();
        let host = serve_once("200 OK", body.clone()).await;
        let settings = fixed_settings(&host);
        let fetched = settings.fetch_postgres().await.unwrap();
        assert_eq!(fetched.as_ref().as_ref(), body.as_slice());
    }
}
