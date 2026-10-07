use crate::{Block, Error, Result};

fn at_alpha(count: u64, alpha: f64) -> Vec<Block> {
    // u128 permits a root above u64::MAX without overflowing public IDs.
    let root = (count as u128).next_power_of_two();
    let mut stack = vec![(0_u128, root)];
    let mut out = Vec::new();
    while let Some((lo, hi)) = stack.pop() {
        if lo >= count as u128 {
            continue;
        }
        let size = hi - lo;
        if size > 1 && (hi > count as u128 || size as f64 > alpha * (count as u128 - lo) as f64) {
            let mid = lo + size / 2;
            stack.push((mid, hi));
            stack.push((lo, mid));
        } else {
            out.push(Block::new(lo as u64, hi as u64).expect("aligned tree range"));
        }
    }
    out
}

/// Tile `[0,count)` with at most `budget` aligned blocks, finest near the present.
/// A budget smaller than `count.count_ones()` cannot tile the range and is rejected.
pub fn cover(count: u64, budget: usize) -> Result<Vec<Block>> {
    if budget == 0 {
        return Err(Error::Invalid("WAKE_LINES must be positive.".into()));
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    let minimum = count.count_ones() as usize;
    if budget < minimum {
        return Err(Error::InsufficientBudget { budget, minimum });
    }
    if count <= budget as u64 {
        return (0..count).map(|i| Block::new(i, i + 1)).collect();
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if at_alpha(count, mid).len() > budget {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mut out = at_alpha(count, hi);
    while out.len() < budget {
        let Some(i) = out.iter().rposition(|b| !b.is_raw()) else {
            break;
        };
        let halves = out[i].halves()?;
        out.splice(i..=i, halves);
    }
    Ok(out)
}
