//! Ranking math: normalized pairwise random-walk, ported from
//! `~/programming/muse-tricks/src/ranking.rs` (itself from `ranking.rs`).
//!
//! Inputs are directed edges `(src, dst, weight)` in muse-tricks
//! orientation: an edge flows **loser -> winner** with weight equal to the
//! winner's share, and winner -> loser with the loser's share. A vote for
//! A over B with probabilities `(0.9, 0.1)` therefore contributes
//! `(B, A, 0.9)` and `(A, B, 0.1)`. [`compute_scores_from_edges`] iterates
//! a random walk with self-loop stay probability until it settles; the
//! winner collects the loser's mass and rises to the top. Verified by the
//! `winner_ranks_first` test below.
//!
//! Scores are only comparable within one connected component, so
//! [`rank_groups`] ranks each component on its own induced subgraph. The
//! sorter requires a single component (enforced in `main.rs`): a capped
//! `--max-votes` run that leaves the graph disconnected is an error, not
//! a silent partial ranking.

use std::collections::{HashMap, HashSet};

pub const MAX_ITERS: usize = 10_000;
pub const TOL: f64 = 1e-8;

pub fn compute_scores_from_edges(
    n: usize,
    edges: impl Iterator<Item = ((usize, usize), f64)>,
    max_iters: usize,
    tol: f64,
) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![1.0];
    }

    let mut raw: HashMap<(usize, usize), f64> = HashMap::new();
    for ((src, dst), w) in edges {
        if src >= n || dst >= n || w <= 0.0 {
            continue;
        }
        *raw.entry((src, dst)).or_insert(0.0) += w;
    }

    let keys: Vec<(usize, usize)> = raw.keys().copied().collect();
    let mut normalized: HashMap<(usize, usize), f64> = HashMap::new();
    for (i, j) in keys {
        if normalized.contains_key(&(i, j)) {
            continue;
        }
        let w_ij = *raw.get(&(i, j)).unwrap_or(&0.0);
        let w_ji = *raw.get(&(j, i)).unwrap_or(&0.0);
        let total = w_ij + w_ji;
        if total <= 0.0 {
            continue;
        }
        normalized.insert((i, j), w_ij / total);
        if w_ji > 0.0 {
            normalized.insert((j, i), w_ji / total);
        }
    }

    let mut out_edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut neighbors: Vec<HashSet<usize>> = vec![HashSet::new(); n];

    for ((src, dst), w) in &normalized {
        out_edges[*src].push((*dst, *w));
        neighbors[*src].insert(*dst);
        neighbors[*dst].insert(*src);
    }

    let weight_sum: Vec<f64> = out_edges
        .iter()
        .map(|es| es.iter().map(|(_, w)| *w).sum())
        .collect();
    let d_max = neighbors.iter().map(|s| s.len()).max().unwrap_or(0);
    if d_max == 0 {
        return vec![1.0 / n as f64; n];
    }
    let d_max_f = d_max as f64;

    let mut scores = vec![1.0 / n as f64; n];
    let mut next = vec![0.0f64; n];

    for _ in 0..max_iters {
        next.fill(0.0);
        for i in 0..n {
            let stay_prob = (d_max_f - weight_sum[i]) / d_max_f;
            next[i] += scores[i] * stay_prob;

            if out_edges[i].is_empty() {
                continue;
            }
            for &(dst, w) in &out_edges[i] {
                next[dst] += scores[i] * (w / d_max_f);
            }
        }

        let diff: f64 = scores
            .iter()
            .zip(next.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();

        scores.clone_from_slice(&next);
        if diff < tol {
            break;
        }
    }

    let sum: f64 = scores.iter().sum();
    if sum.is_finite() && sum > 0.0 {
        for s in &mut scores {
            *s /= sum;
        }
    }
    scores
}

/// One connected hunk: its members, their intra-component ranks, and how
/// many voted pairs hold it together.
///
/// `members`/`pairs` are kept for parity with muse-tricks (callers may
/// inspect connectivity); the sorter currently reads only `ranked`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RankGroup {
    /// Indices into the full `0..n` pool, sorted ascending.
    pub members: Vec<usize>,
    /// (index, score) sorted best-first, scores renormalized within group.
    pub ranked: Vec<(usize, f64)>,
    /// Voted pairs with both ends inside this group.
    pub pairs: usize,
}

