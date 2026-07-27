#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";
const LEAVE_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";

fn terminal_mode(fd: i32) -> libc::termios {
    // SAFETY: `tcgetattr` initializes the complete termios value on success,
    // and every caller supplies a live PTY slave descriptor.
    unsafe {
        let mut mode = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(fd, &mut mode), 0);
        mode
    }
}

fn assert_terminal_mode_restored(before: &libc::termios, after: &libc::termios) {
    let local_flags = libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG;
    let input_flags = libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON;

    assert_eq!(after.c_lflag & local_flags, before.c_lflag & local_flags);
    assert_eq!(after.c_iflag & input_flags, before.c_iflag & input_flags);
    assert_eq!(after.c_oflag & libc::OPOST, before.c_oflag & libc::OPOST);
    assert_eq!(after.c_cc[libc::VMIN], before.c_cc[libc::VMIN]);
    assert_eq!(after.c_cc[libc::VTIME], before.c_cc[libc::VTIME]);
}

fn wait_for_exit(child: &mut std::process::Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("poll TUI child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("TUI did not exit after input");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_tui_with_input(input: &[u8]) -> (ExitStatus, Vec<u8>) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let size = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: all output pointers are valid, and ownership of both returned
    // descriptors is immediately transferred to `File` below.
    let opened = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    assert_eq!(
        opened,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );

    // SAFETY: `openpty` returned two new owned descriptors on success.
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    // SAFETY: same ownership transfer as the master descriptor above.
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let original_mode = terminal_mode(slave.as_raw_fd());

    let stdin = slave.try_clone().expect("clone PTY slave for stdin");
    let stdout = slave.try_clone().expect("clone PTY slave for stdout");
    let stderr = slave.try_clone().expect("clone PTY slave for stderr");
    let mut reader = master.try_clone().expect("clone PTY master for reader");
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let reader_thread = thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut ready_tx = Some(ready_tx);
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    output.extend_from_slice(&buffer[..read]);
                    if ready_tx.is_some()
                        && output
                            .windows(ENTER_ALTERNATE_SCREEN.len())
                            .any(|window| window == ENTER_ALTERNATE_SCREEN)
                    {
                        let _ = ready_tx.take().unwrap().send(());
                    }
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("read TUI PTY output: {error}"),
            }
        }
        output
    });

    let home = TempDir::new().expect("create isolated TUI home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tokenx"))
        .args(["tui", "--no-refresh"])
        .env("HOME", home.path())
        .env("TOKENX_CONFIG_DIR", home.path().join("tokenx-config"))
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn TUI in PTY");

    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("TUI did not enter alternate screen");
    master.write_all(input).expect("write TUI key input");
    master.flush().expect("flush TUI key input");

    let status = wait_for_exit(&mut child);
    let restored_mode = terminal_mode(slave.as_raw_fd());
    assert_terminal_mode_restored(&original_mode, &restored_mode);

    drop(slave);
    drop(master);
    let output = reader_thread.join().expect("join PTY reader");
    assert!(
        output
            .windows(LEAVE_ALTERNATE_SCREEN.len())
            .any(|window| window == LEAVE_ALTERNATE_SCREEN),
        "TUI did not leave the alternate screen"
    );

    (status, output)
}

#[test]
fn q_exits_tui_successfully_after_restoring_terminal() {
    let (status, output) = run_tui_with_input(b"q");
    assert_eq!(
        status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn ctrl_c_exits_tui_with_130_after_restoring_terminal() {
    let (status, output) = run_tui_with_input(b"\x03");
    assert_eq!(
        status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output)
    );
}
