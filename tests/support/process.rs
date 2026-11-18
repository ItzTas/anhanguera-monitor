use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{self, Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitor_hub_plugin::{MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE};

const HANDSHAKE_LIMIT: Duration = Duration::from_secs(30);
const PIPE_LIMIT: Duration = Duration::from_secs(5);
const STOP_GRACE: Duration = Duration::from_secs(5);

pub struct SocketDir {
    path: PathBuf,
}

pub struct RunningPlugin {
    child: Child,
    pub socket: PathBuf,
    stderr: Receiver<String>,
}

pub struct Exited {
    pub status: ExitStatus,
    pub stderr: String,
}

impl SocketDir {
    pub fn create() -> SocketDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let name = format!(
            "anhanguera-{}-{}-{nanos}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = env::temp_dir().join(name);
        fs::create_dir(&path).expect("create a short socket directory");
        SocketDir { path }
    }

    pub fn path(&self) -> &str {
        self.path.to_str().expect("a UTF-8 temporary directory")
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn plugin_env(
    database_url: &str,
    graphql_url: &str,
    sockets: &SocketDir,
) -> Vec<(String, String)> {
    let path = env::var("PATH").unwrap_or_default();
    [
        ("DATABASE_URL", database_url),
        ("AVA_USERNAME", "12345678909"),
        ("AVA_PASSWORD", "test-password"),
        ("COURSES", "Analise e desenvolvimento de sistemas"),
        ("AVA_GRAPHQL_URL", graphql_url),
        ("TMPDIR", sockets.path()),
        ("PATH", &path),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect()
}

pub fn spawn_plugin(env: &[(String, String)]) -> RunningPlugin {
    let mut child = Command::new(env!("CARGO_BIN_EXE_anhanguera-monitor"))
        .arg("plugin")
        .env_clear()
        .env(MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE)
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the plugin");
    let first_line = read_first_line(child.stdout.take().expect("piped stdout"));
    let stderr = read_all(child.stderr.take().expect("piped stderr"));
    let mut plugin = RunningPlugin {
        child,
        socket: PathBuf::new(),
        stderr,
    };
    let Ok(line) = first_line.recv_timeout(HANDSHAKE_LIMIT) else {
        plugin.fail("the plugin printed no handshake line within 30 seconds");
    };
    let Some(socket) = handshake_socket(&line) else {
        plugin.fail(&format!(
            "the first stdout line is not a handshake: {line:?}"
        ));
    };
    plugin.socket = socket;
    plugin
}

pub fn require_sh() -> bool {
    let installed = Command::new("sh")
        .args(["-c", "command -v kill"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if installed {
        return true;
    }
    if env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        panic!("sh with kill is required when CI is set");
    }
    eprintln!("skipping: sh with kill is not available");
    false
}

impl RunningPlugin {
    pub fn stop(&mut self) -> Exited {
        let sent = Command::new("sh")
            .args([
                "-c",
                "kill -TERM \"$1\"",
                "sh",
                &self.child.id().to_string(),
            ])
            .status()
            .expect("run kill");
        assert!(sent.success(), "kill -TERM failed: {sent}");
        let Ok(stderr) = self.stderr.recv_timeout(STOP_GRACE) else {
            self.fail(&format!("the plugin did not stop within {STOP_GRACE:?}"));
        };
        let status = self.child.wait().expect("wait for the plugin");
        Exited { status, stderr }
    }

    fn fail(&mut self, problem: &str) -> ! {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let stderr = self
            .stderr
            .recv_timeout(PIPE_LIMIT)
            .unwrap_or_else(|_| "<the pipe stayed open>".to_owned());
        panic!("{problem}\nplugin stderr:\n{stderr}");
    }
}

impl Drop for RunningPlugin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_first_line(stdout: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, first_line) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = sender.send(line);
        let _ = io::copy(&mut reader, &mut io::sink());
    });
    first_line
}

fn read_all(pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, text) = mpsc::channel();
    thread::spawn(move || {
        let mut all = String::new();
        let _ = BufReader::new(pipe).read_to_string(&mut all);
        let _ = sender.send(all);
    });
    text
}

fn handshake_socket(line: &str) -> Option<PathBuf> {
    let fields: Vec<&str> = line.trim().split('|').collect();
    let ["1", "1", "unix", socket, "grpc"] = fields[..] else {
        return None;
    };
    Some(PathBuf::from(socket))
}
