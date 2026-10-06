pub mod bash;
pub mod files;
pub mod glob;
pub mod grep;
pub mod message;

use std::{collections::HashMap, fmt, pin::Pin, sync::Arc};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use genai::chat::{Tool as GenaiTool, ToolCall, ToolResponse};

use crate::tools::{
    bash::BashTool,
    files::{EditTool, ReadTool, WriteTool},
    glob::GlobTool,
    grep::GrepTool,
};

pub type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
pub enum ToolError {
    UnknownTool { name: String },
    InvalidArgs { tool: String, message: String },
    Execution { tool: String, message: String },
}

impl ToolError {
    fn to_model_payload(&self) -> String {
        let (tool, message) = match self {
            Self::UnknownTool { name } => (name.as_str(), format!("unknown tool `{name}`")),
            Self::InvalidArgs { tool, message } => {
                (tool.as_str(), format!("invalid arguments: {message}"))
            }
            Self::Execution { tool, message } => {
                (tool.as_str(), format!("execution failed: {message}"))
            }
        };

        json!({"error": message, "tool": tool}).to_string()
    }
}

#[derive(Debug)]
pub enum ToolRegisterError {
    Duplicate { name: String },
}

impl fmt::Display for ToolRegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate { name } => write!(f, "A tool named '{name}' already exists."),
        }
    }
}

impl std::error::Error for ToolRegisterError {}

pub trait Tool: Send + Sync + 'static {
    type Args: DeserializeOwned + Send;

    const NAME: &'static str;

    fn description(&self) -> &str;

    fn system_description(&self) -> &str;

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    }

    fn call<'a>(&'a self, args: Self::Args) -> BoxedFuture<'a, anyhow::Result<String>>;
}

pub trait DynTool: Send + Sync {
    fn name(&self) -> &str;
    fn declaration(&self) -> GenaiTool;
    fn get_system_description(&self) -> &str;
    fn call_json<'a>(&'a self, args: Value) -> BoxedFuture<'a, Result<String, ToolError>>;
}

impl<T: Tool> DynTool for T {
    fn name(&self) -> &str {
        T::NAME
    }

    fn declaration(&self) -> GenaiTool {
        GenaiTool::new(T::NAME)
            .with_description(self.description())
            .with_schema(self.parameters())
    }

    fn get_system_description(&self) -> &str {
        self.system_description()
    }

    fn call_json<'a>(&'a self, args: Value) -> BoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let type_args =
                serde_json::from_value::<T::Args>(args).map_err(|e| ToolError::InvalidArgs {
                    tool: T::NAME.to_string(),
                    message: e.to_string(),
                })?;
            match self.call(type_args).await {
                Ok(res) => Ok(res),
                Err(e) => Err(ToolError::Execution {
                    tool: T::NAME.to_string(),
                    message: e.to_string(),
                }),
            }
        })
    }
}

pub struct ToolRouter {
    index: HashMap<String, usize>,
    tools: Vec<Arc<dyn DynTool>>,
}

impl Default for ToolRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRouter {
    pub fn new() -> Self {
        Self {
            index: HashMap::new(),
            tools: Vec::new(),
        }
    }

    pub fn register_builtin_tools(&mut self) -> Result<(), ToolRegisterError> {
        self.register(ReadTool)?;
        self.register(WriteTool)?;
        self.register(EditTool)?;
        self.register(GlobTool)?;
        self.register(GrepTool)?;
        self.register(BashTool)?;

        Ok(())
    }

    pub fn register<T: Tool>(&mut self, tool: T) -> Result<&mut Self, ToolRegisterError> {
        let name = T::NAME.to_string();
        if self.index.contains_key(&name) {
            return Err(ToolRegisterError::Duplicate { name });
        }
        self.tools.push(Arc::new(tool));
        self.index.insert(name, self.tools.len() - 1);
        Ok(self)
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    pub fn declarations(&self) -> Vec<GenaiTool> {
        self.tools.iter().map(|tool| tool.declaration()).collect()
    }

    pub fn get_tool_system_descriptions(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.get_system_description().to_string())
            .collect()
    }

    fn get(&self, name: &str) -> Option<&dyn DynTool> {
        self.index
            .get(name)
            .and_then(|&i| self.tools.get(i))
            .map(Arc::as_ref)
    }

    pub async fn dispatch(&self, call: &ToolCall) -> ToolResponse {
        ToolResponse::from_tool_call(
            call,
            match self.get(&call.fn_name) {
                Some(tool) => match tool.call_json(call.fn_arguments.clone()).await {
                    Ok(res) => res,
                    Err(e) => e.to_model_payload(),
                },
                None => ToolError::UnknownTool {
                    name: call.fn_name.clone(),
                }
                .to_model_payload(),
            },
        )
    }

    pub async fn dispatch_all(&self, call: &[ToolCall]) -> Vec<ToolResponse> {
        futures::future::join_all(call.iter().map(|call| self.dispatch(call))).await
    }
}

const MAX_LINE_LENGTH: usize = 2000;

fn truncate_line(line: &str, max_chars: usize) -> String {
    match line.char_indices().nth(max_chars) {
        Some((idx, _)) => format!(
            "{} ... (line truncated to {} chars)",
            &line[..idx],
            max_chars
        ),
        None => line.to_string(),
    }
}
