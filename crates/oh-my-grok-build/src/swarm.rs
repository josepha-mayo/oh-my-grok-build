use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

pub async fn run_swarm_task_splitting(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    count: usize,
) -> Result<String> {
    if count <= 1 {
        return exec_plain(prompt, model, yolo).await;
    }

    let subtasks = match fetch_subtasks(prompt, model.clone(), yolo, count).await {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => {
            eprintln!("warning: task splitting returned no subtasks; falling back to ensemble");
            return run_swarm_ensemble(prompt, model, yolo, count).await;
        }
        Err(e) => {
            eprintln!("warning: task splitting failed ({e}); falling back to ensemble");
            return run_swarm_ensemble(prompt, model, yolo, count).await;
        }
    };

    let results = run_subtasks(&subtasks, model.clone(), yolo).await?;
    let combined = build_combine_prompt(&results, prompt);
    exec_plain(&combined, model, yolo).await
}

pub(crate) async fn run_swarm_ensemble(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    count: usize,
) -> Result<String> {
    let mut handles = Vec::new();
    for i in 0..count {
        let member_prompt = format!(
            "Swarm member {n}/{total}: {task}\n\nProvide a concise answer.",
            n = i + 1,
            total = count,
            task = prompt
        );
        let model = model.clone();
        handles.push(tokio::spawn(async move {
            exec_plain(&member_prompt, model, yolo).await
        }));
    }

    let mut outputs = Vec::new();
    let mut failed = 0usize;
    for h in handles {
        match h.await? {
            Ok(text) => outputs.push(text),
            Err(e) => {
                eprintln!("{e}");
                failed += 1;
            }
        }
    }

    if outputs.is_empty() {
        bail!("all swarm members failed");
    }

    let winner = swarm_vote(&outputs);
    if failed > 0 {
        eprintln!("warning: {failed} swarm member(s) failed; winner chosen from remaining outputs");
    }
    Ok(winner)
}

