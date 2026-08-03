// Core module - data models, error types, and bridge abstraction
pub mod batch_tracker;
pub mod bridge;
pub mod error;
pub mod job;
pub mod lock;
pub mod models;
pub mod protocol;
pub use batch_tracker::BatchTracker;
pub use bridge::{Bridge, TaskStatusResponse};
pub mod settings;
