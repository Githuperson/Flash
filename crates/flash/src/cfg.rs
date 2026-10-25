//! Control-flow graph utilities for the optimiser: predecessors, reverse post-order, dominators.

use crate::ir::*;

pub struct Cfg {
    pub preds: Vec<Vec<usize>>,
    pub succs: Vec<Vec<usize>>,
    /// Reachable blocks in reverse post-order.
    pub rpo: Vec<usize>,
    /// Position in `rpo`, or `usize::MAX` for unreachable blocks.
    pub rpo_index: Vec<usize>,
    /// Immediate dominator; the entry block and unreachable blocks have `usize::MAX`.
    pub idom: Vec<usize>,
    /// Dominator-tree children.
    pub children: Vec<Vec<usize>>,
}

pub fn successors(t: &Term) -> Vec<usize> {
    match t {
        Term::Jump(b) => vec![*b as usize],
        Term::Br { then_, else_, .. } => vec![*then_ as usize, *else_ as usize],
        _ => vec![],
    }
}

pub fn build(f: &Function) -> Cfg {
    let n = f.blocks.len();
    let succs: Vec<Vec<usize>> = f.blocks.iter().map(|b| successors(&b.term)).collect();
    // Post-order by iterative DFS.
    let mut seen = vec![false; n];
    let mut post = vec![];
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    seen[0] = true;
    while let Some((b, i)) = stack.pop() {
        if i < succs[b].len() {
            stack.push((b, i + 1));
            let s = succs[b][i];
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    let rpo = post;
    let mut rpo_index = vec![usize::MAX; n];
    for (i, b) in rpo.iter().enumerate() {
        rpo_index[*b] = i;
    }
    let mut preds = vec![vec![]; n];
    for &b in &rpo {
        for &s in &succs[b] {
            if !preds[s].contains(&b) {
                preds[s].push(b);
            }
        }
    }
    // Cooper, Harvey, Kennedy: "A Simple, Fast Dominance Algorithm".
    let mut idom = vec![usize::MAX; n];
    if !rpo.is_empty() {
        idom[rpo[0]] = rpo[0];
        let mut changed = true;
        while changed {
            changed = false;
            for &b in rpo.iter().skip(1) {
                let mut new_idom = usize::MAX;
                for &p in &preds[b] {
                    if idom[p] == usize::MAX {
                        continue;
                    }
                    new_idom = if new_idom == usize::MAX { p } else { intersect(&idom, &rpo_index, p, new_idom) };
                }
                if new_idom != idom[b] {
                    idom[b] = new_idom;
                    changed = true;
                }
            }
        }
        idom[rpo[0]] = usize::MAX;
    }
    let mut children = vec![vec![]; n];
    for &b in &rpo {
        if idom[b] != usize::MAX {
            children[idom[b]].push(b);
        }
    }
    Cfg { preds, succs, rpo, rpo_index, idom, children }
}

fn intersect(idom: &[usize], rpo_index: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while rpo_index[a] > rpo_index[b] {
            a = idom[a];
        }
        while rpo_index[b] > rpo_index[a] {
            b = idom[b];
        }
    }
    a
}

/// Does `a` dominate `b`?
pub fn dominates(cfg: &Cfg, a: usize, mut b: usize) -> bool {
    loop {
        if a == b {
            return true;
        }
        let d = cfg.idom[b];
        if d == usize::MAX {
            return false;
        }
        b = d;
    }
}
