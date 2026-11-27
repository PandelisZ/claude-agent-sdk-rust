//! Background control-protocol runtime (Python `_internal/query.py` `Query`).
//!
//! Once connected, a session is driven by two tasks:
//!
//! * the **I/O task** owns the [`Transport`]. It reads stdout frames and
//!   executes write / end-input / close commands sent over a channel, so the
//!   SDK can write (prompts, control responses, control requests) while a
//!   read is pending. Commands take priority over reading; a pending
//!   `read()` is dropped when a command arrives, which is why
//!   [`Transport::read`] must be cancel-safe. Nothing is read while a
//!   command runs, so [`Transport::write`] and [`Transport::close_input`]
//!   must only queue their work, never wait for the CLI to consume it: a CLI
//!   blocked writing a full stdout pipe would otherwise never read the stdin
//!   the SDK is blocked writing. The subprocess transport hands stdin to its
//!   own writer task, so stdout keeps draining during a large write.
//! * the **router task** routes every frame: `control_response` frames
//!   complete the matching pending request, each `control_request` is
//!   answered in its own task (so a slow hook or tool never stalls the
//!   reader, and `control_cancel_request` can abort it), `transcript_mirror`
//!   frames go to the session-store batcher, and every other frame is parsed
//!   and queued for the consumer as soon as it arrives.
//!
//! The router also tracks when the *run* is over (results, session state,
//! background tasks, the run-end ceiling), which decides when stdin may be
//! closed for one-shot queries and streamed prompts.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::{FutureExt, Stream, StreamExt};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::{AbortHandle, JoinHandle};

use crate::error::{
    CLIConnectionError, CLIJSONDecodeError, CLINotFoundError, ClaudeSDKError, ErrorResultTracker,
    MessageParseError, ProcessError, Result,
};
use crate::internal::cli_discovery::ClaudeVersionCheck;
use crate::internal::control::{
    control_error_response_payload, control_request_payload, control_request_response,
    control_timeout_error, initialize_request, new_request_id, parse_control_response,
    request_subtype, ControlCallbacks,
};
use crate::internal::parser::parse_message_value;
use crate::internal::transcript_mirror::TranscriptMirrorBatcher;
use crate::internal::transport::Transport;
use crate::types::hooks::check_initialize_response_hook_on_failure;
use crate::types::{ClaudeAgentOptions, Message, TERMINAL_TASK_STATUSES};

type JsonMap = Map<String, Value>;

/// A stream of parsed SDK messages. Errors are yielded in place; a fatal
/// reader error (process failure, [`crate::ResultError`], corrupt stdout) is
/// the last item.
pub type MessageStream = futures::stream::BoxStream<'static, Result<Message>>;

/// Task types whose completion runs a follow-up turn, and which therefore may
/// still need the control channel after the turn's result frame.
const DEFERRING_TASK_TYPES: [&str; 2] = ["local_agent", "local_workflow"];

const RUN_END_CEILING_ENV: &str = "CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS";
/// The CLI's default wait for background work once stdin is closed.
pub(crate) const DEFAULT_RUN_END_CEILING_MS: u64 = 600_000;
/// The longest ceiling honored (~24.8 days), as upstream.
const MAX_RUN_END_CEILING_MS: u64 = (1 << 31) - 1;

/// Read `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS` as the CLI will see it
/// (Python `run_end_ceiling_ms`): `options.env` overrides the process env;
/// `0` means no limit; anything that is not a non-negative integer means the
/// 10-minute default.
pub(crate) fn run_end_ceiling_ms(options_env: &HashMap<String, String>) -> u64 {
    let raw = match options_env.get(RUN_END_CEILING_ENV) {
        Some(value) => Some(value.clone()),
        None => std::env::var(RUN_END_CEILING_ENV).ok(),
    };
    let Some(raw) = raw else {
        return DEFAULT_RUN_END_CEILING_MS;
    };
    match raw.trim().parse::<i128>() {
        Ok(value) if value >= 0 => u64::try_from(value).unwrap_or(u64::MAX),
        _ => DEFAULT_RUN_END_CEILING_MS,
    }
}

/// What a session runtime is configured with.
pub(crate) struct RuntimeConfig {
    pub callbacks: ControlCallbacks,
    pub transcript_mirror: Option<TranscriptMirrorBatcher>,
    pub version_check: Option<ClaudeVersionCheck>,
    pub run_end_ceiling_ms: u64,
    pub verbatim_prompts: bool,
    /// SDK MCP servers, hooks or `can_use_tool` are configured: the CLI may
    /// send control requests that need a reply, so stdin stays open until
    /// the run ends (Python `_has_bidirectional_needs`).
    pub bidirectional: bool,
}

impl RuntimeConfig {
    /// `default_transport` enables the version check the subprocess
    /// transport performs upstream (it does not apply to custom transports).
    pub(crate) fn from_options(options: &ClaudeAgentOptions, default_transport: bool) -> Self {
        Self {
            callbacks: ControlCallbacks::from_options(options),
            transcript_mirror: TranscriptMirrorBatcher::from_options(options),
            version_check: default_transport.then(|| ClaudeVersionCheck::from_options(options)),
            run_end_ceiling_ms: run_end_ceiling_ms(&options.env),
            verbatim_prompts: options.verbatim_prompts,
            bidirectional: !options.sdk_mcp_servers.is_empty()
                || !options.hooks.is_empty()
                || options.can_use_tool.is_some(),
        }
    }
}

