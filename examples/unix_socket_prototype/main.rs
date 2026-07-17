use crate::{client::UnixClient, server::UnixServer};

mod args;
mod client;
mod server;

use args::Args;
use clap::Parser;

const SOCKET_FILE_PATH: &str = "/tmp/wisp_mux/wisp.sock";

#[tokio::main]
async fn main() {
    // Parse args
    let args = Args::parse();

    // If server flag is set
    if args.server {
        // Start server
        let server = UnixServer::new(SOCKET_FILE_PATH).await;
        server.run().await; // never returns -> process stays up
    } else {
        // Else start client
        let client = UnixClient::new(SOCKET_FILE_PATH).await;
        client.run().await;
    }
}
