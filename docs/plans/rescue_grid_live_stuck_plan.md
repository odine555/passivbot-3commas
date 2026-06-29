# Rescue Grid Live "Stuck Position" Bug — Orchestrator Plan

## Status

- Branch: `rescue-grid`
- Last commit: `d82d71a feat(rescue): tag rescue orders in fills.csv + distinct plot marks`
- Symptom: After the last DCA safety order fills and rescue grid arms, the bot flips a few times, then gets stuck and keeps an open position for days without placing grid orders.
- Backtest engine: verified and working.
- Bug location: **live path only** — state reconstruction from fills (`src/passivbot.py`) and live order-creation gating (`src/live/market_data.py`, `src/live/reconciler.py`).

## For the Orchestrator Agent (READ THIS FIRST)

You are the **orchestrator**. You do **NOT** write code, run tests, edit files, or run backtests yourself. Your only job is to read this plan and spawn focused **subagent workers** to execute the tasks below.

Spawn one subagent per major task. Each subagent should return:

1. A concise summary of what it did.
2. Exact file paths and line numbers it touched.
3. Commands it ran and the results.
4. The next worker it recommends spawning, if any.

If a worker reports a blocker, spawn a narrower worker to investigate that blocker. Do not try to fix it yourself.

## High-Level Hypotheses

The Rust geometry and backtest state machine are solid. The live failure modes are:

1. **Far-from-market limit-order gate drops rescue grid orders**
   - `src/live/market_data.py:134` filters every non-market limit create by `live.limit_order_create_max_market_dist_pct` (default `0.8`).
   - Rescue grid orders are not exempt. After multiple flips, scaled `b` can push grid levels beyond the 80% threshold, so the live executor silently skips them.
   - File: `src/live/market_data.py`, `src/passivbot.py:298` (`order_market_diff`).

2. **State reconstruction mis-classifies flips**
   - `src/passivbot.py:590` (`reconstruct_rescue_states`) detects a flip only when the event *immediately* after a close-all is an opposite-side entry with `psize_after > 0`.
   - If the reopen fill is delayed, missing, has `psize_after == 0`, or reports the wrong `position_side`, reconstruction treats the close as a recovery and deactivates rescue. The remaining opposite-side position is then handled as a normal DCA position that has exhausted its safety orders → no grid.
   - File: `src/passivbot.py:686-711`.

3. **Side-disabled orchestrator branch wipes rescue reverse-grid entries**
   - `passivbot-rust/src/orchestrator.rs:3175-3179` clears all entries if the side is disabled. Rescue reverse-grid adds are not exempt.
   - If live reconstruction or config ever disables a side while a rescue position is open on it, only recovery closes remain.
   - File: `passivbot-rust/src/orchestrator.rs:3152-3208`.

4. **Hold-terminate freeze deadlock**
   - When a flip/WE cap binds with `rescue_on_terminate == "hold"`, `try_rescue_flip` returns `Some([])` and emits nothing until reconstruction sets `rescue_frozen=True`.
   - If reconstruction's `_apply_caps()` does not fire or the cap is detected only in the orchestrator, the slot emits no orders indefinitely.
   - File: `src/passivbot.py:651-667`, `passivbot-rust/src/orchestrator.rs:1734-1758`.

## Task Breakdown

### Task 1 — Reproduce the stuck state

Spawn a worker from `rescue_grid_live_stuck_reproduce.md`.

Goal: produce a minimal, repeatable demonstration of at least one stuck scenario.

Expected deliverables:
- A standalone Python script or pytest that feeds a synthetic fill sequence into `reconstruct_rescue_states` and shows rescue becoming inactive while a position remains open.
- A backtest config/run that exercises 3-5 flips and emits continuous grid orders (baseline: this should work).
- A live-orchestrator snapshot test that shows grid orders being dropped after flips when `limit_order_create_max_market_dist_pct` is active.

### Task 2 — Diagnose and fix

Spawn a worker from `rescue_grid_live_stuck_fix.md`.

Goal: implement fixes for every reproduced failure mode.

Expected deliverables:
- Code changes in `src/passivbot.py`, `src/live/market_data.py`, and/or `passivbot-rust/src/orchestrator.rs` (only if Rust change is strictly necessary).
- Each change must be minimal and target exactly one root cause.
- New regression tests for each failure mode.

### Task 3 — Validate

Spawn a worker from `rescue_grid_live_stuck_validate.md`.

Goal: prove the fixes work and do not regress normal behavior.

Expected deliverables:
- All new regression tests pass.
- Existing rescue backtests still pass.
- Full test suite still passes (or only pre-existing failures remain).
- Documentation updates if user-facing behavior changed.

## Communication Rules

- Use `task` tool to spawn subagents.
- Each subagent must receive the full context of its task and must know it is a worker, not the orchestrator.
- Subagents may spawn their own sub-subagents if needed, but they should return a summary to you.
- Do not commit or push anything unless explicitly asked.

## Definition of Done

- The stuck scenario is reproduced in a test.
- The root cause(s) are fixed in code.
- Regression tests prevent reintroduction.
- Existing rescue backtests and the full test suite pass.
- `CHANGELOG.md` is updated if user-facing live behavior changed.