/// Items queued for the message consumer.
#[allow(clippy::large_enum_variant)] // moved through a channel, never copied
enum StreamItem {
    Message(Message),
    /// A known message type that failed to parse; the stream goes on
    /// (a response stream ends after a malformed `result`).
    ParseError {
        error: ClaudeSDKError,
        is_result: bool,
    },
    /// The reader failed; nothing follows.
    Fatal(ClaudeSDKError),
}

enum ReadEvent {
    Frame(Vec<u8>),
    Eof,
    Error(ClaudeSDKError),
}

enum IoCommand {
    Write {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    EndInput {
        reply: oneshot::Sender<Result<()>>,
    },
    Close {
        reply: oneshot::Sender<Result<()>>,
    },
}

/// An `anyio.Event`: once set, every current and future waiter wakes.
#[derive(Default)]
struct Event {
    set: AtomicBool,
    notify: Notify,
}

impl Event {
    fn set(&self) {
        self.set.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

/// When the run is over (Python's `_run_ended_event` bookkeeping).
struct RunState {
    result_received: bool,
    /// The CLI's latest `session_state_changed` state; `None` while it sends
    /// none (or sends `null`).
    session_state: Option<Value>,
    turn_in_progress: bool,
    /// stdin is closed or the reader is gone: the run stays ended.
    run_final: bool,
    /// Started-but-not-finished deferring tasks.
    inflight_tasks: HashSet<String>,
    /// Replaced by a fresh event when work reopens an ended run; a waiter
    /// woken by the old one still proceeds.
    run_ended: Arc<Event>,
    ceiling_generation: u64,
    ceiling_task: Option<AbortHandle>,
}

impl RunState {
    fn new() -> Self {
        Self {
            result_received: false,
            session_state: None,
            turn_in_progress: false,
            run_final: false,
            inflight_tasks: HashSet::new(),
            run_ended: Arc::new(Event::default()),
            ceiling_generation: 0,
            ceiling_task: None,
        }
    }

    fn state_is(&self, state: &str) -> bool {
        self.session_state.as_ref().and_then(Value::as_str) == Some(state)
    }

    fn state_is_none_or_idle(&self) -> bool {
        self.session_state.is_none() || self.state_is("idle")
    }

    fn clear_ceiling(&mut self) {
        self.ceiling_generation += 1;
        if let Some(task) = self.ceiling_task.take() {
            task.abort();
        }
    }

    fn end_run(&mut self) {
        self.clear_ceiling();
        self.run_ended.set();
    }

    fn maybe_end_run(&mut self) {
        if !self.inflight_tasks.is_empty() {
            tracing::debug!(
                "Turn ended with {} task(s) in flight; keeping stdin open",
                self.inflight_tasks.len()
            );
            return;
        }
        self.end_run();
    }

    fn reopen_run(&mut self) {
        if self.run_ended.is_set() && !self.run_final {
            self.run_ended = Arc::new(Event::default());
        }
    }

    /// Python `_track_task_lifecycle`.
    fn track_task_lifecycle(&mut self, message: &Value) {
        let Some(task_id) = message.get("task_id").and_then(truthy_key) else {
            return;
        };
        match message.get("subtype").and_then(Value::as_str) {
            Some("task_started") => {
                let task_type = message.get("task_type").and_then(Value::as_str);
                if task_type.is_some_and(|t| DEFERRING_TASK_TYPES.contains(&t)) {
                    self.inflight_tasks.insert(task_id);
                }
            }
            Some("task_notification") => {
                self.inflight_tasks.remove(&task_id);
            }
            Some("task_updated") => {
                let status = message
                    .get("patch")
                    .and_then(Value::as_object)
                    .and_then(|patch| patch.get("status"))
                    .and_then(Value::as_str);
                if status.is_some_and(|s| TERMINAL_TASK_STATUSES.contains(&s)) {
                    self.inflight_tasks.remove(&task_id);
                }
            }
            _ => {}
        }
    }
}

/// A truthy JSON value as a set key (task ids are strings in practice).
fn truthy_key(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Null | Value::Bool(false) | Value::String(_) => None,
        Value::Number(n) if n.as_f64() == Some(0.0) => None,
        Value::Array(a) if a.is_empty() => None,
        Value::Object(o) if o.is_empty() => None,
        other => Some(other.to_string()),
    }
}

struct PendingRequest {
    subtype: String,
    tx: oneshot::Sender<Result<JsonMap>>,
}

/// State shared by the runtime handle, the router and the handler tasks.
pub(crate) struct Shared {
    callbacks: Arc<ControlCallbacks>,
    cmd_tx: mpsc::UnboundedSender<IoCommand>,
    pending: Mutex<HashMap<String, PendingRequest>>,
    /// Set (before `pending` is drained) once the router exits, so a request
    /// registered afterwards fails at once instead of timing out.
    reader_done: AtomicBool,
    inflight_requests: Mutex<HashMap<String, AbortHandle>>,
    background: Mutex<Vec<AbortHandle>>,
    run: Mutex<RunState>,
    closed: AtomicBool,
    request_counter: AtomicU64,
    bidirectional: bool,
    ceiling_ms: u64,
    verbatim_prompts: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn not_ready_error() -> ClaudeSDKError {
    CLIConnectionError::new("ProcessTransport is not ready for writing").into()
}

impl Shared {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Apply `verbatim_prompts` to an outgoing user message.
    pub(crate) fn stamp(&self, mut message: Value) -> Value {
        if self.verbatim_prompts {
            if let Value::Object(map) = &mut message {
                map.insert("client_composed".to_string(), Value::Bool(true));
            }
        }
        message
    }

