//! Conversation tree algorithms for the Liberado TUI.
//!
//! Pure functions that build and flatten a conversation tree from `ConvHeader` data.
//! Separated from `app.rs` to keep the App struct focused on state management.

use std::collections::HashSet;

use crate::api::ConvHeader;
use crate::app::VisibleNode;

struct TreeNode {
    header: ConvHeader,
    children: Vec<TreeNode>,
}

fn tree_from_headers(convs: &[ConvHeader], parent_id: Option<&str>) -> Vec<TreeNode> {
    convs
        .iter()
        .filter(|c| c.parent_conversation.as_deref() == parent_id)
        .map(|c| TreeNode {
            header: c.clone(),
            children: tree_from_headers(convs, Some(&c.id)),
        })
        .collect()
}

fn flatten(
    nodes: &[TreeNode],
    collapsed: &HashSet<String>,
    depth: usize,
    ancestors_last: &[bool],
    out: &mut Vec<VisibleNode>,
) {
    for (i, node) in nodes.iter().enumerate() {
        let is_last = i == nodes.len() - 1;
        let has_children = !node.children.is_empty();
        let is_collapsed = collapsed.contains(&node.header.id);
        out.push(VisibleNode {
            header: node.header.clone(),
            depth,
            is_last,
            has_children,
            collapsed: is_collapsed,
            ancestors_last: ancestors_last.to_vec(),
        });
        if has_children && !is_collapsed {
            let mut next = ancestors_last.to_vec();
            next.push(is_last);
            flatten(&node.children, collapsed, depth + 1, &next, out);
        }
    }
}

pub fn visible_tree(
    convs: &[ConvHeader],
    collapsed: &HashSet<String>,
    filter: &str,
) -> Vec<VisibleNode> {
    let roots = tree_from_headers(convs, None);
    let mut out = Vec::new();
    flatten(&roots, collapsed, 0, &[], &mut out);
    if filter.is_empty() {
        return out;
    }
    let lower = filter.to_lowercase();
    out.into_iter()
        .filter(|n| n.header.title.to_lowercase().contains(&lower))
        .collect()
}

pub fn filtered_list<'a>(convs: &'a [ConvHeader], filter: &str) -> Vec<&'a ConvHeader> {
    if filter.is_empty() {
        return convs.iter().collect();
    }
    let lower = filter.to_lowercase();
    convs
        .iter()
        .filter(|c| c.title.to_lowercase().contains(&lower))
        .collect()
}
