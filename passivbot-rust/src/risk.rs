use crate::constants::LONG;
use crate::types::OrderType;
use crate::utils::{
    calc_new_psize_pprice, calc_pside_price_diff_int, calc_wallet_exposure, round_dn,
};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct GateEntriesPosition {
    pub idx: usize,
    pub position_size: f64,
    pub position_price: f64,
    pub c_mult: f64,
}

#[derive(Clone, Debug)]
pub struct GateEntriesCandidate {
    pub idx: usize,
    pub qty: f64,
    pub price: f64,
    pub qty_step: f64,
    pub min_qty: f64,
    pub min_cost: f64,
    pub c_mult: f64,
    pub market_price: f64,
    pub order_type: OrderType,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GateEntriesDecision {
    pub idx: usize,
    pub qty: f64,
    pub price: f64,
    pub order_type: OrderType,
    pub original_order: usize,
}

pub fn gate_entries_by_twel(
    pside: usize,
    balance: f64,
    total_wallet_exposure_limit: f64,
    positions: &[GateEntriesPosition],
    entries: &[GateEntriesCandidate],
) -> Vec<GateEntriesDecision> {
    const EXPOSURE_EPS: f64 = 1e-12;
    const QTY_EPS: f64 = 1e-12;

    if balance <= 0.0 || total_wallet_exposure_limit <= 0.0 {
        return Vec::new();
    }

    #[derive(Clone)]
    struct CandidateInternal {
        data: GateEntriesCandidate,
        distance: f64,
        original_order: usize,
    }

    let mut current_positions: HashMap<usize, (f64, f64, f64)> = HashMap::new();
    let mut current_twe = 0.0_f64;
    for pos in positions {
        if !pos.position_price.is_finite() || pos.position_price <= 0.0 {
            continue;
        }
        if !pos.position_size.is_finite() {
            continue;
        }
        let abs_psize = pos.position_size.abs();
        if abs_psize <= QTY_EPS {
            current_positions.insert(pos.idx, (0.0, pos.position_price, pos.c_mult));
            continue;
        }
        let exposure = calc_wallet_exposure(pos.c_mult, balance, abs_psize, pos.position_price);
        if !exposure.is_finite() {
            continue;
        }
        current_twe += exposure;
        current_positions.insert(pos.idx, (abs_psize, pos.position_price, pos.c_mult));
    }
    if current_twe >= total_wallet_exposure_limit - EXPOSURE_EPS {
        return Vec::new();
    }

    let mut candidates: Vec<CandidateInternal> = Vec::with_capacity(entries.len());
    for (original_order, entry) in entries.iter().enumerate() {
        if !entry.price.is_finite() || entry.price <= 0.0 {
            continue;
        }
        if !entry.market_price.is_finite() || entry.market_price <= 0.0 {
            continue;
        }
        if !entry.qty.is_finite() || entry.qty <= QTY_EPS {
            continue;
        }
        let qty_step = if entry.qty_step > 0.0 {
            entry.qty_step
        } else {
            continue;
        };
        let distance = calc_pside_price_diff_int(pside, entry.market_price, entry.price);
        candidates.push(CandidateInternal {
            data: GateEntriesCandidate {
                qty: entry.qty.abs(),
                qty_step,
                ..entry.clone()
            },
            distance,
            original_order,
        });
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut included: Vec<(usize, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(idx, candidate)| (idx, candidate.data.qty))
        .collect();

    let compute_twe_if_filled = |selection: &[(usize, f64)]| -> f64 {
        let mut pos_state = current_positions.clone();
        for (cand_idx, qty) in selection {
            let qty = qty.max(0.0);
            if qty <= QTY_EPS {
                continue;
            }
            let candidate = &candidates[*cand_idx];
            let entry = pos_state.entry(candidate.data.idx).or_insert((
                0.0,
                candidate.data.price,
                candidate.data.c_mult,
            ));
            let (psize, pprice, c_mult) = *entry;
            let (new_psize, new_pprice) = calc_new_psize_pprice(
                psize,
                pprice,
                qty,
                candidate.data.price,
                candidate.data.qty_step,
            );
            *entry = (new_psize.abs(), new_pprice, c_mult);
        }
        let mut twe = 0.0_f64;
        for (_idx, (psize, pprice, c_mult)) in pos_state.iter() {
            if *psize <= QTY_EPS || *pprice <= 0.0 {
                continue;
            }
            let exposure = calc_wallet_exposure(*c_mult, balance, *psize, *pprice);
            if exposure.is_finite() {
                twe += exposure;
            }
        }
        twe
    };

    let mut twe_if_filled = compute_twe_if_filled(&included);
    if twe_if_filled < total_wallet_exposure_limit - EXPOSURE_EPS {
        let mut decisions: Vec<(usize, GateEntriesDecision)> = included
            .into_iter()
            .map(|(cand_idx, qty)| {
                let candidate = &candidates[cand_idx];
                (
                    candidate.original_order,
                    GateEntriesDecision {
                        idx: candidate.data.idx,
                        qty,
                        price: candidate.data.price,
                        order_type: candidate.data.order_type,
                        original_order: candidate.original_order,
                    },
                )
            })
            .collect();
        decisions.sort_by_key(|(order_idx, _)| *order_idx);
        return decisions
            .into_iter()
            .map(|(_, decision)| decision)
            .collect();
    }

    let mut removal_order: Vec<usize> = (0..candidates.len()).collect();
    removal_order.sort_by(|a, b| {
        candidates[*b]
            .distance
            .partial_cmp(&candidates[*a].distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut removed_stack: Vec<(usize, f64)> = Vec::new();
    for cand_idx in removal_order {
        if twe_if_filled < total_wallet_exposure_limit - EXPOSURE_EPS {
            break;
        }
        if let Some(pos) = included.iter().position(|(idx, _)| *idx == cand_idx) {
            let entry = included.remove(pos);
            twe_if_filled = compute_twe_if_filled(&included);
            removed_stack.push(entry);
        }
    }

    if twe_if_filled >= total_wallet_exposure_limit - EXPOSURE_EPS {
        return Vec::new();
    }

    if let Some((cand_idx, original_qty)) = removed_stack.pop() {
        let candidate = &candidates[cand_idx];
        let mut lo = 0.0_f64;
        let mut hi = original_qty;
        let mut best_qty = 0.0_f64;

        for _ in 0..64 {
            let mid = (lo + hi) / 2.0;
            let mid_rd = round_dn(mid, candidate.data.qty_step);
            if mid_rd <= QTY_EPS {
                hi = mid;
                continue;
            }
            let mut trial = included.clone();
            trial.push((cand_idx, mid_rd));
            let twe_trial = compute_twe_if_filled(&trial);
            if twe_trial < total_wallet_exposure_limit - EXPOSURE_EPS {
                best_qty = mid_rd;
                lo = mid;
            } else {
                hi = mid;
            }
        }

        let meets_min_qty =
            candidate.data.min_qty <= QTY_EPS || best_qty >= candidate.data.min_qty - QTY_EPS;
        let meets_min_cost = candidate.data.min_cost <= QTY_EPS
            || best_qty * candidate.data.price * candidate.data.c_mult
                >= candidate.data.min_cost - 1e-9;

        if best_qty > QTY_EPS && meets_min_qty && meets_min_cost {
            included.push((cand_idx, best_qty));
            twe_if_filled = compute_twe_if_filled(&included);
            if twe_if_filled >= total_wallet_exposure_limit - EXPOSURE_EPS {
                included.pop();
            }
        }
    }

    let mut decisions: Vec<(usize, GateEntriesDecision)> = included
        .into_iter()
        .map(|(cand_idx, qty)| {
            let candidate = &candidates[cand_idx];
            (
                candidate.original_order,
                GateEntriesDecision {
                    idx: candidate.data.idx,
                    qty,
                    price: candidate.data.price,
                    order_type: candidate.data.order_type,
                    original_order: candidate.original_order,
                },
            )
        })
        .collect();
    decisions.sort_by_key(|(order, _)| *order);
    decisions
        .into_iter()
        .map(|(_, decision)| decision)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_pos(idx: usize, psize: f64, pprice: f64, c_mult: f64) -> GateEntriesPosition {
        GateEntriesPosition {
            idx,
            position_size: psize,
            position_price: pprice,
            c_mult,
        }
    }

    fn gate_entry(
        idx: usize,
        qty: f64,
        price: f64,
        market_price: f64,
        qty_step: f64,
        min_qty: f64,
        min_cost: f64,
        c_mult: f64,
        order_type: OrderType,
    ) -> GateEntriesCandidate {
        GateEntriesCandidate {
            idx,
            qty,
            price,
            qty_step,
            min_qty,
            min_cost,
            c_mult,
            market_price,
            order_type,
        }
    }

    #[test]
    fn test_gate_entries_blocks_when_twe_if_filled_exceeds() {
        let balance = 1000.0;
        let twel = 1.0;
        let positions = vec![gate_pos(0, 0.0, 0.0, 1.0)];
        let order_type = OrderType::EntryGridNormalLong;
        let entries = vec![
            gate_entry(0, 5.0, 100.0, 100.0, 0.01, 0.0, 0.0, 1.0, order_type),
            gate_entry(0, 6.0, 100.0, 90.0, 0.01, 0.0, 0.0, 1.0, order_type),
        ];
        let gated = gate_entries_by_twel(LONG, balance, twel, &positions, &entries);
        assert!(!gated.is_empty());
        let mut psize = 0.0;
        let mut pprice = 0.0;
        for decision in gated {
            let template = entries
                .iter()
                .find(|e| e.idx == decision.idx && (e.price - decision.price).abs() < 1e-12)
                .expect("matching entry template");
            let (nps, npp) = calc_new_psize_pprice(
                psize,
                pprice,
                decision.qty,
                decision.price,
                template.qty_step,
            );
            psize = nps;
            pprice = npp;
        }
        let twe = calc_wallet_exposure(
            1.0,
            balance,
            psize.abs(),
            if pprice > 0.0 { pprice } else { 100.0 },
        );
        assert!(
            twe < twel - 1e-12,
            "gated twe {} not strictly below twel {}",
            twe,
            twel
        );
    }

    #[test]
    fn test_gate_entries_blocks_when_current_twe_at_limit() {
        let balance = 1000.0;
        let twel = 0.5;
        // Existing position already at limit
        let positions = vec![gate_pos(0, 5.0, 100.0, 1.0)];
        let entries = vec![gate_entry(
            0,
            1.0,
            100.0,
            100.0,
            0.01,
            0.0,
            0.0,
            1.0,
            OrderType::EntryGridNormalLong,
        )];
        let gated = gate_entries_by_twel(LONG, balance, twel, &positions, &entries);
        assert!(
            gated.is_empty(),
            "expected no entries when current twe meets or exceeds limit"
        );
    }

    #[test]
    fn test_gate_entries_allows_when_below_limit() {
        let balance = 1000.0;
        let twel = 1.0;
        let positions = vec![gate_pos(0, 0.0, 0.0, 1.0)];
        let entries = vec![gate_entry(
            0,
            4.0,
            100.0,
            100.0,
            0.01,
            0.0,
            0.0,
            1.0,
            OrderType::EntryGridNormalLong,
        )];
        let gated = gate_entries_by_twel(LONG, balance, twel, &positions, &entries);
        assert_eq!(gated.len(), 1);
        assert!((gated[0].qty - 4.0).abs() < 1e-12);
        assert!((gated[0].price - 100.0).abs() < 1e-12);
    }
}