    fn enqueue_write(&self, data: Vec<u8>) -> Result<oneshot::Receiver<Result<()>>> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(IoCommand::Write { data, reply })
            .map_err(|_| not_ready_error())?;
        Ok(rx)
    }

    /// Write one JSON value as a line.
    pub(crate) async fn write_json(&self, value: &Value) -> Result<()> {
        let mut data = serde_json::to_vec(value)?;
        data.push(b'\n');
        let rx = self.enqueue_write(data)?;
        rx.await.map_err(|_| not_ready_error())?
    }

    /// Close stdin (Python `transport.end_input()`).
    pub(crate) async fn end_input(&self) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(IoCommand::EndInput { reply })
            .map_err(|_| not_ready_error())?;
        rx.await.map_err(|_| not_ready_error())?
    }

    fn begin_control_request(&self, request: Value) -> Result<PendingCall> {
        let subtype = request_subtype(&request);
        let counter = self.request_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let request_id = new_request_id(counter);
        let (tx, response) = oneshot::channel();
        lock(&self.pending).insert(
            request_id.clone(),
            PendingRequest {
                subtype: subtype.clone(),
                tx,
            },
        );
        let fail = |error| {
            lock(&self.pending).remove(&request_id);
            error
        };
        if self.reader_done.load(Ordering::SeqCst) {
            return Err(fail(ended_before_response(&subtype)));
        }
        let mut data = serde_json::to_vec(&control_request_payload(&request_id, request))
            .map_err(|e| fail(e.into()))?;
        data.push(b'\n');
        let written = self.enqueue_write(data).map_err(fail)?;
        Ok(PendingCall {
            request_id,
            subtype,
            written,
            response,
        })
    }

    async fn finish_control_request(
        &self,
        call: PendingCall,
        timeout: Duration,
    ) -> Result<JsonMap> {
        let PendingCall {
            request_id,
            subtype,
            written,
            response,
        } = call;
        let wait = async {
            match written.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(not_ready_error()),
            }
            match response.await {
                Ok(result) => result,
                Err(_) => Err(ended_before_response(&subtype)),
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => {
                lock(&self.pending).remove(&request_id);
                Err(error)
            }
            Err(_) => {
                lock(&self.pending).remove(&request_id);
                Err(control_timeout_error(&subtype))
            }
        }
    }

    /// Send a control request and wait for its response (Python
    /// `Query._send_control_request`).
    pub(crate) async fn send_control_request(
        &self,
        request: Value,
        timeout: Duration,
    ) -> Result<JsonMap> {
        let call = self.begin_control_request(request)?;
        self.finish_control_request(call, timeout).await
    }

