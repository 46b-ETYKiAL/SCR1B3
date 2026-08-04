//! Nested `#tag` tree over a vault's notes — the click-to-filter tag surface.
//!
//! [`build`] turns the per-note tag sets of a vault into ONE flattened,
//! depth-annotated list of [`TagNode`]s in pre-order (a parent is immediately
//! followed by its whole subtree), so a UI renders an indented tree with a plain
//! loop and no recursion.
//!
//! Three properties make the tree agree with the `tag:` search operator exactly,
//! which is what lets "click a node" be implemented as "set the filter to
//! `tag:<node>`" rather than as a second, drifting selection rule:
//!
//! - **Ancestors are materialised.** A vault that only ever writes
//!   `#project/frontend` still gets a `project` node (via
//!   [`super::tags::with_ancestors`]) — because `tag:project` DOES match that
//!   note, so a tree without the parent would hide a reachable filter.
//! - **A node's count is descendant-INCLUSIVE**, computed with the very same
//!   [`super::tags::tag_matches`] relation the `tag:` operator uses. The number
//!   beside a node is therefore exactly how many notes clicking it selects.
//! - **Case is folded**, mirroring the case-insensitive `tag:` operator, so
//!   `#Project` and `#project` are ONE node that selects both notes rather than
//!   two nodes that each select both.
//!
//! Pure — no I/O, no allocation beyond the returned tree.

use super::tags::{tag_matches, with_ancestors};

/// One row of the flattened tag tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagNode {
    /// The full `/`-joined tag path, lowercased (`project/frontend`). This is
    /// what a `tag:` filter is built from.
    pub tag: String,
    /// The last `/`-segment — what an indented tree row displays (`frontend`).
    pub segment: String,
    /// Nesting depth: `0` for a root tag, `1` for its child, and so on.
    pub depth: usize,
    /// How many notes carry this tag OR any descendant of it.
    pub note_count: usize,
}

/// Build the flattened, depth-annotated tag tree for a vault.
///
/// `note_tags` yields one slice per note — that note's tag set WITHOUT the
/// leading `#` (i.e. exactly what `notes::meta::note_tags` produces). Notes with
/// no tags contribute nothing but still count toward nothing, so passing the
/// whole index is correct.
///
/// The result is sorted in pre-order by `/`-separated SEGMENTS, not by raw
/// string: a sibling like `project-old` sorts between `project` and
/// `project/frontend` under plain lexicographic order (`-` < `/`), which would
/// split a parent's subtree in half and break the indentation.
#[must_use]
pub fn build<'a, I>(note_tags: I) -> Vec<TagNode>
where
    I: IntoIterator<Item = &'a [String]>,
{
    // Lowercase once, up front: every later comparison (ancestor expansion,
    // descendant counting, node identity) then agrees with the case-insensitive
    // `tag:` operator without re-folding at each site.
    let per_note: Vec<Vec<String>> = note_tags
        .into_iter()
        .map(|tags| tags.iter().map(|t| t.to_lowercase()).collect())
        .collect();
    let flat: Vec<String> = per_note.iter().flatten().cloned().collect();

    let mut tags = with_ancestors(flat.iter());
    tags.sort_by(|a, b| segments(a).cmp(&segments(b)));

    tags.into_iter()
        .map(|tag| {
            let note_count = per_note
                .iter()
                .filter(|note| note.iter().any(|t| tag_matches(t, &tag)))
                .count();
            let depth = tag.matches('/').count();
            let segment = tag.rsplit('/').next().unwrap_or(tag.as_str()).to_string();
            TagNode {
                tag,
                segment,
                depth,
                note_count,
            }
        })
        .collect()
}

