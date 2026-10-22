pub mod analysis;
pub mod frontend;
pub mod highlight;
pub mod semantic;
mod server;
pub mod workspace;

/// Serve the LSP protocol on standard input and output.
///
/// # Errors
///
/// Returns an error after draining owned work if a server invariant or
/// supervised background task fails.
pub async fn run() -> std::io::Result<()> {
    server::run().await
}
