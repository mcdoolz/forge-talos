//! Transcript parsing for `agy` output.
//!
//! After an `agy --print` invocation completes, the full structured transcript
//! lives at `~/.gemini/antigravity-cli/brain/<conv-id>/.system_generated/logs/transcript.jsonl`.
//!
//! [`TranscriptReader`] reads this file and extracts the assistant's response
//! entries into a [`TalosResponse`].

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tracing::{debug, warn};

use crate::config::TalosConfig;
use crate::error::{Result, TalosError};
use crate::lib_types::TalosResponse;

/// Reads and parses `agy` transcript files.
pub struct TranscriptReader<'a> {
    config: &'a TalosConfig,
}

impl<'a> TranscriptReader<'a> {
    /// Create a new reader bound to the given config.
    pub fn new(config: &'a TalosConfig) -> Self {
        Self { config }
    }

    /// Read the transcript for the given conversation and construct a
    /// [`TalosResponse`]. The `duration` is the wall-clock time of the
    /// invocation (measured by the caller).
    #[tracing::instrument(skip(self), level = "debug")]
    pub async fn read_response(
        &self,
        conversation_id: &str,
        duration: Duration,
    ) -> Result<TalosResponse> {
        let path = self.transcript_path(conversation_id);

        if !path.exists() {
            return Err(TalosError::TranscriptNotFound {
                path: path.display().to_string(),
            });
        }

        let raw = tokio::fs::read_to_string(&path).await?;
        debug!(
            path = %path.display(),
            lines = raw.lines().count(),
            "read transcript file"
        );

        Self::parse_transcript(&raw, conversation_id, duration)
    }

    /// Resolve the filesystem path to the transcript JSONL file.
    pub fn transcript_path(&self, conversation_id: &str) -> PathBuf {
        self.config
            .data_dir()
            .join("brain")
            .join(conversation_id)
            .join(".system_generated/logs/transcript.jsonl")
    }

    // ── Internal parsing ─────────────────────────────────────────────

    /// Parse raw JSONL content into a [`TalosResponse`].
    ///
    /// Strategy:
    /// 1. Find the **last** `USER_INPUT` line (marks the start of the
    ///    most recent turn).
    /// 2. Collect all subsequent `PLANNER_RESPONSE` / `TEXT_RESPONSE`
    ///    entries.
    /// 3. Extract `content`, `tool_calls`, and artifact references.
    fn parse_transcript(
        raw: &str,
        conversation_id: &str,
        duration: Duration,
    ) -> Result<TalosResponse> {
        let lines: Vec<Value> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l))
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Find the index of the last USER_INPUT entry.
        let last_user_idx = lines
            .iter()
            .rposition(|v| v.get("type").and_then(Value::as_str) == Some("USER_INPUT"))
            .unwrap_or(0);

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        let mut artifacts: Vec<String> = Vec::new();

        for entry in &lines[last_user_idx + 1..] {
            let entry_type = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();

            match entry_type {
                "PLANNER_RESPONSE" | "TEXT_RESPONSE" => {
                    // Extract text content.
                    if let Some(content) = entry.get("content").and_then(Value::as_str) {
                        if !content.is_empty() {
                            text_parts.push(content.to_string());
                        }
                    }

                    // Extract nested content array (some transcript formats use this).
                    if let Some(content_arr) = entry.get("content").and_then(Value::as_array) {
                        for item in content_arr {
                            if let Some(text) = item.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    text_parts.push(text.to_string());
                                }
                            }
                        }
                    }

                    // Extract tool calls.
                    if let Some(calls) = entry.get("tool_calls").and_then(Value::as_array) {
                        tool_calls.extend(calls.iter().cloned());
                    }

                    // Extract artifact paths.
                    if let Some(arts) = entry.get("artifacts").and_then(Value::as_array) {
                        for art in arts {
                            if let Some(path) = art.as_str() {
                                artifacts.push(path.to_string());
                            } else if let Some(path) =
                                art.get("path").and_then(Value::as_str)
                            {
                                artifacts.push(path.to_string());
                            }
                        }
                    }
                }
                _ => {
                    // Skip other entry types (e.g. TOOL_RESULT, SYSTEM).
                }
            }
        }

        if text_parts.is_empty() && tool_calls.is_empty() {
            warn!(conversation_id, "transcript contained no response entries");
        }

        Ok(TalosResponse {
            text: text_parts.join("\n"),
            conversation_id: conversation_id.to_string(),
            tool_calls,
            artifacts,
            duration,
        })
    }
}

/// Attempt to extract a conversation ID from agy's stderr output.
///
/// `agy` typically prints a line like:
/// ```text
/// Conversation ID: abc-123-def
/// ```
pub fn extract_conversation_id(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        let line = line.trim();
        if let Some(id) = line.strip_prefix("Conversation ID:") {
            let id = id.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
        // Also handle the format: "conversation_id=..."
        if let Some(rest) = line.strip_prefix("conversation_id=") {
            let id = rest.trim().trim_matches('"');
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_conv_id_from_stderr() {
        let stderr = "Loading config...\nConversation ID: abc-123\nDone.";
        assert_eq!(
            extract_conversation_id(stderr),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn parse_simple_transcript() {
        let jsonl = r#"{"type":"USER_INPUT","content":"hello"}
{"type":"PLANNER_RESPONSE","content":"Hi there!","tool_calls":[],"step_index":0}
"#;
        let resp =
            TranscriptReader::parse_transcript(jsonl, "test-conv", Duration::from_secs(1))
                .unwrap();
        assert_eq!(resp.text, "Hi there!");
        assert!(resp.tool_calls.is_empty());
    }
}
