use crate::error::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type HookFuture = Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send>>;
type HookFn = dyn Fn(serde_json::Value, Option<String>, HookContext) -> HookFuture + Send + Sync;

#[derive(Debug, Clone, Default)]
pub struct HookContext {}

#[derive(Clone)]
pub struct HookCallback(Arc<HookFn>);

impl std::fmt::Debug for HookCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HookCallback").field(&"<callback>").finish()
    }
}

impl HookCallback {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(serde_json::Value, Option<String>, HookContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value>> + Send + 'static,
    {
        Self(Arc::new(move |input, tool_use_id, context| {
            Box::pin(callback(input, tool_use_id, context))
        }))
    }

    pub async fn call(
        &self,
        input: serde_json::Value,
        tool_use_id: Option<String>,
        context: HookContext,
    ) -> Result<serde_json::Value> {
        (self.0)(input, tool_use_id, context).await
    }
}

#[derive(Debug, Clone, Default)]
pub struct HookMatcher {
    pub matcher: Option<String>,
    pub hooks: Vec<HookCallback>,
    pub timeout: Option<f64>,
}

impl HookMatcher {
    pub fn new(callback: HookCallback) -> Self {
        Self {
            matcher: None,
            hooks: vec![callback],
            timeout: None,
        }
    }

    pub fn matcher(mut self, matcher: impl Into<String>) -> Self {
        self.matcher = Some(matcher.into());
        self
    }

    pub fn timeout(mut self, timeout: f64) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

pub type HookMap = HashMap<String, Vec<HookMatcher>>;
