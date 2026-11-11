//! DropCraft-Kern: API-Client und Sync-Engine. Wird von der Desktop-App
//! benutzt und soll später auch in der Handy-App laufen.

pub mod api;
pub mod bandwidth;
pub mod hooks;
pub mod progress;
pub mod state;
pub mod sync;

pub use api::{Api, ApiError, Me, RemoteEntry, Share, ShareSettings, UserInfo};
pub use bandwidth::{Bandwidth, BandwidthRef, PauseFlag};
pub use hooks::{FullDownload, LocalHooks};
pub use progress::{Progress, ProgressRef};
pub use state::State;
pub use sync::{Event, EventKind, Report, SyncEngine, SyncError};
