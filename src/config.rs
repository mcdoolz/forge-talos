//! Configuration for the Talos crate.
//!
//! [`TalosConfig`] is loaded from a `talos.toml` file and controls how
//! `agy` is invoked, which model to use, timeouts, and concurrency limits.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::error::{Result, TalosError};

// ── Top-level config ─────────────────────────────────────────────────

/// Root configuration structure parsed from `talos.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct TalosConfig {
    /// How and where to run `agy`.
    #[serde(default)]
    pub agy: AgyConfig,

    /// Default values applied to every invocation unless overridden.
    #[serde(default)]
    pub defaults: DefaultsConfig,

    /// Resource limits.
    #[serde(default)]
    pub limits: LimitsConfig,
}

// ── Agy execution mode ──────────────────────────────────────────────

/// Controls how the `agy` binary is reached.
#[derive(Debug, Clone, Deserialize)]
pub struct AgyConfig {
    /// Execution mode: `"local"`, `"docker"`, or `"ssh"`.
    #[serde(default = "AgyConfig::default_mode")]
    pub mode: String,

    /// Docker container name/ID (required when `mode = "docker"`).
    pub container: Option<String>,

    /// SSH hostname (required when `mode = "ssh"`).
    pub host: Option<String>,

    /// SSH user (defaults to current user).
    pub user: Option<String>,

    /// Override path to the `agy` binary. Defaults to `"agy"` (i.e. $PATH lookup).
    #[serde(default = "AgyConfig::default_binary_path")]
    pub binary_path: String,
}

impl AgyConfig {
    fn default_mode() -> String {
        "local".into()
    }

    fn default_binary_path() -> String {
        "agy".into()
    }
}

impl Default for AgyConfig {
    fn default() -> Self {
        Self {
            mode: Self::default_mode(),
            container: None,
            host: None,
            user: None,
            binary_path: Self::default_binary_path(),
        }
    }
}

// ── Defaults ────────────────────────────────────────────────────────

/// Default values applied to every request unless overridden per-request.
#[derive(Debug, Clone, Deserialize)]
pub struct DefaultsConfig {
    /// Model name passed to `--model`. Defaults to `"gemini-flash-agent"`.
    #[serde(default = "DefaultsConfig::default_model")]
    pub model: String,

    /// Timeout for `--print-timeout`, in seconds. Defaults to 300 (5 min).
    #[serde(default = "DefaultsConfig::default_timeout_secs")]
    pub timeout_secs: u64,

    /// Directory containing skill TOML files.
    pub skills_dir: Option<String>,

    /// Override for the agy data directory (`~/.gemini/antigravity-cli`).
    pub data_dir: Option<String>,
}

impl DefaultsConfig {
    fn default_model() -> String {
        "gemini-flash-agent".into()
    }

    fn default_timeout_secs() -> u64 {
        300
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            model: Self::default_model(),
            timeout_secs: Self::default_timeout_secs(),
            skills_dir: None,
            data_dir: None,
        }
    }
}

// ── Limits ──────────────────────────────────────────────────────────

/// Resource limits to prevent runaway usage.
#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    /// Maximum number of concurrent `agy` invocations. Defaults to 4.
    #[serde(default = "LimitsConfig::default_max_concurrent")]
    pub max_concurrent: usize,

    /// Maximum prompt size in bytes. Defaults to 1 MiB.
    #[serde(default = "LimitsConfig::default_max_prompt_bytes")]
    pub max_prompt_bytes: usize,
}

impl LimitsConfig {
    fn default_max_concurrent() -> usize {
        4
    }

    fn default_max_prompt_bytes() -> usize {
        1_048_576 // 1 MiB
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_concurrent: Self::default_max_concurrent(),
            max_prompt_bytes: Self::default_max_prompt_bytes(),
        }
    }
}

// ── Loading ─────────────────────────────────────────────────────────

impl TalosConfig {
    /// Load configuration from an explicit TOML file path.
    #[tracing::instrument(level = "debug")]
    pub async fn load(path: impl AsRef<Path> + std::fmt::Debug) -> Result<Self> {
        let path = path.as_ref();
        let contents = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| TalosError::ConfigError(format!("failed to read {}: {e}", path.display())))?;
        let config: TalosConfig = toml::from_str(&contents)?;
        config.validate()?;
        info!(path = %path.display(), "loaded talos config");
        Ok(config)
    }

    /// Discover and load `talos.toml` by searching well-known locations:
    ///
    /// 1. `./talos.toml` (current working directory)
    /// 2. `$HOME/.config/talos/talos.toml`
    /// 3. `/etc/talos/talos.toml`
    ///
    /// If no file is found, returns a default configuration.
    #[tracing::instrument(level = "debug")]
    pub async fn discover() -> Result<Self> {
        let candidates = Self::discovery_paths();

        for path in &candidates {
            if path.exists() {
                debug!(path = %path.display(), "discovered talos.toml");
                return Self::load(path).await;
            }
        }

        info!("no talos.toml found — using defaults");
        Ok(Self::default())
    }

    /// Returns the ordered list of paths checked by [`Self::discover`].
    fn discovery_paths() -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(3);

        // 1. CWD
        paths.push(PathBuf::from("talos.toml"));

        // 2. $HOME/.config/talos/talos.toml
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".config/talos/talos.toml"));
        }

        // 3. System-wide
        paths.push(PathBuf::from("/etc/talos/talos.toml"));

        paths
    }

    /// Validate that mode-specific required fields are present.
    fn validate(&self) -> Result<()> {
        match self.agy.mode.as_str() {
            "local" => Ok(()),
            "docker" => {
                if self.agy.container.is_none() {
                    return Err(TalosError::ConfigError(
                        "agy.container is required when mode = \"docker\"".into(),
                    ));
                }
                Ok(())
            }
            "ssh" => {
                if self.agy.host.is_none() {
                    return Err(TalosError::ConfigError(
                        "agy.host is required when mode = \"ssh\"".into(),
                    ));
                }
                Ok(())
            }
            other => Err(TalosError::ConfigError(format!(
                "unknown agy.mode: \"{other}\" (expected \"local\", \"docker\", or \"ssh\")"
            ))),
        }
    }

    /// Resolve the agy data directory, falling back to `~/.gemini/antigravity-cli`.
    pub fn data_dir(&self) -> PathBuf {
        if let Some(ref dir) = self.defaults.data_dir {
            PathBuf::from(dir)
        } else {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".gemini/antigravity-cli")
        }
    }
}

impl Default for TalosConfig {
    fn default() -> Self {
        Self {
            agy: AgyConfig::default(),
            defaults: DefaultsConfig::default(),
            limits: LimitsConfig::default(),
        }
    }
}
