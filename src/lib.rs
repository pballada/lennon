#[cfg(not(unix))]
compile_error!(
    "Lennon currently supports macOS and Linux; Windows process-group support is not implemented."
);
pub mod cli;
pub mod config;
pub mod context;
pub mod harness;
pub mod provider;
pub mod runner;
pub mod store;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    message.into().into()
}
