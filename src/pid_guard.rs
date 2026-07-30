use std::path::PathBuf;

pub(crate) struct PidGuard(u32);

impl PidGuard {
    pub fn new(pid: u32, conv_id: &str) -> Self {
        if let Some(mut path) = dirs::home_dir() {
            path.push(".talos");
            path.push("pid_map");
            let _ = std::fs::create_dir_all(&path);
            path.push(pid.to_string());
            let _ = std::fs::write(&path, conv_id);
        }
        Self(pid)
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Some(mut path) = dirs::home_dir() {
            path.push(".talos");
            path.push("pid_map");
            path.push(self.0.to_string());
            let _ = std::fs::remove_file(&path);
        }
    }
}
