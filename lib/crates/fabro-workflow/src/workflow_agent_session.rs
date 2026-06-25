use crate::stage_scope::StageScope;

pub(crate) fn session_id(
    run_id: &str,
    node_id: &str,
    visit: u32,
    backend: &str,
    parallel_group_id: Option<&str>,
    parallel_branch_id: Option<&str>,
) -> String {
    let mut id = format!(
        "workflow-{}-{}-{}-v{}",
        safe_segment(backend),
        safe_segment(run_id),
        safe_segment(node_id),
        visit
    );
    if let Some(group) = parallel_group_id {
        id.push_str("-pg");
        id.push_str(&safe_segment(group));
    }
    if let Some(branch) = parallel_branch_id {
        id.push_str("-pb");
        id.push_str(&safe_segment(branch));
    }
    id
}

pub(crate) fn session_id_for_scope(run_id: &str, scope: &StageScope, backend: &str) -> String {
    session_id(
        run_id,
        &scope.node_id,
        scope.visit,
        backend,
        scope.parallel_group_id.as_ref().map(ToString::to_string).as_deref(),
        scope
            .parallel_branch_id
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
    )
}

fn safe_segment(value: &str) -> String {
    let safe: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = safe.trim_matches('-');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_path_safe_and_distinct_by_visit() {
        assert_eq!(
            session_id("run/1", "verify node", 2, "tmux", None, None),
            "workflow-tmux-run-1-verify-node-v2"
        );
    }

    #[test]
    fn session_id_includes_branch_identity_when_present() {
        assert_eq!(
            session_id("r", "n", 1, "tmux", Some("group/a"), Some("branch b")),
            "workflow-tmux-r-n-v1-pggroup-a-pbbranch-b"
        );
    }
}
