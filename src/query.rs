//! One-shot queries (Python `query()`).
//!
//! Every entry point connects, sends the prompt, and reads messages until
//! the CLI's output ends (so every result of a multi-prompt stream, and
//! follow-up turns woken by background tasks, are included). Control
//! requests are answered by the background runtime throughout. stdin is
//! closed once the run is over: right after the prompt when nothing needs
//! the control channel, otherwise when the run ends (session state `idle`,
//! or the result with no tracked background task in flight, bounded by
//! `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`).

use crate::error::Result;
use crate::internal::control::initialize_timeout_duration;
use crate::internal::runtime::{MessageStream, Runtime, RuntimeConfig, Shared};
use crate::internal::session_resume::{
    apply_materialized_options, materialize_resume_session, MaterializedResume,
};
use crate::internal::session_store_validation::validate_session_store_options;
use crate::internal::transport::{SubprocessCLITransport, Transport, TransportOptions};
use crate::types::config::warn_if_can_use_tool_shadowed;
use crate::types::{ClaudeAgentOptions, Message};
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::Arc;

/// Token usage information from a query response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
}

/// Result from a one-shot query to Claude.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// The text content of the response.
    pub content: String,
    /// Token usage statistics, if available.
    pub usage: Option<TokenUsage>,
    /// The reason the response finished (e.g., "end_turn", "max_tokens").
    pub finish_reason: String,
}

/// The prompt of a streaming one-shot query (Python
/// `prompt: str | AsyncIterable[dict]`).
pub enum QueryPrompt {
    /// A single user prompt.
    Text(String),
    /// Raw stream-json messages, each written as one line, as given (apart
    /// from the `verbatim_prompts` stamp).
    Messages(BoxStream<'static, serde_json::Value>),
}

impl QueryPrompt {
    /// A prompt streamed from `messages`.
    pub fn messages<S>(messages: S) -> Self
    where
        S: Stream<Item = serde_json::Value> + Send + 'static,
    {
        QueryPrompt::Messages(messages.boxed())
    }
}

impl std::fmt::Debug for QueryPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryPrompt::Text(text) => f.debug_tuple("Text").field(text).finish(),
            QueryPrompt::Messages(_) => f.debug_tuple("Messages").field(&"<stream>").finish(),
        }
    }
}

impl From<String> for QueryPrompt {
    fn from(text: String) -> Self {
        QueryPrompt::Text(text)
    }
}

impl From<&str> for QueryPrompt {
    fn from(text: &str) -> Self {
        QueryPrompt::Text(text.to_string())
    }
}

impl From<BoxStream<'static, serde_json::Value>> for QueryPrompt {
    fn from(messages: BoxStream<'static, serde_json::Value>) -> Self {
        QueryPrompt::Messages(messages)
    }
}

/// Perform a one-shot query to Claude Code.
///
/// This is a convenience function for simple, stateless interactions where you do not
/// need bidirectional communication or conversation management. It creates a temporary
/// client, sends a single prompt, and returns the complete response.
///
/// # Arguments
///
/// * `prompt` - The prompt to send to Claude.
/// * `options` - Optional configuration options. If None, default options are used.
///
/// # Returns
///
/// A `QueryResult` containing the response content, usage statistics, and finish reason.
///
/// # Example
///
/// ```rust
/// use claude_code_sdk_rust::query;
///
/// async fn example() -> Result<(), Box<dyn std::error::Error>> {
///     let result = query("What is Rust?", None).await?;
///     println!("{}", result.content);
///     Ok(())
/// }
/// ```
pub async fn query(
    prompt: impl Into<String>,
    options: Option<ClaudeAgentOptions>,
) -> Result<QueryResult> {
    let messages = query_messages(prompt, options).await?;
    Ok(summarize_messages(messages))
}

/// Stream a one-shot query's messages as they arrive (Python `query()`).
///
/// The prompt is a string or a [`QueryPrompt::messages`] stream. The CLI is
/// started on the first poll. The stream ends when the CLI's output ends;
/// an error (including a malformed message) is the last item. Dropping the
/// stream early closes the session in the background.
///
/// ```rust,no_run
/// use futures::StreamExt;
/// use claude_code_sdk_rust::{stream_query, Message};
///
/// # async fn example() -> claude_code_sdk_rust::Result<()> {
/// let mut messages = stream_query("What is Rust?", None);
/// while let Some(message) = messages.next().await {
///     if let Message::ResultMsg { result, .. } = message? {
///         println!("{result:?}");
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub fn stream_query(
    prompt: impl Into<QueryPrompt>,
    options: Option<ClaudeAgentOptions>,
) -> MessageStream {
    run_stream_query(prompt.into(), options.unwrap_or_default(), None)
}

