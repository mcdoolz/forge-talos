//! # Talos
//!
//! Async Rust API for invoking the Google Antigravity `agy` CLI as a
//! subprocess and parsing the structured results.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! # async fn example() -> forge_talos::Result<()> {
//! let talos = forge_talos::Talos::discover().await?;
//! let answer = talos.ask("Explain quantum entanglement in one paragraph.").await?;
//! println!("{answer}");
//! # Ok(())
//! # }
//! ```
//!
//! ## Streaming
//!
//! ```rust,no_run
//! # async fn example() -> forge_talos::Result<()> {
//! use tokio_stream::StreamExt;
//! use forge_talos::{Talos, TalosRequest, Model, TalosEvent};
//!
//! let talos = Talos::discover().await?;
//! let req = TalosRequest::new("Write a haiku about Rust.");
//! let mut stream = talos.invoke_stream(req).await?;
//!
//! while let Some(event) = stream.next().await {
//!     match event {
//!         TalosEvent::TextChunk(chunk) => print!("{chunk}"),
//!         TalosEvent::Complete(resp) => println!("\n--- done in {:?} ---", resp.duration),
//!         TalosEvent::Error(e) => eprintln!("error: {e}"),
//!     }
//! }
//! # Ok(())
//! # }
//! ```

pub mod command;
pub mod config;
pub mod error;
pub mod stream;
pub mod transcript;
mod pid_guard;

// Re-export the public API at the crate root.
pub use config::TalosConfig;
pub use error::{TalosError, Result};
pub use stream::{TalosEvent, TalosStream};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use tracing::{debug, info, instrument};

use command::CommandBuilder;
use transcript::{TranscriptReader, extract_conversation_id};

// ── Core types ──────────────────────────────────────────────────────

/// Gemini model variants supported by `agy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Model {
    /// `gemini-flash-agent` — fast, capable, default choice.
    GeminiFlash,
    /// `gemini-pro-agent` — highest quality, slower.
    GeminiPro,
    /// `gemini-flash-lite-agent` — fastest, lightest.
    GeminiFlashLite,
}

impl Model {
    /// Returns the CLI flag value for `--model`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Model::GeminiFlash => "gemini-3.6-flash-high",
            Model::GeminiPro => "gemini-3.1-pro-high",
            Model::GeminiFlashLite => "gemini-3.5-flash-medium",
        }
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for Model {
    fn default() -> Self {
        Model::GeminiFlash
    }
}

/// A request to invoke `agy`.
#[derive(Debug, Clone)]
pub struct TalosRequest {
    /// The prompt text to send to the model.
    pub prompt: String,

    /// Which Gemini model to use.
    pub model: Model,

    /// Optional project directory for `--project`.
    pub project: Option<String>,

    /// Skill file paths to load.
    pub skills: Vec<String>,

    /// Optional conversation ID for continuing a session.
    pub conversation_id: Option<String>,

    /// Environment variables forwarded to `agy` via `--env`.
    pub environment: HashMap<String, String>,

    /// Override the default timeout (in seconds).
    pub timeout_override: Option<u64>,
}

impl TalosRequest {
    /// Create a minimal request with just a prompt.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: Model::default(),
            project: None,
            skills: Vec::new(),
            conversation_id: None,
            environment: HashMap::new(),
            timeout_override: None,
        }
    }

    /// Set the model for this request.
    pub fn with_model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    /// Set the conversation ID to resume a previous session.
    pub fn with_conversation_id(mut self, id: impl Into<String>) -> Self {
        self.conversation_id = Some(id.into());
        self
    }

    /// Set the project directory.
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    /// Set the timeout override in seconds.
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout_override = Some(seconds);
        self
    }

    /// Add an environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    /// Resolve the model name, using the string representation.
    pub(crate) fn model_name(&self) -> &str {
        self.model.as_str()
    }
}

/// The structured response from an `agy` invocation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TalosResponse {
    /// The full text output from the model.
    pub text: String,

    /// The conversation ID assigned by `agy`.
    pub conversation_id: String,

    /// Any tool calls made during the response.
    pub tool_calls: Vec<serde_json::Value>,

    /// Paths to any artifacts produced.
    pub artifacts: Vec<String>,

    /// Wall-clock duration of the invocation.
    #[serde(with = "duration_serde")]
    pub duration: Duration,
}

/// Serde helpers for `std::time::Duration`.
mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> std::result::Result<S::Ok, S::Error> {
        d.as_secs_f64().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Duration, D::Error> {
        let secs = f64::deserialize(d)?;
        Ok(Duration::from_secs_f64(secs))
    }
}

// Internal module alias so other modules can reference these types
// without circular `use crate::` issues during initial compilation.
pub(crate) mod lib_types {
    pub use crate::{TalosRequest, TalosResponse};
}

// ── Talos client ────────────────────────────────────────────────────

/// The main entry point for invoking `agy`.
///
/// `Talos` is cheaply cloneable (`Arc`-backed) and safe to share across
/// tasks and threads.
#[derive(Clone)]
pub struct Talos {
    config: Arc<TalosConfig>,
    semaphore: Arc<Semaphore>,
}

impl std::fmt::Debug for Talos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Talos")
            .field("mode", &self.config.agy.mode)
            .field("model", &self.config.defaults.model)
            .field("max_concurrent", &self.config.limits.max_concurrent)
            .finish()
    }
}