    /// Restart the run-end ceiling (Python `_arm_run_end_ceiling`).
    fn arm_ceiling(self: &Arc<Self>, run: &mut RunState) {
        run.clear_ceiling();
        if self.ceiling_ms == 0
            || run.run_ended.is_set()
            || run.run_final
            || self.is_closed()
            || run.turn_in_progress
            || !self.bidirectional
        {
            return;
        }
        let generation = run.ceiling_generation;
        let millis = self.ceiling_ms.min(MAX_RUN_END_CEILING_MS);
        let shared = Arc::clone(self);
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(millis)).await;
            let mut run = lock(&shared.run);
            if run.ceiling_generation != generation {
                return;
            }
            run.ceiling_task = None;
            if !run.inflight_tasks.is_empty() {
                tracing::debug!(
                    "No 'idle' {millis}ms after the last result, but {} tracked task(s) \
                     still in flight; keeping stdin open",
                    run.inflight_tasks.len()
                );
                return;
            }
            tracing::debug!("No 'idle' {millis}ms after the last result; ending the run");
            run.end_run();
        });
        run.ceiling_task = Some(task.abort_handle());
    }

    /// Python `_rearm_run_end_ceiling_between_turns`.
    fn rearm_ceiling_between_turns(self: &Arc<Self>, run: &mut RunState) {
        if run.result_received && !run.state_is_none_or_idle() && !run.state_is("requires_action") {
            self.arm_ceiling(run);
        }
    }

    /// Python `_on_session_state`.
    fn on_session_state(self: &Arc<Self>, run: &mut RunState, state: Option<&Value>) {
        run.session_state = state.filter(|state| !state.is_null()).cloned();
        if run.state_is("idle") {
            if run.result_received {
                run.maybe_end_run();
            }
            return;
        }
        run.reopen_run();
        if run.state_is("requires_action") {
            run.clear_ceiling();
        } else {
            self.rearm_ceiling_between_turns(run);
        }
    }

    /// Wait for the end of the run (when bidirectional), then close stdin
    /// (Python `wait_for_result_and_end_input`).
    pub(crate) async fn wait_for_result_and_end_input(&self) -> Result<()> {
        if self.bidirectional {
            let event = Arc::clone(&lock(&self.run).run_ended);
            event.wait().await;
        }
        {
            let mut run = lock(&self.run);
            run.run_final = true;
            run.clear_ceiling();
        }
        self.end_input().await
    }

    /// Write every prompt from `stream`, then close stdin once the run ends,
    /// or at once if nothing was written (Python `stream_input`).
    pub(crate) async fn stream_input<S>(&self, stream: S)
    where
        S: Stream<Item = Value>,
    {
        futures::pin_mut!(stream);
        let mut written = 0usize;
        loop {
            // A panicking prompt stream is the Rust form of upstream's
            // raising iterator: stop reading it, but still close stdin, or
            // the CLI would wait for input forever.
            let message = match AssertUnwindSafe(stream.next()).catch_unwind().await {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(panic) => {
                    tracing::error!(
                        "Prompt stream failed; closing stdin: prompt stream panicked: {}",
                        panic_text(&panic).unwrap_or_default()
                    );
                    break;
                }
            };
            if self.is_closed() {
                break;
            }
            {
                let mut run = lock(&self.run);
                run.reopen_run();
                run.result_received = false;
                run.clear_ceiling();
            }
            if let Err(error) = self.write_json(&self.stamp(message)).await {
                tracing::error!("Prompt stream failed; closing stdin: {error}");
                break;
            }
            written += 1;
        }
        let result = if written > 0 {
            self.wait_for_result_and_end_input().await
        } else {
            lock(&self.run).run_final = true;
            self.end_input().await
        };
        if let Err(error) = result {
            tracing::debug!("Error closing input stream: {error}");
        }
    }

    fn fail_pending(&self, error: Option<&ClaudeSDKError>) {
        self.reader_done.store(true, Ordering::SeqCst);
        let pending: Vec<_> = lock(&self.pending).drain().collect();
        for (_, request) in pending {
            if let Some(error) = error {
                let _ = request.tx.send(Err(clone_error(error)));
            }
            // Dropping the sender fails the request with "ended before
            // response".
        }
    }

    fn abort_children(&self) {
        for (_, task) in lock(&self.inflight_requests).drain() {
            task.abort();
        }
        for task in lock(&self.background).drain(..) {
            task.abort();
        }
        lock(&self.run).clear_ceiling();
    }
}

fn ended_before_response(subtype: &str) -> ClaudeSDKError {
    CLIConnectionError::new(format!("control request ended before response: {subtype}")).into()
}

/// A control request written (or queued) whose response is still owed.
pub(crate) struct PendingCall {
    request_id: String,
    subtype: String,
    written: oneshot::Receiver<Result<()>>,
    response: oneshot::Receiver<Result<JsonMap>>,
}

/// The running session: owns the I/O and router tasks.
pub(crate) struct Runtime {
    inner: Option<RuntimeInner>,
}

struct RuntimeInner {
    shared: Arc<Shared>,
    messages: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<StreamItem>>>,
    router: JoinHandle<()>,
    router_shutdown: Option<oneshot::Sender<()>>,
    io: JoinHandle<Box<dyn Transport>>,
}