/// [`stream_query`] over a caller-supplied transport. Options that become CLI
/// flags are not applied to it, and store-backed resume is skipped.
pub fn stream_query_with_transport(
    prompt: impl Into<QueryPrompt>,
    options: Option<ClaudeAgentOptions>,
    transport: Box<dyn Transport>,
) -> MessageStream {
    run_stream_query(prompt.into(), options.unwrap_or_default(), Some(transport))
}

/// Perform a one-shot query and return the full message sequence.
///
/// The collected form of [`stream_query`]: fails on the first error.
pub async fn query_messages(
    prompt: impl Into<String>,
    options: Option<ClaudeAgentOptions>,
) -> Result<Vec<Message>> {
    let prompt = prompt.into();
    collect_query(options.unwrap_or_default(), None, move |shared| {
        send_text_prompt(shared, prompt)
    })
    .await
}

/// [`query_messages`] over a caller-supplied transport.
pub async fn query_messages_with_transport(
    prompt: impl Into<String>,
    options: Option<ClaudeAgentOptions>,
    transport: Box<dyn Transport>,
) -> Result<Vec<Message>> {
    let prompt = prompt.into();
    collect_query(
        options.unwrap_or_default(),
        Some(transport),
        move |shared| send_text_prompt(shared, prompt),
    )
    .await
}

/// Perform a one-shot query from a stream of raw prompt messages.
///
/// Each streamed value is written to the CLI as one JSON line, as given
/// (apart from the `verbatim_prompts` stamp); unlike
/// [`crate::ClaudeAgentClient::query_stream`] no `session_id` is filled in.
pub async fn query_stream_messages<S>(
    stream: S,
    options: Option<ClaudeAgentOptions>,
) -> Result<Vec<Message>>
where
    S: Stream<Item = serde_json::Value>,
{
    collect_query(
        options.unwrap_or_default(),
        None,
        move |shared| async move { shared.stream_input(stream).await },
    )
    .await
}

/// [`query_stream_messages`] over a caller-supplied transport.
pub async fn query_stream_messages_with_transport<S>(
    stream: S,
    options: Option<ClaudeAgentOptions>,
    transport: Box<dyn Transport>,
) -> Result<Vec<Message>>
where
    S: Stream<Item = serde_json::Value>,
{
    collect_query(
        options.unwrap_or_default(),
        Some(transport),
        move |shared| async move { shared.stream_input(stream).await },
    )
    .await
}

/// A connected one-shot session; closed (and its materialized resume
/// directory removed) by [`QuerySession::close`], or in the background when
/// dropped.
struct QuerySession {
    runtime: Option<Runtime>,
    materialized: Option<MaterializedResume>,
}

impl QuerySession {
    /// Python `InternalClient.process_query` up to the prompt: validate,
    /// materialize a store-backed resume (default transport only), connect,
    /// start the reader and initialize. Tears everything down on failure.
    async fn open(
        options: ClaudeAgentOptions,
        transport: Option<Box<dyn Transport>>,
    ) -> Result<Self> {
        validate_session_store_options(&options)?;
        warn_if_can_use_tool_shadowed(&options);
        let custom_transport = transport.is_some();
        let materialized = if custom_transport {
            None
        } else {
            materialize_resume_session(&options).await?
        };
        let mut session = QuerySession {
            runtime: None,
            materialized,
        };
        let options = match &session.materialized {
            Some(materialized) => apply_materialized_options(&options, materialized),
            None => options,
        };
        let mut transport = transport.unwrap_or_else(|| {
            Box::new(SubprocessCLITransport::new(TransportOptions::from(
                &options,
            )))
        });
        if let Err(error) = transport.connect().await {
            let _ = transport.close().await;
            session.close().await;
            return Err(error);
        }
        let (runtime, init) = Runtime::start(
            transport,
            RuntimeConfig::from_options(&options, !custom_transport),
        );
        let runtime = session.runtime.insert(runtime);
        if let Err(error) = runtime
            .finish_initialize(init, initialize_timeout_duration())
            .await
        {
            session.close().await;
            return Err(error);
        }
        Ok(session)
    }

    fn runtime(&self) -> &Runtime {
        self.runtime.as_ref().expect("query session is open")
    }

    /// Start the prompt in the background (for [`stream_query`]).
    async fn spawn_prompt(&self, prompt: QueryPrompt) -> Result<()> {
        let runtime = self.runtime();
        match prompt {
            QueryPrompt::Text(prompt) => {
                let shared = runtime.shared();
                shared
                    .write_json(&shared.stamp(text_prompt(prompt)))
                    .await?;
                runtime.spawn_end_input_after_run();
            }
            QueryPrompt::Messages(messages) => runtime.spawn_stream_input(messages),
        }
        Ok(())
    }

    /// Close the runtime (and transport) before removing the materialized
    /// resume directory the CLI was reading.
    async fn close(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            let (result, _) = runtime.close().await;
            if let Err(error) = result {
                tracing::debug!("Error closing the query transport: {error}");
            }
        }
        if let Some(materialized) = self.materialized.take() {
            materialized.cleanup().await;
        }
    }
}

