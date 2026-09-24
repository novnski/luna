//! Owner-run, routing-only reverse tunnel for LUNA's L29 guarded RPC socket.
//! A relay credential admits a route; only the environment's independent L29
//! per-socket service can grant an RPC principal.

pub mod protocol;
mod server;
mod tunnel;

pub use protocol::{ClientKey, RelayConfig, RouteConfig, Target, read_private_secret};
pub use server::serve;
pub use tunnel::{client_socket, run_client_forward, run_host};