impl Runtime {
    /// Start the I/O and router tasks on a connected transport and send the
    /// `initialize` request; finish it with [`Runtime::finish_initialize`].
    ///
    /// The initialize write is queued before the I/O task starts, so the
    /// transport sees it before its first `read()`.
    pub(crate) fn start(transport: Box<dyn Transport>, config: RuntimeConfig) -> (Self, InitCall) {
        let RuntimeConfig {
            callbacks,
            transcript_mirror,
            version_check,
            run_end_ceiling_ms,
            verbatim_prompts,
            bidirectional,
        } = config;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (frame_tx, frame_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let mut transcript_mirror = transcript_mirror;
        if let Some(mirror) = &mut transcript_mirror {
            // The drain task reports straight to the consumer. A weak sender:
            // the message stream still ends when the router exits.
            let errors = msg_tx.downgrade();
            mirror.set_error_sink(Arc::new(move |message| {
                if let Some(errors) = errors.upgrade() {
                    let _ = errors.send(StreamItem::Message(message));
                }
            }));
        }
        let blocks_on_failure = callbacks.hooks_block_on_failure;
        let init_request = initialize_request(&callbacks);
        let shared = Arc::new(Shared {
            callbacks: Arc::new(callbacks),
            cmd_tx,
            pending: Mutex::new(HashMap::new()),
            reader_done: AtomicBool::new(false),
            inflight_requests: Mutex::new(HashMap::new()),
            background: Mutex::new(Vec::new()),
            run: Mutex::new(RunState::new()),
            closed: AtomicBool::new(false),
            request_counter: AtomicU64::new(0),
            bidirectional,
            ceiling_ms: run_end_ceiling_ms,
            verbatim_prompts,
        });
        let init = InitCall {
            call: shared.begin_control_request(init_request),
            blocks_on_failure,
        };
        let io = tokio::spawn(io_loop(transport, cmd_rx, frame_tx));
        let router = tokio::spawn(router_loop(
            Arc::clone(&shared),
            frame_rx,
            shutdown_rx,
            msg_tx,
            RouterState {
                mirror: transcript_mirror,
                version_check,
                tracker: ErrorResultTracker::new(),
            },
        ));
        let runtime = Self {
            inner: Some(RuntimeInner {
                shared,
                messages: Arc::new(tokio::sync::Mutex::new(msg_rx)),
                router,
                router_shutdown: Some(shutdown_tx),
                io,
            }),
        };
        (runtime, init)
    }

    fn inner(&self) -> &RuntimeInner {
        self.inner.as_ref().expect("runtime used after close")
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.inner().shared
    }

    /// Wait for the initialize response and check that a fail-closed hook
    /// configuration is honored.
    pub(crate) async fn finish_initialize(
        &self,
        init: InitCall,
        timeout: Duration,
    ) -> Result<JsonMap> {
        let response = self
            .shared()
            .finish_control_request(init.call?, timeout)
            .await?;
        check_initialize_response_hook_on_failure(
            init.blocks_on_failure,
            &Value::Object(response.clone()),
        )?;
        Ok(response)
    }

    /// Stream the parsed messages as they arrive. With `stop_at_result` the
    /// stream ends after (and including) the next `ResultMessage`.
    pub(crate) fn messages(&self, stop_at_result: bool) -> MessageStream {
        message_stream(Arc::clone(&self.inner().messages), stop_at_result, false)
    }

    /// Like [`Runtime::messages`], but the stream keeps the queue to itself
    /// from its first poll until it ends, so a turn is not split with
    /// another consumer. Only for streams that are polled to the end (a
    /// background forwarder), never for streams handed to the caller.
    pub(crate) fn exclusive_messages(&self, stop_at_result: bool) -> MessageStream {
        message_stream(Arc::clone(&self.inner().messages), stop_at_result, true)
    }

    /// Run `stream_input` for `stream` as a background task, aborted when
    /// the runtime closes.
    pub(crate) fn spawn_stream_input<S>(&self, stream: S)
    where
        S: Stream<Item = Value> + Send + 'static,
    {
        let shared = Arc::clone(self.shared());
        self.spawn_background(async move { shared.stream_input(stream).await });
    }

    /// Run `wait_for_result_and_end_input` as a background task.
    pub(crate) fn spawn_end_input_after_run(&self) {
        let shared = Arc::clone(self.shared());
        self.spawn_background(async move {
            if let Err(error) = shared.wait_for_result_and_end_input().await {
                tracing::debug!("Error closing input stream: {error}");
            }
        });
    }

    fn spawn_background<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let shared = self.shared();
        let task = tokio::spawn(future);
        let mut background = lock(&shared.background);
        background.retain(|task| !task.is_finished());
        background.push(task.abort_handle());
    }

    /// Close the session (Python `Query.close`): stop the router (final
    /// transcript-mirror flush), cancel in-flight handlers and background
    /// tasks, then close the transport. Returns the transport.
    ///
    /// The close runs in its own task (Python shields `Query.close`): a
    /// caller that stops waiting, for example under a timeout, must not
    /// leave the handlers, the ceiling and the CLI process half torn down.
    pub(crate) async fn close(mut self) -> (Result<()>, Option<Box<dyn Transport>>) {
        let Some(inner) = self.inner.take() else {
            return (Ok(()), None);
        };
        match tokio::spawn(inner.close()).await {
            Ok(closed) => closed,
            Err(error) => (
                Err(ClaudeSDKError::Other(format!(
                    "closing the session failed: {error}"
                ))),
                None,
            ),
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = inner.close().await;
                });
            }
            Err(_) => inner.abort(),
        }
    }
}

impl RuntimeInner {
    async fn close(mut self) -> (Result<()>, Option<Box<dyn Transport>>) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some(shutdown) = self.router_shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = (&mut self.router).await;
        self.shared.abort_children();
        let (reply, rx) = oneshot::channel();
        let result = if self.shared.cmd_tx.send(IoCommand::Close { reply }).is_ok() {
            rx.await.unwrap_or(Ok(()))
        } else {
            Ok(())
        };
        let transport = (&mut self.io).await.ok();
        (result, transport)
    }

    fn abort(self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.abort_children();
        self.router.abort();
        self.io.abort();
    }
}

/// The initialize request started by [`Runtime::start`].
pub(crate) struct InitCall {
    call: Result<PendingCall>,
    blocks_on_failure: bool,
}

