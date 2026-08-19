// Todos command: read back the session's persisted TodoWrite list (`/todos`).

use super::{CommandContext, CommandResult, SlashCommand};
use async_trait::async_trait;
use claurst_tools::todo_write::{load_todos, parse_confidence};
use serde_json::Value;

pub struct TodosCommand;

/// Render the persisted list as one line per item, with a header count.
///
/// Takes the raw values rather than a session id so the formatting is testable
/// without a writable config directory.
fn format_todos(todos: &[Value]) -> String {
    if todos.is_empty() {
        return "No todos in this session. The model writes them with TodoWrite.".to_string();
    }

    let status_of = |todo: &Value| -> String {
        todo.get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
            .to_string()
    };
    let done = todos
        .iter()
        .filter(|todo| status_of(todo) == "completed")
        .count();

    let mut out = format!("Todos ({}/{} done)\n", done, todos.len());
    for todo in todos {
        let glyph = match status_of(todo).as_str() {
            "completed" => "[x]",
            "in_progress" => "[>]",
            _ => "[ ]",
        };
        let content = todo
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        out.push_str(&format!("  {} {}", glyph, content));
        // Prefer the score recorded at completion, matching the checklist
        // block in the transcript.
        let score = todo
            .get("completion_confidence")
            .and_then(parse_confidence)
            .or_else(|| todo.get("confidence").and_then(parse_confidence));
        if let Some(score) = score {
            out.push_str(&format!(" [{}%]", score));
        }
        out.push('\n');
    }
    out
}

#[async_trait]
impl SlashCommand for TodosCommand {
    fn name(&self) -> &str {
        "todos"
    }

    fn description(&self) -> &str {
        "Show the session's todo list"
    }

    fn help(&self) -> &str {
        "Usage:\n\
         /todos    — list the todos the model recorded for this session\n\n\
         The list is written by the TodoWrite tool and persists across turns.\n\
         Items show their status and, when the model supplied one, a\n\
         confidence percentage."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        CommandResult::Message(format_todos(&load_todos(&ctx.session_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_empty_list_says_so_rather_than_printing_a_header() {
        let out = format_todos(&[]);
        assert!(out.contains("No todos"), "{out:?}");
        assert!(!out.contains("0/0"), "{out:?}");
    }

    #[test]
    fn every_item_appears_with_its_status_glyph() {
        let out = format_todos(&[
            json!({"id":"1","content":"Locate files","status":"completed"}),
            json!({"id":"2","content":"Build importer","status":"in_progress"}),
            json!({"id":"3","content":"Wire adapter","status":"pending"}),
        ]);
        assert!(out.contains("Todos (1/3 done)"), "{out:?}");
        assert!(out.contains("[x] Locate files"), "{out:?}");
        assert!(out.contains("[>] Build importer"), "{out:?}");
        assert!(out.contains("[ ] Wire adapter"), "{out:?}");
    }

    #[test]
    fn confidence_is_shown_when_present_and_omitted_when_not() {
        let out = format_todos(&[
            json!({"id":"1","content":"Scored","status":"pending","confidence":70}),
            json!({"id":"2","content":"Unscored","status":"pending"}),
        ]);
        assert!(out.contains("[ ] Scored [70%]"), "{out:?}");
        assert!(out.contains("[ ] Unscored\n"), "{out:?}");
    }

    #[test]
    fn a_completed_item_prefers_its_completion_score() {
        let out = format_todos(&[json!({
            "id": "1",
            "content": "Done",
            "status": "completed",
            "confidence": 40,
            "completion_confidence": 95
        })]);
        assert!(out.contains("[95%]"), "{out:?}");
        assert!(!out.contains("[40%]"), "{out:?}");
    }

    #[test]
    fn a_missing_status_reads_as_pending_rather_than_dropping_the_item() {
        let out = format_todos(&[json!({"id":"1","content":"Nameless status"})]);
        assert!(out.contains("[ ] Nameless status"), "{out:?}");
    }
}
