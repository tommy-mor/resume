//! Pair choosing for the resume sorter, ported from
//! `~/programming/muse-tricks/src/pair.rs` (which itself preserves the
//! `scope_rank.rs` pool heart): degree tracking so no item is starred,
//! bridging-first when the vote graph is disconnected, overlap
//! deprioritization so the same item is not re-dealt immediately.
//!
//! The sorter calls [`suggest_next_pair`] repeatedly; each chosen pair is
//! judged once by the OpenRouter Decisions model, then recorded in the
//! `voted` set. Dealing order never affects correctness of the final
//! ranking (the random walk consumes the whole edge set), but
//! bridging-first guarantees the graph becomes connected in the minimum
//! `components - 1` votes.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// Canonical unordered pair `(lo, hi)`. The voted set holds these.
fn canon(a: u32, b: u32) -> (u32, u32) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Connected-component ids for the `0..n` pool, joined only by voted pairs.
/// Never-voted items are singleton components. One id per index.
pub fn component_ids(n: u32, voted: &HashSet<(u32, u32)>) -> Vec<usize> {
    let n = n as usize;
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            if ra < rb {
                parent[rb] = ra;
            } else {
                parent[ra] = rb;
            }
        }
    }
    for &(a, b) in voted {
        let (a, b) = (a as usize, b as usize);
        if a < n && b < n && a != b {
            union(&mut parent, a, b);
        }
    }
    let mut root_to_id: HashMap<usize, usize> = HashMap::new();
    let mut out = vec![0usize; n];
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        let r = find(&mut parent, i);
        let next = root_to_id.len();
        *slot = *root_to_id.entry(r).or_insert(next);
    }
    out
}

/// Deal the next pair to judge: bridging-first across components, else
/// lowest-degree-first, never voted, never `excluded`. `current` is the
/// pair just judged — sharing its items costs overlap. Returns `None`
/// when every pair is voted/excluded (tournament complete).
///
/// The sorter never has pending pairs (judgment is immediate), so it
/// passes an empty `pending` set; the parameter is kept for parity with
/// muse-tricks so the deleted/pending semantics stay visible.
pub fn suggest_next_pair(
    n: u32,
    voted: &HashSet<(u32, u32)>,
    pending: &HashSet<(u32, u32)>,
    current: Option<(u32, u32)>,
    excluded: Option<&HashSet<(u32, u32)>>,
) -> Option<(u32, u32)> {
    let n_usize = n as usize;
    if n < 2 {
        return None;
    }
    let current = current.map(|(a, b)| canon(a, b));
    let is_excluded = |pair: &(u32, u32)| excluded.is_some_and(|s| s.contains(pair));
    let is_open = |a: u32, b: u32| {
        let p = canon(a, b);
        voted.contains(&p) || pending.contains(&p)
    };

    // Degree inside the pool: how many voted opponents each item already has.
    // A lex-first unvoted scan would star the earliest item until all of its
    // pairs were judged; degree preference spreads the love instead.
    let mut degree = vec![0usize; n_usize];
    for &(a, b) in voted {
        let (a, b) = (a as usize, b as usize);
        if a < n_usize && b < n_usize && a != b {
            degree[a] += 1;
            degree[b] += 1;
        }
    }
    let overlap = |a: u32, b: u32| match &current {
        Some((x, y)) => u8::from(a == *x || a == *y) + u8::from(b == *x || b == *y),
        None => 0,
    };

    // When the pool subgraph is disconnected, a vote inside one component
    // can never connect it to the others. Prefer a bridging pair first: any
    // single cross-component vote merges two components. Among bridges,
    // attach to the largest components first (a singleton joins the main
    // ranking rather than pairing singletons together), then keep the
    // low-degree preference so one item is not starred.
    let comp = component_ids(n, voted);
    let num_components = comp.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    if num_components > 1 {
        let mut comp_size = vec![0usize; num_components];
        for &c in &comp {
            comp_size[c] += 1;
        }
        let mut best: Option<(u8, Reverse<usize>, usize, usize, u32, u32)> = None;
        for a in 0..n {
            for b in (a + 1)..n {
                if comp[a as usize] == comp[b as usize] {
                    continue;
                }
                let pair = canon(a, b);
                if current.as_ref() == Some(&pair) || is_excluded(&pair) {
                    continue;
                }
                if is_open(a, b) {
                    continue;
                }
                let size_sum = comp_size[comp[a as usize]] + comp_size[comp[b as usize]];
                let key = (
                    overlap(a, b),
                    Reverse(size_sum),
                    degree[a as usize].max(degree[b as usize]),
                    degree[a as usize] + degree[b as usize],
                    a,
                    b,
                );
                if best.as_ref().is_none_or(|best| key < *best) {
                    best = Some(key);
                }
            }
        }
        if let Some((_, _, _, _, a, b)) = best {
            return Some((a, b));
        }
    }

    let mut best: Option<(u8, usize, usize, u32, u32)> = None;
    for a in 0..n {
        for b in (a + 1)..n {
            let pair = canon(a, b);
            if current.as_ref() == Some(&pair) || is_excluded(&pair) {
                continue;
            }
            if is_open(a, b) {
                continue;
            }
            let key = (
                overlap(a, b),
                degree[a as usize].max(degree[b as usize]),
                degree[a as usize] + degree[b as usize],
                a,
                b,
            );
            if best.as_ref().is_none_or(|best| key < *best) {
                best = Some(key);
            }
        }
    }
    best.map(|(_, _, _, a, b)| (a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voted(pairs: &[(u32, u32)]) -> HashSet<(u32, u32)> {
        pairs.iter().map(|&(a, b)| canon(a, b)).collect()
    }

    #[test]
    fn bridges_disconnected_graph_first() {
        let v = voted(&[(0, 1), (1, 2), (3, 4)]);
        let next = suggest_next_pair(5, &v, &HashSet::new(), None, None).unwrap();
        let in_abc = |x: u32| x <= 2;
        assert!((in_abc(next.0) && !in_abc(next.1)) || (!in_abc(next.0) && in_abc(next.1)));
    }

    #[test]
    fn does_not_star_first_item() {
        let v = voted(&[(0, 1), (0, 2), (0, 3)]);
        let next = suggest_next_pair(4, &v, &HashSet::new(), Some((0, 1)), None).unwrap();
        assert_ne!(next.0, 0);
        assert_ne!(next.1, 0);
    }

    #[test]
    fn none_when_tournament_done() {
        let v = voted(&[(0, 1), (0, 2), (1, 2)]);
        assert_eq!(suggest_next_pair(3, &v, &HashSet::new(), None, None), None);
    }
}
