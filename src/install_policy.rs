//! Shared Windows/macOS time budgets for terminal-client installation.
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SOURCE_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
pub const OFFICIAL_INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
pub const MIRROR_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
pub const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

// Bound npm's internal retries too, so a stalled registry does not spend the
// entire installation budget retrying the same connection.
pub fn npm_network_environment() -> [(&'static str, &'static str); 4] {
    [
        ("npm_config_fetch_timeout", "30000"),
        ("npm_config_fetch_retries", "1"),
        ("npm_config_fetch_retry_mintimeout", "1000"),
        ("npm_config_fetch_retry_maxtimeout", "3000"),
    ]
}