/// Split `0..n` into connected groups by voted pairs, rank each on its own
/// induced subgraph, biggest group first. Singletons come last as
/// one-member groups with score 1.0; zero-member pools give nothing.
/// `edges` are full-pool directed edges; only intra-group edges count.
pub fn rank_groups(
    n: usize,
    voted: &HashSet<(usize, usize)>,
    edges: &[(usize, usize, f64)],
    max_iters: usize,
    tol: f64,
) -> Vec<RankGroup> {
    if n == 0 {
        return vec![];
    }
    // Union-find over voted pairs: the same components the dealer bridges.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for &(a, b) in voted {
        if a < n && b < n && a != b {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                if ra < rb {
                    parent[rb] = ra;
                } else {
                    parent[ra] = rb;
                }
            }
        }
    }
    let mut root_to_group: HashMap<usize, usize> = HashMap::new();
    let mut members: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        let next = members.len();
        let gi = *root_to_group.entry(r).or_insert(next);
        if gi == members.len() {
            members.push(Vec::new());
        }
        members[gi].push(i);
    }
    for m in &mut members {
        m.sort();
    }
    members.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    members
        .into_iter()
        .map(|mem| {
            let map: HashMap<usize, usize> =
                mem.iter().enumerate().map(|(li, &gi)| (gi, li)).collect();
            let local_edges = edges
                .iter()
                .filter_map(|&(s, d, w)| Some(((*map.get(&s)?, *map.get(&d)?), w)));
            let local = compute_scores_from_edges(mem.len(), local_edges, max_iters, tol);
            let mut ranked: Vec<(usize, f64)> = mem
                .iter()
                .enumerate()
                .map(|(li, &gi)| (gi, local.get(li).copied().unwrap_or(0.0)))
                .collect();
            ranked.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            let pairs = voted
                .iter()
                .filter(|&&(a, b)| a < n && b < n && map.contains_key(&a) && map.contains_key(&b))
                .count();
            RankGroup {
                members: mem,
                ranked,
                pairs,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hs(pairs: &[(usize, usize)]) -> HashSet<(usize, usize)> {
        pairs
            .iter()
            .map(|&(a, b)| if a <= b { (a, b) } else { (b, a) })
            .collect()
    }

    #[test]
    fn winner_ranks_first() {
        // Item 0 beats item 1 with probabilities (0.9, 0.1): edges flow
        // loser->winner weighted by the winner's share.
        let v = hs(&[(0, 1)]);
        let edges = vec![(1, 0, 0.9), (0, 1, 0.1)];
        let groups = rank_groups(2, &v, &edges, MAX_ITERS, TOL);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].ranked[0].0, 0, "winner must rank first: {:?}", groups[0].ranked);
        assert!(groups[0].ranked[0].1 > groups[0].ranked[1].1);
    }

    #[test]
    fn three_way_chain_orders_correctly() {
        // 2 beats 1 (2:1) and 1 beats 0 (2:1) -> 2 > 1 > 0. Edges flow
        // loser -> winner weighted by the winner's share.
        let v = hs(&[(0, 1), (1, 2)]);
        let edges = vec![
            (0, 1, 0.67),
            (1, 0, 0.33),
            (1, 2, 0.67),
            (2, 1, 0.33),
        ];
        let groups = rank_groups(3, &v, &edges, MAX_ITERS, TOL);
        let order: Vec<usize> = groups[0].ranked.iter().map(|&(i, _)| i).collect();
        assert_eq!(order, vec![2, 1, 0]);
    }

    #[test]
    fn groups_split_disconnected_graph() {
        let v = hs(&[(0, 1), (2, 3)]);
        let edges = vec![(1, 0, 0.75), (0, 1, 0.25), (3, 2, 0.75), (2, 3, 0.25)];
        let groups = rank_groups(5, &v, &edges, MAX_ITERS, TOL);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[2].members, vec![4]);
    }
}