impl Talos {
    /// Create a `Talos` client from a configuration loaded from the given path.
    #[instrument(level = "info", skip_all)]
    pub async fn from_config(path: impl AsRef<std::path::Path> + std::fmt::Debug) -> Result<Self> {
        let config = TalosConfig::load(path).await?;
        Ok(Self::from_loaded_config(config))
    }

    /// Create a `Talos` client by auto-discovering `talos.toml`.
    #[instrument(level = "info")]
    pub async fn discover() -> Result<Self> {
        let config = TalosConfig::discover().await?;
        Ok(Self::from_loaded_config(config))
    }

    /// Create a `Talos` client from an already-loaded configuration.
    pub fn from_loaded_config(config: TalosConfig) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.limits.max_concurrent));
        info!(
            mode = %config.agy.mode,
            model = %config.defaults.model,
            max_concurrent = config.limits.max_concurrent,
            "talos client initialized"
        );
        Self {
            config: Arc::new(config),
            semaphore,
        }
    }

    /// Create a `Talos` client with default configuration.
    pub fn with_defaults() -> Self {
        Self::from_loaded_config(TalosConfig::default())
    }

    /// Invoke `agy` and wait for the complete response.
    ///
    /// This acquires a concurrency permit, spawns the process, waits for
    /// it to exit, and then parses the transcript for the structured result.
    #[instrument(skip(self, req), fields(prompt_len = req.prompt.len(), model = %req.model))]
    pub async fn invoke(&self, req: TalosRequest) -> Result<TalosResponse> {
        // Validate prompt size.
        if req.prompt.len() > self.config.limits.max_prompt_bytes {
            return Err(TalosError::ParseError(format!(
                "prompt exceeds max size ({} > {} bytes)",
                req.prompt.len(),
                self.config.limits.max_prompt_bytes
            )));
        }

        // Acquire concurrency permit.
        let _permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| TalosError::ConcurrencyLimit)?;

        debug!("concurrency permit acquired");

        let start = Instant::now();
        let mut cmd = CommandBuilder::new(&self.config, &req).build();

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TalosError::AgyNotFound
            } else {
                TalosError::IoError(e)
            }
        })?;

        let _guard = child.id().map(|pid| pid_guard::PidGuard::new(pid, req.conversation_id.as_deref().unwrap_or("unknown")));

        let output = tokio::time::timeout(
            Duration::from_secs(
                req.timeout_override
                    .unwrap_or(self.config.defaults.timeout_secs),
            ),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| TalosError::Timeout)?
        .map_err(|e| TalosError::IoError(e))?;

        let duration = start.elapsed();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        debug!(
            exit_code = output.status.code(),
            stderr_len = stderr.len(),
            stdout_len = stdout.len(),
            ?duration,
            "agy process completed"
        );

        if !output.status.success() {
            return Err(TalosError::ProcessFailed {
                exit_code: output.status.code().unwrap_or(-1),
                stderr,
            });
        }

        // Try to get a structured response from the transcript first,
        // falling back to the raw stdout text.
        let conv_id = req
            .conversation_id
            .clone()
            .or_else(|| extract_conversation_id(&stderr))
            .or_else(|| extract_conversation_id(&stdout));

        if let Some(ref conv_id) = conv_id {
            let reader = TranscriptReader::new(&self.config);
            match reader.read_response(conv_id, duration).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    debug!(?e, "transcript not available, falling back to stdout");
                }
            }
        }

        // Fallback: construct response from raw stdout.
        Ok(TalosResponse {
            text: stdout.trim().to_string(),
            conversation_id: conv_id.unwrap_or_default(),
            tool_calls: Vec::new(),
            artifacts: Vec::new(),
            duration,
        })
    }

    /// Invoke `agy` and return a streaming response.
    ///
    /// Yields [`TalosEvent::TextChunk`] for each line of output, then
    /// [`TalosEvent::Complete`] when the process exits.
    #[instrument(skip(self, req), fields(prompt_len = req.prompt.len(), model = %req.model))]
    pub async fn invoke_stream(&self, req: TalosRequest) -> Result<TalosStream> {
        // Validate prompt size.
        if req.prompt.len() > self.config.limits.max_prompt_bytes {
            return Err(TalosError::ParseError(format!(
                "prompt exceeds max size ({} > {} bytes)",
                req.prompt.len(),
                self.config.limits.max_prompt_bytes
            )));
        }

        // Acquire concurrency permit (held by the caller until the
        // stream is dropped — we don't try_acquire here because the
        // caller needs to hold the stream).
        let _permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| TalosError::ConcurrencyLimit)?;

        let mut cmd = CommandBuilder::new(&self.config, &req).build();

        let child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TalosError::AgyNotFound
            } else {
                TalosError::IoError(e)
            }
        })?;

        debug!("agy process spawned for streaming");

        TalosStream::new(child, self.config.clone(), req.conversation_id)
    }

    // ── Convenience methods ─────────────────────────────────────────

    /// Simple one-shot: send a prompt, get back text.
    ///
    /// Uses the default model from config.
    pub async fn ask(&self, prompt: &str) -> Result<String> {
        let req = TalosRequest::new(prompt);
        let resp = self.invoke(req).await?;
        Ok(resp.text)
    }

    /// One-shot with an explicit model choice.
    pub async fn ask_with(&self, prompt: &str, model: Model) -> Result<String> {
        let req = TalosRequest::new(prompt).with_model(model);
        let resp = self.invoke(req).await?;
        Ok(resp.text)
    }

    /// Returns a reference to the underlying config.
    pub fn config(&self) -> &TalosConfig {
        &self.config
    }
}
