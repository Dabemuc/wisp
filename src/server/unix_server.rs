use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::Winsize;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::mux::Mux;
use crate::common::protocoll::{ClientMessage, ServerMessage, read_msg, write_msg};

/// Commands the async side sends TO the (single-threaded, !Send) mux.
enum MuxCmd {
    /// A client attached: register its frame channel and adopt its size.
    Attach {
        frames: UnboundedSender<ServerMessage>,
        cols: u16,
        rows: u16,
    },
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
}

/// A cheap, cloneable handle the async tasks use to talk to the mux thread.
/// Sending pushes a command and nudges the mux thread's poll via a self-pipe.
#[derive(Clone)]
struct MuxHandle {
    cmd_tx: std_mpsc::Sender<MuxCmd>,
    wake: Arc<OwnedFd>,
}

impl MuxHandle {
    fn send(&self, cmd: MuxCmd) {
        let _ = self.cmd_tx.send(cmd);
        // Nudge the mux thread's poll(). One byte, non-blocking, coalescing — if the
        // pipe is already "armed" a failed write is harmless (a wake is already pending).
        let byte = [0u8; 1];
        unsafe {
            libc::write(self.wake.as_raw_fd(), byte.as_ptr().cast(), 1);
        }
    }
}

pub struct UnixServer {
    listener: UnixListener,
    socket_file_path: String,
    mux: MuxHandle,
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

        // Self-pipe used to wake the mux thread's poll() when a command arrives.
        let mut pipe_fds = [0i32; 2];
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            panic!("[SERVER] Failed to create wake pipe");
        }
        let wake_r = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
        let wake_w = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        // Write end non-blocking so MuxHandle::send never blocks.
        unsafe {
            let f = libc::fcntl(wake_w.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(wake_w.as_raw_fd(), libc::F_SETFL, f | libc::O_NONBLOCK);
        }

        let (cmd_tx, cmd_rx) = std_mpsc::channel::<MuxCmd>();

        // The Mux is BORN on this thread and never leaves it — that's what makes its
        // !Send contents legal. Only Send things (commands, senders, byte vecs) cross.
        std::thread::spawn(move || run_mux(cmd_rx, wake_r));

        UnixServer {
            listener,
            socket_file_path: socket_file_path.to_owned(),
            mux: MuxHandle {
                cmd_tx,
                wake: Arc::new(wake_w),
            },
        }
    }

    /// Accept loop — never returns, so it keeps the process (and runtime) alive.
    pub async fn run(self) {
        let shutdown = Arc::new(Notify::new());
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    if let Ok((conn, _)) = accepted {
                        let s = shutdown.clone();
                        let mux = self.mux.clone();
                        tokio::spawn(async move { handle_connection(conn, s, mux).await; });
                    }
                }
                _ = shutdown.notified() => break,
            }
        }
        let _ = std::fs::remove_file(self.socket_file_path); // don't leave a stale socket
    }
}

/// Per-connection async task. Reads client messages -> mux, and drains a per-client
/// frame channel -> socket (on its own writer task, so a framed socket read is never
/// cancelled mid-message).
async fn handle_connection(conn: UnixStream, shutdown: Arc<Notify>, mux: MuxHandle) {
    let (mut rd, mut wr) = conn.into_split();
    let (frame_tx, mut frame_rx) = unbounded_channel::<ServerMessage>();

    // mux/server -> this client's socket
    let writer = tokio::spawn(async move {
        while let Some(msg) = frame_rx.recv().await {
            if write_msg(&mut wr, &msg).await.is_err() {
                break;
            }
        }
    });

    // client's socket -> mux
    loop {
        match read_msg::<_, ClientMessage>(&mut rd).await {
            Ok(ClientMessage::Attach { cols, rows }) => {
                println!("[SERVER] A client attached ({cols}x{rows})");
                mux.send(MuxCmd::Attach {
                    frames: frame_tx.clone(),
                    cols,
                    rows,
                });
            }
            Ok(ClientMessage::Input(bytes)) => mux.send(MuxCmd::Input(bytes)),
            Ok(ClientMessage::Resize { cols, rows }) => mux.send(MuxCmd::Resize { cols, rows }),
            Ok(ClientMessage::ListSessions) => {
                let _ = frame_tx.send(ServerMessage::Sessions(vec![]));
            }
            Ok(ClientMessage::KillServer) => {
                println!("[SERVER] Killed");
                shutdown.notify_one();
                break;
            }
            Err(_) => break, // client disconnected
        }
    }
    writer.abort();
}

