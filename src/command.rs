//! Command construction for `agy` invocations.
//!
//! [`CommandBuilder`] takes a [`TalosConfig`] and [`TalosRequest`] and produces
//! a configured [`tokio::process::Command`] ready to spawn, handling all three
//! execution modes (local, docker, ssh).

use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

use crate::config::TalosConfig;
use crate::lib_types::TalosRequest;

/// Builds the appropriate `tokio::process::Command` for an `agy` invocation.
pub struct CommandBuilder<'a> {
    config: &'a TalosConfig,
    request: &'a TalosRequest,
}

impl<'a> CommandBuilder<'a> {
    /// Create a new builder from the given config and request.
    pub fn new(config: &'a TalosConfig, request: &'a TalosRequest) -> Self {
        Self { config, request }
    }

    /// Build the command. The prompt is passed as a direct argument to
    /// `tokio::process::Command`, which does **not** invoke a shell, so
    /// special characters in the prompt are handled safely.
    pub fn build(&self) -> Command {
        let agy_args = self.agy_args();

        let mut cmd = match self.config.agy.mode.as_str() {
            "docker" => self.docker_command(&agy_args),
            "ssh" => self.ssh_command(&agy_args),
            // "local" or anything else falls through to local
            _ => self.local_command(&agy_args),
        };

        // Capture stdout for streaming, stderr for error reporting.
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // Prevent the child from inheriting stdin.
        cmd.stdin(Stdio::null());

        debug!(mode = %self.config.agy.mode, args = ?agy_args, "built agy command");

        cmd
    }

    // ── Argument assembly ────────────────────────────────────────────

    /// Assemble the flat list of arguments passed to `agy` itself.
    fn agy_args(&self) -> Vec<String> {
        let mut args = Vec::with_capacity(16);

        // --print "prompt text"
        args.push("--print".into());
        args.push(self.request.prompt.clone());

        // --model
        args.push("--model".into());
        args.push(self.request.model_name().to_string());

        // --print-timeout  (e.g. "300s")
        let timeout = self
            .request
            .timeout_override
            .unwrap_or(self.config.defaults.timeout_secs);
        args.push("--print-timeout".into());
        args.push(format!("{timeout}s"));

        // --dangerously-skip-permissions
        args.push("--dangerously-skip-permissions".into());

        // --project (optional)
        if let Some(ref project) = self.request.project {
            args.push("--project".into());
            args.push(project.clone());
        }

        // --conversation (optional)
        if let Some(ref conv_id) = self.request.conversation_id {
            args.push("--conversation".into());
            args.push(conv_id.clone());
        }

        // Environment variables forwarded as --env KEY=VALUE
        for (key, value) in &self.request.environment {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }

        args
    }

    // ── Mode-specific command construction ──────────────────────────

    /// Local mode: invoke `agy` directly.
    fn local_command(&self, agy_args: &[String]) -> Command {
        let mut cmd = Command::new(&self.config.agy.binary_path);
        cmd.args(agy_args);
        cmd
    }

    /// Docker mode: `docker exec <container> agy ...`
    fn docker_command(&self, agy_args: &[String]) -> Command {
        let mut cmd = Command::new("docker");
        cmd.arg("exec");

        // Container name (validated at config load time)
        let container = self
            .config
            .agy
            .container
            .as_deref()
            .expect("container validated in config");
        cmd.arg(container);

        cmd.arg(&self.config.agy.binary_path);
        cmd.args(agy_args);
        cmd
    }

    /// SSH mode: `ssh [user@]host agy ...`
    fn ssh_command(&self, agy_args: &[String]) -> Command {
        let mut cmd = Command::new("ssh");

        let host = self
            .config
            .agy
            .host
            .as_deref()
            .expect("host validated in config");

        // Build the target string: user@host or just host
        let target = match &self.config.agy.user {
            Some(user) => format!("{user}@{host}"),
            None => host.to_string(),
        };
        cmd.arg(&target);

        // For SSH we pass the agy binary and its args as separate arguments.
        // `tokio::process::Command` will handle quoting for the SSH transport.
        cmd.arg(&self.config.agy.binary_path);
        cmd.args(agy_args);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TalosConfig;
    use crate::lib_types::TalosRequest;
    use crate::Model;

    #[test]
    fn local_command_has_correct_args() {
        let config = TalosConfig::default();
        let request = TalosRequest {
            prompt: "hello world".into(),
            model: Model::GeminiFlash,
            project: None,
            skills: vec![],
            conversation_id: None,
            environment: Default::default(),
            timeout_override: None,
        };

        let builder = CommandBuilder::new(&config, &request);
        let cmd = builder.build();

        let program = cmd.as_std().get_program().to_str().unwrap();
        assert_eq!(program, "agy");

        let args: Vec<&str> = cmd
            .as_std()
            .get_args()
            .filter_map(|a| a.to_str())
            .collect();

        assert!(args.contains(&"--print"));
        assert!(args.contains(&"hello world"));
        assert!(args.contains(&"--model"));
        assert!(args.contains(&"--dangerously-skip-permissions"));
    }
}
