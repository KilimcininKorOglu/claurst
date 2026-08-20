// History commands: `/undo`, `/revert`, `/checkpoints`, `/snapshot`.
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::*;
use async_trait::async_trait;

pub struct UndoCommand;
pub struct RevertCommand;
pub struct CheckpointsCommand;
pub struct SnapshotDiffCommand;

// ---- /undo (alias for /revert targeting the most recent assistant turn) ----

#[async_trait]
impl SlashCommand for UndoCommand {
    fn name(&self) -> &str {
        "undo"
    }
    fn aliases(&self) -> Vec<&str> {
        vec![]
    }
    fn description(&self) -> &str {
        "Revert all file changes from the last assistant turn (alias: /revert)"
    }
    fn help(&self) -> &str {
        "Usage: /undo\n\nReverts all file changes made during the most recent assistant turn.\n\
         For finer control use /revert. To list what changed, use /checkpoints."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        RevertCommand.execute("", ctx).await
    }
}

// ---- /revert ---------------------------------------------------------------

#[async_trait]
impl SlashCommand for RevertCommand {
    fn name(&self) -> &str {
        "revert"
    }
    fn description(&self) -> &str {
        "Revert file changes from an assistant turn back to pre-turn state"
    }
    fn help(&self) -> &str {
        "Usage: /revert [<n>|<uuid>]\n\n\
         Without args: revert the most recent assistant turn.\n\
         With a number n: revert the n-th most recent assistant turn (1 = latest).\n\
         With a uuid: revert the turn whose message id starts with that string.\n\n\
         This uses the shadow-git snapshot to restore all files that were\n\
         changed during the target turn, and removes that turn (and any later\n\
         turns) from the session transcript.\n\n\
         Examples:\n\
           /revert        — revert last turn\n\
           /revert 2      — revert the second-to-last turn\n\
           /revert abc123 — revert the turn with uuid starting 'abc123'"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let snap = match mikmik_core::snapshot::get_or_create(&ctx.working_dir) {
            Some(s) => s,
            None => {
                return CommandResult::Error(
                    "Snapshot system unavailable (git not found or not a git repo).".into(),
                )
            }
        };

        // Collect assistant messages that have a snapshot patch (newest last).
        let checkpoints: Vec<&mikmik_core::types::Message> = ctx
            .messages
            .iter()
            .filter(|m| m.role == mikmik_core::types::Role::Assistant && m.snapshot_patch.is_some())
            .collect();

        if checkpoints.is_empty() {
            return CommandResult::Message(
                "No revertible turns found. Run /checkpoints to see recorded file changes.".into(),
            );
        }

        // Select the target turn.
        let args = args.trim();
        let target = if args.is_empty() {
            checkpoints.last().copied()
        } else if let Ok(n) = args.parse::<usize>() {
            if n == 0 || n > checkpoints.len() {
                return CommandResult::Error(format!(
                    "Turn {} out of range (1–{}).",
                    n,
                    checkpoints.len()
                ));
            }
            Some(checkpoints[checkpoints.len() - n])
        } else {
            checkpoints
                .iter()
                .copied()
                .find(|m| m.uuid.as_deref().is_some_and(|u| u.starts_with(args)))
        };

        let target = match target {
            Some(m) => m,
            None => return CommandResult::Error(format!("No turn found matching '{args}'.")),
        };

        // Collect all patches from this turn onward to revert.
        let target_uuid = match target.uuid.clone() {
            Some(u) => u,
            None => return CommandResult::Error("Target turn has no uuid; cannot revert.".into()),
        };

        let patches: Vec<mikmik_core::snapshot::Patch> = ctx
            .messages
            .iter()
            .skip_while(|m| m.uuid.as_deref() != Some(&target_uuid))
            .filter_map(|m| m.snapshot_patch.clone())
            .collect();

        if patches.is_empty() {
            return CommandResult::Message("No file changes recorded for that turn.".into());
        }

        // Revert files.
        snap.revert(&patches).await;

        // Record the revert in the session transcript. NON-DESTRUCTIVE (#234):
        // rather than truncating, point the active leaf at the turn *before* the
        // target so the reverted turn (and everything after it) is retained on a
        // sibling branch that can be returned to. `branch_before` only falls
        // back to a destructive truncate for legacy/unchained transcripts.
        let project_root = mikmik_core::session_storage::transcript_root_for(&ctx.working_dir);
        let path =
            match mikmik_core::session_storage::transcript_path(&project_root, &ctx.session_id) {
                Ok(p) => p,
                Err(e) => return CommandResult::Error(format!("Invalid session ID: {e}")),
            };
        if path.exists() {
            if let Err(e) = mikmik_core::session_storage::branch_before(&path, &target_uuid).await {
                return CommandResult::Error(format!(
                    "Reverted files but could not update transcript: {e}"
                ));
            }
        }

        let file_count: usize = patches.iter().map(|p| p.files.len()).sum();
        CommandResult::Message(format!(
            "Reverted {} file(s) changed during turn {}. Later turns kept on a branch.",
            file_count,
            &target_uuid[..target_uuid.len().min(8)],
        ))
    }
}

