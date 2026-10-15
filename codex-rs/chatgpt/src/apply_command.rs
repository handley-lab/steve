use std::path::PathBuf;

use codex_git_utils::ApplyGitRequest;
use codex_git_utils::apply_git_patch;

use crate::get_task::GetTaskResponse;
use crate::get_task::OutputItem;
use crate::get_task::PrOutputItem;

pub async fn apply_diff_from_task(
    task_response: GetTaskResponse,
    cwd: Option<PathBuf>,
) -> anyhow::Result<()> {
    let diff_turn = match task_response.current_diff_task_turn {
        Some(turn) => turn,
        None => anyhow::bail!("No diff turn found"),
    };
    let output_diff = diff_turn.output_items.iter().find_map(|item| match item {
        OutputItem::Pr(PrOutputItem { output_diff }) => Some(output_diff),
        _ => None,
    });
    match output_diff {
        Some(output_diff) => apply_diff(&output_diff.diff, cwd).await,
        None => anyhow::bail!("No PR output item found"),
    }
}

async fn apply_diff(diff: &str, cwd: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = cwd.unwrap_or(std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()));
    let req = ApplyGitRequest {
        cwd,
        diff: diff.to_string(),
        revert: false,
        preflight: false,
    };
    let res = apply_git_patch(&req)?;
    if res.exit_code != 0 {
        anyhow::bail!(
            "Git apply failed (applied={}, skipped={}, conflicts={})\nstdout:\n{}\nstderr:\n{}",
            res.applied_paths.len(),
            res.skipped_paths.len(),
            res.conflicted_paths.len(),
            res.stdout,
            res.stderr
        );
    }
    println!("Successfully applied diff");
    Ok(())
}