async fn exec_plain(prompt: &str, model: Option<String>, yolo: bool) -> Result<String> {
    let prompt_file = crate::write_prompt_temp(prompt).await?;
    let _prompt_guard = crate::PromptFileGuard(prompt_file.clone());
    let output_file =
        std::env::temp_dir().join(format!("omgb-swarm-out-{}.txt", uuid::Uuid::new_v4()));
    let exe = std::env::current_exe()?;

    let mut cmd = Command::new(&exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--output-file")
        .arg(&output_file)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(m) = &model {
        cmd.arg("--model").arg(m);
    }
    if yolo {
        cmd.arg("--yolo");
    }

    let status = cmd.status().await?;
    if !status.success() {
        bail!("exec failed");
    }

    let text = tokio::fs::read_to_string(&output_file).await?;
    let _ = tokio::fs::remove_file(&output_file).await;
    Ok(text)
}

async fn fetch_subtasks(
    prompt: &str,
    model: Option<String>,
    yolo: bool,
    count: usize,
) -> Result<Vec<String>> {
    let split_prompt = build_split_prompt(count, prompt);
    let prompt_file = crate::write_prompt_temp(&split_prompt).await?;
    let _prompt_guard = crate::PromptFileGuard(prompt_file.clone());
    let exe = std::env::current_exe()?;

    let mut cmd = Command::new(&exe);
    cmd.arg("exec")
        .arg("--prompt-file")
        .arg(&prompt_file)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(m) = &model {
        cmd.arg("--model").arg(m);
    }
    if yolo {
        cmd.arg("--yolo");
    }

    let out = cmd.output().await?;
    if !out.status.success() {
        bail!("task splitting failed");
    }

    let text = String::from_utf8_lossy(&out.stdout);
    parse_subtasks(&text).context("failed to parse subtasks")
}

async fn run_subtasks(
    subtasks: &[String],
    model: Option<String>,
    yolo: bool,
) -> Result<Vec<String>> {
    let mut handles = Vec::new();
    for subtask in subtasks {
        let subtask = subtask.clone();
        let model = model.clone();
        handles.push(tokio::spawn(async move {
            exec_plain(&subtask, model, yolo).await
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        match h.await? {
            Ok(text) => results.push(text),
            Err(e) => eprintln!("warning: subtask failed: {e}"),
        }
    }

    if results.is_empty() {
        bail!("all subtasks failed");
    }
    Ok(results)
}

pub(crate) fn build_split_prompt(count: usize, prompt: &str) -> String {
    format!(
        "Split the following task into exactly {count} self-contained subtasks \
         that can each be solved independently. Return ONLY a JSON array of strings, \
         with no markdown, no explanation, no code fences.\n\nExample: [\"subtask 1\", \"subtask 2\"]\n\nTask: {prompt}"
    )
}

pub(crate) fn build_combine_prompt(results: &[String], original: &str) -> String {
    let mut prompt =
        "Combine the following subtask results into a single coherent final answer.\n\nResults:\n"
            .to_string();
    for (i, r) in results.iter().enumerate() {
        prompt.push_str(&format!("{}. {r}\n", i + 1));
    }
    prompt.push_str(&format!("\nOriginal task: {original}"));
    prompt
}

pub(crate) fn parse_subtasks(raw: &str) -> Option<Vec<String>> {
    let trimmed = raw.trim();

    fn try_array(s: &str) -> Option<Vec<String>> {
        serde_json::from_str::<Vec<String>>(s)
            .ok()
            .filter(|a| !a.is_empty())
    }

    if let Some(arr) = try_array(trimmed) {
        return Some(arr);
    }

    // If the response is a JSON object, it's not a subtask list.
    if serde_json::from_str::<serde_json::Value>(trimmed)
        .is_ok_and(|v| v.is_object() || v.is_array())
    {
        return None;
    }

    let mut start = None;
    let mut end = None;
    for (idx, _) in trimmed.match_indices("```") {
        if start.is_none() {
            start = Some(idx);
        } else if end.is_none() {
            end = Some(idx);
            break;
        }
    }
    if let (Some(s), Some(e)) = (start, end) {
        let inner = trimmed[s + 3..e].trim();
        let inner = inner.strip_prefix("json").unwrap_or(inner).trim();
        if let Some(arr) = try_array(inner) {
            return Some(arr);
        }
        if serde_json::from_str::<serde_json::Value>(inner).is_ok_and(|v| v.is_object()) {
            return None;
        }
    }

    if let Some(s) = trimmed.find('[')
        && let Some(e) = trimmed.rfind(']')
        && s < e
    {
        let candidate = &trimmed[s..=e];
        if let Some(arr) = try_array(candidate) {
            return Some(arr);
        }
    }

    // Fallback: numbered list like "1. task one\n2. task two".
    let numbered: Vec<String> = trimmed
        .lines()
        .filter_map(|line| {
            let s = line.trim();
            let rest = s
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(['.', ')'])
                .trim_start();
            if rest.len() < s.len() && !rest.is_empty() {
                Some(rest.to_string())
            } else {
                None
            }
        })
        .collect();
    if !numbered.is_empty() && numbered.len() <= 20 {
        return Some(numbered);
    }

    None
}

fn swarm_vote(outputs: &[String]) -> String {
    let mut best = String::new();
    let mut best_count = 0usize;
    let mut best_index = usize::MAX;
    let mut seen: std::collections::HashMap<String, (usize, usize, String)> =
        std::collections::HashMap::new();

    for (i, o) in outputs.iter().enumerate() {
        let key = o.trim().to_string();
        let entry = seen.entry(key).or_insert_with(|| (0, i, o.clone()));
        entry.0 += 1;

        let (count, idx, original) = (entry.0, entry.1, &entry.2);
        if count > best_count || (count == best_count && idx < best_index) {
            best_count = count;
            best_index = idx;
            best = original.clone();
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_prompt_contains_count_and_task() {
        let p = build_split_prompt(3, "write a poem");
        assert!(p.contains("exactly 3"));
        assert!(p.contains("write a poem"));
        assert!(p.contains("JSON array of strings"));
    }

    #[test]
    fn combine_prompt_includes_results_and_original() {
        let p = build_combine_prompt(&["res1".into(), "res2".into()], "orig");
        assert!(p.contains("res1"));
        assert!(p.contains("res2"));
        assert!(p.contains("Original task: orig"));
    }

    #[test]
    fn parse_subtasks_valid_array() {
        let raw = r#"["a", "b", "c"]"#;
        assert_eq!(parse_subtasks(raw).unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn parse_subtasks_markdown_fence() {
        let raw = "```json\n[\"x\", \"y\"]\n```";
        assert_eq!(parse_subtasks(raw).unwrap(), vec!["x", "y"]);
    }

    #[test]
    fn parse_subtasks_extracts_array_from_text() {
        let raw = "Here is the split: [\"one\", \"two\"] done";
        assert_eq!(parse_subtasks(raw).unwrap(), vec!["one", "two"]);
    }

    #[test]
    fn parse_subtasks_fallback_on_empty_or_invalid() {
        assert!(parse_subtasks("[]").is_none());
        assert!(parse_subtasks("not json").is_none());
        assert!(parse_subtasks("{\"foo\": [\"a\"]}").is_none());
    }

    #[test]
    fn parse_subtasks_numbered_list() {
        let raw = "1. first task\n2) second task\n3. third task";
        assert_eq!(
            parse_subtasks(raw).unwrap(),
            vec!["first task", "second task", "third task"]
        );
    }

    #[test]
    fn swarm_vote_picks_majority() {
        let outputs = vec!["a".into(), "b".into(), "a".into(), "c".into()];
        assert_eq!(swarm_vote(&outputs), "a");
    }

    #[test]
    fn run_swarm_task_splitting_signature_compiles() {
        // Constructing the future does not perform I/O; it only type-checks the public API.
        let _fut = run_swarm_task_splitting("test", None, false, 2);
        drop(_fut);
    }
}