impl Drop for QuerySession {
    fn drop(&mut self) {
        let runtime = self.runtime.take();
        let materialized = self.materialized.take();
        if runtime.is_none() && materialized.is_none() {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let mut session = QuerySession {
                        runtime,
                        materialized,
                    };
                    session.close().await;
                });
            }
            Err(_) => drop(runtime),
        }
    }
}

/// The user message a string prompt is sent as.
fn text_prompt(prompt: String) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "session_id": "",
        "message": {"role": "user", "content": prompt},
        "parent_tool_use_id": null,
    })
}

/// Write the string prompt, then close stdin once the run ends.
async fn send_text_prompt(shared: Arc<Shared>, prompt: String) {
    if let Err(error) = shared.write_json(&shared.stamp(text_prompt(prompt))).await {
        tracing::error!("Failed to write the prompt; closing stdin: {error}");
    }
    if let Err(error) = shared.wait_for_result_and_end_input().await {
        tracing::debug!("Error closing input stream: {error}");
    }
}

/// Run a query to completion, driving `send_prompt` alongside the reader.
async fn collect_query<F, Fut>(
    options: ClaudeAgentOptions,
    transport: Option<Box<dyn Transport>>,
    send_prompt: F,
) -> Result<Vec<Message>>
where
    F: FnOnce(Arc<Shared>) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut session = QuerySession::open(options, transport).await?;
    let result = {
        let mut messages = session.runtime().messages(false);
        let input = send_prompt(Arc::clone(session.runtime().shared()));
        tokio::pin!(input);
        let mut input_done = false;
        let mut collected = Vec::new();
        loop {
            tokio::select! {
                biased;
                _ = &mut input, if !input_done => input_done = true,
                item = messages.next() => match item {
                    Some(Ok(message)) => collected.push(message),
                    Some(Err(error)) => break Err(error),
                    None => break Ok(collected),
                },
            }
        }
    };
    session.close().await;
    result
}

fn run_stream_query(
    prompt: QueryPrompt,
    options: ClaudeAgentOptions,
    transport: Option<Box<dyn Transport>>,
) -> MessageStream {
    #[allow(clippy::large_enum_variant)] // the one-off start state
    enum State {
        Start(QueryPrompt, ClaudeAgentOptions, Option<Box<dyn Transport>>),
        Running(QuerySession, MessageStream),
        Done,
    }
    futures::stream::unfold(
        State::Start(prompt, options, transport),
        |mut state| async move {
            loop {
                match state {
                    State::Start(prompt, options, transport) => {
                        let mut session = match QuerySession::open(options, transport).await {
                            Ok(session) => session,
                            Err(error) => return Some((Err(error), State::Done)),
                        };
                        let messages = session.runtime().messages(false);
                        if let Err(error) = session.spawn_prompt(prompt).await {
                            drop(messages);
                            session.close().await;
                            return Some((Err(error), State::Done));
                        }
                        state = State::Running(session, messages);
                    }
                    State::Running(mut session, mut messages) => {
                        return match messages.next().await {
                            Some(Ok(message)) => {
                                Some((Ok(message), State::Running(session, messages)))
                            }
                            Some(Err(error)) => {
                                drop(messages);
                                session.close().await;
                                Some((Err(error), State::Done))
                            }
                            None => {
                                drop(messages);
                                session.close().await;
                                None
                            }
                        };
                    }
                    State::Done => return None,
                }
            }
        },
    )
    .boxed()
}

fn summarize_messages(messages: Vec<Message>) -> QueryResult {
    let mut content_parts: Vec<String> = Vec::new();
    let mut usage: Option<TokenUsage> = None;
    let mut finish_reason = String::from("unknown");

    for message in messages {
        match message {
            Message::AssistantMsg { content, .. } => {
                for block in &content.content {
                    if let crate::types::ContentBlock::Text { text } = block {
                        content_parts.push(text.clone());
                    }
                }
            }
            Message::ResultMsg {
                usage: msg_usage,
                stop_reason,
                result,
                ..
            } => {
                if let Some(result_text) = result {
                    if content_parts.is_empty() {
                        content_parts.push(result_text);
                    }
                }
                if let Some(u) = msg_usage {
                    usage = extract_token_usage(&u);
                }
                if let Some(reason) = stop_reason {
                    finish_reason = reason;
                }
            }
            _ => {}
        }
    }

    QueryResult {
        content: content_parts.join(""),
        usage,
        finish_reason,
    }
}

/// Extract TokenUsage from a JSON map.
fn extract_token_usage(
    usage_map: &serde_json::Map<String, serde_json::Value>,
) -> Option<TokenUsage> {
    let input_tokens = usage_map
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)?;
    let output_tokens = usage_map
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)?;
    let total_tokens = usage_map
        .get("total_tokens")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)?;

    Some(TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
    })
}