// ---- /checkpoints ----------------------------------------------------------

#[async_trait]
impl SlashCommand for CheckpointsCommand {
    fn name(&self) -> &str {
        "checkpoints"
    }
    fn description(&self) -> &str {
        "List assistant turns that have recorded file changes"
    }
    fn help(&self) -> &str {
        "Usage: /checkpoints\n\nShows all assistant turns in this session that modified files,\n\
         with file counts.  Use /revert <n> to roll back to a specific turn."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let checkpoints: Vec<(usize, &mikmik_core::types::Message)> = ctx
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.role == mikmik_core::types::Role::Assistant && m.snapshot_patch.is_some()
            })
            .collect();

        if checkpoints.is_empty() {
            return CommandResult::Message(
                "No file-change checkpoints recorded yet for this session.\n\
                 Checkpoints are created automatically when the assistant modifies files."
                    .into(),
            );
        }

        let total = checkpoints.len();
        let mut lines = vec![format!("{} checkpoint(s):", total)];
        for (rank, (_, msg)) in checkpoints.iter().rev().enumerate() {
            let uuid_short = msg
                .uuid
                .as_deref()
                .map(|u| &u[..u.len().min(8)])
                .unwrap_or("?");
            let file_count = msg.snapshot_patch.as_ref().map_or(0, |p| p.files.len());
            let preview: Vec<String> = msg
                .snapshot_patch
                .as_ref()
                .map(|p| {
                    p.files
                        .iter()
                        .take(3)
                        .map(|f| {
                            f.file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            let preview_str = if preview.len() == file_count {
                preview.join(", ")
            } else {
                format!("{}, …", preview.join(", "))
            };
            lines.push(format!(
                "  [{}] {} — {} file(s): {}",
                rank + 1,
                uuid_short,
                file_count,
                preview_str
            ));
        }
        lines.push(String::new());
        lines.push("Use /revert <n> to revert to before turn [n].".into());
        CommandResult::Message(lines.join("\n"))
    }
}

// ---- /snapshot (show snapshot diff for a recorded turn) ------------------

#[async_trait]
impl SlashCommand for SnapshotDiffCommand {
    fn name(&self) -> &str {
        "snapshot"
    }
    fn description(&self) -> &str {
        "Show shadow-git diff of file changes from an assistant turn"
    }
    fn help(&self) -> &str {
        "Usage: /snapshot [<n>|<hash>]\n\n\
         Without args: show unified diff for the most recent assistant turn.\n\
         With a number: show diff for the n-th most recent turn (1 = latest).\n\
         With a hash: show diff against that explicit snapshot tree hash.\n\n\
         See also: /checkpoints (list turns), /revert (roll back files)."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let snap = match mikmik_core::snapshot::get_or_create(&ctx.working_dir) {
            Some(s) => s,
            None => {
                return CommandResult::Error(
                    "Snapshot system unavailable (git not found or not a git repo).".into(),
                )
            }
        };

        let args = args.trim();

        // If a raw hash was passed, use it directly.
        let hash = if !args.is_empty()
            && args.chars().all(|c| c.is_ascii_hexdigit())
            && args.len() >= 8
        {
            args.to_string()
        } else {
            // Otherwise find the n-th most recent checkpoint.
            let checkpoints: Vec<&mikmik_core::snapshot::Patch> = ctx
                .messages
                .iter()
                .filter_map(|m| {
                    if m.role == mikmik_core::types::Role::Assistant {
                        m.snapshot_patch.as_ref()
                    } else {
                        None
                    }
                })
                .collect();

            if checkpoints.is_empty() {
                return CommandResult::Message(
                    "No snapshot checkpoints recorded yet. File changes will appear here after the next assistant turn.".into()
                );
            }

            let idx = if args.is_empty() {
                0
            } else {
                match args.parse::<usize>() {
                    Ok(n) if n >= 1 && n <= checkpoints.len() => n - 1,
                    _ => {
                        return CommandResult::Error(format!(
                            "Turn '{}' out of range (1–{}).",
                            args,
                            checkpoints.len()
                        ))
                    }
                }
            };
            // Reverse so idx=0 is newest.
            let patch = checkpoints[checkpoints.len() - 1 - idx];
            patch.hash.clone()
        };

        let diff = snap.diff(&hash).await;
        if diff.is_empty() {
            CommandResult::Message(format!(
                "No changes since snapshot {}.",
                &hash[..hash.len().min(8)]
            ))
        } else {
            CommandResult::Message(diff)
        }
    }
}

// ---- /checkpoint -----------------------------------------------------------

pub struct CheckpointCommand;

#[async_trait]
impl SlashCommand for CheckpointCommand {
    fn name(&self) -> &str {
        "checkpoint"
    }

    fn description(&self) -> &str {
        "List conversation checkpoints, or return to one"
    }

    fn help(&self) -> &str {
        "Usage: /checkpoint [list|restore <n>]\n\n\
         A checkpoint is a point in the conversation, recorded at the end of\n\
         each turn. Restoring one drops the turns after it; they stay on disk\n\
         in the session transcript and the checkpoint before them can be\n\
         restored again.\n\n\
         This is about the conversation. /checkpoints (plural) lists the turns\n\
         that changed files, and /revert rolls those files back."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let session = match mikmik_core::history::load_session(&ctx.session_id).await {
            Ok(session) => session,
            Err(e) => {
                return CommandResult::Error(format!(
                    "No saved session to read checkpoints from: {e}"
                ))
            }
        };

        if session.checkpoints.is_empty() {
            return CommandResult::Message(
                "No checkpoints yet. One is recorded at the end of each turn.".into(),
            );
        }

        let mut parts = args.split_whitespace();
        match parts.next().unwrap_or("list") {
            "list" => {
                let mut lines = vec![format!("{} checkpoint(s):", session.checkpoints.len())];
                for (i, cp) in session.checkpoints.iter().enumerate() {
                    lines.push(format!(
                        "  [{}] {} message(s) — {}{}",
                        i + 1,
                        cp.message_idx,
                        cp.created_at.format("%Y-%m-%d %H:%M"),
                        cp.label
                            .as_deref()
                            .map(|l| format!(" — {l}"))
                            .unwrap_or_default(),
                    ));
                }
                lines.push(String::new());
                lines.push("Use /checkpoint restore <n> to go back to one.".to_string());
                CommandResult::Message(lines.join("\n"))
            }
            "restore" => {
                let Some(n) = parts.next().and_then(|n| n.parse::<usize>().ok()) else {
                    return CommandResult::Error(
                        "Usage: /checkpoint restore <n>. Run /checkpoint list to see them.".into(),
                    );
                };
                if n == 0 || n > session.checkpoints.len() {
                    return CommandResult::Error(format!(
                        "Checkpoint {n} out of range (1–{}).",
                        session.checkpoints.len()
                    ));
                }
                // Restored against the live conversation rather than the saved
                // one: the turn in progress has not been written yet.
                let mut live = session.clone();
                live.messages = ctx.messages.clone();
                match mikmik_core::history::restore_checkpoint(&mut live, n - 1) {
                    Some(dropped) if dropped.is_empty() => {
                        CommandResult::Message("Already at that checkpoint.".into())
                    }
                    Some(_) => CommandResult::SetMessages(live.messages),
                    None => CommandResult::Error(
                        "That checkpoint is past the end of this conversation.".into(),
                    ),
                }
            }
            other => CommandResult::Error(format!(
                "Unknown subcommand: {other}\n\nUsage: /checkpoint [list|restore <n>]"
            )),
        }
    }
}
