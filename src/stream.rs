//! Streaming response support for `agy` invocations.
//!
//! [`TalosStream`] wraps a running `agy` child process and yields
//! [`TalosEvent`] items as the process produces output. Text chunks
//! are emitted line-by-line from stdout, and a final [`TalosEvent::Complete`]
//! is emitted when the process exits successfully.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout};
use tokio_stream::Stream;
use tracing::debug;

use crate::error::{TalosError, Result};
use crate::lib_types::TalosResponse;

/// Events emitted by a [`TalosStream`].
#[derive(Debug, Clone)]
pub enum TalosEvent {
    /// A chunk of text output from `agy`'s stdout.
    TextChunk(String),

    /// The process has exited and the full structured response is available.
    Complete(TalosResponse),

    /// An error occurred during streaming.
    Error(String),
}

/// Internal state machine for the stream.
enum StreamState {
    /// Actively reading lines from stdout.
    Reading,
    /// Process has exited; we need to build the final response.
    Finishing,
    /// Stream is exhausted.
    Done,
}

/// An async [`Stream`] over the output of a running `agy` process.
///
/// Yields [`TalosEvent::TextChunk`] for each line of stdout, then
/// a final [`TalosEvent::Complete`] with the parsed transcript once
/// the process exits.
pub struct TalosStream {
    lines: Lines<BufReader<ChildStdout>>,
    child: Child,
    state: StreamState,
    /// Reserved for future use (transcript parsing after stream completes).
    #[allow(dead_code)]
    config: std::sync::Arc<crate::config::TalosConfig>,
    conversation_id: Option<String>,
    collected_text: Vec<String>,
    start: Instant,
    #[allow(dead_code)]
    _guard: Option<crate::pid_guard::PidGuard>,
}

impl Unpin for TalosStream {}

impl TalosStream {
    /// Create a new stream from a spawned child process.
    ///
    /// The child's stdout must have been piped (this is enforced by
    /// [`CommandBuilder`](crate::command::CommandBuilder)).
    pub(crate) fn new(
        mut child: Child,
        config: std::sync::Arc<crate::config::TalosConfig>,
        conversation_id: Option<String>,
    ) -> Result<Self> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TalosError::ParseError("child stdout not piped".into()))?;

        let reader = BufReader::new(stdout);
        let lines = AsyncBufReadExt::lines(reader);

        let _guard = child.id().map(|pid| {
            crate::pid_guard::PidGuard::new(pid, conversation_id.as_deref().unwrap_or("unknown"))
        });

        Ok(Self {
            lines,
            child,
            state: StreamState::Reading,
            config,
            conversation_id,
            collected_text: Vec::new(),
            start: Instant::now(),
            _guard,
        })
    }

    /// Consume the stream and collect all text chunks into a single
    /// [`TalosResponse`]. This is a convenience for callers who want
    /// the streaming API's process management but don't need incremental
    /// output.
    pub async fn collect_response(mut self) -> Result<TalosResponse> {
        use tokio_stream::StreamExt;

        let mut final_response = None;

        while let Some(event) = StreamExt::next(&mut self).await {
            match event {
                TalosEvent::Complete(resp) => {
                    final_response = Some(resp);
                }
                TalosEvent::Error(e) => {
                    return Err(TalosError::ParseError(e));
                }
                TalosEvent::TextChunk(_) => {
                    // Chunks are already accumulated internally.
                }
            }
        }

        final_response.ok_or_else(|| TalosError::ParseError("stream ended without response".into()))
    }
}

impl Stream for TalosStream {
    type Item = TalosEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;

        match this.state {
            StreamState::Reading => {
                let lines = Pin::new(&mut this.lines);
                match lines.poll_next_line(cx) {
                    Poll::Ready(Ok(Some(line))) => {
                        this.collected_text.push(line.clone());
                        Poll::Ready(Some(TalosEvent::TextChunk(line)))
                    }
                    Poll::Ready(Ok(None)) => {
                        // stdout closed — process is finishing.
                        this.state = StreamState::Finishing;
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Poll::Ready(Err(e)) => {
                        this.state = StreamState::Done;
                        Poll::Ready(Some(TalosEvent::Error(e.to_string())))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
            StreamState::Finishing => {
                // Reap the child process. `try_wait` is non-blocking:
                // it returns `Ok(Some(status))` if exited, `Ok(None)` if
                // still running, or `Err` on failure.
                match this.child.try_wait() {
                    Ok(Some(status)) => {
                        let duration = this.start.elapsed();
                        this.state = StreamState::Done;

                        if !status.success() {
                            let code = status.code().unwrap_or(-1);
                            return Poll::Ready(Some(TalosEvent::Error(format!(
                                "agy exited with code {code}"
                            ))));
                        }

                        // Build the response from collected text.
                        let text = this.collected_text.join("\n");
                        let conv_id = this
                            .conversation_id
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());

                        debug!(conversation_id = %conv_id, "stream complete");

                        let response = TalosResponse {
                            text,
                            conversation_id: conv_id,
                            tool_calls: Vec::new(),
                            artifacts: Vec::new(),
                            duration,
                        };

                        Poll::Ready(Some(TalosEvent::Complete(response)))
                    }
                    Ok(None) => {
                        // Child hasn't exited yet — stdout closed but
                        // process still running. Re-register waker.
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Err(e) => {
                        this.state = StreamState::Done;
                        Poll::Ready(Some(TalosEvent::Error(e.to_string())))
                    }
                }
            }
            StreamState::Done => Poll::Ready(None),
        }
    }
}
