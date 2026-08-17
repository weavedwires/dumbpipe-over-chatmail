#![cfg_attr(target_os = "windows", allow(unused_imports, dead_code))]
use std::{
    io::{self, BufRead, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStderr, Command, Stdio},
    str::FromStr,
    sync::{mpsc, Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use dumbpipe::NodeTicket;
use iroh::SecretKey;
use rand::Rng;

// binary path
fn dumbpipe_bin() -> &'static str {
    env!("CARGO_BIN_EXE_dumbpipe")
}

/// Read `n` lines from `reader`, returning the bytes read including the newlines.
///
/// This assumes that the header lines are ASCII and can be parsed byte by byte.
fn read_ascii_lines(mut n: usize, reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut buf = [0u8; 1];
    let mut res = Vec::new();
    loop {
        if reader.read(&mut buf)? != 1 {
            break;
        }
        let char = buf[0];
        res.push(char);
        if char != b'\n' {
            continue;
        }
        if n > 1 {
            n -= 1;
        } else {
            break;
        }
    }
    Ok(res)
}

fn wait2() -> Arc<Barrier> {
    Arc::new(Barrier::new(2))
}

/// generate a random, non privileged port
fn random_port() -> u16 {
    rand::thread_rng().gen_range(10000u16..60000)
}

/// Relays to exercise with `--no-direct`. All hostnames use the trailing-dot
/// FQDN form. These are self-hosted RU relays that speak the plain WebSocket
/// relay protocol (no `Sec-WebSocket-Protocol` negotiation), unlike the
/// default n0 staging/production relays that iroh 0.35 is incompatible with.
const RELAYS: &[&str] = &[
    "https://dnd.wb.ru.",       // RU
    "https://chat.gluek.info.", // RU
    "https://cm1.wwire.su.",    // RU
];

/// How long the listen side has to print a ticket.
const RELAY_TICKET_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a connect attempt through a relay may take.
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the listen side may take to shut down after the connection closes.
const RELAY_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads the stderr of a `listen` process, sending the printed ticket (if any)
/// on the channel, then keeps draining the stderr so the pipe never fills up.
fn drain_listen_ticket(stderr: ChildStderr) -> (mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let reader = io::BufReader::new(stderr);
        let mut sent = false;
        for line in reader.lines().map_while(Result::ok) {
            if !sent && line.starts_with("dumbpipe connect ") {
                if let Some(ticket) = line.split_whitespace().last() {
                    let _ = tx.send(ticket.to_owned());
                    sent = true;
                }
            }
        }
    });
    (rx, handle)
}

/// Drains the stderr of a process into an unbounded channel for diagnostics.
fn capture_stderr(stderr: ChildStderr) -> (mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let reader = io::BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (rx, handle)
}

/// Joins all currently buffered stderr lines into a single string.
fn drain_all(rx: &mpsc::Receiver<String>) -> String {
    rx.try_iter().collect::<Vec<_>>().join("\n")
}

