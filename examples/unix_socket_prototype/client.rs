use std::{
    fs::File,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::Duration,
};

use tokio::net::UnixStream;

pub struct UnixClient {
    _socket: UnixStream,
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
        UnixClient { _socket: socket }
    }

    pub async fn run(self) {
        // Perform some demo actions. Usually this would be the clients loop
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

    // Daemon logs go to a file, not our terminal (a detached server has no business
    // writing to the client's tty). Open it in the PARENT — file ops aren't allowed
    // between fork and exec.
    std::fs::create_dir_all("/tmp/wisp_mux").expect("[CLIENT] couldn't create runtime dir");
    let log =
        File::create("/tmp/wisp_mux/wisp_server.log").expect("[CLIENT] couldn't open server log");
    let log2 = log.try_clone().expect("[CLIENT] couldn't clone log fd");

    let mut cmd = Command::new(exe);
    cmd.arg("--server")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));

    // Double-fork + setsid so the real server is orphaned onto init (pid 1), which
    // becomes its reaper — the client is no longer its parent at all.
    unsafe {
        cmd.pre_exec(|| {
            // New session: detaches from the controlling terminal.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Fork again: the intermediate exits, the grandchild goes on to exec.
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => Ok(()), // grandchild -> becomes the server (adopted by init)
                _ => libc::_exit(0), // intermediate -> exit so the grandchild is orphaned
            }
        });
    }

    let mut child = cmd.spawn().expect("[CLIENT] failed to spawn server");
    // `child` is the *intermediate*, which exits immediately after forking — so this
    // returns right away and reaps it. No zombie, and the lint is satisfied.
    let _ = child.wait();
}
