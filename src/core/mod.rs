// Core module - data models and error types
pub mod models;
pub mod error;
pub mod protocol;
pub mod lock;
pub mod config;
pub mod batch_tracker;
pub use batch_tracker::BatchTracker;
pub mod db;