/// Unless `exclusive`, the receiver is locked only while an item is
/// awaited, never between items: a stream that is kept alive but no longer
/// polled (say, a `receive_response()` loop the caller broke out of to
/// interrupt) must not block a later receive. Streams polled concurrently
/// share the queue, as upstream's iterators share one memory stream.
fn message_stream(
    messages: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<StreamItem>>>,
    stop_at_result: bool,
    exclusive: bool,
) -> MessageStream {
    type Receiver = mpsc::UnboundedReceiver<StreamItem>;
    enum State {
        Shared(Arc<tokio::sync::Mutex<Receiver>>),
        Held(tokio::sync::OwnedMutexGuard<Receiver>),
    }
    futures::stream::unfold(Some(State::Shared(messages)), move |state| async move {
        let item = match state? {
            State::Shared(messages) if !exclusive => {
                let item = messages.lock().await.recv().await?;
                (item, State::Shared(messages))
            }
            State::Shared(messages) => {
                let mut receiver = messages.lock_owned().await;
                (receiver.recv().await?, State::Held(receiver))
            }
            State::Held(mut receiver) => (receiver.recv().await?, State::Held(receiver)),
        };
        let (item, next) = item;
        match item {
            StreamItem::Message(message) => {
                let done = stop_at_result && matches!(message, Message::ResultMsg { .. });
                Some((Ok(message), (!done).then_some(next)))
            }
            StreamItem::ParseError { error, is_result } => {
                // A malformed result still ends the turn.
                let done = stop_at_result && is_result;
                Some((Err(error), (!done).then_some(next)))
            }
            StreamItem::Fatal(error) => Some((Err(error), None)),
        }
    })
    .boxed()
}

async fn io_loop(
    mut transport: Box<dyn Transport>,
    mut commands: mpsc::UnboundedReceiver<IoCommand>,
    frames: mpsc::UnboundedSender<ReadEvent>,
) -> Box<dyn Transport> {
    let mut reading = true;
    loop {
        let command = if reading {
            tokio::select! {
                biased;
                command = commands.recv() => command,
                read = transport.read() => {
                    let event = match read {
                        Ok(Some(frame)) => ReadEvent::Frame(frame),
                        Ok(None) => ReadEvent::Eof,
                        Err(error) => ReadEvent::Error(error),
                    };
                    let keep_reading = matches!(event, ReadEvent::Frame(_));
                    if frames.send(event).is_err() || !keep_reading {
                        reading = false;
                    }
                    continue;
                }
            }
        } else {
            commands.recv().await
        };
        match command {
            Some(IoCommand::Write { data, reply }) => {
                let _ = reply.send(transport.write(&data).await);
            }
            Some(IoCommand::EndInput { reply }) => {
                let _ = reply.send(transport.close_input().await);
            }
            Some(IoCommand::Close { reply }) => {
                let _ = reply.send(transport.close().await);
                return transport;
            }
            None => {
                let _ = transport.close().await;
                return transport;
            }
        }
    }
}

struct RouterState {
    mirror: Option<TranscriptMirrorBatcher>,
    version_check: Option<ClaudeVersionCheck>,
    tracker: ErrorResultTracker,
}

async fn router_loop(
    shared: Arc<Shared>,
    mut frames: mpsc::UnboundedReceiver<ReadEvent>,
    mut shutdown: oneshot::Receiver<()>,
    messages: mpsc::UnboundedSender<StreamItem>,
    mut state: RouterState,
) {
    // If the router unwinds or is dropped before its teardown below, still
    // end the run and fail pending requests, so no waiter hangs.
    let mut teardown = RouterTeardown {
        shared: &shared,
        messages: &messages,
        armed: true,
    };
    let outcome: Result<()> = loop {
        let event = tokio::select! {
            biased;
            _ = &mut shutdown => break Ok(()),
            event = frames.recv() => event,
        };
        match event {
            None | Some(ReadEvent::Eof) => break Ok(()),
            Some(ReadEvent::Error(error)) => break Err(error),
            Some(ReadEvent::Frame(bytes)) => {
                if shared.is_closed() {
                    break Ok(());
                }
                let value = match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value) => value,
                    Err(error) => {
                        break Err(CLIJSONDecodeError::new(
                            String::from_utf8_lossy(&bytes).to_string(),
                            error,
                        )
                        .into())
                    }
                };
                let routed = AssertUnwindSafe(route_frame(&shared, &mut state, &messages, value))
                    .catch_unwind()
                    .await;
                if let Err(panic) = routed {
                    break Err(ClaudeSDKError::Other(format!(
                        "message router panicked: {}",
                        panic_text(&panic).unwrap_or_default()
                    )));
                }
            }
        }
    };

    // Python's reader `finally`: flush what the mirror still holds (its
    // errors reach the consumer before any fatal error), end the run for
    // good, and close the message stream.
    if AssertUnwindSafe(flush_mirror(&mut state))
        .catch_unwind()
        .await
        .is_err()
    {
        tracing::error!("transcript mirror final flush panicked");
    }
    teardown.armed = false;
    {
        let mut run = lock(&shared.run);
        run.run_final = true;
        run.end_run();
    }
    match outcome {
        Ok(()) => shared.fail_pending(None),
        Err(error) => {
            // A non-zero exit after an `is_error` result is that result's
            // failure, not a crash.
            let error = state.tracker.replace(error);
            if !matches!(error, ClaudeSDKError::Result(_)) {
                tracing::error!("Fatal error in message reader: {error}");
            }
            shared.fail_pending(Some(&error));
            let _ = messages.send(StreamItem::Fatal(error));
        }
    }
}

