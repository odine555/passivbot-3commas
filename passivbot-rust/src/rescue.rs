//! Rescue-grid pure functions (no I/O), mirroring the style of `entries.rs` /
//! `closes.rs` and reusing the shared `Order` type.
//!
//! See `01_RESCUE_SPEC.md` ("Grid behaviour", "Flip sizing (derived)",
//! "Grid behaviour (traditional grid, re-fills)") and `03_PARAMS_AND_MATH.md`
//! (geometry constraints + worked two-flip simulation that the unit tests below
//! reproduce).
//!
//! Within a rescue *cycle* two fixed-level grids sit on the position:
//!   * recovery grid — favorable direction, `n_rescue_fav` rungs, spans `2·b`,
//!     spacing `2·b / n_rescue_fav`, each rung qty = `position_qty / n_rescue_fav`.
//!     LONG cycle -> sells above the anchor; SHORT cycle -> buys below.
//!   * reverse grid — adverse direction, `n_rescue_rev` rungs, SAME spacing and
//!     SAME per-rung qty; deepest rung = the flip trigger. LONG -> buys below;
//!     SHORT -> sells above.
//!
//! All prices are exact (un-quantized); the orchestrator is responsible for any
//! exchange price/qty quantization, exactly as `calc_entries_*` does.

use crate::types::{Order, OrderType};

/// The side the *current* rescue cycle holds. Kept local (the orchestrator's
/// `PositionSide` lives in a private module) — T5 converts at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueSide {
    Long,
    Short,
}

impl RescueSide {
    /// The side of the next cycle after a flip (the opposite side).
    #[inline]
    pub const fn flipped(self) -> RescueSide {
        match self {
            RescueSide::Long => RescueSide::Short,
            RescueSide::Short => RescueSide::Long,
        }
    }
}

/// Which kind of order filled, used by the grid re-fill rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillKind {
    /// A recovery (take-profit / close) order filled.
    RecoveryClose,
    /// An adverse add (reverse-grid entry) order filled.
    AdverseAdd,
}

const EPS: f64 = 1e-9;

// ---------------------------------------------------------------------------
// Geometry helpers (pure scalars).
// ---------------------------------------------------------------------------

/// Uniform grid spacing within a cycle, as a fraction of the anchor price:
/// `2·b / n_rescue_fav`. Reverse grid uses the SAME spacing.
#[inline]
pub fn rescue_grid_spacing(b: f64, n_rescue_fav: usize) -> f64 {
    if n_rescue_fav == 0 {
        0.0
    } else {
        2.0 * b / n_rescue_fav as f64
    }
}

/// Per-rung quantity (absolute coins) shared by recovery and reverse grids:
/// `position_qty / n_rescue_fav`.
#[inline]
pub fn rescue_rung_qty(position_qty_abs: f64, n_rescue_fav: usize) -> f64 {
    if n_rescue_fav == 0 {
        0.0
    } else {
        position_qty_abs.abs() / n_rescue_fav as f64
    }
}

/// Price at a signed grid level `j` for the cycle. `j > 0` is the favorable
/// (recovery) direction, `j < 0` is the adverse (reverse) direction, `j == 0` is
/// the anchor. For a LONG cycle favorable is up; for SHORT it is down.
#[inline]
pub fn rescue_level_price(
    side: RescueSide,
    anchor: f64,
    b: f64,
    n_rescue_fav: usize,
    j: i32,
) -> f64 {
    let spacing = rescue_grid_spacing(b, n_rescue_fav);
    let frac = j as f64 * spacing;
    match side {
        RescueSide::Long => anchor * (1.0 + frac),
        RescueSide::Short => anchor * (1.0 - frac),
    }
}

/// The flip-trigger price (deepest reverse level = `j = -n_rescue_rev`).
#[inline]
pub fn rescue_flip_trigger_price(
    side: RescueSide,
    anchor: f64,
    b: f64,
    n_rescue_fav: usize,
    n_rescue_rev: usize,
) -> f64 {
    rescue_level_price(side, anchor, b, n_rescue_fav, -(n_rescue_rev as i32))
}

// ---------------------------------------------------------------------------
// Grid order generation.
// ---------------------------------------------------------------------------

