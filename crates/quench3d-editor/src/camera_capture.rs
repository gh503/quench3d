//! Native camera child ownership; no process starts until an explicit UI request.
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
pub(super) struct CaptureProcess {
    child: Option<Child>,
    pub(super) directory: Option<PathBuf>,
    log: Option<PathBuf>,
    pub(super) status: String,
    #[cfg(test)]
    pub(super) test_command: Option<(Command, PathBuf)>,
}

impl CaptureProcess {
    pub(super) fn running(&self) -> bool {
        self.child.is_some()
    }

    pub(super) fn poll(&mut self) {
        let result = self.child.as_mut().map(Child::try_wait);
        match result {
            Some(Ok(Some(exit))) => {
                self.child.take();
                let detail = self.log.as_deref().map(log_tail).unwrap_or_default();
                self.status = if detail.contains("mediapipe") || detail.contains("MediaPipe") {
                    format!("人体识别组件不可用；请使用包含人体识别运行组件的完整发行包，或为所选 Python 安装 MediaPipe。{detail}")
                } else if detail.contains("ModuleNotFoundError")
                    || detail.contains("No module named")
                {
                    format!("缺少 Python 摄像头依赖（OpenCV / numpy）。请为所选 Python 安装随包 requirements.txt，或选择已安装依赖的 Python。{detail}")
                } else if detail.contains("无法打开输入源") {
                    "无法打开摄像头：请检查设备、系统权限或是否被其他程序占用。".into()
                } else {
                    format!("采集进程已退出（{exit}）。{detail}")
                };
            }
            Some(Err(error)) => self.status = format!("无法读取采集进程状态：{error}"),
            _ => {}
        }
    }

    pub(super) fn start(&mut self, source: u32, pose: bool) -> Result<PathBuf, String> {
        self.poll();
        if self.running() {
            return self
                .directory
                .clone()
                .ok_or_else(|| "采集目录不存在".into());
        }
        #[cfg(test)]
        if let Some((command, directory)) = self.test_command.take() {
            self.launch(command, directory.clone())?;
            return Ok(directory);
        }
        let executable = std::env::current_exe().map_err(|e| format!("无法定位编辑器：{e}"))?;
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let roots = resource_roots(&executable, &repo);
        let bundled = executable.parent().map(|bin| {
            bin.join(if cfg!(windows) {
                "quench3d-camera.exe"
            } else {
                "quench3d-camera"
            })
        });
        let explicit = std::env::var_os("QUENCH3D_CAMERA_WORKER").map(PathBuf::from);
        if let Some(worker) = select_worker(explicit, bundled) {
            let directory = session_directory()?;
            let mut command = Command::new(&worker);
            capture_arguments(&mut command, &directory, source, pose);
            self.launch(command, directory.clone())
                .map_err(|e| format!("无法启动摄像头运行组件 {}：{e}", worker.display()))?;
            return Ok(directory);
        }
        let script = roots
            .iter()
            .map(|r| r.join("tools/camera_mocap/camera_mocap.py"))
            .find(|p| p.is_file())
            .ok_or("缺少摄像头采集脚本；请使用包含 tools/camera_mocap 的完整发行包。")?;
        let mut candidates = Vec::new();
        if let Some(explicit) = std::env::var_os("QUENCH3D_CAMERA_PYTHON") {
            candidates.push(PathBuf::from(explicit));
        } else {
            for root in &roots {
                for venv in [".venv", "venv"] {
                    let candidate = root.join(venv).join(if cfg!(windows) {
                        "Scripts/python.exe"
                    } else {
                        "bin/python"
                    });
                    if candidate.is_file() {
                        candidates.push(candidate);
                    }
                }
            }
            candidates.extend([PathBuf::from("python3"), PathBuf::from("python")]);
        }
        let directory = session_directory()?;
        let mut errors = Vec::new();
        for python in candidates {
            let command = preview_command(&python, &script, &directory, source, pose);
            #[cfg(windows)]
            let command = {
                use std::os::windows::process::CommandExt;
                let mut command = command;
                command.creation_flags(0x08000000);
                command
            };
            // Probe only spawn availability, never import or access a camera before this request.
            match self.launch(command, directory.clone()) {
                Ok(()) => return Ok(directory),
                Err(e) => errors.push(e),
            }
        }
        Err(format!(
            "无法启动 Python。请安装 Python 或设置 QUENCH3D_CAMERA_PYTHON 为解释器路径。{}",
            errors.join("；")
        ))
    }

