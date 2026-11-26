use std::error::Error;
use std::fmt;
use std::time::Duration;

use rig_agent::agent::AgentBuilder;
use rig_agent::completion::{PromptError, StructuredOutputError};
use rig_core::providers::openai::{OpenAI, OpenAIConfig};
use rig_reqwest::{ReqwestClient, reqwest};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelPurpose {
    Plan,
    VisualReview,
    ExplainFindings,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelCall {
    pub profile_id: Uuid,
    pub purpose: ModelPurpose,
    pub reservation_id: Uuid,
    pub max_output_tokens: u64,
    pub deadline: Duration,
}

#[derive(Debug)]
pub struct ModelReply<T> {
    pub output: T,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelError {
    Configuration,
    InvalidReservation,
    Deadline,
    Provider,
    InvalidOutput,
    BudgetExceeded,
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "Invalid model profile configuration",
            Self::InvalidReservation => "Model request does not match an available reservation",
            Self::Deadline => "Model request timed out",
            Self::Provider => "The configured model provider could not complete the request",
            Self::InvalidOutput => "The model response did not match the accepted structure",
            Self::BudgetExceeded => "The model response exceeded its reserved budget",
        })
    }
}

impl Error for ModelError {}

pub struct AgentModelClient {
    profile_id: Uuid,
    provider: OpenAI,
    model: String,
}

impl AgentModelClient {
    pub fn new_openai_chat(
        profile_id: Uuid,
        endpoint: String,
        credential: String,
        model: String,
    ) -> Result<Self, ModelError> {
        let url = reqwest::Url::parse(&endpoint).map_err(|_| ModelError::Configuration)?;
        if profile_id.is_nil()
            || credential.trim().is_empty()
            || model.trim().is_empty()
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ModelError::Configuration);
        }
        let mut transport = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        if matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) {
            transport = transport.no_proxy();
        }
        let transport = transport.build().map_err(|_| ModelError::Configuration)?;
        let provider = OpenAIConfig::new(credential)
            .with_base_url(endpoint)
            .connect(ReqwestClient::from(transport));
        Ok(Self {
            profile_id,
            provider,
            model,
        })
    }

    /// The Core supplies the model profile and the limits for each authorized call.
    /// A provider failure leaves the next attempt to the workflow policy.
    pub async fn request<T>(
        &self,
        call: ModelCall,
        prompt: &str,
    ) -> Result<ModelReply<T>, ModelError>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        if call.profile_id != self.profile_id
            || call.reservation_id.is_nil()
            || call.max_output_tokens == 0
            || call.deadline.is_zero()
        {
            return Err(ModelError::InvalidReservation);
        }
        let scope = match call.purpose {
            ModelPurpose::Plan => "Propose checks for the registered workflow and requested scope.",
            ModelPurpose::VisualReview => {
                "Provide evidence for review. Human approval remains a separate decision."
            }
            ModelPurpose::ExplainFindings => {
                "Explain the supplied check findings and indicate uncertainty."
            }
        };
        let agent = AgentBuilder::new(self.provider.chat(self.model.clone()))
            .preamble(scope)
            .default_max_turns(1)
            .max_tokens(call.max_output_tokens)
            .build();
        let response = tokio::time::timeout(call.deadline, async {
            agent.prompt_typed::<T>(prompt).retries(0).await
        })
        .await
        .map_err(|_| ModelError::Deadline)?
        .map_err(|error| match error {
            StructuredOutputError::Prompt(PromptError::MaxTurns { .. }) => {
                ModelError::BudgetExceeded
            }
            StructuredOutputError::Prompt(
                PromptError::OutputSchemaMismatch { .. } | PromptError::UnknownToolCall { .. },
            )
            | StructuredOutputError::Deserialization { .. }
            | StructuredOutputError::EmptyResponse => ModelError::InvalidOutput,
            StructuredOutputError::Prompt(_) => ModelError::Provider,
        })?;
        if response.requests() != 1
            || response
                .usage
                .output_tokens
                .is_some_and(|used| used > call.max_output_tokens)
        {
            return Err(ModelError::BudgetExceeded);
        }
        Ok(ModelReply {
            output: response.output,
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
        })
    }
}