/// Recovery grid: `n_rescue_fav` favorable-direction closing orders, equal qty,
/// spanning `2·b` from the anchor. LONG -> sells above; SHORT -> buys below.
/// Close sign convention matches `closes.rs`: long close qty < 0, short close
/// qty > 0.
pub fn calc_rescue_recovery_grid(
    side: RescueSide,
    position_qty_abs: f64,
    anchor: f64,
    b: f64,
    n_rescue_fav: usize,
) -> Vec<Order> {
    let mut out = Vec::with_capacity(n_rescue_fav);
    if n_rescue_fav == 0 || anchor <= 0.0 || position_qty_abs.abs() <= EPS {
        return out;
    }
    let rung = rescue_rung_qty(position_qty_abs, n_rescue_fav);
    for k in 1..=n_rescue_fav {
        let price = rescue_level_price(side, anchor, b, n_rescue_fav, k as i32);
        let (qty, order_type) = match side {
            RescueSide::Long => (-rung, OrderType::RescueRecoveryCloseLong),
            RescueSide::Short => (rung, OrderType::RescueRecoveryCloseShort),
        };
        out.push(Order {
            qty,
            price,
            order_type,
        });
    }
    out
}

/// Reverse grid: `n_rescue_rev` adverse-direction adds, SAME spacing and SAME
/// per-rung qty as the recovery grid; deepest rung is the flip trigger.
/// LONG -> buys below; SHORT -> sells above. Add sign convention matches
/// `entries.rs`: long add qty > 0, short add qty < 0.
pub fn calc_rescue_reverse_grid(
    side: RescueSide,
    position_qty_abs: f64,
    anchor: f64,
    b: f64,
    n_rescue_fav: usize,
    n_rescue_rev: usize,
) -> Vec<Order> {
    let mut out = Vec::with_capacity(n_rescue_rev);
    if n_rescue_rev == 0 || n_rescue_fav == 0 || anchor <= 0.0 || position_qty_abs.abs() <= EPS {
        return out;
    }
    let rung = rescue_rung_qty(position_qty_abs, n_rescue_fav);
    for k in 1..=n_rescue_rev {
        let price = rescue_level_price(side, anchor, b, n_rescue_fav, -(k as i32));
        let (qty, order_type) = match side {
            RescueSide::Long => (rung, OrderType::RescueReverseEntryLong),
            RescueSide::Short => (-rung, OrderType::RescueReverseEntryShort),
        };
        out.push(Order {
            qty,
            price,
            order_type,
        });
    }
    out
}

