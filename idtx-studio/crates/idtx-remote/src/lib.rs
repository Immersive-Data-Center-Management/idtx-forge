//! IDTX remote server connectivity.
//!
//! Configures a server connection ([`config`]), authenticates against it
//! ([`auth`]), and lists, health-checks, and logs into it ([`client`]). The
//! editor's remote browser uses this to discover server-hosted stages; the
//! asset bytes themselves are fetched through the `idtx-usd-resolver-http`
//! resolver plugin.

pub mod auth;
pub mod client;
pub mod config;
pub mod error;

pub use auth::{AuthError, AuthProvider, AuthRegistry, StudioConfigProvider, host_of};
pub use client::{RemoteEntry, health_check, list_files, login};
pub use config::{
    ServerConfig, cache_dir, project_cache_dir, project_cache_dir_in, load_config, load_configs, resolve_password, save_config,
    save_configs,
};
pub use error::{ConfigError, RemoteError};
