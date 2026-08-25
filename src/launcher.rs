use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Enough that a python traceback survives, since a stack dying from the phone is
/// otherwise a truncated tail with the real error already scrolled off.
const KEPT_LINES: usize = 40;

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedCommand {
    pub name: String,
    pub command: String,
}

struct Running {
    name: String,
    pgid: u32,
    started: Instant,
}

#[derive(Default)]
struct Output {
    lines: VecDeque<String>,
    running: Option<Running>,
    /// The name and exit code of the last command to finish. A code of `None`
    /// means it was signalled, which is what a kill looks like.
    finished: Option<(String, Option<i32>)>,
}

pub struct Launcher {
    file: PathBuf,
    commands: Mutex<Vec<SavedCommand>>,
    output: Arc<Mutex<Output>>,
}

impl Launcher {
    pub fn new(file: PathBuf) -> Self {
        let commands = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            file,
            commands: Mutex::new(commands),
            output: Arc::default(),
        }
    }

    pub fn view(&self) -> serde_json::Value {
        let output = self.output.lock().unwrap();
        json!({
            "commands": *self.commands.lock().unwrap(),
            "running": output.running.as_ref().map(|running| json!({
                "name": running.name,
                "seconds": running.started.elapsed().as_secs_f64(),
            })),
            "lines": output.lines,
            "finished": output.finished.as_ref().map(|(name, code)| json!({
                "name": name,
                "code": code,
            })),
        })
    }

    fn persist(&self, commands: &[SavedCommand]) -> Result<()> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.file, serde_json::to_string_pretty(commands)?)?;
        Ok(())
    }

    /// Puts a message in the same stream the launched command writes to, so a
    /// refused action is visible where the user is already looking.
    pub fn note(&self, line: String) {
        push(&self.output, line);
    }

    pub fn save_command(&self, name: &str, command: &str) -> Result<()> {
        let (name, command) = (name.trim(), command.trim());
        if name.is_empty() || command.is_empty() {
            return Err(anyhow!("a launch command needs both a name and a command"));
        }
        let mut commands = self.commands.lock().unwrap();
        match commands.iter_mut().find(|saved| saved.name == name) {
            Some(saved) => saved.command = command.to_owned(),
            None => commands.push(SavedCommand {
                name: name.to_owned(),
                command: command.to_owned(),
            }),
        }
        self.persist(&commands)?;
        Ok(())
    }

    pub fn delete_command(&self, name: &str) -> Result<()> {
        let mut commands = self.commands.lock().unwrap();
        commands.retain(|saved| saved.name != name);
        self.persist(&commands)?;
        Ok(())
    }

    pub fn run(&self, name: &str) -> Result<()> {
        if self.output.lock().unwrap().running.is_some() {
            return Err(anyhow!("something is already running; stop it first"));
        }
        let command = self
            .commands
            .lock()
            .unwrap()
            .iter()
            .find(|saved| saved.name == name)
            .ok_or_else(|| anyhow!("no launch command named {name}"))?
            .command
            .clone();

        // Its own process group, so stopping it takes down the whole tree rather
        // than just the shell that spawned the real work.
        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;

        let pgid = child.id();
        {
            let mut output = self.output.lock().unwrap();
            output.lines.clear();
            output.finished = None;
            output.running = Some(Running {
                name: name.to_owned(),
                pgid,
                started: Instant::now(),
            });
        }
        push(&self.output, format!("$ {command}"));

        if let Some(stream) = child.stdout.take() {
            drain(stream, Arc::clone(&self.output));
        }
        if let Some(stream) = child.stderr.take() {
            drain(stream, Arc::clone(&self.output));
        }

        let output = Arc::clone(&self.output);
        let name = name.to_owned();
        std::thread::spawn(move || {
            let status = child.wait();
            let code = status.as_ref().ok().and_then(|status| status.code());
            push(
                &output,
                match code {
                    Some(0) => format!("{name} finished"),
                    Some(code) => format!("{name} exited with code {code}"),
                    None => format!("{name} was killed"),
                },
            );
            let mut output = output.lock().unwrap();
            output.running = None;
            output.finished = Some((name, code));
        });
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        let pgid = self
            .output
            .lock()
            .unwrap()
            .running
            .as_ref()
            .map(|running| running.pgid)
            .ok_or_else(|| anyhow!("nothing is running"))?;
        kill_group(pgid);
        Ok(())
    }

    /// Runs on its own thread because the sweep sleeps between passes and the
    /// caller is a websocket that has to stay responsive.
    pub fn kill_blueprint(self: &Arc<Self>) {
        let launcher = Arc::clone(self);
        std::thread::spawn(move || {
            let _ = launcher.stop();
            for line in sweep() {
                push(&launcher.output, line);
            }
        });
    }
}

