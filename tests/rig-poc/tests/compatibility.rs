use std::collections::BTreeSet;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

use rig::AgentBuilder;
use rig::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};
use rig::completion::{CompletionRequest, PromptError};
use rig::message::{ImageMediaType, Message, UserContent};
use rig::run::{AgentRun, AgentRunStep, ModelTurn, ModelTurnOutcome, PendingToolCall, TurnPolicy};
use rig::test_utils::{MockCompletionModel, MockTurn};
use rig::tool::{Tool, ToolContext, ToolSet};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

// A deliberately small probe type, not Dataset Doctor's production schema.
#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
struct ProbePlan {
    template: ProbeTemplate,
    dataset_id: String,
    max_samples: u32,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ProbeTemplate {
    QualityCheck,
}

const PLAN: &str = r#"{"template":"quality_check","dataset_id":"synthetic-dataset","max_samples":4}"#;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectArgs {
    dataset_id: String,
}

#[derive(Clone, Default)]
struct InspectDataset {
    calls: Arc<AtomicUsize>,
}

impl Tool for InspectDataset {
    const NAME: &'static str = "inspect_dataset";
    type Error = std::io::Error;
    type Args = InspectArgs;
    type Output = String;

    fn description(&self) -> String {
        "Return a deterministic synthetic dataset summary for compatibility tests.".into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"dataset_id": {"type": "string"}},
            "required": ["dataset_id"],
            "additionalProperties": false
        })
    }

    async fn call(&self, _context: &mut ToolContext, args: Self::Args) -> Result<String, std::io::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"dataset_id": args.dataset_id, "images": 4}).to_string())
    }
}

struct StopBeforeTool;

impl AgentHook for StopBeforeTool {
    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        if event.tool_name().is_some() {
            DispatchAction::stop("synthetic Core denial before tool dispatch")
        } else {
            DispatchAction::proceed()
        }
    }
}

fn tool_turn(id: &str) -> MockTurn {
    MockTurn::tool_call(id, InspectDataset::NAME, json!({"dataset_id": "synthetic-dataset"}))
}

#[tokio::test]
async fn typed_plan_is_parsed_and_schema_is_sent() {
    let model = MockCompletionModel::text(PLAN);
    let agent = AgentBuilder::new(model.clone()).build();
    let result = agent.prompt_typed::<ProbePlan>("Plan a synthetic quality check.").await.unwrap();
    assert_eq!(result.output, ProbePlan {
        template: ProbeTemplate::QualityCheck,
        dataset_id: "synthetic-dataset".into(),
        max_samples: 4,
    });
    assert_eq!(model.request_count(), 1);
    assert!(model.requests()[0].output_schema.is_some());
}

#[tokio::test]
async fn invalid_typed_plan_fails_with_a_bounded_call_budget() {
    let model = MockCompletionModel::from_turns([
        MockTurn::text(r#"{"template":"arbitrary_shell","dataset_id":"synthetic-dataset","max_samples":4}"#),
        MockTurn::text(PLAN),
    ]);
    let agent = AgentBuilder::new(model.clone()).build();
    let result = agent.prompt_typed::<ProbePlan>("Return an allowed plan.")
        .max_turns(1).retries(0).await;
    assert!(result.is_err(), "an out-of-enum plan must not become a typed plan");
    assert_eq!(model.request_count(), 1, "schema repair must respect the model-call budget");
}

#[tokio::test]
async fn provider_failure_retry_is_explicit_and_bounded() {
    let model = MockCompletionModel::from_turns([
        MockTurn::error("synthetic transient provider failure"),
        MockTurn::text(PLAN),
    ]);
    let agent = AgentBuilder::new(model.clone()).build();
    let result = agent.prompt_typed::<ProbePlan>("Return a quality plan.")
        .retries(1).await.unwrap();
    assert_eq!(result.output.max_samples, 4);
    assert_eq!(model.request_count(), 2);
}

#[tokio::test]
async fn image_and_annotation_context_reach_the_model_request() {
    // Public-domain synthetic one-pixel PNG; no user image or network URL.
    let image = UserContent::image_base64(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=",
        Some(ImageMediaType::PNG),
        None,
    );
    let prompt = Message::User { content: vec![
        UserContent::text(r#"{"bbox":[0,0,1,1],"category":"synthetic_light"}"#),
        image,
    ] };
    let model = MockCompletionModel::text("synthetic review only");
    let agent = AgentBuilder::new(model.clone()).build();
    agent.prompt(prompt).await.unwrap();
    let requests = model.requests();
    let request = &requests[0];
    let images: Vec<_> = request.chat_history.iter().flat_map(|m| m.images()).collect();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].media_type, Some(ImageMediaType::PNG));
    let encoded = serde_json::to_string(&request.chat_history).unwrap();
    assert!(encoded.contains("synthetic_light"));
    assert!(encoded.contains("bbox"));
}

