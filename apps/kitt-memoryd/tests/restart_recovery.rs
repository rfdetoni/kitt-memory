use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(addr: &str, root: &Path, token_path: &Path, db_path: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_kitt-memoryd"))
            .env("KITT_MEMORY_ADDR", addr)
            .env("KITT_MEMORY_CONFIG_DIR", root.join("config"))
            .env("KITT_MEMORY_DATA_DIR", root.join("data"))
            .env("KITT_MEMORY_TOKEN_PATH", token_path)
            .env("KITT_MEMORY_DB", db_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut daemon = Self { child };
        daemon.wait_until_ready(addr);
        daemon
    }

    fn wait_until_ready(&mut self, addr: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if TcpStream::connect(addr).is_ok() {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("kitt-memoryd exited before readiness: {status}");
            }
            assert!(Instant::now() < deadline, "kitt-memoryd readiness timeout");
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.kill();
    }
}

fn temp_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "kitt-memoryd-restart-{}-{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ))
}

fn request(addr: &str, token: &str, id: &str, operation: &str, arguments: Value) -> Value {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let frame = json!({
        "token": token,
        "envelope": {
            "version": 1,
            "id": id,
            "kind": "memory.manage.request",
            "payload": {
                "operation": operation,
                "arguments": arguments
            }
        }
    });
    let mut encoded = serde_json::to_vec(&frame).unwrap();
    encoded.push(b'\n');
    stream.write_all(&encoded).unwrap();
    stream.flush().unwrap();

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(response["kind"], "memory.manage.response", "{response}");
    response["payload"].clone()
}

#[test]
fn running_job_is_reclaimed_after_memoryd_process_restart() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let token = "a".repeat(64);
    let token_path = root.join("auth.token");
    let db_path = root.join("memory.sqlite3");
    fs::write(&token_path, &token).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);

    let mut first = Daemon::start(&addr, &root, &token_path, &db_path);
    let enqueued = request(
        &addr,
        &token,
        "enqueue-1",
        "job.enqueue",
        json!({
            "phase": "extract",
            "source_id": "restart-source",
            "source_revision": "r1",
            "source_watermark": "1",
            "input_digest": "restart-digest"
        }),
    );
    let job_id = enqueued["job"]["id"].as_str().unwrap().to_string();

    let claimed = request(
        &addr,
        &token,
        "claim-1",
        "job.claim",
        json!({
            "phase": "extract",
            "owner": "worker-before-crash",
            "lease_seconds": 5
        }),
    );
    assert_eq!(claimed["job"]["id"], job_id);
    assert_eq!(claimed["job"]["attempt"], 1);

    first.kill();
    drop(first);
    thread::sleep(Duration::from_secs(6));

    let _second = Daemon::start(&addr, &root, &token_path, &db_path);
    let reclaimed = request(
        &addr,
        &token,
        "claim-2",
        "job.claim",
        json!({
            "phase": "extract",
            "owner": "worker-after-restart",
            "lease_seconds": 5
        }),
    );
    assert_eq!(reclaimed["job"]["id"], job_id);
    assert_eq!(reclaimed["job"]["attempt"], 2);
    assert_eq!(
        reclaimed["job"]["lease_owner"].as_str(),
        Some("worker-after-restart")
    );

    let completed = request(
        &addr,
        &token,
        "complete-1",
        "job.complete",
        json!({
            "id": job_id,
            "owner": "worker-after-restart",
            "output_digest": "restart-output"
        }),
    );
    assert_eq!(completed["changed"], true);

    let next = request(
        &addr,
        &token,
        "claim-3",
        "job.claim",
        json!({
            "phase": "extract",
            "owner": "worker-third",
            "lease_seconds": 5
        }),
    );
    assert!(next["job"].is_null(), "{next}");

    let _ = fs::remove_dir_all(root);
}