fn drain(stream: impl std::io::Read + Send + 'static, output: Arc<Mutex<Output>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            push(&output, line);
        }
    });
}

fn push(output: &Mutex<Output>, line: String) {
    let mut output = output.lock().unwrap();
    if output.lines.len() == KEPT_LINES {
        output.lines.pop_front();
    }
    output.lines.push_back(line);
}

fn kill_group(pgid: u32) {
    // A negative pid means the whole process group. Group 1 would be init's, and
    // `kill -9 -1` means every process the user owns, so it is never worth risking.
    if pgid > 1 {
        let _ = Command::new("kill")
            .args(["-9", &format!("-{pgid}")])
            .status();
    }
}

/// The `kd` script, inlined. Returns what it would have printed.
fn sweep() -> Vec<String> {
    let mut log = Vec::new();
    let mut pids = blueprint_pids();

    if pids.is_empty() {
        log.push("no blueprint processes found".to_owned());
    } else {
        log.push(format!("killing {} process(es)", pids.len()));
        kill_all(&pids);
        std::thread::sleep(Duration::from_millis(500));
        pids = blueprint_pids();
        if !pids.is_empty() {
            // AppArmor's tcpdump profile rejects signals from a confined label, so
            // a plain kill EPERMs even as root; an unconfined label is accepted.
            log.push(format!("{} left, retrying unconfined", pids.len()));
            let _ = Command::new("florp")
                .args(["aa-exec", "-p", "unconfined", "--", "kill", "-9"])
                .args(pids.iter().map(|pid| pid.to_string()))
                .status();
            std::thread::sleep(Duration::from_millis(500));
            pids = blueprint_pids();
        }
        log.push(match pids.len() {
            0 => "all blueprint processes killed".to_owned(),
            left => format!("{left} process(es) survived"),
        });
    }

    // 7446 is zenoh's multicast RPC port; a stray peer there wedges the next run.
    for port in [7446, 7779, 9090, 3030, 9876, 9877, 10000] {
        for pid in port_pids(port) {
            // Blender hosts the MCP on 9876 and is not part of a blueprint.
            if process_command(pid).is_some_and(|command| command.to_lowercase().contains("blender"))
            {
                log.push(format!("skipping blender on port {port}"));
                continue;
            }
            log.push(format!("killing pid {pid} on port {port}"));
            kill_all(&[pid]);
        }
    }

    let containers = Command::new("docker")
        .args(["ps", "-q"])
        .args(
            ["dimos", "ognav", "rosnav", "nav_stack"]
                .iter()
                .flat_map(|name| ["--filter".to_owned(), format!("name={name}")]),
        )
        .output();
    if let Ok(containers) = containers {
        let ids: Vec<String> = String::from_utf8_lossy(&containers.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !ids.is_empty() {
            log.push(format!("removing {} docker container(s)", ids.len()));
            let _ = Command::new("docker").arg("kill").args(&ids).status();
            let _ = Command::new("docker").args(["rm", "-f"]).args(&ids).status();
        }
    }

    log
}

fn kill_all(pids: &[u32]) {
    if !pids.is_empty() {
        let _ = Command::new("kill")
            .arg("-9")
            .args(pids.iter().map(|pid| pid.to_string()))
            .status();
    }
}

fn blueprint_pids() -> Vec<u32> {
    let ours = std::process::id();
    processes()
        .into_iter()
        .filter(|(pid, command)| *pid != ours && is_blueprint(command))
        .map(|(pid, _)| pid)
        .collect()
}

fn processes() -> Vec<(u32, String)> {
    let Ok(output) = Command::new("ps").args(["axo", "pid=,args="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            Some((pid.parse().ok()?, command.trim().to_owned()))
        })
        .collect()
}

fn process_command(pid: u32) -> Option<String> {
    processes()
        .into_iter()
        .find(|(other, _)| *other == pid)
        .map(|(_, command)| command)
}

fn is_blueprint(command: &str) -> bool {
    // Killing our own server would take the UI down with the blueprint, and
    // killing the agent that is driving this would be worse.
    if command.contains("web_ctrl") || command.contains("claude") {
        return false;
    }
    // Matching the directory rather than a list of binary names keeps new native
    // modules covered; a fixed list once left three orphans holding 220 GB each.
    if command.contains("/result/bin/") || command.contains("/rust/target/release/") {
        return true;
    }
    // `dimos`, `dimos2` .. `dimos6` are separate clones, each with its own venv.
    if command.contains("/.venv/bin/python") && command.contains("dimos") {
        return true;
    }
    if command.contains("tcpdump") && command.contains("dimos") {
        return true;
    }
    [
        "dimos run ",
        "dimos-viewer",
        "Model.x86_64",
        "Unity",
        "unity_envs",
        "gzserver",
        "gzclient",
        "ign-gazebo",
        "ignition-gazebo",
        "gz sim ",
        "ros-navigation-autonomy-stack",
        "/opt/ros/",
        "ros2",
        "roscore",
        "rosmaster",
        "roslaunch",
        "rosout",
        "joy_node",
        "teleop_twist_joy",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn port_pids(port: u16) -> Vec<u32> {
    let Ok(output) = Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .filter(|pid| *pid != std::process::id())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sweep_never_matches_the_server_running_it() {
        assert!(!is_blueprint("/nix/store/abc-web_ctrl-0.1.0/bin/web_ctrl"));
        assert!(!is_blueprint("target/release/web_ctrl --port 8099"));
        assert!(!is_blueprint("claude --dangerously-skip-permissions"));
    }

    #[test]
    fn the_sweep_matches_every_dimos_clone_and_native_module() {
        assert!(is_blueprint("/home/x/repos/dimos4/.venv/bin/python3 -m dimos"));
        assert!(is_blueprint("/home/x/repos/dimos/rust/target/release/mls_planner"));
        assert!(is_blueprint("/home/x/dimos/result/bin/fastlio2"));
        assert!(is_blueprint("dimos run alfred_nav"));
        assert!(is_blueprint("tcpdump -i any host dimos"));
        assert!(!is_blueprint("/usr/bin/python3 -m http.server"));
        assert!(!is_blueprint("bash -lc echo hello"));
    }

    #[test]
    fn saving_a_command_replaces_the_one_with_the_same_name() {
        // Nested under a directory that does not exist yet, because the default
        // lives in ~/.dimos and a fresh robot has no such folder.
        let directory = std::env::temp_dir().join(format!("launch_{}", std::process::id()));
        let file = directory.join("temp_web_control.json");
        let launcher = Launcher::new(file.clone());
        launcher.save_command("nav", "echo one").unwrap();
        launcher.save_command("nav", "echo two").unwrap();
        launcher.save_command("slam", "echo three").unwrap();

        let reloaded = Launcher::new(file.clone());
        let commands = reloaded.commands.lock().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].command, "echo two");

        assert!(launcher.save_command("", "echo").is_err());
        assert!(launcher.save_command("nav", "  ").is_err());
        let _ = std::fs::remove_dir_all(directory);
    }
}
