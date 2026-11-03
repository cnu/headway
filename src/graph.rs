//! Lays out the commit graph: which column each commit sits in and which lines
//! pass through each row. Lanes never shift sideways, so lines stay straight.

use crate::git::Commit;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub col: usize,
    /// Lines from the top edge of the row: (from column, to column at the dot or bottom).
    pub top: Vec<(usize, Target)>,
    /// Lines from the dot to the bottom edge: column at bottom.
    pub to_parents: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
pub enum Target {
    /// Ends at this row's dot.
    Dot,
    /// Passes straight through to the bottom of the row in the same column.
    Through,
}

pub struct Graph {
    pub rows: Vec<Row>,
    pub width: usize,
}

pub fn layout(commits: &[Commit]) -> Graph {
    let known: HashMap<&str, ()> = commits.iter().map(|c| (c.hash.as_str(), ())).collect();
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    let mut width = 1;

    for c in commits {
        // Column: the first lane waiting for this commit, otherwise a free slot.
        let col = match lanes.iter().position(|l| l.as_deref() == Some(&c.hash)) {
            Some(i) => i,
            None => alloc(&mut lanes),
        };

        let mut top = Vec::new();
        for (i, l) in lanes.iter().enumerate() {
            if let Some(h) = l {
                if *h == c.hash {
                    top.push((i, Target::Dot));
                } else {
                    top.push((i, Target::Through));
                }
            }
        }
        // Lanes that were waiting for this commit end here.
        for l in lanes.iter_mut() {
            if l.as_deref() == Some(&c.hash) {
                *l = None;
            }
        }

        let mut to_parents = Vec::new();
        let parents: Vec<&String> = c.parents.iter().filter(|p| known.contains_key(p.as_str())).collect();
        for (pi, p) in parents.iter().enumerate() {
            let existing = lanes.iter().position(|l| l.as_deref() == Some(p.as_str()));
            let target = match existing {
                Some(i) => i,
                None => {
                    if pi == 0 && lanes[col].is_none() {
                        lanes[col] = Some((*p).clone());
                        col
                    } else {
                        let i = alloc(&mut lanes);
                        lanes[i] = Some((*p).clone());
                        i
                    }
                }
            };
            to_parents.push(target);
        }

        while lanes.last().is_some_and(|l| l.is_none()) {
            lanes.pop();
        }
        width = width.max(lanes.len()).max(col + 1);
        rows.push(Row { col, top, to_parents });
    }
    Graph { rows, width }
}

fn alloc(lanes: &mut Vec<Option<String>>) -> usize {
    match lanes.iter().position(|l| l.is_none()) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Commits listed newest first, each as (id, parent ids).
    fn commits(list: &[(&str, &[&str])]) -> Vec<Commit> {
        list.iter()
            .map(|(h, ps)| Commit {
                hash: h.to_string(),
                parents: ps.iter().map(|p| p.to_string()).collect(),
                author: String::new(),
                email: String::new(),
                time: 0,
                summary: String::new(),
                refs: vec![],
            })
            .collect()
    }

    #[test]
    fn straight_history_uses_one_lane() {
        let g = layout(&commits(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]));
        assert_eq!(g.width, 1);
        assert!(g.rows.iter().all(|r| r.col == 0));
        assert_eq!(g.rows[0].to_parents, [0]);
        assert!(g.rows[0].top.is_empty());
        assert!(g.rows[2].to_parents.is_empty());
    }

    #[test]
    fn two_branch_tips_get_their_own_lanes() {
        // x and y both grew from a.
        let g = layout(&commits(&[("x", &["a"]), ("y", &["a"]), ("a", &[])]));
        assert_eq!(g.width, 2);
        assert_eq!(g.rows[0].col, 0);
        assert_eq!(g.rows[1].col, 1);
        // Row y: lane 0 (waiting for a) passes straight through.
        assert!(matches!(g.rows[1].top[..], [(0, Target::Through)]));
        // y's line joins the lane already heading to a.
        assert_eq!(g.rows[1].to_parents, [0]);
    }

    #[test]
    fn merge_opens_a_second_lane_for_the_other_parent() {
        let g = layout(&commits(&[("m", &["b", "s"]), ("s", &["a"]), ("b", &["a"]), ("a", &[])]));
        assert_eq!(g.rows[0].to_parents, [0, 1]);
        assert_eq!(g.rows[1].col, 1);
        assert_eq!(g.width, 2);
    }

    #[test]
    fn parents_outside_the_loaded_history_are_ignored() {
        // Only the newest commit is loaded; its parent isn't.
        let g = layout(&commits(&[("c", &["not-loaded"])]));
        assert!(g.rows[0].to_parents.is_empty());
        assert_eq!(g.width, 1);
    }

    #[test]
    fn octopus_merge_spreads_over_several_lanes() {
        let g = layout(&commits(&[("m", &["a", "b", "c"]), ("a", &[]), ("b", &[]), ("c", &[])]));
        assert_eq!(g.rows[0].to_parents, [0, 1, 2]);
        assert_eq!(g.width, 3);
        assert_eq!(g.rows.iter().map(|r| r.col).collect::<Vec<_>>(), [0, 0, 1, 2]);
    }

    #[test]
    fn freed_lanes_are_reused() {
        // After branch tip t ends at a, a later tip should reuse a free lane, not add one.
        let g = layout(&commits(&[("t", &["a"]), ("u", &["a"]), ("a", &["z"]), ("v", &["z"]), ("z", &[])]));
        assert_eq!(g.width, 2);
    }
}
