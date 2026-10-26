//! Configuration, lifecycle and bounded event types shared by the frontends and servers.

mod config;
mod service;

pub use config::{Config, HttpConfig, NfsConfig, TftpConfig, default_config_path};
pub use service::{
    Endpoint, Event, EventSender, Protocol, ServerHandle, ServiceState, Transport, emit,
};