/// Polls `try_wait` until the child exits or `timeout` elapses.
fn wait_for_child(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return true,
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// Runs a full `listen`/`connect` roundtrip through a single relay with direct
/// connections disabled. Panics (naming the relay) on any failure.
fn relay_no_direct_roundtrip(relay: &str) {
    const LISTEN_TO_CONNECT: &[u8] = b"hello from listen";
    const CONNECT_TO_LISTEN: &[u8] = b"hello from connect";

    // Start a listen process bound to only the given relay, no direct connections.
    let mut listen = Command::new(dumbpipe_bin())
        .args(["listen", "--no-direct", "--relay", relay])
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|_| panic!("relay {relay}: failed to spawn listen"));

    // Write the listen->connect payload and close the pipe so the listen side
    // reaches EOF on its send side once the stream is set up. The write is done
    // before the connect process even spawns, so there is no EOF race: the bytes
    // stay buffered in the pipe until the listener forwards them.
    let mut listen_stdin = listen.stdin.take().unwrap();
    listen_stdin
        .write_all(LISTEN_TO_CONNECT)
        .unwrap_or_else(|_| panic!("relay {relay}: failed to write listen stdin"));
    drop(listen_stdin);

    // Extract the ticket from stderr, with a timeout.
    let (ticket_rx, listen_err_thread) = drain_listen_ticket(listen.stderr.take().unwrap());
    let ticket = match ticket_rx.recv_timeout(RELAY_TICKET_TIMEOUT) {
        Ok(ticket) => ticket,
        Err(_) => {
            let _ = listen.kill();
            let _ = listen.wait();
            panic!("relay {relay}: listen printed no ticket within {RELAY_TICKET_TIMEOUT:?}");
        }
    };

    // Start a connect process that dials only via the same relay.
    let mut connect = Command::new(dumbpipe_bin())
        .args(["connect", "--no-direct", "--relay", relay, ticket.as_str()])
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|_| panic!("relay {relay}: failed to spawn connect"));
    let (connect_err_rx, connect_err_thread) = capture_stderr(connect.stderr.take().unwrap());
    connect
        .stdin
        .take()
        .unwrap()
        .write_all(CONNECT_TO_LISTEN)
        .unwrap_or_else(|_| panic!("relay {relay}: failed to write connect stdin"));

    // Wait for connect to finish; it should exit on its own once both sides EOF'd.
    if !wait_for_child(&mut connect, RELAY_CONNECT_TIMEOUT) {
        let _ = connect.kill();
        let _ = connect.wait();
        panic!(
            "relay {relay}: connect timed out after {RELAY_CONNECT_TIMEOUT:?}\n{}",
            drain_all(&connect_err_rx)
        );
    }
    let connect_status = connect.wait().unwrap();

    let mut connect_out = Vec::new();
    connect
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut connect_out)
        .unwrap();

    assert!(
        connect_status.success(),
        "relay {relay}: connect failed with {connect_status}\n{}",
        drain_all(&connect_err_rx)
    );
    assert!(
        connect_out.starts_with(LISTEN_TO_CONNECT),
        "relay {relay}: connect received wrong data: {:?}",
        String::from_utf8_lossy(&connect_out)
    );

    // Once the connection is closed the listen side should exit on its own.
    if !wait_for_child(&mut listen, RELAY_CLEANUP_TIMEOUT) {
        let _ = listen.kill();
        let _ = listen.wait();
    }
    let mut listen_out = Vec::new();
    listen
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut listen_out)
        .unwrap();

    assert!(
        listen_out.starts_with(CONNECT_TO_LISTEN),
        "relay {relay}: listen received wrong data: {:?}",
        String::from_utf8_lossy(&listen_out)
    );

    let _ = listen_err_thread.join();
    let _ = connect_err_thread.join();
}

/// Connects two endpoints through each configured relay with direct
/// connections disabled; fails (print the relay) if any single relay fails.
#[test]
#[ignore = "flaky"]
fn no_direct_all_relays_roundtrip() {
    for relay in RELAYS {
        relay_no_direct_roundtrip(relay);
    }
}

