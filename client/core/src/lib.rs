//! DropCraft-Kern: API-Client und Sync-Engine. Wird von der Desktop-App
//! benutzt und soll später auch in der Handy-App laufen.

pub mod api;
pub mod state;
pub mod sync;

pub use api::{Api, ApiError, Me, RemoteEntry};
pub use state::State;
pub use sync::{Event, EventKind, Report, SyncEngine, SyncError};
