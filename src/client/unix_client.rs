use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};

use nix::pty::Winsize;

use crate::client::raw_mode_guard::RawModeGuard;
use crate::common::protocoll::{ClientMessage, ServerMessage, read_msg, write_msg};

const ROWS: u16 = 32;
const COLS: u16 = 80;

nix::ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, Winsize);

pub struct UnixClient {
    socket: UnixStream,
}

impl UnixClient {
    pub async fn new(socket_file_path: &str) -> Self {
        // Connect-first: the socket FILE existing doesn't mean a server is listening.
        let socket = match UnixStream::connect(socket_file_path).await {
            Ok(s) => s,
            Err(_) => {
                // Missing or stale — start the server (it clears any stale file on bind) and retry.
                println!("[CLIENT] No live server. Starting one ...");
                start_server_process().await;
                connect_with_retries(socket_file_path, 20, Duration::from_millis(50))
                    .await
                    .expect("[CLIENT] Failed to connect after starting server")
            }
        };
        println!("[CLIENT] Connected");
        UnixClient { socket }
    }

    /// The client is a thin async pump: keyboard + resize -> server, frames -> screen.
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        // Raw mode on the controlling terminal; restored automatically on drop (any exit path).
        let _raw = RawModeGuard::enable()?;

        // Split the socket so the reader task and this input loop don't fight over `&mut`.
        let (mut rd, mut wr) = self.socket.into_split();

        // Attach, reporting our terminal size (the server has no terminal to measure).
        let ws = query_winsize();
        write_msg(
            &mut wr,
            &ClientMessage::Attach {
                cols: ws.ws_col,
                rows: ws.ws_row,
            },
        )
        .await?;

        // server -> screen, in its OWN task. Framed reads span two awaits, so they must not
        // live in a `select!` arm (a cancellation mid-frame would desync the stream forever).
        let mut reader = tokio::spawn(async move {
            let mut out = tokio::io::stdout();
            loop {
                match read_msg::<_, ServerMessage>(&mut rd).await {
                    Ok(ServerMessage::Frame(bytes)) => {
                        if out.write_all(&bytes).await.is_err() {
                            break;
                        }
                        let _ = out.flush().await;
                    }
                    Ok(ServerMessage::Bell) => {
                        let _ = out.write_all(b"\x07").await;
                        let _ = out.flush().await;
                    }
                    Ok(ServerMessage::Sessions(_)) => { /* sessionizer: later */ }
                    Err(_) => break, // server gone / disconnected
                }
            }
        });

        // Keyboard input: a blocking reader thread forwards tty bytes over a channel.
        // (macOS's kqueue can't register a tty fd, so tokio's AsyncFd returns EINVAL on
        // one — reading on a real thread sidesteps that and is portable. Raw mode is
        // already set on the terminal device, so these reads deliver raw keystrokes.)
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        std::thread::spawn(move || {
            let mut tty = match File::open("/dev/tty") {
                Ok(f) => f,
                Err(_) => return,
            };
            let mut buf = [0u8; 4096];
            loop {
                match tty.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break; // receiver gone
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Terminal resize notifications, as an awaitable stream (no signal handler needed).
        let mut winch = signal(SignalKind::window_change())?;

        loop {
            tokio::select! {
                // Server disconnected (or we got detached) -> quit.
                _ = &mut reader => break,

                // Terminal resized -> re-measure and tell the server.
                _ = winch.recv() => {
                    let ws = query_winsize();
                    write_msg(
                        &mut wr,
                        &ClientMessage::Resize { cols: ws.ws_col, rows: ws.ws_row },
                    )
                    .await?;
                }

                // Keyboard bytes from the reader thread -> forward to the server.
                maybe = rx.recv() => {
                    match maybe {
                        Some(bytes) => write_msg(&mut wr, &ClientMessage::Input(bytes)).await?,
                        None => break, // reader thread ended (tty EOF)
                    }
                }
            }
        }

        reader.abort(); // if we exited for a reason other than the reader ending, stop it
        Ok(())
    }

    /// Connect and tell the server to shut down.
    pub async fn kill_server(mut self) {
        let _ = write_msg(&mut self.socket, &ClientMessage::KillServer).await;
    }
}

async fn connect_with_retries(
    path: &str,
    attempts: u32,
    delay: Duration,
) -> Result<UnixStream, std::io::Error> {
    let mut last = None;
    for _ in 0..attempts {
        match UnixStream::connect(path).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(delay).await;
            }
        }
    }
    Err(last.unwrap())
}

async fn start_server_process() {
    let exe = std::env::current_exe().expect("[CLIENT] couldn't find own exe path");

    // Daemon logs go to a file, not our terminal. Open it in the PARENT — file ops aren't
    // allowed between fork and exec.
    std::fs::create_dir_all("/tmp/wisp_mux").expect("[CLIENT] couldn't create runtime dir");
    let log =
        File::create("/tmp/wisp_mux/wisp_server.log").expect("[CLIENT] couldn't open server log");
    let log2 = log.try_clone().expect("[CLIENT] couldn't clone log fd");

    let mut cmd = Command::new(exe);
    cmd.arg("--server")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));

    // Double-fork + setsid so the real server is orphaned onto init (pid 1), which becomes
    // its reaper — the client is no longer its parent at all.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => Ok(()), // grandchild -> becomes the server (adopted by init)
                _ => libc::_exit(0), // intermediate -> exit so the grandchild is orphaned
            }
        });
    }

    let mut child = cmd.spawn().expect("[CLIENT] failed to spawn server");
    // The intermediate exits immediately after forking, so this returns right away and
    // reaps it — no zombie, and the real server is owned by init.
    let _ = child.wait();
}

/// Ask the real terminal (fd 0) for its current size.
fn query_winsize() -> Winsize {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ok = unsafe { tiocgwinsz(std::io::stdin().as_raw_fd(), &mut ws) }.is_ok();
    if !ok || ws.ws_col == 0 || ws.ws_row == 0 {
        ws.ws_col = COLS; // fallback when stdin isn't a tty
        ws.ws_row = ROWS;
    }
    ws
}