/// Full traditional-grid order set for an active cycle, keyed off INVENTORY DEPTH
/// (not the instantaneous price) so it behaves like a real resting grid and banks
/// round-trip spread without leaking.
///
/// The per-rung qty is FIXED within the cycle at `base_qty / n_rescue_fav`, where
/// `base_qty` is the position the cycle started with (arming position, or the sized
/// flip position). The number of net reverse-grid rungs currently held is
/// `net_adds = round((position − base) / rung)`; the deepest still-held level is
/// `bottom = −net_adds` (negative while adds are held, positive while a recovery
/// sweep has sold original inventory). Resting orders are then the proper grid:
///   * a CLOSE (recovery) at every level `bottom+1 ..= n_rescue_fav` — i.e. one
///     level toward the anchor from the bottom of inventory, exactly the
///     `rescue_refill_order` "opposing order toward the anchor" rule;
///   * an ADD (reverse) at every level `−n_rescue_rev ..= bottom−1` — one level
///     adverse of the bottom, down to the flip trigger.
///
/// Because the boundary tracks held inventory rather than the current price, a
/// sub-level wiggle places NO new close just above the price (the stateless
/// price-keyed version did, and it filled on the bounce → an unpaired sell that
/// leaked inventory at a loss). Here a buy at level `k` is always paired by a sell
/// resting at `k+1`, so each completed round trip realizes exactly one spacing
/// (`spacing · rung`) of spread. Total close qty `= (n_fav + net_adds)·rung =
/// position`, so a full favorable sweep still closes the whole position.
///
/// Returns `(closes, adds)`. Close sign matches `closes.rs` (long < 0, short > 0);
/// add sign matches `entries.rs` (long > 0, short < 0). Prices are un-quantized.
pub fn calc_rescue_grid_orders(
    side: RescueSide,
    base_qty_abs: f64,
    position_qty_abs: f64,
    anchor: f64,
    b: f64,
    n_rescue_fav: usize,
    n_rescue_rev: usize,
) -> (Vec<Order>, Vec<Order>) {
    let mut closes = Vec::new();
    let mut adds = Vec::new();
    if n_rescue_fav == 0 || anchor <= 0.0 || base_qty_abs.abs() <= EPS {
        return (closes, adds);
    }
    let rung = base_qty_abs.abs() / n_rescue_fav as f64;
    if rung <= EPS {
        return (closes, adds);
    }
    // Net reverse-grid rungs held above the cycle base: >0 when adverse adds have
    // filled, <0 once a recovery sweep has sold original inventory below the base.
    let net_adds = ((position_qty_abs.abs() - base_qty_abs.abs()) / rung).round() as i32;
    // Deepest still-held level. Closes rest one level toward the anchor from it; adds
    // rest one level adverse of it.
    let bottom = -net_adds;
    let last_close = n_rescue_fav as i32;
    let first_close = (bottom + 1).max(-(n_rescue_rev as i32) + 1);
    for j in first_close..=last_close {
        let price = rescue_level_price(side, anchor, b, n_rescue_fav, j);
        let (qty, order_type) = match side {
            RescueSide::Long => (-rung, OrderType::RescueRecoveryCloseLong),
            RescueSide::Short => (rung, OrderType::RescueRecoveryCloseShort),
        };
        closes.push(Order {
            qty,
            price,
            order_type,
        });
    }
    // Add levels down to the flip trigger (-n_rescue_rev). After recovery sells
    // (bottom > 0) the nearest refilled add may sit at/above the anchor.
    let first_add = -(n_rescue_rev as i32);
    let last_add = (bottom - 1).min(n_rescue_fav as i32 - 1);
    for j in first_add..=last_add {
        let price = rescue_level_price(side, anchor, b, n_rescue_fav, j);
        let (qty, order_type) = match side {
            RescueSide::Long => (rung, OrderType::RescueReverseEntryLong),
            RescueSide::Short => (-rung, OrderType::RescueReverseEntryShort),
        };
        adds.push(Order {
            qty,
            price,
            order_type,
        });
    }
    (closes, adds)
}

// ---------------------------------------------------------------------------
// Flip sizing (derived). `b` here is the POST-scale value for the new cycle.
// ---------------------------------------------------------------------------

/// `b` scaled between cycles: `b · rescue_grid_step_scale`.
#[inline]
pub fn rescue_scaled_b(b: f64, rescue_grid_step_scale: f64) -> f64 {
    b * rescue_grid_step_scale
}

/// Full-sweep recovery profit of a notional `v` at break-even `b`:
/// `R(V) = V · b · (n_rescue_fav + 1) / n_rescue_fav`.
#[inline]
pub fn rescue_full_sweep_recovery(v: f64, b: f64, n_rescue_fav: usize) -> f64 {
    if n_rescue_fav == 0 {
        0.0
    } else {
        v * b * (n_rescue_fav as f64 + 1.0) / n_rescue_fav as f64
    }
}

/// New-cycle notional sized so a full recovery sweep nets `coverage · debt`:
/// `V_new = coverage·D / ( b·(n_fav+1)/n_fav )`. `b` is the post-scale value.
#[inline]
pub fn rescue_flip_notional(
    debt: f64,
    b_new: f64,
    n_rescue_fav: usize,
    rescue_recovery_coverage: f64,
) -> f64 {
    let denom = if n_rescue_fav == 0 {
        0.0
    } else {
        b_new * (n_rescue_fav as f64 + 1.0) / n_rescue_fav as f64
    };
    if denom.abs() <= EPS {
        0.0
    } else {
        rescue_recovery_coverage * debt / denom
    }
}

/// New-cycle quantity (coins): `qty_new = V_new / anchor`. `anchor` is the flip
/// price (the new cycle's anchor).
#[inline]
pub fn rescue_flip_qty(
    debt: f64,
    b_new: f64,
    n_rescue_fav: usize,
    rescue_recovery_coverage: f64,
    anchor: f64,
) -> f64 {
    if anchor <= 0.0 {
        return 0.0;
    }
    rescue_flip_notional(debt, b_new, n_rescue_fav, rescue_recovery_coverage) / anchor
}

// ---------------------------------------------------------------------------
// Flip / recovery detection.
// ---------------------------------------------------------------------------

