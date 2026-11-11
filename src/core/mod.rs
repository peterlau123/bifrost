// Core module - data models, error types, and bridge abstraction
pub mod batch_tracker;
pub mod bridge;
pub mod error;
pub mod fetcher;
pub mod http_bridge;
pub mod job;
pub mod lock;
pub mod models;
pub mod protocol;
pub mod pusher;
pub mod sdnotify;
pub mod ssh_bridge;
pub use batch_tracker::BatchTracker;
pub use bridge::{Bridge, TaskStatusResponse};
pub mod settings;
pub mod warmup;
