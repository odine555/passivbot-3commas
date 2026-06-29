# Task 3 — Validate the Rescue Grid Live Fix

## Your Role

You are a **worker subagent**. Your job is to verify that the fixes applied by the previous worker actually work and do not break existing behavior. Read `docs/plans/rescue_grid_live_stuck_fix.md` first to understand what was changed.

You may run commands, inspect code, and write additional tests if you discover gaps. Do not commit unless explicitly asked.

## Validation Checklist

### 1. Rust unit tests for rescue geometry

Run the rescue-specific Rust tests to confirm the pure geometry is still correct.

```bash
cd /work/passivbot-3commas/passivbot-rust
cargo test rescue -- --nocapture
```

Expected: all tests pass.

If you changed `passivbot-rust/src/orchestrator.rs`, also run:

```bash
cargo test
```

Expected: all tests pass.

### 2. Rebuild the PyO3 extension

If any Rust file changed, rebuild the extension before Python tests:

```bash
cd /work/passivbot-3commas/passivbot-rust
maturin develop --release
cd /work/passivbot-3commas
```

### 3. New regression tests

Verify the new tests added by the fix worker pass. They should cover:

- Rescue orders exempt from `limit_order_create_max_market_dist_pct`.
- `reconstruct_rescue_states` correctly detects flips even with delayed/missing reopen fills when live position is present.
- `reconstruct_rescue_states` treats close-all as recovery when the position is truly flat.
- Rescue reverse-grid entries survive when the side is disabled (Rust-side test if applicable).
- Hold-terminate cap sets `rescue_frozen=True`.

Run them explicitly:

```bash
cd /work/passivbot-3commas
source venv/bin/activate
python -m pytest tests/<new_test_file>.py -v
```

If the fix worker added tests to an existing file, run that file instead.

### 4. Rescue backtest regression

Run a backtest with rescue enabled and multiple flips. Use either an existing config or the minimal config created by the reproduction worker.

```bash
cd /work/passivbot-3commas
source venv/bin/activate
python -m passivbot_cli.main backtest configs/rescue_test.json
```

(Adjust the command to the actual config path and CLI invocation from `docs/ai/commands.md`.)

Inspect the output:
- `fills.csv` must contain `RescueRecoveryClose*`, `RescueReverseEntry*`, `RescueFlipClose*`, `RescueFlipEntry*` entries.
- No gap longer than a few candles without any rescue orders while a position is open (unless the cap hit or the position is flat).
- The final position is either flat, hold-frozen, or closed by market-close depending on `rescue_on_terminate`.

### 5. Live orchestrator snapshot regression

Run the live snapshot test script created by the reproduction worker (or create one if missing). Verify:

- Rescue recovery closes and reverse entries are emitted even when far from market.
- The distance gate no longer drops rescue orders.
- If `limit_order_create_max_market_dist_pct` is set very low (e.g. `0.05`), only non-rescue orders are dropped; rescue grid orders remain.

### 6. Full test suite

Run the full Python test suite:

```bash
cd /work/passivbot-3commas
source venv/bin/activate
python -m pytest tests/ -x
```

If there are pre-existing failures, document them. The fix must not introduce new failures.

For a quicker sanity check, run:

```bash
python -m pytest tests/ -x -q
```

### 7. Lint / type check

If the project uses linting tools, run them on changed files:

```bash
python -m ruff check src/passivbot.py src/live/market_data.py passivbot-rust/src/orchestrator.rs
python -m black --check src/passivbot.py src/live/market_data.py
```

(Use the tools actually configured in `pyproject.toml` / `.prospector.yml` / pre-commit hooks.)

### 8. Documentation review

Check whether these docs need updates:

- `docs/live.md` lines 142-152 — the "Known limitation: resting flip-order re-emission" section. If the fix changes this behavior, update or remove the note.
- `docs/rescue_grid.md` lines 185-196 — the live backtest vs live note.
- `CHANGELOG.md` under `## Unreleased` if user-facing live behavior changed.

### 9. Edge-case audit

Read the changed code and verify these edge cases are handled:

- Rescue order exemption from the distance gate does not accidentally exempt non-rescue orders.
- `reconstruct_rescue_states` still returns inactive defaults when rescue is disabled.
- When rescue is active and the position goes flat, it still deactivates.
- When a cap binds with `rescue_on_terminate == "market_close"`, the market close order is still emitted.
- When a cap binds with `rescue_on_terminate == "hold"`, the position is frozen and no rescue orders are emitted.
- The fix does not change backtest behavior (backtest and live reconstruction should still agree on the same fill sequence).

## Deliverables

Return a report containing:
1. A summary of every validation command you ran and its result.
2. Any test failures, with the test name and the failure text.
3. Any pre-existing failures that are unrelated to the fix.
4. Confirmation that `CHANGELOG.md` and relevant docs are updated (or a note that no update was needed).
5. A final go/no-go recommendation.

If the fix is not fully validated, list the remaining gaps and recommend the next worker task.