/// True when price has reached the deepest reverse (flip-trigger) level. A LONG
/// cycle flips when price falls to/through the trigger; a SHORT cycle when it
/// rises to/through it.
#[inline]
pub fn rescue_flip_triggered(side: RescueSide, price: f64, flip_trigger_price: f64) -> bool {
    match side {
        RescueSide::Long => price <= flip_trigger_price + EPS,
        RescueSide::Short => price >= flip_trigger_price - EPS,
    }
}

/// True when the cycle has recovered. The position must be net-zero, and the
/// cumulative banked profit must have covered the actual DEBT.
///
/// The threshold is `debt` itself, NOT `coverage · debt`: `rescue_recovery_coverage`
/// is the fee/funding buffer applied to flip SIZING (a full sweep banks ≈
/// `coverage · D` gross → ≈ `D` net after fees). Comparing net-of-fee banked against
/// `coverage · D` would consume the buffer on both sides and never cross.
///
/// `D == 0` (cycle 0, debt never realized, or fully cleared) is satisfied by flat
/// alone — the tiny fee epsilon of a break-even sweep must not block deactivation,
/// otherwise the slot is stuck flat-but-active and DCA never resumes.
#[inline]
pub fn rescue_recovered(position_qty_abs: f64, banked_profit: f64, debt: f64) -> bool {
    if position_qty_abs.abs() > EPS {
        return false;
    }
    debt <= EPS || banked_profit + EPS >= debt
}

// ---------------------------------------------------------------------------
// Grid re-fill rule. Level prices/sizes are FIXED within a cycle, so a re-fill
// is just the opposing order one level toward the anchor.
// ---------------------------------------------------------------------------

/// On a fill at signed grid level `filled_level`, return the opposing order one
/// level toward the anchor:
///   * a recovery (close) fill -> place an ADD one level toward the anchor;
///   * an adverse-add fill      -> place a RECOVERY (close) one level toward the
///     anchor.
/// Returns `None` if there is no level toward the anchor (i.e. the fill was at
/// the anchor itself, `filled_level == 0`).
pub fn rescue_refill_order(
    side: RescueSide,
    anchor: f64,
    b: f64,
    n_rescue_fav: usize,
    position_qty_abs: f64,
    filled_level: i32,
    kind: FillKind,
) -> Option<Order> {
    if filled_level == 0 || n_rescue_fav == 0 || anchor <= 0.0 {
        return None;
    }
    // one level toward the anchor (|j| decreases by 1)
    let toward = filled_level - filled_level.signum();
    let price = rescue_level_price(side, anchor, b, n_rescue_fav, toward);
    let rung = rescue_rung_qty(position_qty_abs, n_rescue_fav);
    let (qty, order_type) = match (kind, side) {
        // recovery fill -> opposing ADD (reverse-grid entry)
        (FillKind::RecoveryClose, RescueSide::Long) => (rung, OrderType::RescueReverseEntryLong),
        (FillKind::RecoveryClose, RescueSide::Short) => (-rung, OrderType::RescueReverseEntryShort),
        // adverse-add fill -> opposing RECOVERY (close)
        (FillKind::AdverseAdd, RescueSide::Long) => (-rung, OrderType::RescueRecoveryCloseLong),
        (FillKind::AdverseAdd, RescueSide::Short) => (rung, OrderType::RescueRecoveryCloseShort),
    };
    Some(Order {
        qty,
        price,
        order_type,
    })
}