/// Tests the basic functionality of the connect and listen pair
///
/// Connect and listen both write a limited amount of data and then EOF.
/// The interaction should stop when both sides have EOF'd.
#[test]
#[ignore = "flaky"]
fn connect_listen_happy() {
    // the bytes provided by the listen command
    let listen_to_connect = b"hello from listen";
    let connect_to_listen = b"hello from connect";
    let mut listen = duct::cmd(dumbpipe_bin(), ["listen", "--relay", RELAYS[0]])
        .env_remove("RUST_LOG") // disable tracing
        .stdin_bytes(listen_to_connect)
        .stderr_to_stdout() //
        .reader()
        .unwrap();
    // read the first 3 lines of the header, and parse the last token as a ticket
    let header = read_ascii_lines(3, &mut listen).unwrap();
    let header = String::from_utf8(header).unwrap();
    let ticket = header.split_ascii_whitespace().last().unwrap();
    let ticket = NodeTicket::from_str(ticket).unwrap();

    let connect = duct::cmd(
        dumbpipe_bin(),
        ["connect", "--relay", RELAYS[0], &ticket.to_string()],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stdin_bytes(connect_to_listen)
    .stderr_null()
    .stdout_capture()
    .run()
    .unwrap();

    assert!(connect.status.success());
    assert!(connect.stdout.starts_with(listen_to_connect));

    let mut listen_stdout = Vec::new();
    listen.read_to_end(&mut listen_stdout).unwrap();
    assert!(listen_stdout.starts_with(connect_to_listen));
}

/// Tests the basic functionality of the connect and listen pair
///
/// Connect and listen both write a limited amount of data and then EOF.
/// The interaction should stop when both sides have EOF'd.
#[test]
#[ignore = "flaky"]
fn connect_listen_custom_alpn_happy() {
    // the bytes provided by the listen command
    let listen_to_connect = b"hello from listen";
    let connect_to_listen = b"hello from connect";
    let mut listen = duct::cmd(
        dumbpipe_bin(),
        [
            "listen",
            "--relay",
            RELAYS[0],
            "--custom-alpn",
            "utf8:mysuperalpn/0.1.0",
        ],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stdin_bytes(listen_to_connect)
    .stderr_to_stdout() //
    .reader()
    .unwrap();
    // read the first 3 lines of the header, and parse the last token as a ticket
    let header = read_ascii_lines(3, &mut listen).unwrap();
    let header = String::from_utf8(header).unwrap();
    let ticket = header.split_ascii_whitespace().last().unwrap();
    let ticket = NodeTicket::from_str(ticket).unwrap();

    let connect = duct::cmd(
        dumbpipe_bin(),
        [
            "connect",
            "--relay",
            RELAYS[0],
            &ticket.to_string(),
            "--custom-alpn",
            "utf8:mysuperalpn/0.1.0",
        ],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stdin_bytes(connect_to_listen)
    .stderr_null()
    .stdout_capture()
    .run()
    .unwrap();

    assert!(connect.status.success());
    assert!(connect.stdout.starts_with(listen_to_connect));

    let mut listen_stdout = Vec::new();
    listen.read_to_end(&mut listen_stdout).unwrap();
    assert!(listen_stdout.starts_with(connect_to_listen));
}

#[cfg(unix)]
#[test]
fn connect_listen_ctrlc_connect() {
    use nix::{
        sys::signal::{self, Signal},
        unistd::Pid,
    };
    // the bytes provided by the listen command
    let mut listen = duct::cmd(dumbpipe_bin(), ["listen", "--relay", RELAYS[0]])
        .env_remove("RUST_LOG") // disable tracing
        .stdin_bytes(b"hello from listen\n")
        .stderr_to_stdout() //
        .reader()
        .unwrap();
    // read the first 3 lines of the header, and parse the last token as a ticket
    let header = read_ascii_lines(3, &mut listen).unwrap();
    let header = String::from_utf8(header).unwrap();
    let ticket = header.split_ascii_whitespace().last().unwrap();
    let ticket = NodeTicket::from_str(ticket).unwrap();

    let mut connect = duct::cmd(
        dumbpipe_bin(),
        ["connect", "--relay", RELAYS[0], &ticket.to_string()],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stderr_null()
    .stdout_capture()
    .reader()
    .unwrap();
    // wait until we get a line from the listen process
    read_ascii_lines(1, &mut connect).unwrap();
    for pid in connect.pids() {
        signal::kill(Pid::from_raw(pid as i32), Signal::SIGINT).unwrap();
    }

    let mut tmp = Vec::new();
    // we don't care about the results. This test is just to make sure that the
    // listen command stops when the connect command stops.
    listen.read_to_end(&mut tmp).ok();
    connect.read_to_end(&mut tmp).ok();
}

#[cfg(unix)]
#[test]
fn connect_listen_ctrlc_listen() {
    use std::time::Duration;

    use nix::{
        sys::signal::{self, Signal},
        unistd::Pid,
    };
    // the bytes provided by the listen command
    let mut listen = duct::cmd(dumbpipe_bin(), ["listen", "--relay", RELAYS[1]])
        .env_remove("RUST_LOG") // disable tracing
        .stderr_to_stdout()
        .reader()
        .unwrap();
    // read the first 3 lines of the header, and parse the last token as a ticket
    let header = read_ascii_lines(3, &mut listen).unwrap();
    let header = String::from_utf8(header).unwrap();
    let ticket = header.split_ascii_whitespace().last().unwrap();
    let ticket = NodeTicket::from_str(ticket).unwrap();

    let mut connect = duct::cmd(
        dumbpipe_bin(),
        ["connect", "--relay", RELAYS[1], &ticket.to_string()],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stderr_null()
    .stdout_capture()
    .reader()
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    for pid in listen.pids() {
        signal::kill(Pid::from_raw(pid as i32), Signal::SIGINT).unwrap();
    }

    let mut tmp = Vec::new();
    // we don't care about the results. This test is just to make sure that the
    // listen command stops when the connect command stops.
    listen.read_to_end(&mut tmp).ok();
    connect.read_to_end(&mut tmp).ok();
}

// TODO: figure out why this is flaky on windows
#[test]
#[cfg(unix)]
#[ignore = "flaky"]
fn listen_tcp_happy() {
    let b1 = wait2();
    let b2 = b1.clone();
    let port = random_port();
    // start a dummy tcp server and wait for a single incoming connection
    let host_port = format!("localhost:{port}");
    let host_port_2 = host_port.clone();
    std::thread::spawn(move || {
        let server = TcpListener::bind(host_port_2).unwrap();
        b1.wait();
        let (mut stream, _addr) = server.accept().unwrap();
        stream.write_all(b"hello from tcp").unwrap();
        stream.flush().unwrap();
        drop(stream);
    });
    // wait for the tcp listener to start
    b2.wait();
    // start a dumbpipe listen-tcp process
    let mut listen_tcp = duct::cmd(
        dumbpipe_bin(),
        ["listen-tcp", "--relay", RELAYS[1], "--host", &host_port],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stderr_to_stdout() //
    .reader()
    .unwrap();
    let header = read_ascii_lines(4, &mut listen_tcp).unwrap();
    let header = String::from_utf8(header).unwrap();
    let ticket = header.split_ascii_whitespace().last().unwrap();
    let ticket = NodeTicket::from_str(ticket).unwrap();
    // poke the listen-tcp process with a connect command
    let connect = duct::cmd(
        dumbpipe_bin(),
        ["connect", "--relay", RELAYS[1], &ticket.to_string()],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stderr_null()
    .stdout_capture()
    .stdin_bytes(b"hello from connect")
    .run()
    .unwrap();
    assert!(connect.status.success());
    assert!(connect.stdout.starts_with(b"hello from tcp"));
}

#[test]
fn connect_tcp_happy() {
    let port = random_port();
    let host_port = format!("localhost:{port}");
    // start a dumbpipe listen process just so the connect-tcp command has something to connect to
    let mut listen = duct::cmd(dumbpipe_bin(), ["listen", "--relay", RELAYS[2]])
        .env_remove("RUST_LOG") // disable tracing
        .stdin_bytes(b"hello from listen\n")
        .stderr_to_stdout() //
        .reader()
        .unwrap();
    let header = read_ascii_lines(3, &mut listen).unwrap();
    let header = String::from_utf8(header).unwrap();
    let ticket = header.split_ascii_whitespace().last().unwrap();
    let ticket = NodeTicket::from_str(ticket).unwrap();
    let ticket = ticket.to_string();

    // start a dumbpipe connect-tcp process
    let _connect_tcp = duct::cmd(
        dumbpipe_bin(),
        [
            "connect-tcp",
            "--relay",
            RELAYS[2],
            "--addr",
            &host_port,
            &ticket,
        ],
    )
    .env_remove("RUST_LOG") // disable tracing
    .stderr_to_stdout() //
    .reader()
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));

    //
    let mut conn = TcpStream::connect(host_port).unwrap();
    conn.write_all(b"hello from tcp").unwrap();
    conn.flush().unwrap();
    let mut buf = Vec::new();
    conn.read_to_end(&mut buf).unwrap();
    assert_eq!(&buf, b"hello from listen\n");
}

/// Integration test for Unix-domain socket tunneling.
///
/// Validates end-to-end operation between `listen-unix` and `connect-unix`:
/// - A dummy backend server echoes a reply.
/// - `listen-unix` connects to the backend and exposes a ticket.
/// - `connect-unix` consumes the ticket and exposes a new Unix socket.
/// - The test exchanges messages to assert correct data flow.
#[cfg(all(test, unix))]
mod unix_socket_tests {
    use std::{
        io::{BufRead, Read, Write},
        net::Shutdown,
        os::unix::net::{UnixListener, UnixStream},
        path::{Path, PathBuf},
        sync::{Arc, Barrier},
        time::{Duration, Instant},
    };

    use tempfile::TempDir;

    use super::*;

    /// Polls until the condition returns true or timeout is reached.
    fn wait_until<F>(timeout: Duration, mut condition: F)
    where
        F: FnMut() -> bool,
    {
        let deadline = Instant::now() + timeout;
        while !condition() {
            if Instant::now() >= deadline {
                panic!("timeout waiting for condition");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Waits until a filesystem path exists.
    fn wait_for_path<P: AsRef<Path>>(path: P, timeout: Duration) {
        let p = path.as_ref().to_path_buf();
        wait_until(timeout, move || p.exists());
    }

    /// Generate a temp directory with a Unix socket path
    fn temp_socket_path() -> (TempDir, PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("test.sock");
        (temp_dir, socket_path)
    }

    /// Helper to drain stderr from a process in a background thread
    fn drain_stderr(
        stderr: std::process::ChildStderr,
        prefix: &'static str,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("[{prefix}] {line}");
            }
        })
    }

    /// A dummy unix server that accepts multiple connections and handles them properly.
    fn dummy_unix_server(
        socket_path: PathBuf,
        barrier: Arc<Barrier>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let _ = std::fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path).unwrap();
            barrier.wait();
            // Accept connections in a loop
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    // Handle each connection in a new thread
                    std::thread::spawn(move || {
                        let mut buf = vec![0; 1024];
                        // Block here waiting for data from the client via the proxy
                        if let Ok(n) = stream.read(&mut buf) {
                            if n > 0 {
                                // once we get data, write a response
                                if stream.write_all(b"hello from unix").is_ok() {
                                    // cleanly shutdown the write side
                                    stream.shutdown(Shutdown::Write).ok();
                                }
                            }
                        }
                        // now drain the read side to allow the client to close gracefully
                        while stream.read(&mut buf).unwrap_or(0) > 0 {}
                    });
                } else {
                    break;
                }
            }
        })
    }

    #[test]
    fn unix_socket_roundtrip() {
        // Create temp socket paths for the backend and the client-facing side.
        let (_tmp_dir, backend_sock) = temp_socket_path();
        let client_sock = backend_sock.with_extension("client");

        // Barrier to sync backend server readiness.
        let barrier = Arc::new(Barrier::new(2));

        // Spawn a dummy backend server.
        let _backend_thread = dummy_unix_server(backend_sock.clone(), barrier.clone());

        // Wait for the backend to be ready.
        barrier.wait();

        // Actively probe the backend server to ensure it's accepting connections.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if UnixStream::connect(&backend_sock).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if UnixStream::connect(&backend_sock).is_err() {
            panic!("backend server not connectable after 5s");
        }

        // Launch listen-unix targeting the backend.
        let mut listen_proc = std::process::Command::new(dumbpipe_bin())
            .args([
                "listen-unix",
                "--relay",
                RELAYS[0],
                "--socket-path",
                backend_sock.to_str().unwrap(),
            ])
            .env_remove("RUST_LOG")
            .stdout(std::process::Stdio::null()) // We don't need stdout
            .stderr(std::process::Stdio::piped()) // We must read stderr
            .spawn()
            .expect("spawn listen-unix");

        // Extract the ticket from the stderr output.
        let listen_stderr = listen_proc.stderr.take().unwrap();
        let mut ticket = String::new();
        let mut stderr_reader = std::io::BufReader::new(listen_stderr);
        for line in stderr_reader.by_ref().lines() {
            let line = line.unwrap();
            eprintln!("[listen-unix-stderr] {line}");
            if line.contains("connect-unix") {
                ticket = line.split_whitespace().last().unwrap().to_owned();
                break;
            }
        }
        assert!(!ticket.is_empty(), "Failed to get ticket");

        // Continue draining listen-unix stderr using helper
        let listen_stderr_thread = std::thread::spawn(move || {
            for line in stderr_reader.lines().map_while(Result::ok) {
                eprintln!("[listen-unix-stderr] {line}");
            }
        });

        // Launch connect-unix, exposing the client socket.
        let mut connect_proc = std::process::Command::new(dumbpipe_bin())
            .args([
                "connect-unix",
                "--relay",
                RELAYS[0],
                "--socket-path",
                client_sock.to_str().unwrap(),
                &ticket,
            ])
            .env_remove("RUST_LOG")
            .stdout(std::process::Stdio::null()) // We don't need stdout
            .stderr(std::process::Stdio::piped()) // We must read stderr
            .spawn()
            .expect("spawn connect-unix");

        // Drain the stderr of the connect process using helper
        let connect_stderr = connect_proc.stderr.take().unwrap();
        let connect_stderr_thread = drain_stderr(connect_stderr, "connect-unix-stderr");

        // Wait for connect-unix to create its socket.
        wait_for_path(&client_sock, Duration::from_secs(5));

        // Perform the end-to-end exchange.
        let mut client = UnixStream::connect(&client_sock).expect("connect to client socket");
        client
            .write_all(b"hello from client")
            .expect("client write");

        // Don't shutdown write immediately - let the backend respond first
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).expect("client read");
        assert_eq!(&reply, b"hello from unix");

        // Clean up child processes.
        listen_proc.kill().ok();
        listen_proc.wait().ok();
        connect_proc.kill().ok();
        connect_proc.wait().ok();
        listen_stderr_thread.join().ok();
        connect_stderr_thread.join().ok();
    }
}

