# Rescue Grid (Recovery Mode)

> :warning: **Experimental and high-risk.** Rescue Grid is not a save-all. A sufficiently
> adverse, trend-only market will drive it into its caps and realize (or hold) a large loss.
> Read the [Risk](#risk-and-divergence) section before enabling it, run it on a small
> allocation first, and keep human intervention as the final backstop.

Rescue Grid is an optional recovery mode for the 3commas-style DCA build. When a DCA
position exhausts its safety orders and is still under water, rescue takes over the
`(symbol, side)`: it overlays a two-sided grid, banks round-trip spread while price stays
in range, and — when price runs fully adverse — **flips** the position to the opposite side,
sized so that its own recovery grid would break even the accumulated loss on a retrace. It
can flip several times, and is hard-capped by a flip count and a dedicated wallet-exposure
ceiling.

Rescue is configured per side under `bot.long.*` and `bot.short.*`, and is **disabled by
default**. While inactive, the bot behaves exactly like a normal DCA bot.

## How it works

### Arming

Rescue arms when the configured DCA safety order fills while the position is under water.
By default (`rescue_trigger_so_index = -1`) that is the **last** safety order — i.e. the DCA
grid has been fully spent and the position is still losing. At arming:

- `side` = the current position side, `anchor` = the current price,
- `b` (the break-even distance) = `|avg_entry / price - 1|`, derived from the position — **not**
  a parameter,
- carried `debt` = 0 (no loss is realized until the first flip).

### The two grids

While rescue is active, the normal DCA reentries and the single take-profit close are
suppressed and replaced by two fixed-level grids anchored on the position:

- **Recovery grid** (favorable direction): `n_rescue_fav` equal-quantity orders spanning `2 * b`
  from the anchor, with spacing `2 * b / n_rescue_fav`. Equal-quantity exits put the average
  exit at the midpoint `+b` = break-even of the current side, which is why the span is `2 * b`.
- **Reverse grid** (adverse direction): `n_rescue_rev` orders at the **same spacing**, each
  adding the same per-rung quantity as a recovery order. The deepest reverse rung is the
  **flip trigger**.

With the default `n_rescue_rev = n_rescue_fav / 2` (5/10), the reverse span equals `b`, so the
flip lands exactly the break-even distance from the anchor.

### Grid re-fills (banking spread)

The overlay is a real grid, not a one-shot ladder. Within a cycle the rung **prices and sizes
are fixed** (computed once when the cycle began). When a rung fills, an opposing order is placed
one level toward the anchor — a recovery fill places an add, an adverse add places a recovery —
exactly like a grid bot. So if price oscillates inside the band it banks round-trip spread,
which speeds recovery. (Under the average-cost accounting model this shows up as an improved
cost basis / lower break-even, materializing as realized profit when the position returns to
flat.)

### Flip

A cycle ends one of two ways:

- **Recovery:** the position returns to flat with enough banked profit to cover the carried
  debt. Rescue deactivates and normal DCA resumes on that symbol.
- **Flip:** price reaches the deepest reverse (flip-trigger) rung. The entire current position
  is closed, its realized loss is added to `debt`, and the opposite side is opened at the flip
  price (the new anchor), sized for recovery. Then `b` scales by `rescue_grid_step_scale`, both
  grids are recomputed, and the flip count increments.

The flipped position is sized so a full recovery sweep nets `rescue_recovery_coverage x debt`:

```text
V_new = rescue_recovery_coverage * debt / ( b * (n_rescue_fav + 1) / n_rescue_fav )
```

where `b` is the post-scale value for the new cycle. The default coverage `1.05` leaves ~5%
over the summed losses for fees/funding. Nothing about the flip size is a parameter — it falls
out of `debt`, `b`, and the rung counts.

### Caps and termination

Rescue stops when whichever of these binds first:

- `rescue_max_flips` — hard stop on the flip count (default `5`).
- `rescue_wallet_exposure_limit` — a separate, higher wallet-exposure ceiling that **replaces**
  the normal `total_wallet_exposure_limit` crop while rescue is active (default `10.0`).

On a cap, `rescue_on_terminate` decides the outcome:

- `hold` — keep the position and place no further orders (a frozen bag you then manage manually).
- `market_close` — close at market, realize the loss, and end rescue (the symbol returns to
  normal DCA).

## Parameters

The nine configured parameters (under `bot.{long,short}`):

| Parameter | Type | Default | Optimizable | Meaning |
| --- | --- | --- | --- | --- |
| `rescue_enabled` | bool | `false` | no | Master switch for rescue on that side. |
| `rescue_trigger_so_index` | int | `-1` | no | Which filled DCA safety order arms rescue. `-1` = the last safety order. |
| `n_rescue_fav` | int | `10` | yes `[4, 20]` | Recovery (favorable) grid rung count. |
| `n_rescue_rev` | int | `5` | yes `[2, 10]` | Reverse (adverse) grid rung count; deepest rung = flip trigger. |
| `rescue_grid_step_scale` | float | `1.1` | yes `[1.0, 1.5]` | Multiplier applied to `b` at each flip. |
| `rescue_recovery_coverage` | float | `1.05` | no | Size the flip so a full sweep nets `coverage x debt` (fee/funding buffer). |
| `rescue_wallet_exposure_limit` | float | `10.0` | no | Dedicated exposure ceiling used instead of the normal limit while active. |
| `rescue_max_flips` | int | `5` | no | Hard cap on flips before terminating. |
| `rescue_on_terminate` | str | `"hold"` | no | At a cap: `hold` or `market_close`. |

**Derived, never configured:** the break-even distance `b` (at arming and post-scale), every
grid order size, the grid spacings, the carried `debt`, the `side`, the `anchor` price, the flip
notional/quantity, and the `flip_count`.

Only `n_rescue_fav`, `n_rescue_rev`, and `rescue_grid_step_scale` are exposed to the optimizer
(with side-specific prefixes such as `long_n_rescue_fav`); the rest are behavioural / safety
settings. See [Optimizing](optimizing.md#rescue-grid-parameters).

## Enabling rescue

Set `rescue_enabled` on the side you want to protect; the other parameters have working
defaults:

```json
"bot": {
  "long": {
    "rescue_enabled": true,
    "rescue_trigger_so_index": -1,
    "n_rescue_fav": 10,
    "n_rescue_rev": 5,
    "rescue_grid_step_scale": 1.1,
    "rescue_recovery_coverage": 1.05,
    "rescue_wallet_exposure_limit": 10.0,
    "rescue_max_flips": 5,
    "rescue_on_terminate": "hold"
  }
}
```

## Worked example (two flips)

Inputs: a long position, anchor `100`, notional `1000` (10 coins), average entry `108`
(unrealized PnL `-80`), so `b0 = 8%`. Params: `n_rescue_fav = 10`, `n_rescue_rev = 5`,
`rescue_grid_step_scale = 1.1`, coverage `1.0` (used here for clean numbers; production default
is `1.05`). Spacing = `2 * 0.08 / 10 = 1.6%`.

Cycle 0 (long, anchor 100): recovery sells at `101.60 ... 116.00`, reverse adds (buys) at
`98.40, 96.80, 95.20, 93.60, 92.00`, where **92.00 is the flip trigger** (= `anchor * (1 - b)`).
If price falls straight to 92, the 5 adds fill, the position becomes 15 coins at average
`103.73`, and closing it realizes a loss of `-176` → **debt = 176**.

Flip 1 → short at 92, sized `V1 = 176 / (0.088 * 1.1) = 1818` (new `b = 8.8%`).

Cycle 1 (short, anchor 92): the symmetric grid puts the flip trigger at `100.10`
(= `anchor * (1 + b)`). A monotonic rise to 100.10 fills the 5 reverse adds, realizes `-192`,
and **debt becomes 368**.

Flip 2 → long at 100.10, sized `V2 = 368 / (0.0968 * 1.1) = 3456` (new `b = 9.68%`). A full
recovery now needs price to rise to ~`119.47` (+19.4%) to clear the debt.

| State | Notional | Debt | b | Favorable move to clear |
| --- | --- | --- | --- | --- |
| arming (long) | 1000 | 0 | 8.0% | 16.0% |
| after flip 1 (short) | 1818 | 176 | 8.8% | 17.6% |
| after flip 2 (long) | 3456 | 368 | 9.68% | 19.4% |
| (projected) flip 3 | ~6.5k | ~700 | 10.6% | 21.3% |
| (projected) flip 4 | ~12k | ~1.3k | 11.7% | 23.5% |

A single retrace at any point clears the whole debt via the recovery grid, and oscillation
inside a band banks extra spread that reduces the debt faster than this monotonic table shows.

## Risk and divergence

Notional grows roughly **1.9x per adverse flip**, and the favorable move needed to clear the
debt widens each flip. A run of consecutive adverse flips therefore diverges — which is the
entire reason rescue is hard-capped. With the defaults, `rescue_wallet_exposure_limit = 10`
is typically exhausted around flip 3-4, before `rescue_max_flips = 5`.

Practical guidance:

- Size `rescue_wallet_exposure_limit` deliberately. It bypasses the normal exposure crop while
  rescue is active, so it is the real ceiling on how much the account can commit to a rescue.
- Decide `rescue_on_terminate` ahead of time. `market_close` bounds the loss automatically;
  `hold` keeps the bag and requires you to step in.
- Rescue complements, but does not replace, the other risk tools — see
  [Risk Management](risk_management.md), including Auto-Unstuck and the Equity Hard Stop Loss.

## Backtest vs live

- **Backtest:** fully simulated by the shared Rust engine and verified across arming,
  monotonic-flip, oscillation/banking, caps (both terminate modes), and recovery scenarios.
  See [Backtesting](backtesting.md#rescue-grid-in-backtests).
- **Live:** rescue state is reconstructed from realized fills each cycle (via the
  `FillEventsManager`), and the orchestrator emits the grid plus the flip / `market_close`
  orders. There is a known follow-up: while a flip's resting orders are unfilled, the same
  close + reopen is re-emitted each tick at the same price/quantity until fill-based
  reconstruction flips the side. This is intended to be idempotent to a price/quantity-matching
  reconciler but has not yet been verified end-to-end against a live reconciler. See
  [Running live](live.md#rescue-grid-recovery-mode-live-behaviour).