// ---------------------------------------------------------------------------
// Unit tests — reproduce the worked two-flip simulation from 03 (coverage = 1.0
// for clean numbers).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    const PRICE_TOL: f64 = 0.01; // prices to ~2 decimals
    const REL_TOL: f64 = 0.01; // notionals/qty within ~1%

    fn approx(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "expected {a} ≈ {b} (tol {tol})");
    }

    fn approx_rel(a: f64, b: f64, rel: f64) {
        assert!(
            (a - b).abs() <= rel * b.abs(),
            "expected {a} ≈ {b} (rel {rel})"
        );
    }

    // ---- Cycle 0: long, anchor 100, V 1000 (10 coins), b 8% ----
    #[test]
    fn cycle0_recovery_grid() {
        let rec = calc_rescue_recovery_grid(RescueSide::Long, 10.0, 100.0, 0.08, 10);
        assert_eq!(rec.len(), 10);
        // sells above: 101.60 .. 116.00 step 1.60, 1.0 coin each (qty < 0 = sell).
        let expected = [
            101.60, 103.20, 104.80, 106.40, 108.00, 109.60, 111.20, 112.80, 114.40, 116.00,
        ];
        for (o, &p) in rec.iter().zip(expected.iter()) {
            approx(o.price, p, PRICE_TOL);
            approx(o.qty, -1.0, REL_TOL);
            assert_eq!(o.order_type, OrderType::RescueRecoveryCloseLong);
        }
    }

    #[test]
    fn cycle0_reverse_grid() {
        let rev = calc_rescue_reverse_grid(RescueSide::Long, 10.0, 100.0, 0.08, 10, 5);
        assert_eq!(rev.len(), 5);
        // buys below: 98.40, 96.80, 95.20, 93.60, 92.00 (flip), 1.0 coin each.
        let expected = [98.40, 96.80, 95.20, 93.60, 92.00];
        for (o, &p) in rev.iter().zip(expected.iter()) {
            approx(o.price, p, PRICE_TOL);
            approx(o.qty, 1.0, REL_TOL);
            assert_eq!(o.order_type, OrderType::RescueReverseEntryLong);
        }
        // deepest reverse == flip trigger == 92.00
        approx(
            rescue_flip_trigger_price(RescueSide::Long, 100.0, 0.08, 10, 5),
            92.00,
            PRICE_TOL,
        );
        approx(rev.last().unwrap().price, 92.00, PRICE_TOL);
    }

    // ---- Flip 1: D = 176, b_new = 8.8% -> V1 ≈ 1818 (≈19.76 coins) ----
    #[test]
    fn flip1_sizing() {
        let b_new = rescue_scaled_b(0.08, 1.1);
        approx(b_new, 0.088, 1e-9);
        let v1 = rescue_flip_notional(176.0, b_new, 10, 1.0);
        approx_rel(v1, 1818.0, REL_TOL);
        let q1 = rescue_flip_qty(176.0, b_new, 10, 1.0, 92.0);
        approx_rel(q1, 19.76, REL_TOL);
    }

    // ---- Cycle 1: short, anchor 92, b 8.8%, spacing 1.76% ----
    #[test]
    fn cycle1_recovery_grid() {
        // V1/anchor coins from flip 1
        let q1 = rescue_flip_qty(176.0, rescue_scaled_b(0.08, 1.1), 10, 1.0, 92.0);
        approx(rescue_grid_spacing(0.088, 10), 0.0176, 1e-9);
        let rec = calc_rescue_recovery_grid(RescueSide::Short, q1, 92.0, 0.088, 10);
        assert_eq!(rec.len(), 10);
        // buys below (close-short): 90.38 .. 75.81
        let expected = [
            90.38, 88.76, 87.14, 85.52, 83.90, 82.28, 80.67, 79.05, 77.43, 75.81,
        ];
        for (o, &p) in rec.iter().zip(expected.iter()) {
            approx(o.price, p, PRICE_TOL);
            assert!(o.qty > 0.0, "short close qty should be > 0");
            approx_rel(o.qty, 1.976, REL_TOL);
            assert_eq!(o.order_type, OrderType::RescueRecoveryCloseShort);
        }
    }

    #[test]
    fn cycle1_reverse_grid() {
        let q1 = rescue_flip_qty(176.0, rescue_scaled_b(0.08, 1.1), 10, 1.0, 92.0);
        let rev = calc_rescue_reverse_grid(RescueSide::Short, q1, 92.0, 0.088, 10, 5);
        assert_eq!(rev.len(), 5);
        // sells above (add-short): 93.62 .. 100.10 (flip)
        let expected = [93.62, 95.24, 96.86, 98.48, 100.10];
        for (o, &p) in rev.iter().zip(expected.iter()) {
            approx(o.price, p, PRICE_TOL);
            assert!(o.qty < 0.0, "short add qty should be < 0");
            approx_rel(o.qty, -1.976, REL_TOL);
            assert_eq!(o.order_type, OrderType::RescueReverseEntryShort);
        }
        approx(
            rescue_flip_trigger_price(RescueSide::Short, 92.0, 0.088, 10, 5),
            100.10,
            PRICE_TOL,
        );
    }

    // ---- Flip 2: D = 368, b_new = 9.68% -> V2 ≈ 3456 ----
    #[test]
    fn flip2_sizing() {
        let b_new = rescue_scaled_b(0.088, 1.1);
        approx(b_new, 0.0968, 1e-9);
        let v2 = rescue_flip_notional(368.0, b_new, 10, 1.0);
        approx_rel(v2, 3456.0, REL_TOL);
    }

    // ---- detection ----
    #[test]
    fn flip_detection() {
        // long flips when price falls to the trigger
        assert!(rescue_flip_triggered(RescueSide::Long, 92.00, 92.00));
        assert!(rescue_flip_triggered(RescueSide::Long, 91.50, 92.00));
        assert!(!rescue_flip_triggered(RescueSide::Long, 92.50, 92.00));
        // short flips when price rises to the trigger
        assert!(rescue_flip_triggered(RescueSide::Short, 100.10, 100.10));
        assert!(rescue_flip_triggered(RescueSide::Short, 101.00, 100.10));
        assert!(!rescue_flip_triggered(RescueSide::Short, 99.00, 100.10));
    }

    #[test]
    fn recovery_detection() {
        // flat with banked profit >= debt -> recovered (threshold is debt, not
        // coverage*debt: the coverage buffer lives in flip sizing as the fee cushion).
        assert!(rescue_recovered(0.0, 176.0, 176.0));
        assert!(rescue_recovered(0.0, 200.0, 176.0));
        // not flat -> not recovered, regardless of banked
        assert!(!rescue_recovered(1.0, 200.0, 176.0));
        // flat but banked short of the debt -> not recovered
        assert!(!rescue_recovered(0.0, 150.0, 176.0));
        // D == 0 (cycle 0 / fully cleared): flat alone is enough, even with a tiny
        // net-of-fees negative banked from a break-even sweep.
        assert!(rescue_recovered(0.0, 0.0, 0.0));
        assert!(rescue_recovered(0.0, -0.5, 0.0));
        // flat exactly at the debt fires (fee epsilon must not block it).
        assert!(rescue_recovered(0.0, 176.0, 176.0));
    }

    // ---- re-fill rule ----
    #[test]
    fn refill_recovery_places_add_toward_anchor() {
        // long cycle, recovery sell at level +1 (101.60) fills -> place a BUY add
        // one level toward anchor, i.e. level 0 = anchor = 100.00.
        let o = rescue_refill_order(
            RescueSide::Long,
            100.0,
            0.08,
            10,
            10.0,
            1,
            FillKind::RecoveryClose,
        )
        .unwrap();
        approx(o.price, 100.00, PRICE_TOL);
        assert!(o.qty > 0.0);
        assert_eq!(o.order_type, OrderType::RescueReverseEntryLong);
    }

    // ---- traditional-grid re-fill is emergent from inventory-keyed regeneration ----
    #[test]
    fn grid_orders_refill_and_bank_one_oscillation() {
        // Long cycle, anchor 100, b 8%, n_fav 10, n_rev 5, base 10 coins.
        // spacing = 1.6% -> level(-1)=98.40, level(0)=100.00, level(+1)=101.60.
        let (n_fav, n_rev) = (10usize, 5usize);
        let (anchor, b) = (100.0, 0.08);
        let base = 10.0;

        // Step 1: position == base (no adds held). Recovery sells start at +1
        // (101.60), adds start at -1 (98.40). Rung = base/n_fav = 1.0 coin.
        let (closes0, adds0) =
            calc_rescue_grid_orders(RescueSide::Long, base, base, anchor, b, n_fav, n_rev);
        assert_eq!(closes0.len(), 10); // levels +1..+10
        assert_eq!(adds0.len(), 5); // levels -1..-5
        approx(closes0[0].price, 101.60, PRICE_TOL); // nearest close = level +1
        approx(closes0[0].qty, -1.0, REL_TOL);
        // Adds are emitted deepest-first; the one nearest the anchor is level -1.
        let nearest_add = adds0.last().unwrap();
        approx(nearest_add.price, 98.40, PRICE_TOL);
        approx(nearest_add.qty, 1.0, REL_TOL);
        // No close rests at/below the anchor yet.
        assert!(closes0.iter().all(|o| o.price > anchor + PRICE_TOL));

        // Step 2: the add at level -1 (98.40) has filled -> position grew to 11
        // coins (one net add). Regenerating the grid must (re)place a RECOVERY (sell)
        // one level toward the anchor, i.e. exactly at the anchor 100.00 (level 0) —
        // the emergent re-fill. Rung stays FIXED at base/n_fav = 1.0.
        let (closes1, _adds1) =
            calc_rescue_grid_orders(RescueSide::Long, base, 11.0, anchor, b, n_fav, n_rev);
        assert_eq!(closes1.len(), 11); // levels 0..+10
        let refill = &closes1[0];
        approx(refill.price, 100.00, PRICE_TOL); // sell back at the anchor
        approx(refill.qty, -1.0, REL_TOL); // fixed rung, not 11/10
        assert_eq!(refill.order_type, OrderType::RescueRecoveryCloseLong);

        // Step 3: price returns to the anchor -> that refill sell fills. One full
        // oscillation banks spacing*qty (the realized round trip = exactly one
        // spacing, minus fees in the live path):
        let round_trip_profit = refill.qty.abs() * (refill.price - nearest_add.price);
        approx(round_trip_profit, 1.60, PRICE_TOL); // 1 coin * (100.00 - 98.40)
    }

    // ---- a sub-level wiggle must NOT place an unpaired sell (the old leak) ----
    #[test]
    fn grid_orders_no_unpaired_sell_on_subgap_wiggle() {
        // base 10, no adds held. The nearest close must always rest one full level
        // above the bottom of inventory (the anchor here), never at a level the price
        // merely dipped below — so a wiggle inside a gap can't sell inventory.
        let (n_fav, n_rev) = (10usize, 5usize);
        let (anchor, b, base) = (100.0, 0.08, 10.0);
        // position unchanged at base regardless of where price wandered.
        let (closes, _adds) =
            calc_rescue_grid_orders(RescueSide::Long, base, base, anchor, b, n_fav, n_rev);
        // nearest close is a full spacing above the anchor; nothing rests at/below it.
        approx(closes[0].price, 101.60, PRICE_TOL);
        assert!(closes.iter().all(|o| o.price >= anchor + 1.0));
    }

    #[test]
    fn grid_orders_recovery_sweep_places_buy_back() {
        // After one recovery sell (position = base - rung), the bottom moves to +1,
        // so a BUY refill must rest one level toward the anchor at level 0 (100.00).
        let (n_fav, n_rev) = (10usize, 5usize);
        let (anchor, b, base) = (100.0, 0.08, 10.0);
        let (closes, adds) =
            calc_rescue_grid_orders(RescueSide::Long, base, 9.0, anchor, b, n_fav, n_rev);
        assert_eq!(closes.len(), 9); // levels +2..+10
        approx(closes[0].price, 103.20, PRICE_TOL); // nearest close now level +2
        let nearest_add = adds.last().unwrap();
        approx(nearest_add.price, 100.00, PRICE_TOL); // buy back at the anchor
        approx(nearest_add.qty, 1.0, REL_TOL);
    }

    #[test]
    fn grid_orders_sizes_fixed_within_cycle() {
        // Rung qty must not drift as the position grows/shrinks within a cycle; it is
        // always base/n_fav regardless of inventory depth.
        let mk = |pos: f64| {
            calc_rescue_grid_orders(RescueSide::Long, 10.0, pos, 100.0, 0.08, 10, 5)
                .0[0]
                .qty
                .abs()
        };
        approx(mk(10.0), 1.0, REL_TOL); // at base, 10 close levels
        approx(mk(11.0), 1.0, REL_TOL); // 1 add held, 11 close levels
        approx(mk(12.0), 1.0, REL_TOL); // 2 adds held, 12 close levels
        approx(mk(9.0), 1.0, REL_TOL); // 1 recovery sold, 9 close levels
    }

    #[test]
    fn refill_adverse_add_places_recovery_toward_anchor() {
        // long cycle, adverse add at level -2 (96.80) fills -> place a SELL recovery
        // one level toward anchor, i.e. level -1 = 98.40.
        let o = rescue_refill_order(
            RescueSide::Long,
            100.0,
            0.08,
            10,
            10.0,
            -2,
            FillKind::AdverseAdd,
        )
        .unwrap();
        approx(o.price, 98.40, PRICE_TOL);
        assert!(o.qty < 0.0);
        assert_eq!(o.order_type, OrderType::RescueRecoveryCloseLong);
        // a fill exactly at the anchor has no level toward it
        assert!(
            rescue_refill_order(RescueSide::Long, 100.0, 0.08, 10, 10.0, 0, FillKind::AdverseAdd)
                .is_none()
        );
    }
}