/// The mux event loop. Owns the (!Send) Mux for the whole process lifetime. This is the
/// old single-process reactor, re-plumbed: "stdin" is the command channel (woken by the
/// self-pipe), "stdout" is each attached client's frame channel.
fn run_mux(cmd_rx: std_mpsc::Receiver<MuxCmd>, wake_r: OwnedFd) {
    // Child shells must see a TERM matching what libghostty-vt emulates. Set it here,
    // before Mux::new forks any pane.
    unsafe { std::env::set_var("TERM", "xterm-256color") };

    let mut mux = Mux::new(Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    })
    .expect("[SERVER] Failed to start Mux");

    let mut clients: Vec<UnboundedSender<ServerMessage>> = Vec::new();
    let wake_fd = wake_r.as_raw_fd();

    loop {
        // --- who is ready? wake pipe (commands) + all pane fds (output) ---
        let (wake_ready, ready_panes) = {
            let pane_fds: Vec<(usize, usize, BorrowedFd)> = mux.pane_fds().collect();
            let mut fds = Vec::with_capacity(pane_fds.len() + 1);
            fds.push(PollFd::new(wake_r.as_fd(), PollFlags::POLLIN));
            for (_, _, fd) in &pane_fds {
                fds.push(PollFd::new(fd.as_fd(), PollFlags::POLLIN));
            }

            match poll(&mut fds, PollTimeout::NONE) {
                Ok(_) => {}
                Err(Errno::EINTR) => continue,
                Err(_) => return,
            }

            let readable = |f: &PollFd| {
                f.revents()
                    .unwrap_or(PollFlags::empty())
                    .intersects(PollFlags::POLLIN | PollFlags::POLLHUP)
            };
            let wake_ready = readable(&fds[0]);
            let ready_panes: Vec<(usize, usize)> = pane_fds
                .iter()
                .enumerate()
                .filter(|(slot, _)| readable(&fds[slot + 1]))
                .map(|(_, (w, p, _))| (*w, *p))
                .collect();
            (wake_ready, ready_panes)
        }; // pane_fds/fds dropped -> mux is free for &mut again

        let mut dirty = false;

        // --- commands from clients ---
        if wake_ready {
            // Clear the wake pipe (one read is enough; leftover just causes another wake).
            let mut drain = [0u8; 256];
            unsafe {
                libc::read(wake_fd, drain.as_mut_ptr().cast(), drain.len());
            }
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    MuxCmd::Attach { frames, cols, rows } => {
                        let _ = mux.resize(winsize(cols, rows));
                        clients.push(frames);
                        dirty = true; // send the current screen to the newcomer
                    }
                    MuxCmd::Input(bytes) => {
                        let _ = mux.handle_input(&bytes);
                        dirty = true;
                    }
                    MuxCmd::Resize { cols, rows } => {
                        let _ = mux.resize(winsize(cols, rows));
                        dirty = true;
                    }
                }
            }
        }

        // --- pane output ---
        let mut exited = Vec::new();
        for (w, p) in &ready_panes {
            match mux.pump(*w, *p) {
                Ok(true) => dirty = true,
                Ok(false) => exited.push((*w, *p)),
                Err(_) => exited.push((*w, *p)),
            }
        }
        for (w, p) in exited.into_iter().rev() {
            let _ = mux.close_pane(w, p);
            dirty = true;
        }

        // --- render once, broadcast to attached clients (dropping dead ones) ---
        if dirty
            && !clients.is_empty()
            && let Ok(frame) = mux.render_frame()
        {
            let bytes = frame.into_bytes();
            clients.retain(|c| c.send(ServerMessage::Frame(bytes.clone())).is_ok());
        }
    }
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}
