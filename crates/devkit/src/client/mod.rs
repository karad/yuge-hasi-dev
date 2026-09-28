//! SteamOS client identity, trusted-host enrollment, and remote connections.

mod connection;
mod identity;
mod process;

/// Device connection type and SSH argument builder.
pub use connection::{Connection, ssh_args};
/// Client identity and trusted-host operations.
pub use identity::{config, fingerprint, init, pair, public_key, trust};
/// Process execution interface and system implementation.
pub use process::{Runner, SystemRunner, execute};
