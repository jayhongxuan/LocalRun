//! M3-3 对话页：OpenAI 兼容调用本地 llama-server + 工具调用体系。
//! 非流式版先打通（stream=false），返回完整 JSON；流式后续迭代。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

/// 对话消息。字段名必须保持 OpenAI 的 snake_case，否则 llama-server 收不到
/// assistant.tool_calls / tool.tool_call_id，多轮工具调用会失效。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String, // system / user / assistant / tool
    /// 兼容字符串、内容分段数组（附件/视觉）和 assistant 工具调用时的 null。
    #[serde(default)]
    pub content: Value,
    #[serde(rename = "tool_calls", skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(rename = "tool_call_id", skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub ttype: String, // "function"
    pub function: ToolFunction,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: String, // JSON 字符串
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub ttype: String,
    pub function: ToolFuncDef,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolFuncDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 一次对话请求（前端发来历史消息 + 可选工具表）
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

fn has_image_part(content: &Value) -> bool {
    match content {
        Value::Array(parts) => parts.iter().any(|part| {
            part.get("type").and_then(Value::as_str) == Some("image_url")
        }),
        _ => false,
    }
}

fn validate_content(content: &Value) -> Result<(), String> {
    match content {
        Value::Null | Value::String(_) | Value::Array(_) => Ok(()),
        _ => Err("消息 content 必须是字符串或内容分段数组".to_string()),
    }
}

fn reject_unsupported_images(messages: &[ChatMessage]) -> Result<(), String> {
    if messages.iter().any(|m| has_image_part(&m.content)) {
        Err(
            "VISION_UNSUPPORTED: 当前 LocalRun 启动的 llama-server 未配置视觉投影文件（mmproj），\
             不能处理图片附件。请移除图片后重试；文本附件仍可正常使用。"
                .to_string(),
        )
    } else {
        Ok(())
    }
}

/// 调用本地 llama-server 的 /v1/chat/completions（非流式），返回原始 JSON 文本
#[tauri::command]
pub async fn chat_completion(
    state: State<'_, crate::AppState>,
    req: ChatRequest,
) -> Result<String, String> {
    if req.messages.is_empty() {
        return Err("对话消息为空".to_string());
    }
    for message in &req.messages {
        validate_content(&message.content)?;
    }

    // 当前 start_model 没有配置 mmproj（视觉投影文件），llama-server 无法解析
    // image_url 分段。提前给出明确错误，避免把请求发出去后只得到晦涩的 400。
    reject_unsupported_images(&req.messages)?;

    let st = state.runtime.status.lock().unwrap().clone();
    if !st.running || !st.ready {
        return Err("模型未运行：请先在「运行」页启动模型".to_string());
    }
    let port = st.port;

    let mut body = serde_json::json!({
        "messages": req.messages,
        "temperature": req.temperature.unwrap_or(0.7),
        "max_tokens": req.max_tokens.unwrap_or(2048),
        "stream": false,
    });
    if let Some(tools) = req.tools {
        body["tools"] =
            serde_json::to_value(tools).map_err(|e| format!("工具表序列化失败：{e}"))?;
    }

    // 本地推理可能长达数分钟（大 prompt + 长生成），不能用 health check 的 3s 短超时客户端
    let client = reqwest::Client::builder()
        .user_agent("LocalRun/0.1 (self-hosted model manager)")
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("构建本地客户端失败：{e}"))?;
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求本地服务失败：{e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败：{e}"))?;
    if !status.is_success() {
        return Err(format!(
            "本地服务返回 {status}：{}",
            text.chars().take(500).collect::<String>()
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_fields_use_openai_snake_case() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: Value::String(String::new()),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                ttype: "function".to_string(),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"/tmp/a.txt\"}".to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        };
        let value = serde_json::to_value(&message).unwrap();
        assert!(value.get("tool_calls").is_some());
        assert!(value.get("toolCalls").is_none());

        let tool_message: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "tool",
            "content": "ok",
            "tool_call_id": "call_1"
        }))
        .unwrap();
        assert_eq!(tool_message.tool_call_id.as_deref(), Some("call_1"));
        let tool_value = serde_json::to_value(tool_message).unwrap();
        assert_eq!(tool_value["tool_call_id"], "call_1");
    }

    #[test]
    fn content_accepts_string_array_and_null() {
        for content in [
            serde_json::json!("hello"),
            serde_json::json!([{"type": "text", "text": "hello"}]),
            Value::Null,
        ] {
            let message: ChatMessage = serde_json::from_value(serde_json::json!({
                "role": "user",
                "content": content,
            }))
            .unwrap();
            assert!(validate_content(&message.content).is_ok());
        }
    }

    #[test]
    fn detects_image_parts() {
        let content = serde_json::json!([
            {"type": "text", "text": "look"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
        ]);
        assert!(has_image_part(&content));
        assert!(!has_image_part(&serde_json::json!("plain text")));
    }

    #[test]
    fn rejects_image_messages_with_clear_code() {
        let message = ChatMessage {
            role: "user".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
            ]),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        let error = reject_unsupported_images(&[message]).unwrap_err();
        assert!(error.starts_with("VISION_UNSUPPORTED:"), "{error}");
        assert!(error.contains("mmproj"), "{error}");
    }
}
