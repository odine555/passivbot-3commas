# Task 2 — Fix the Rescue Grid Live "Stuck Position" Bug

## Your Role

You are a **worker subagent**. Your job is to implement fixes for the root causes identified by the reproduction worker. Read `docs/plans/rescue_grid_live_stuck_reproduce.md` first if it is available; otherwise work from the findings reported by the previous worker.

You may edit production code and add tests. Keep changes minimal and targeted. Do not run the full test suite yourself unless asked; write tests and run targeted commands only.

## Fix Priority Order

Address the causes below in this order. Each fix must be isolated in its own commit-sized change set so it can be reviewed independently.

### Fix 1 — Exempt rescue grid orders from the far-from-market limit gate

**Root cause:** `src/live/market_data.py:134` (`_filter_limit_order_creations_by_market_distance`) applies `live.limit_order_create_max_market_dist_pct` to every non-market limit order. Rescue grid orders are intentionally far from market after several flips; the gate silently drops them and the position stops gridding.

**Expected change:**

In `_filter_limit_order_creations_by_market_distance`, before computing the distance for an order, check whether it is a rescue order. If `pb_order_type` starts with `rescue_`, keep the order unconditionally.

Example guard (verify exact field names in `order` dict):

```python
pb_type = str(order.get("pb_order_type", "")).lower()
if pb_type.startswith("rescue_"):
    kept.append(order)
    continue
```

Place this guard immediately after the `market` order exemption (`:147-149`).

**Rationale:** Rescue grid geometry is computed by the Rust orchestrator using the dedicated `rescue_wallet_exposure_limit` and is not subject to the normal DCA distance constraints. The far-from-market gate was designed to prevent bad initial entries and deep reentries from being rejected repeatedly by exchanges; it should not suppress a deliberate recovery grid.

**Regression test:** Add a test in `tests/` that passes rescue recovery closes and reverse entries far beyond the threshold and asserts they are kept.

### Fix 2 — Make `reconstruct_rescue_states` robust to missing or delayed reopen fills

**Root cause:** `src/passivbot.py:686-711` detects a flip only when the event immediately after a close-all is an opposite-side entry with `psize_after > 0`. If the reopen fill is delayed or missing from the current event window, the function deactivates rescue and the opposite-side position is treated as a normal DCA bag.

**Expected change:**

Option A (preferred if feasible): Track a pending-flip state across the event stream and look back/forward for the matching opposite-side open.

Option B (safer fallback): When a close-all occurs while rescue is active, do **not** immediately decide between flip and recovery. Instead, mark the cycle as `pending_flip` and continue scanning. If an opposite-side entry for the same symbol appears before the stream ends, treat it as the flip. If the stream ends without one, then treat it as recovery.

A minimal patch at the end of stream (`:764-766`):

```python
# End of stream: a still-pending flat with no reopen is a recovery.
if active and flat_pending:
    # If we have not seen the opposite-side reopen by now, this is a recovery.
    active = False
    cur_side = None
```

Consider extending this so that `flat_pending` is also resolved by checking the current exchange position side in `_rescue_state_for_position` (`:10571-10631`). If `reconstruct_rescue_states` returns an active state on side A but the live `pos["size"]` shows the position is actually on side B, the function should relocate the active state to side B or return inactive defaults and log a warning.

**Important:** Do not over-engineer. The simplest robust rule is:
- If `flat_pending` is true at end of stream and the current live position is non-zero on the opposite side, treat it as a flip (debt += realized_loss, flip_count += 1, cur_side = opposite, anchor = last known flip price or current price).
- If the current live position is flat, treat it as recovery.

This requires passing the live `pos` dict into `reconstruct_rescue_states` or handling it in `_rescue_state_for_position`. Prefer the latter to keep the function signature stable.

**Regression test:** Add tests in `tests/` for:
- Close-all followed immediately by opposite-side entry → flip detected.
- Close-all with no opposite-side entry in stream but live position is on opposite side → flip detected.
- Close-all with no opposite-side entry and live position flat → recovery.
- Close-all with `psize_after == 0` on reopen fill but live position non-zero → flip detected.

### Fix 3 — Prevent side-disabled wipe of rescue reverse-grid entries

**Root cause:** `passivbot-rust/src/orchestrator.rs:3175-3179` clears all entries if `enabled_long`/`enabled_short` is false. Rescue reverse-grid adds are entries and are wiped, leaving only recovery closes.

**Expected change:**

In the orchestrator, if `s.long.rescue_active` (or `s.short.rescue_active`) is true, do **not** clear entries for that side even if the side is otherwise disabled. Rescue overlay must take precedence over the normal side-enable flag.

Locate the entries clearing blocks (`:3175-3179` for long, `:3204-3206` for short) and add a rescue guard:

```rust
} else {
    for s in per_long.iter_mut().filter_map(|v| v.as_mut()) {
        if !s.long.rescue_active && !s.long.rescue_frozen {
            s.entries.clear();
        }
    }
}
```

This is a Rust change. Follow the Rust source-of-truth rule from `AGENTS.md`.

**Regression test:** Add a Rust-side test or a Python integration test that passes `enabled_long=False` with `rescue_active=True` and asserts that rescue reverse entries are present.

### Fix 4 — Fix hold-terminate freeze deadlock

**Root cause:** `passivbot-rust/src/orchestrator.rs:1754-1758` returns `Some(Vec::new())` for a hold-terminate cap. The slot emits nothing until the Python layer sets `rescue_frozen=True`. `reconstruct_rescue_states._apply_caps` (`src/passivbot.py:651-667`) does set `frozen=True`, but only when the cap is detected from fills. If the orchestrator detects the cap first (e.g. because the position size is already above the WE limit), the two can disagree.

**Expected change:**

Option A: Make the orchestrator's `try_rescue_flip` return a special marker or set a flag that tells the Python caller the slot is now frozen. Because the orchestrator is stateless, this would need to be returned in the output JSON.

Option B (preferred): Move the hold-terminate freeze decision entirely to the reconstruction layer. When `_apply_caps` in `reconstruct_rescue_states` detects a cap with `on_terminate == "hold"`, set `frozen=True`. The orchestrator then sees `rescue_frozen=True` on the next tick and emits nothing. The `try_rescue_flip` `hold` branch should still return `Some([])` as a safety net, but the primary freeze must come from reconstruction.

Additionally, review `_apply_caps` (`:651-667`). It currently computes WE as `base_qty * anchor * c_mult / balance`. This uses the cycle's `base_qty` and `anchor`, not the current live position. If the live position has grown or shrunk, the WE check may be wrong. Consider computing WE from the current live position passed into `_rescue_state_for_position`.

**Regression test:** Add a test where flip_count >= max_flips and on_terminate="hold" and assert `rescue_frozen=True` and `rescue_active=False`.

## Implementation Notes

- Always prefer Python-side fixes for live-specific state/reconstruction issues. Only change Rust if the orchestrator's routing logic itself is wrong.
- Add `logging.warning(...)` or `logging.info(...)` when rescue orders are exempted from a gate or when reconstruction relocates state based on live position. This aids future debugging.
- Do not change backtest behavior unless the same bug exists there.
- Update `docs/live.md` section "Known limitation: resting flip-order re-emission" if the fix changes that behavior.
- Update `CHANGELOG.md` under `## Unreleased` if user-facing live behavior changed.

## Deliverables

Return a report containing:
1. Each fix applied, with exact file paths and line numbers.
2. The reasoning for each fix.
3. Any new or modified tests.
4. Commands you ran to verify targeted behavior.
5. Any remaining concerns or follow-up tasks.
