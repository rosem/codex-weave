use async_trait::async_trait;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_protocol::protocol::SessionSource;
use codex_protocol::weave::WeaveRelayToolArgs;

pub struct WeaveRelayHandler;

#[async_trait]
impl ToolHandler for WeaveRelayHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "weave_relay_actions handler received unsupported payload".to_string(),
                ));
            }
        };

        let session_source = turn.client.get_session_source();
        if matches!(session_source, SessionSource::Exec | SessionSource::Mcp) {
            return Err(FunctionCallError::RespondToModel(format!(
                "weave_relay_actions is unsupported in {session_source} sessions"
            )));
        }

        let args: WeaveRelayToolArgs = parse_arguments(&arguments)?;
        let result = session
            .request_weave_relay(turn.as_ref(), call_id, args)
            .await
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "weave relay was cancelled before receiving a response".to_string(),
                )
            })?;
        let success = Some(result.status == "ok");
        let content = serde_json::to_string(&result).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize weave relay response: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success,
        })
    }
}
