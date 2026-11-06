//! The `rag_search` tool exposed to turns.

use super::*;

pub struct RagSearchTool {
    retriever: Arc<dyn Retriever>,
    max_k: usize,
}

impl RagSearchTool {
    pub fn new(retriever: Arc<dyn Retriever>, max_k: usize) -> Self {
        Self {
            retriever,
            max_k: max_k.clamp(1, 20),
        }
    }
}

#[async_trait]
impl Tool for RagSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rag_search".into(),
            description:
                "Search the indexed private document corpus. Returns passages tagged with a document id and page for exact citations."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search question, in any supported language"},
                    "k": {"type": "integer", "minimum": 1, "maximum": self.max_k}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if query.is_empty() || query.chars().count() > 2000 {
            return Err(SeraError::Validation("invalid search query".into()));
        }
        let k = args
            .get("k")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(6)
            .clamp(1, self.max_k);
        let chunks = self.retriever.retrieve(&context.user_id, query, k).await?;
        if chunks.is_empty() {
            return Ok(ToolOutput {
                content: "No indexed evidence matched this query.".into(),
                data: Some(json!({"chunks": []})),
                ..ToolOutput::default()
            });
        }
        Ok(ToolOutput {
            content: evidence_block(&chunks),
            data: Some(json!({"chunks": chunks})),
            ..ToolOutput::default()
        })
    }
}