/// Wait until everything the mirror holds, including batches already being
/// drained in the background, has reached the store; its `mirror_error`
/// messages are queued before this returns.
async fn flush_mirror(state: &mut RouterState) {
    if let Some(mirror) = &mut state.mirror {
        mirror.flush().await;
    }
}

/// The router's teardown for an exit that skips the normal one (a panic
/// outside `route_frame`, or the task being dropped).
struct RouterTeardown<'a> {
    shared: &'a Shared,
    messages: &'a mpsc::UnboundedSender<StreamItem>,
    armed: bool,
}

impl Drop for RouterTeardown<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        {
            let mut run = lock(&self.shared.run);
            run.run_final = true;
            run.end_run();
        }
        let error = ClaudeSDKError::Other("message router stopped unexpectedly".to_string());
        self.shared.fail_pending(Some(&error));
        let _ = self.messages.send(StreamItem::Fatal(error));
    }
}

/// Route one stdout frame (Python `Query._read_messages` loop body).
async fn route_frame(
    shared: &Arc<Shared>,
    state: &mut RouterState,
    messages: &mpsc::UnboundedSender<StreamItem>,
    value: Value,
) {
    if let Some(check) = &mut state.version_check {
        check.observe(&value);
    }
    let msg_type = value.get("type").and_then(Value::as_str).map(str::to_owned);
    match msg_type.as_deref() {
        Some("control_response") => {
            route_control_response(shared, &value);
            return;
        }
        Some("control_request") => {
            if !shared.is_closed() {
                spawn_control_handler(shared, value);
            }
            return;
        }
        Some("control_cancel_request") => {
            if let Some(id) = value.get("request_id").and_then(Value::as_str) {
                if let Some(task) = lock(&shared.inflight_requests).remove(id) {
                    task.abort();
                }
            }
            return;
        }
        Some("transcript_mirror") => {
            // Never waits on the store: a flush it triggers runs in the
            // mirror's drain task, so routing goes on meanwhile.
            if let Some(mirror) = &mut state.mirror {
                if let Err(error) = mirror.enqueue_value(&value) {
                    tracing::warn!("transcript mirror enqueue failed: {error}");
                }
            }
            return;
        }
        _ => {}
    }

    let subtype = value.get("subtype").and_then(Value::as_str);
    if msg_type.as_deref() == Some("system") {
        let mut run = lock(&shared.run);
        let had_tasks_in_flight = !run.inflight_tasks.is_empty();
        run.track_task_lifecycle(&value);
        if had_tasks_in_flight && run.inflight_tasks.is_empty() {
            shared.rearm_ceiling_between_turns(&mut run);
        }
        if subtype == Some("session_state_changed") {
            shared.on_session_state(&mut run, value.get("state"));
            // Frames sent only because the SDK asked for them
            // (CLAUDE_CODE_SDK_READS_SESSION_STATE); the caller did not opt in.
            if value.get("sdk_host_only") == Some(&Value::Bool(true)) {
                return;
            }
        }
    }

    if msg_type.as_deref() == Some("result") {
        // Consumers observing the result can rely on the store being up to
        // date for this turn.
        flush_mirror(state).await;
        let mut run = lock(&shared.run);
        run.result_received = true;
        run.turn_in_progress = false;
        if run.state_is_none_or_idle() || !shared.bidirectional {
            run.maybe_end_run();
        } else if !run.state_is("requires_action") {
            shared.arm_ceiling(&mut run);
        }
    } else if matches!(msg_type.as_deref(), Some("assistant" | "stream_event"))
        && value.get("parent_tool_use_id").is_none_or(Value::is_null)
    {
        // A main-thread turn is under way.
        let mut run = lock(&shared.run);
        run.turn_in_progress = true;
        run.reopen_run();
        run.clear_ceiling();
    }
    state.tracker.observe(&value);

    let is_result = msg_type.as_deref() == Some("result");
    let item = match parse_message_value(value) {
        Ok(Some(message)) => StreamItem::Message(message),
        Ok(None) => return,
        Err(error) => StreamItem::ParseError { error, is_result },
    };
    let _ = messages.send(item);
}

fn route_control_response(shared: &Shared, value: &Value) {
    let Some(response) = value.get("response").and_then(Value::as_object) else {
        return;
    };
    let Some(request_id) = response.get("request_id").and_then(Value::as_str) else {
        return;
    };
    let Some(pending) = lock(&shared.pending).remove(request_id) else {
        return;
    };
    let _ = pending
        .tx
        .send(parse_control_response(response, &pending.subtype));
}

/// Answer one `control_request` in its own task, tracked by request id so a
/// `control_cancel_request` can abort it (no response is written then).
fn spawn_control_handler(shared: &Arc<Shared>, value: Value) {
    let Some(request_id) = value
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    // Hold the table lock across the spawn so the task's own removal cannot
    // run before its entry exists.
    let mut inflight = lock(&shared.inflight_requests);
    let task_shared = Arc::clone(shared);
    let id = request_id.clone();
    let task = tokio::spawn(async move {
        let callbacks = Arc::clone(&task_shared.callbacks);
        let response = AssertUnwindSafe(control_request_response(&callbacks, &value))
            .catch_unwind()
            .await
            .unwrap_or_else(|panic| {
                Some(control_error_response_payload(&id, &panic_message(&panic)))
            });
        if let Some(response) = response {
            if let Err(error) = task_shared.write_json(&response).await {
                tracing::debug!("could not write control response for {id}: {error}");
            }
        }
        lock(&task_shared.inflight_requests).remove(&id);
    });
    inflight.insert(request_id, task.abort_handle());
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic_text(panic).unwrap_or_else(|| "control request handler panicked".to_string())
}

