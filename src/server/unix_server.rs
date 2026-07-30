use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::session::SessionHandle;
use crate::common::protocoll::{ClientMessage, ServerMessage, read_msg, write_msg};
use crate::server::business_objects::ServerCommand;

pub struct UnixServer {
    listener: UnixListener,
    socket_file_path: String,
    session: SessionHandle,
    shutdown: Arc<Notify>,
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

        // Shared shutdown signal: fired by KillServer or when the session's last window
        // exits. The accept loop selects on it.
        let shutdown = Arc::new(Notify::new());

        // One session for now. Multi-session = a registry of these (Step 2).
        let session = SessionHandle::spawn(shutdown.clone());

        UnixServer {
            listener,
            socket_file_path: socket_file_path.to_owned(),
            session,
            shutdown,
        }
    }

    /// Accept loop — never returns, so it keeps the process (and runtime) alive.
    pub async fn run(self) {
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    if let Ok((conn, _)) = accepted {
                        let s = self.shutdown.clone();
                        let session = self.session.clone();
                        tokio::spawn(async move { handle_connection(conn, s, session).await; });
                    }
                }
                _ = self.shutdown.notified() => break,
            }
        }
        let _ = std::fs::remove_file(self.socket_file_path); // don't leave a stale socket
    }
}

/// Per-connection async task. Reads client messages -> session, and drains a per-client
/// frame channel -> socket (on its own writer task, so a framed socket read is never
/// cancelled mid-message).
async fn handle_connection(conn: UnixStream, shutdown: Arc<Notify>, session: SessionHandle) {
    let (mut rd, mut wr) = conn.into_split();
    let (frame_tx, mut frame_rx) = unbounded_channel::<ServerMessage>();

    // session -> this client's socket
    let writer = tokio::spawn(async move {
        while let Some(msg) = frame_rx.recv().await {
            if write_msg(&mut wr, &msg).await.is_err() {
                break;
            }
        }
    });

    // client's socket -> session
    loop {
        match read_msg::<_, ClientMessage>(&mut rd).await {
            Ok(ClientMessage::ExecuteServerCommand(cmd)) => {
                match handle_command(&session, &frame_tx, &shutdown, cmd.into()) {
                    CommandResult::Shutdown => break,
                    _ => continue,
                }
            }
            Ok(ClientMessage::Input(bytes)) => session.handle_input(bytes),
            Ok(ClientMessage::Resize { cols, rows }) => session.handle_resize(cols, rows),
            Err(_) => break, // client disconnected
        }
    }
    writer.abort();
}

enum CommandResult {
    Shutdown,
    None,
}

fn handle_command(
    session: &SessionHandle,
    frame_tx: &UnboundedSender<ServerMessage>,
    shutdown: &Arc<Notify>,
    cmd: ServerCommand,
) -> CommandResult {
    match cmd {
        ServerCommand::KillServer => {
            println!("[SERVER] Killed");
            shutdown.notify_one();
            return CommandResult::Shutdown;
        }
        ServerCommand::ListSessions => {
            // Not impleneted/used yet. Send empty array for now
            let _ = frame_tx.send(ServerMessage::Sessions(vec![]));
        }
        ServerCommand::Attach(ts) => session.handle_attach(frame_tx.clone(), ts.cols, ts.rows),
        ServerCommand::Session(sess_cmd) => session.handle_command(sess_cmd),
    }

    CommandResult::None
}
