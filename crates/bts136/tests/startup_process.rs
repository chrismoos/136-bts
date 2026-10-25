use bts136::{
    core::store::{NewSms, StateStore},
    l3::sms::{DisplayTime, Metadata, SmsMessage, UserData, Validity},
    link::rach::MobileIdentity,
};
use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self::with_stamp(stamp)
    }
    fn with_stamp(stamp: u128) -> Self {
        loop {
            let sequence = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "136bts-startup-{}-{stamp}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating startup test directory failed: {error}"),
            }
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_136-bts"));
        command.current_dir(&self.0);
        command
    }
}
#[test]
fn parallel_startup_directories_remain_distinct_with_identical_clock_timestamps() {
    let first = std::thread::spawn(|| Directory::with_stamp(0));
    let second = std::thread::spawn(|| Directory::with_stamp(0));
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert_ne!(first.0, second.0);
    assert!(first.0.is_dir());
    assert!(second.0.is_dir());
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn noop_startup_process_prepares_stops_and_reopens_persistent_provisioning() {
    let directory = Directory::new();
    fs::write(
        directory.0.join("config.yaml"),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        ))
        .replace("listen: 127.0.0.1:8080", "listen: 127.0.0.1:0"),
    )
    .unwrap();
    let payload = SmsMessage::Deliver {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: true,
            manual_acknowledgment: false,
            new_message: true,
        },
        validity: Validity::Indefinite,
        display_time: DisplayTime::Default,
        user_data: UserData::Ira(b"preserved at startup".to_vec()),
    };
    for run in 0..2 {
        let output = directory
            .command()
            .args(["--check-startup", "config.yaml"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        for field in [
            "startup_check=passed",
            "forward_hz=870030000",
            "reverse_hz=825030000",
            "tx_sample_rate_hz=388800",
            "rx_sample_rate_hz=388800",
            "subscribers=2",
            "authentication=disabled",
            "stop_acknowledged=true",
            "serving_cell=false",
        ] {
            assert!(stdout.contains(field), "missing {field}: {stdout}");
        }
        if run == 0 {
            let mut store = StateStore::open(directory.0.join("136-bts.sqlite3")).unwrap();
            store
                .enqueue(NewSms {
                    dedup_key: "process-startup",
                    sender: MobileIdentity::Min34(1),
                    recipient: MobileIdentity::Min34(2),
                    message: &payload,
                    created_at_unix_ms: 0,
                })
                .unwrap();
        }
    }
    let store = StateStore::open(directory.0.join("136-bts.sqlite3")).unwrap();
    assert_eq!(store.enabled_subscribers().unwrap().len(), 2);
    let pending = store.pending_for(MobileIdentity::Min34(2)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].sms_pdu, payload.encode().unwrap());
    assert!(!store.relay_accepted(pending[0].id).unwrap());
}
#[test]
fn invalid_configuration_fails_before_database_creation_and_bad_arguments_show_usage() {
    let directory = Directory::new();
    let yaml = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    ))
    .replace("listen: 127.0.0.1:8080", "listen: 127.0.0.1:0")
    .replace("channel: 1", "channel: 0");
    fs::write(directory.0.join("invalid.yaml"), yaml).unwrap();
    let output = directory
        .command()
        .args(["--check-startup", "invalid.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("startup_check=failed")
    );
    assert!(!directory.0.join("136-bts.sqlite3").exists());
    for arguments in [
        vec![],
        vec!["--check-startup"],
        vec!["--check-startup", "invalid.yaml", "extra"],
        vec!["--help", "extra"],
    ] {
        let output = directory.command().args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8(output.stderr).unwrap().contains("Usage:"));
    }
    assert!(
        directory
            .command()
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn bounded_serving_process_starts_and_stops_with_structured_logs() {
    let directory = Directory::new();
    let yaml = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    ))
    .replace("listen: 127.0.0.1:8080", "listen: 127.0.0.1:0")
    .replace("format: text", "format: json");
    fs::write(directory.0.join("config.yaml"), yaml).unwrap();
    let output = directory
        .command()
        .env_remove("RUST_LOG")
        .args(["--run", "config.yaml", "--duration-seconds", "1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logs = String::from_utf8(output.stderr).unwrap();
    let configuration = logs
        .lines()
        .find(|line| line.contains("\"message\":\"cell configuration\""))
        .unwrap();
    for field in [
        "\"band\":\"Cellular800\"",
        "\"channel\":1",
        "\"control_slot\":1",
        "\"tx_frequency_hz\":870030000",
        "\"rx_frequency_hz\":825030000",
    ] {
        assert!(
            configuration.contains(field),
            "missing {field}: {configuration}"
        );
    }
    assert!(logs.find("cell configuration").unwrap() < logs.find("cell started").unwrap());
    assert!(logs.contains("cell started"));
    assert!(logs.contains("cell stopped"));
    assert!(logs.contains("\"stop_acknowledged\":true"));
    let store = StateStore::open(directory.0.join("136-bts.sqlite3")).unwrap();
    assert_eq!(store.enabled_subscribers().unwrap().len(), 2);
}

#[test]
fn rust_log_overrides_yaml_filter_in_text_and_json_modes() {
    const RUN_SECONDS: &str = "1";
    for format in ["text", "json"] {
        for (yaml_filter, rust_log, expect_info) in [("off", "info", true), ("info", "warn", false)]
        {
            let directory = Directory::new();
            let yaml = include_str!("fixtures/two-phones.yaml")
                .replace("listen: 127.0.0.1:8080", "listen: 127.0.0.1:0")
                .replace("filter: info", &format!("filter: {yaml_filter}"))
                .replace("format: text", &format!("format: {format}"));
            fs::write(directory.0.join("config.yaml"), yaml).unwrap();
            let output = directory
                .command()
                .env("RUST_LOG", rust_log)
                .args(["--run", "config.yaml", "--duration-seconds", RUN_SECONDS])
                .output()
                .unwrap();
            let logs = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{format}, {rust_log}: {logs}");
            assert_eq!(
                logs.contains("cell started"),
                expect_info,
                "{format}, {rust_log}: {logs}"
            );
            assert_eq!(
                logs.contains("cell stopped"),
                expect_info,
                "{format}, {rust_log}: {logs}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn sigterm_stops_both_streams_and_exits_successfully() {
    use std::{
        io::{BufRead, BufReader, Read},
        process::Stdio,
    };
    let directory = Directory::new();
    fs::write(
        directory.0.join("config.yaml"),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        ))
        .replace("listen: 127.0.0.1:8080", "listen: 127.0.0.1:0"),
    )
    .unwrap();
    let mut child = directory
        .command()
        .env_remove("RUST_LOG")
        .args(["--run", "config.yaml", "--duration-seconds", "10"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut logs = BufReader::new(child.stderr.take().unwrap());
    let mut first = String::new();
    let mut started = false;
    for _ in 0..20 {
        let mut line = String::new();
        if logs.read_line(&mut line).unwrap() == 0 {
            break;
        }
        started |= line.contains("cell started");
        first.push_str(&line);
        if started {
            break;
        }
    }
    assert!(started, "{first}");
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    logs.read_to_string(&mut first).unwrap();
    assert!(child.wait().unwrap().success(), "{first}");
    assert!(first.contains("cell stopped"));
    assert!(first.contains("stop_acknowledged=true"));
}
