use crate::betty_blocks::actions::actions::{call, Error as ActionError, RunInput, RunPayload};
use crate::types::*;
use rust_mcp_schema::ContentBlock;
use serde_json::Value;

pub fn execute_mapped_action(action_id: &str, arguments: &Value) -> Result<ActionResponse, String> {
    let input_json = serde_json::to_string(arguments)
        .map_err(|e| format!("failed to serialize arguments: {e}"))?;

    let payload = RunPayload {
        input: input_json,
        configurations: "{}".to_string(),
    };

    let run_input = RunInput {
        action_id: action_id.to_string(),
        payload,
    };

    match call(&run_input) {
        Ok(output) => {
            let data: Option<Value> = serde_json::from_str(&output.result).ok();
            Ok(ActionResponse {
                success: true,
                data,
                error: None,
            })
        }
        Err(e) => {
            let error_msg = match e {
                ActionError::RunFailed(msg) => format!("action execution failed: {msg}"),
                ActionError::Forbidden => "action forbidden: insufficient permissions".to_string(),
            };
            Ok(ActionResponse {
                success: false,
                data: None,
                error: Some(error_msg),
            })
        }
    }
}

pub fn parse_action_output(action_response: &ActionResponse) -> Result<Vec<ContentBlock>, String> {
    if !action_response.success {
        let error_msg = action_response.error.as_deref().unwrap_or("Unknown error");
        return Ok(vec![ContentBlock::text_content(format!(
            "Error: {}",
            error_msg
        ))]);
    }

    // Parse the action data into content blocks
    match &action_response.data {
        Some(data) => {
            if let Some(text) = data.as_str() {
                Ok(vec![ContentBlock::text_content(text.to_string())])
            }
            // If the data is an object with a "text" field, use that
            else if let Some(text) = data.get("text").and_then(|t| t.as_str()) {
                Ok(vec![ContentBlock::text_content(text.to_string())])
            }
            else if let Some(content_array) = data.get("content").and_then(|c| c.as_array()) {
                parse_content_array(content_array)
            }
            else {
                Ok(vec![ContentBlock::text_content(
                    serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string()),
                )])
            }
        }
        None => Ok(vec![ContentBlock::text_content(
            "Action completed successfully".to_string(),
        )]),
    }
}

fn parse_content_array(content_array: &[Value]) -> Result<Vec<ContentBlock>, String> {
    let mut blocks = Vec::new();

    for item in content_array {
        if let Some(content_type) = item.get("type").and_then(|t| t.as_str()) {
            match content_type {
                "text" => {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        blocks.push(ContentBlock::text_content(text.to_string()));
                    }
                }
                "image" => {
                    if let (Some(data), Some(mime_type)) = (
                        item.get("data").and_then(|d| d.as_str()),
                        item.get("mimeType").and_then(|m| m.as_str()),
                    ) {
                        blocks.push(ContentBlock::image_content(
                            data.to_string(),
                            mime_type.to_string(),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_text_output() {
        let response = ActionResponse {
            success: true,
            data: Some(json!({
                "text": "Hello, world!"
            })),
            error: None,
        };

        let content = parse_action_output(&response).unwrap();
        assert_eq!(content.len(), 1);
    }

    #[test]
    fn test_parse_error_output() {
        let response = ActionResponse {
            success: false,
            data: None,
            error: Some("Something went wrong".to_string()),
        };

        let content = parse_action_output(&response).unwrap();
        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::TextContent(text_content) => {
                assert!(text_content.text.contains("Error:"));
            }
            _ => panic!("Expected text content block"),
        }
    }
}