/// The `/`-separated segments of `tag`, the pre-order sort key. Comparing
/// segment vectors puts a parent immediately before its children (a shorter
/// vector that is a prefix sorts first) while keeping an unrelated sibling that
/// merely shares a character prefix outside the subtree.
fn segments(tag: &str) -> Vec<&str> {
    tag.split('/').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build from string literals, one `&str` list per note.
    fn tree(notes: &[&[&str]]) -> Vec<TagNode> {
        let owned: Vec<Vec<String>> = notes
            .iter()
            .map(|n| n.iter().map(|s| (*s).to_string()).collect())
            .collect();
        build(owned.iter().map(Vec::as_slice))
    }

    fn tags_of(nodes: &[TagNode]) -> Vec<&str> {
        nodes.iter().map(|n| n.tag.as_str()).collect()
    }

    #[test]
    fn an_empty_vault_has_no_tree() {
        assert!(tree(&[]).is_empty());
        assert!(
            tree(&[&[], &[]]).is_empty(),
            "untagged notes contribute none"
        );
    }

    #[test]
    fn a_parent_node_exists_even_when_only_the_leaf_was_ever_written() {
        // The note only ever writes `#project/frontend`, but `tag:project`
        // matches it — so the tree MUST offer the parent, or that reachable
        // filter is invisible in the UI.
        let nodes = tree(&[&["project/frontend"]]);
        assert_eq!(tags_of(&nodes), vec!["project", "project/frontend"]);
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[0].segment, "project");
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(
            nodes[1].segment, "frontend",
            "a row displays its own segment, not the whole path"
        );
    }

    #[test]
    fn a_parent_counts_every_descendant_note_not_just_its_own() {
        // The count beside a node is a PROMISE about what clicking it selects.
        // Counting only exact-tag notes would say "1" and then select 2.
        let nodes = tree(&[
            &["project/frontend"],
            &["project/backend"],
            &["project"],
            &["idea"],
        ]);
        let by_tag = |t: &str| {
            nodes
                .iter()
                .find(|n| n.tag == t)
                .unwrap_or_else(|| panic!("node {t} missing"))
                .note_count
        };
        assert_eq!(by_tag("project"), 3, "the exact note plus both descendants");
        assert_eq!(by_tag("project/frontend"), 1);
        assert_eq!(by_tag("project/backend"), 1);
        assert_eq!(by_tag("idea"), 1);
    }

    #[test]
    fn a_note_carrying_two_descendants_is_counted_once_by_the_parent() {
        // Counting tag OCCURRENCES rather than NOTES would say 2 here and the
        // number would not match the filtered list length.
        let nodes = tree(&[&["project/frontend", "project/backend"]]);
        assert_eq!(
            nodes
                .iter()
                .find(|n| n.tag == "project")
                .expect("parent node")
                .note_count,
            1,
            "one note, however many of its tags sit under the parent"
        );
    }

    #[test]
    fn a_subtree_stays_contiguous_past_a_sibling_that_sorts_between_it_and_its_parent() {
        // `-` (0x2D) sorts BEFORE `/` (0x2F), so plain lexicographic order is
        // [project, project-old, project/frontend] — the parent separated from
        // its child by an unrelated root, which breaks the indented render.
        // Segment-wise ordering keeps the subtree together.
        let nodes = tree(&[&["project"], &["project-old"], &["project/frontend"]]);
        assert_eq!(
            tags_of(&nodes),
            vec!["project", "project/frontend", "project-old"],
            "pre-order: a parent is immediately followed by its whole subtree"
        );
        assert_eq!(
            nodes[2].depth, 0,
            "`project-old` is a ROOT tag, not a child of `project`"
        );
    }

    #[test]
    fn deep_nesting_reports_its_real_depth() {
        let nodes = tree(&[&["a/b/c/d"]]);
        assert_eq!(tags_of(&nodes), vec!["a", "a/b", "a/b/c", "a/b/c/d"]);
        assert_eq!(
            nodes.iter().map(|n| n.depth).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            nodes.iter().map(|n| n.segment.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
    }

    #[test]
    fn case_is_folded_into_one_node_that_selects_both_spellings() {
        // The `tag:` operator is case-insensitive, so two spellings must not
        // become two nodes that each claim (and select) both notes.
        let nodes = tree(&[&["Project"], &["project"]]);
        assert_eq!(tags_of(&nodes), vec!["project"], "one folded node");
        assert_eq!(nodes[0].note_count, 2, "and it selects both notes");
    }

    #[test]
    fn an_unrelated_tag_sharing_a_prefix_is_not_counted_as_a_descendant() {
        // `projectx` starts with `project` but is NOT under it — the count must
        // use the `/`-boundary relation, not a bare `starts_with`.
        let nodes = tree(&[&["project"], &["projectx"]]);
        assert_eq!(tags_of(&nodes), vec!["project", "projectx"]);
        assert_eq!(nodes[0].note_count, 1, "projectx is a different root tag");
        assert_eq!(nodes[1].note_count, 1);
        assert_eq!(nodes[1].depth, 0);
    }
}
