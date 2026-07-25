//! Simple usage example for `forge-talos`.
//!
//! Run with:
//! ```sh
//! cargo run --example simple
//! ```

use tokio_stream::StreamExt;
use forge_talos::{Model, Talos, TalosEvent, TalosRequest, Result};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for logging if available
    tracing_subscriber::fmt::init();

    println!("=== Talos Client Initialization ===");
    // Auto-discover talos.toml or fall back to defaults
    let talos = Talos::discover().await?;
    println!("Client initialized with config: {:?}", talos.config());

    // Example 1: One-shot convenience method `ask`
    println!("\n=== Example 1: Simple Ask ===");
    let prompt = "Explain the concept of an AI agent in two concise sentences.";
    println!("Prompt: \"{prompt}\"");
    match talos.ask(prompt).await {
        Ok(answer) => println!("Response:\n{answer}"),
        Err(e) => eprintln!("Error executing ask: {e}"),
    }

    // Example 2: Structured request using TalosRequest builder
    println!("\n=== Example 2: Structured Request ===");
    let req = TalosRequest::new("List 3 key architectural benefits of using Rust for system services.")
        .with_model(Model::GeminiPro)
        .with_timeout(120);

    match talos.invoke(req).await {
        Ok(resp) => {
            println!("Response text:\n{}", resp.text);
            println!("Conversation ID: {}", resp.conversation_id);
            println!("Duration: {:?}", resp.duration);
            println!("Tool calls count: {}", resp.tool_calls.len());
            println!("Artifacts count: {}", resp.artifacts.len());
        }
        Err(e) => eprintln!("Error executing structured invoke: {e}"),
    }

    // Example 3: Streaming response with invoke_stream
    println!("\n=== Example 3: Streaming Output ===");
    let stream_req = TalosRequest::new("Write a 4-line poem about automated build systems.")
        .with_model(Model::GeminiFlash);

    match talos.invoke_stream(stream_req).await {
        Ok(mut stream) => {
            println!("Streaming output:");
            while let Some(event) = stream.next().await {
                match event {
                    TalosEvent::TextChunk(chunk) => println!("  [chunk] {chunk}"),
                    TalosEvent::Complete(resp) => {
                        println!("\nStream completed successfully!");
                        println!("Final conversation ID: {}", resp.conversation_id);
                        println!("Wall-clock duration: {:?}", resp.duration);
                    }
                    TalosEvent::Error(err_msg) => {
                        eprintln!("\nStream error encountered: {err_msg}");
                    }
                }
            }
        }
        Err(e) => eprintln!("Error initializing stream: {e}"),
    }

    Ok(())
}