/// A panic payload's message, when it is a string.
fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> Option<String> {
    if let Some(message) = panic.downcast_ref::<&str>() {
        Some((*message).to_string())
    } else {
        panic.downcast_ref::<String>().cloned()
    }
}

/// Duplicate an error for each pending request (the SDK error type is not
/// `Clone`). Variants keep their kind and text.
pub(crate) fn clone_error(error: &ClaudeSDKError) -> ClaudeSDKError {
    match error {
        ClaudeSDKError::CLIConnection(e) => CLIConnectionError::new(e.message.clone()).into(),
        ClaudeSDKError::CLINotFound(e) => CLINotFoundError {
            message: e.message.clone(),
            cli_path: e.cli_path.clone(),
        }
        .into(),
        ClaudeSDKError::Process(e) => {
            ProcessError::new(e.message.clone(), e.exit_code, e.stderr.clone()).into()
        }
        ClaudeSDKError::Result(e) => ClaudeSDKError::Result(e.clone()),
        ClaudeSDKError::CLIJSONDecode(e) => {
            CLIJSONDecodeError::new(e.line.clone(), e.original_error.to_string()).into()
        }
        ClaudeSDKError::MessageParse(e) => ClaudeSDKError::MessageParse(MessageParseError {
            message: e.message.clone(),
            data: e.data.clone(),
        }),
        ClaudeSDKError::IO(e) => std::io::Error::new(e.kind(), e.to_string()).into(),
        ClaudeSDKError::Serialization(e) => {
            ClaudeSDKError::Serialization(<serde_json::Error as serde::de::Error>::custom(e))
        }
        ClaudeSDKError::Session(m) => ClaudeSDKError::Session(m.clone()),
        ClaudeSDKError::MCP(m) => ClaudeSDKError::MCP(m.clone()),
        ClaudeSDKError::ControlRequest { subtype, message } => ClaudeSDKError::ControlRequest {
            subtype: subtype.clone(),
            message: message.clone(),
        },
        ClaudeSDKError::Other(m) => ClaudeSDKError::Other(m.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_end_ceiling_reads_options_env_then_default() {
        let mut env = HashMap::new();
        env.insert(RUN_END_CEILING_ENV.to_string(), "0".to_string());
        assert_eq!(run_end_ceiling_ms(&env), 0);
        env.insert(RUN_END_CEILING_ENV.to_string(), "1500".to_string());
        assert_eq!(run_end_ceiling_ms(&env), 1500);
        for bad in ["-1", "1e6", "abc", ""] {
            env.insert(RUN_END_CEILING_ENV.to_string(), bad.to_string());
            assert_eq!(
                run_end_ceiling_ms(&env),
                DEFAULT_RUN_END_CEILING_MS,
                "{bad}"
            );
        }
    }

    #[test]
    fn task_lifecycle_tracks_only_deferring_tasks() {
        let mut run = RunState::new();
        run.track_task_lifecycle(&serde_json::json!({
            "subtype": "task_started", "task_id": "a", "task_type": "local_agent"
        }));
        run.track_task_lifecycle(&serde_json::json!({
            "subtype": "task_started", "task_id": "shell", "task_type": "local_bash"
        }));
        run.track_task_lifecycle(&serde_json::json!({
            "subtype": "task_started", "task_id": "w", "task_type": "local_workflow"
        }));
        assert_eq!(run.inflight_tasks.len(), 2);
        run.track_task_lifecycle(&serde_json::json!({
            "subtype": "task_updated", "task_id": "a", "patch": {"status": "running"}
        }));
        assert_eq!(run.inflight_tasks.len(), 2);
        run.track_task_lifecycle(&serde_json::json!({
            "subtype": "task_updated", "task_id": "a", "patch": {"status": "completed"}
        }));
        run.track_task_lifecycle(&serde_json::json!({
            "subtype": "task_notification", "task_id": "w"
        }));
        assert!(run.inflight_tasks.is_empty());
    }

    #[test]
    fn reopen_only_replaces_an_ended_non_final_run() {
        let mut run = RunState::new();
        let first = Arc::clone(&run.run_ended);
        run.reopen_run();
        assert!(Arc::ptr_eq(&first, &run.run_ended));
        run.end_run();
        assert!(first.is_set());
        run.reopen_run();
        assert!(!run.run_ended.is_set());
        run.end_run();
        run.run_final = true;
        run.reopen_run();
        assert!(run.run_ended.is_set());
    }

    #[test]
    fn clone_error_keeps_kind_and_text() {
        let error: ClaudeSDKError = ProcessError::new("boom", Some(2), "err").into();
        let cloned = clone_error(&error);
        assert_eq!(cloned.to_string(), error.to_string());
        assert_eq!(cloned.exit_code(), Some(2));
    }
}