/// `save-ticket` persists a freshly generated secret to `iroh-secret.txt` in
/// the current directory, later commands reuse it, and `IROH_SECRET` wins over
/// the file. No network access.
#[test]
fn save_ticket_and_reuse() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    let run = |args: &[&str], env: Option<(&str, &str)>| -> (String, String) {
        let mut cmd = Command::new(dumbpipe_bin());
        cmd.args(args).current_dir(dir).env_remove("IROH_SECRET");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            String::from_utf8(out.stdout).unwrap().trim().to_owned(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };

    // save-ticket writes the secret file and prints a ticket.
    let (ticket1, stderr1) = run(&["save-ticket"], None);
    assert!(!ticket1.is_empty());
    assert!(stderr1.contains("using secret key"), "stderr: {stderr1}");

    let secret_path = dir.join("iroh-secret.txt");
    assert!(secret_path.is_file());
    let saved = std::fs::read_to_string(&secret_path).unwrap();
    SecretKey::from_str(saved.trim()).expect("saved secret parses");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&secret_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    // generate-ticket reuses the saved secret, without generating a new one.
    let (ticket2, stderr2) = run(&["generate-ticket"], None);
    assert_eq!(ticket1, ticket2);
    assert!(!stderr2.contains("using secret key"), "stderr: {stderr2}");

    // IROH_SECRET takes priority over the file.
    let other = "a".repeat(64);
    let (ticket3, _) = run(&["generate-ticket"], Some(("IROH_SECRET", other.as_str())));
    assert_ne!(ticket1, ticket3);
}
