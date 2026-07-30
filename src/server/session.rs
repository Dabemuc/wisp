use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::Winsize;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;

use super::mux::Mux;
use crate::common::dtos::{CursorDataDTO, FrameDataDTO};
use crate::common::protocoll::ServerMessage;
use crate::server::business_objects::SessionCommand;

enum MessageToSessionThread {
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
    ExecuteCommand(SessionCommand),
}

/// A cheap, cloneable handle to one session (one mux thread). Sending pushes a command
/// and nudges the mux thread's poll via a self-pipe.
#[derive(Clone)]
pub struct SessionHandle {
    cmd_tx: std_mpsc::Sender<MessageToSessionThread>,
    wake: Arc<OwnedFd>,
}

impl SessionHandle {
    /// Spawn a new session: a dedicated thread that owns the !Send `Mux`, plus a
    /// self-pipe to wake its poll(). `shutdown` is fired when the session's last window
    /// exits. Returns a handle to talk to it.
    pub fn spawn(shutdown: Arc<Notify>) -> Self {
        // Self-pipe used to wake the mux thread's poll() when a command arrives.
        let mut pipe_fds = [0i32; 2];
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            panic!("[SESSION] Failed to create wake pipe");
        }
        let wake_r = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
        let wake_w = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        // Write end non-blocking so `send` never blocks.
        unsafe {
            let f = libc::fcntl(wake_w.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(wake_w.as_raw_fd(), libc::F_SETFL, f | libc::O_NONBLOCK);
        }

        let (cmd_tx, cmd_rx) = std_mpsc::channel::<MessageToSessionThread>();

        // The Mux is BORN on this thread and never leaves it — that's what makes its
        // !Send contents legal. Only Send things (commands, senders, byte vecs) cross.
        std::thread::spawn(move || run_session(cmd_rx, wake_r, shutdown));

        SessionHandle {
            cmd_tx,
            wake: Arc::new(wake_w),
        }
    }

    pub fn handle_command(&self, cmd: SessionCommand) {
        self.send(MessageToSessionThread::ExecuteCommand(cmd));
    }

    /// TODO: To be folded into command
    pub fn handle_attach(&self, frames: UnboundedSender<ServerMessage>, cols: u16, rows: u16) {
        self.send(MessageToSessionThread::Attach { frames, cols, rows })
    }

    pub fn handle_input(&self, bytes: Vec<u8>) {
        self.send(MessageToSessionThread::Input(bytes))
    }

    pub fn handle_resize(&self, cols: u16, rows: u16) {
        self.send(MessageToSessionThread::Resize { cols, rows })
    }

    fn send(&self, cmd: MessageToSessionThread) {
        let _ = self.cmd_tx.send(cmd);
        // Nudge the mux thread's poll(). One byte, non-blocking, coalescing — if the pipe
        // is already "armed" a failed write is harmless (a wake is already pending).
        let byte = [0u8; 1];
        unsafe {
            libc::write(self.wake.as_raw_fd(), byte.as_ptr().cast(), 1);
        }
    }
}

/// The session event loop. Owns the (!Send) `Mux` for the session's lifetime. This is the
/// old single-process reactor, re-plumbed: "stdin" is the command channel (woken by the
/// self-pipe), "stdout" is each attached client's frame channel.
fn run_session(
    cmd_rx: std_mpsc::Receiver<MessageToSessionThread>,
    wake_r: OwnedFd,
    shutdown: Arc<Notify>,
) {
    // Child shells must see a TERM matching what libghostty-vt emulates. Set it here,
    // before Mux::new forks any pane.
    unsafe { std::env::set_var("TERM", "xterm-256color") };

    let mut mux = Mux::new(Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    })
    .expect("[SESSION] Failed to start Mux");

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
                    MessageToSessionThread::Attach { frames, cols, rows } => {
                        let _ = mux.resize(winsize(cols, rows));
                        clients.push(frames);
                        dirty = true; // send the current screen to the newcomer
                    }
                    MessageToSessionThread::Input(bytes) => {
                        let _ = mux.handle_input(&bytes);
                        dirty = true;
                    }
                    MessageToSessionThread::Resize { cols, rows } => {
                        let _ = mux.resize(winsize(cols, rows));
                        dirty = true;
                    }
                    MessageToSessionThread::ExecuteCommand(cmd) => {
                        let _ = handle_command(&mut mux, cmd);
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
            // If that was the last window's last pane, the session is over: tell the
            // server to shut down (dropping every client connection, so clients exit
            // cleanly). Single-session behaviour — Step 2 will instead remove just this
            // session from a registry and only shut down when none remain.
            if let Ok(0) = mux.close_pane(w, p) {
                shutdown.notify_one();
                return;
            }
            dirty = true;
        }

        // --- render once, broadcast to attached clients (dropping dead ones) ---
        if dirty
            && !clients.is_empty()
            && let Ok((frame, focused_cursor, border_cells)) = mux.render_frame()
        {
            let (window_ids, focused_window_id) = mux.get_window_info();
            let cursor_dto: Option<CursorDataDTO> =
                focused_cursor.as_ref().map(|cur| CursorDataDTO {
                    screen_x: cur.screen_x,
                    screen_y: cur.screen_y,
                    shape: cur.shape,
                });

            clients.retain(|c| {
                c.send(ServerMessage::FrameData(FrameDataDTO {
                    rendered_panes: frame.clone().into_bytes(),
                    focused_cursor: cursor_dto.clone(),
                    border_cells: border_cells.clone(),
                    window_ids: window_ids.clone(),
                    focused_window_id,
                }))
                .is_ok()
            });
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

fn handle_command(mux: &mut Mux, cmd: SessionCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        SessionCommand::SplitFocusedWindow(dir) => {
            // Just mutate — the mux thread re-renders once after handling input.
            mux.split_focused(dir)
        }
        SessionCommand::CreateNewWindow => mux.create_new_window(),
        SessionCommand::SwitchToWindow(window_id) => mux.switch_to_window(window_id),
        SessionCommand::FocusPane(dir) => mux.focus_pane(dir),
    }
}