    fn launch(&mut self, mut command: Command, directory: PathBuf) -> Result<(), String> {
        self.poll();
        if self.running() {
            return Ok(());
        }
        fs::create_dir_all(&directory).map_err(|e| format!("无法创建采集目录：{e}"))?;
        let path = directory.join("capture.log");
        let log = File::create(&path).map_err(|e| format!("无法创建采集日志：{e}"))?;
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| e.to_string())?;
        self.child = Some(child);
        self.directory = Some(directory);
        self.log = Some(path);
        self.status = "摄像头启动中，等待首帧…".into();
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            match child.wait() {
                Ok(_) => self.status = "摄像头已停止".into(),
                Err(e) => self.status = format!("等待采集进程退出失败：{e}"),
            }
        }
    }
}
impl Drop for CaptureProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn select_worker(explicit: Option<PathBuf>, bundled: Option<PathBuf>) -> Option<PathBuf> {
    explicit.or_else(|| bundled.filter(|p| p.is_file()))
}
fn resource_roots(executable: &Path, repo: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(bin) = executable.parent() {
        if let Some(root) = bin.parent() {
            roots.push(root.to_path_buf());
        }
        roots.push(bin.to_path_buf());
    }
    roots.push(repo.to_path_buf());
    roots
}
fn session_directory() -> Result<PathBuf, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("quench3d-camera-{}-{stamp}", std::process::id()));
    fs::create_dir(&directory).map_err(|e| format!("无法创建独立采集目录：{e}"))?;
    Ok(directory)
}
fn preview_command(
    python: &Path,
    script: &Path,
    directory: &Path,
    source: u32,
    pose: bool,
) -> Command {
    let mut command = Command::new(python);
    command.arg("-u").arg(script);
    capture_arguments(&mut command, directory, source, pose);
    command
}
fn capture_arguments(command: &mut Command, directory: &Path, source: u32, pose: bool) {
    command.args([
        "--source",
        &source.to_string(),
        "--no-display",
        "--no-depth",
    ]);
    if !pose {
        command.arg("--preview-only");
    }
    command.arg("--out").arg(directory);
}
fn log_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return String::new();
    };
    if file
        .seek(SeekFrom::Start(size.saturating_sub(8192)))
        .is_err()
    {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.take(8192).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn python() -> &'static str {
        if cfg!(windows) {
            "python"
        } else {
            "python3"
        }
    }
    #[test]
    fn managed_worker_starts_stops_and_exits_without_camera() {
        let mut worker = CaptureProcess::default();
        let directory = session_directory().unwrap();
        let mut command = Command::new(python());
        command.args(["-c", "import time; time.sleep(60)"]);
        worker.launch(command, directory.clone()).unwrap();
        assert!(worker.running(), "launch must own a real live child");
        let id = worker.child.as_ref().unwrap().id();
        let mut duplicate = Command::new("nonexistent-quench-camera-worker");
        duplicate.arg("unused");
        worker.launch(duplicate, directory).unwrap();
        assert_eq!(worker.child.as_ref().unwrap().id(), id);
        worker.stop();
        assert!(!worker.running());
        let mut failed = Command::new(python());
        failed.args([
            "-c",
            "import sys; sys.stderr.write('No module named cv2\\n'); sys.exit(3)",
        ]);
        worker.launch(failed, session_directory().unwrap()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while worker.running() && std::time::Instant::now() < deadline {
            worker.poll();
            std::thread::yield_now();
        }
        assert!(!worker.running());
        assert!(worker.status.contains("缺少 Python"));
    }
    #[test]
    fn preview_arguments_and_packaged_paths_preserve_spaces() {
        let roots = resource_roots(Path::new("/my package/bin/editor"), Path::new("/repo"));
        assert_eq!(roots[0], PathBuf::from("/my package"));
        let command = preview_command(
            Path::new("python"),
            Path::new("/my script/camera.py"),
            Path::new("/output camera"),
            2,
            false,
        );
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-u",
                "/my script/camera.py",
                "--source",
                "2",
                "--no-display",
                "--no-depth",
                "--preview-only",
                "--out",
                "/output camera"
            ]
        );
        let directory = session_directory().unwrap();
        let path = directory.join("log");
        fs::write(&path, vec![b'x'; 20000]).unwrap();
        assert_eq!(log_tail(&path).len(), 8192);
    }
    #[test]
    fn worker_priority_and_pose_mode_do_not_require_python_script() {
        let directory = session_directory().unwrap();
        let bundled = directory.join("quench3d-camera");
        fs::write(&bundled, b"fake").unwrap();
        let missing = directory.join("explicit missing worker");
        assert_eq!(
            select_worker(Some(missing.clone()), Some(bundled.clone())),
            Some(missing)
        );
        assert_eq!(select_worker(None, Some(bundled.clone())), Some(bundled));
        assert_eq!(select_worker(None, Some(directory.join("absent"))), None);
        let mut command = Command::new("standalone worker");
        capture_arguments(&mut command, &directory, 3, true);
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|a| a == "--preview-only"));
        assert_eq!(args[0], "--source");
        assert_eq!(args[1], "3");
        assert!(!args.iter().any(|a| a.ends_with(".py")));
    }
    #[test]
    fn dropping_worker_reaps_owned_process() {
        let directory = session_directory().unwrap();
        let marker = directory.join("should-not-exist");
        let mut command = Command::new(python());
        command.arg("-c").arg("import pathlib,sys,time; time.sleep(0.3); pathlib.Path(sys.argv[1]).write_text('orphan')").arg(&marker);
        let mut worker = CaptureProcess::default();
        worker.launch(command, directory).unwrap();
        drop(worker);
        std::thread::sleep(std::time::Duration::from_millis(450));
        assert!(
            !marker.exists(),
            "dropped child must not continue executing"
        );
    }
}
