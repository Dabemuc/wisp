use tokio::net::unix::SocketAddr;
use tokio::net::{UnixListener, UnixStream};

pub struct UnixServer {
    listener: UnixListener,
}

impl UnixServer {
    pub async fn new(socket_file_path: &str) -> Self {
        std::fs::create_dir_all("/tmp/wisp_mux").expect("[SERVER] Failed to create runtime dir");
        if std::path::Path::new(socket_file_path).exists() {
            std::fs::remove_file(socket_file_path).expect("[SERVER] Failed to remove stale socket");
        }
        let listener =
            UnixListener::bind(socket_file_path).expect("[SERVER] Failed to bind to socket");
        println!("[SERVER] Listening on {}", socket_file_path);
        UnixServer { listener }
    }

    /// Accept loop — never returns, so it keeps the process (and runtime) alive.
    pub async fn run(self) {
        loop {
            match self.listener.accept().await {
                Ok((conn, addr)) => {
                    tokio::spawn(async move {
                        handle_connection(conn, addr).await;
                    });
                }
                Err(e) => eprintln!("[SERVER] accept error: {e}"), // log, keep looping
            }
        }
    }
}

async fn handle_connection(conn: UnixStream, addr: SocketAddr) {
    println!("[SERVER] A Client connected: {:?} - {:?}", conn, addr);
}