#[tokio::test]
async fn registered_tool_round_trip_preserves_result() {
    let tool = InspectDataset::default();
    let model = MockCompletionModel::from_turns([tool_turn("call-1"), MockTurn::text("review-ready")]);
    let agent = AgentBuilder::new(model.clone()).tool(tool.clone()).build();
    let response = agent.prompt("Inspect the synthetic dataset.").max_turns(2).await.unwrap();
    assert_eq!(response.output(), "review-ready");
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.request_count(), 2);
    let requests = model.requests();
    let results: Vec<_> = requests[1].chat_history.iter().flat_map(|m| m.results()).collect();
    assert_eq!(results.len(), 1, "follow-up must contain the real registered tool's result");
    let serialized = serde_json::to_string(&results).unwrap();
    assert!(serialized.contains("synthetic-dataset"));
    assert!(serialized.contains("images"));
}

#[tokio::test]
async fn unknown_tool_is_rejected_without_execution() {
    let tool = InspectDataset::default();
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("unknown-1", "write_final_without_review", json!({})),
    ]);
    let agent = AgentBuilder::new(model.clone()).tool(tool.clone()).build();
    let result = agent.prompt("Inspect only.").max_turns(2).await;
    assert!(result.is_err());
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
    assert_eq!(model.request_count(), 1);
}

#[tokio::test]
async fn hook_stop_prevents_registered_tool_execution() {
    let tool = InspectDataset::default();
    let model = MockCompletionModel::from_turns([tool_turn("deny-1")]);
    let agent = AgentBuilder::new(model.clone()).tool(tool.clone()).build();
    let error = agent.prompt("Inspect the synthetic dataset.").max_turns(2)
        .add_hook(StopBeforeTool).await.unwrap_err();
    assert!(matches!(error, PromptError::Cancelled { .. }));
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
    assert_eq!(model.request_count(), 1);
}

#[tokio::test]
async fn model_tool_loop_stops_at_configured_budget() {
    let tool = InspectDataset::default();
    let model = MockCompletionModel::from_turns([tool_turn("loop-1"), tool_turn("loop-2"), tool_turn("loop-3")]);
    let agent = AgentBuilder::new(model.clone()).tool(tool.clone()).build();
    let error = agent.prompt("Keep inspecting.").max_turns(2).await.unwrap_err();
    assert!(matches!(error, PromptError::MaxTurns { .. }));
    assert_eq!(model.request_count(), 2);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn pending_tool_checkpoint_round_trip_resumes_to_completion() {
    let model = MockCompletionModel::from_turns([tool_turn("resume-1"), MockTurn::text("resumed")]);
    let tool = InspectDataset::default();
    let mut tools = ToolSet::default();
    tools.add_tool(tool.clone());
    let definitions = tools.tool_definitions();
    let names: BTreeSet<String> = definitions.iter().map(|d| d.name.to_string()).collect();
    let mut run = AgentRun::new("Inspect the synthetic dataset.").max_turns(2);
    let AgentRunStep::CallModel { prompt, history, .. } = run.next_step().unwrap() else {
        panic!("expected model boundary");
    };
    let response = model.call(CompletionRequest::new(prompt).messages(history).tools(definitions.clone())).await.unwrap();
    let outcome = run.model_response(ModelTurn::new(
        response.head(), response.choice.clone(), response.usage,
        TurnPolicy::new(names.clone(), None, None).unwrap(), response.raw.clone(),
    )).unwrap();
    assert!(!matches!(outcome, ModelTurnOutcome::NeedsResolution(_)));
    assert!(matches!(run.next_step().unwrap(), AgentRunStep::CallTools { .. }));
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0, "serialization boundary precedes tool side effects");

    let checkpoint = serde_json::to_string(&run).unwrap();
    let mut restored: AgentRun = serde_json::from_str(&checkpoint).unwrap();
    let AgentRunStep::CallTools { calls } = restored.next_step().unwrap() else {
        panic!("restored run must re-emit pending calls");
    };
    assert_eq!(calls.len(), 1);
    for call in calls {
        match call {
            PendingToolCall::Execute(call) => {
                assert_eq!(call.name(), InspectDataset::NAME);
                let mut context = ToolContext::new();
                let result = tools.execute(call.name(), call.arguments().to_string(), &mut context).await;
                restored.answer(call.answer(result)).unwrap();
            }
            other => panic!("unexpected pending call: {other:?}"),
        }
    }
    let AgentRunStep::CallModel { prompt, history, .. } = restored.next_step().unwrap() else {
        panic!("expected follow-up after restored tool result");
    };
    let response = model.call(CompletionRequest::new(prompt).messages(history).tools(definitions)).await.unwrap();
    restored.model_response(ModelTurn::new(
        response.head(), response.choice.clone(), response.usage,
        TurnPolicy::new(names, None, None).unwrap(), response.raw.clone(),
    )).unwrap();
    let AgentRunStep::Done(response) = restored.next_step().unwrap() else {
        panic!("restored run should finish");
    };
    assert_eq!(response.output(), "resumed");
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.request_count(), 2);
}

#[test]
fn incompatible_checkpoint_format_is_rejected() {
    let mut checkpoint = serde_json::to_value(AgentRun::new("synthetic task")).unwrap();
    let format = checkpoint["format"].as_u64().unwrap();
    checkpoint["format"] = json!(format + 1);
    assert!(serde_json::from_value::<AgentRun>(checkpoint).is_err());
}
