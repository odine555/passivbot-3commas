use crate::types::{
    BotParams, ExchangeParams, Order, OrderType, Position, StateParams, TrailingPriceBundle,
};
use crate::utils::{
    calc_ema_price_ask, calc_ema_price_bid, calc_new_psize_pprice, calc_wallet_exposure,
    calc_wallet_exposure_if_filled, cost_to_qty, interpolate, quantize_price, quantize_qty, round_,
    round_dn, round_up, RoundingMode,
};

// ---------------------------------------------------------------------------
// 3commas-style DCA ladder helpers (Pass 1). Small, pure free functions.
// G(r, n) = Σ_{k=0}^{n-1} r^k = n if r == 1 else (r^n - 1)/(r - 1)
// ---------------------------------------------------------------------------
pub fn dca_geometric_sum(r: f64, n: usize) -> f64 {
    if n == 0 {
        0.0
    } else if r == 1.0 {
        n as f64
    } else {
        (r.powi(n as i32) - 1.0) / (r - 1.0)
    }
}

pub fn dca_so_price_long(base: f64, n: usize, deviation: f64, step_scale: f64) -> f64 {
    base * (1.0 - deviation * dca_geometric_sum(step_scale, n))
}

pub fn dca_so_price_short(base: f64, n: usize, deviation: f64, step_scale: f64) -> f64 {
    base * (1.0 + deviation * dca_geometric_sum(step_scale, n))
}

pub fn dca_so_cost(base_order_cost: f64, n: usize, so1_ratio: f64, volume_scale: f64) -> f64 {
    // n is 1-based safety-order index
    base_order_cost * so1_ratio * volume_scale.powi((n as i32) - 1)
}

pub fn wallet_exposure_limit_with_allowance(bot_params: &BotParams) -> f64 {
    // 3commas DCA: the per-position wallet-exposure limit is a hard ceiling.
    bot_params.wallet_exposure_limit
}

pub fn calc_initial_entry_qty(
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
    balance: f64,
    entry_price: f64,
) -> f64 {
    f64::max(
        calc_min_entry_qty(entry_price, &exchange_params),
        round_(
            cost_to_qty(
                balance
                    * wallet_exposure_limit_with_allowance(bot_params)
                    * bot_params.entry_initial_qty_pct,
                entry_price,
                exchange_params.c_mult,
            ),
            exchange_params.qty_step,
        ),
    )
}

pub fn calc_min_entry_qty(entry_price: f64, exchange_params: &ExchangeParams) -> f64 {
    f64::max(
        exchange_params.min_qty,
        round_up(
            cost_to_qty(
                exchange_params.min_cost,
                entry_price,
                exchange_params.c_mult,
            ),
            exchange_params.qty_step,
        ),
    )
}

pub fn calc_cropped_reentry_qty(
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
    position: &Position,
    wallet_exposure: f64,
    balance: f64,
    entry_qty: f64,
    entry_price: f64,
    wallet_exposure_limit_cap: f64,
) -> (f64, f64) {
    let effective_wallet_exposure_limit = f64::min(
        wallet_exposure_limit_cap,
        wallet_exposure_limit_with_allowance(bot_params),
    );
    let position_size_abs = position.size.abs();
    let entry_qty_abs = entry_qty.abs();
    let wallet_exposure_if_filled = calc_wallet_exposure_if_filled(
        balance,
        position_size_abs,
        position.price,
        entry_qty_abs,
        entry_price,
        &exchange_params,
    );
    let min_entry_qty = calc_min_entry_qty(entry_price, &exchange_params);
    if wallet_exposure_if_filled > effective_wallet_exposure_limit * 1.01 {
        // reentry too big. Crop current reentry qty.
        let entry_qty_abs = interpolate(
            effective_wallet_exposure_limit,
            &[wallet_exposure, wallet_exposure_if_filled],
            &[position_size_abs, position_size_abs + entry_qty_abs],
        ) - position_size_abs;
        (
            wallet_exposure_if_filled,
            f64::max(
                round_(entry_qty_abs, exchange_params.qty_step),
                min_entry_qty,
            ),
        )
    } else {
        (
            wallet_exposure_if_filled,
            f64::max(entry_qty_abs, min_entry_qty),
        )
    }
}

pub fn calc_grid_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    wallet_exposure_limit_cap: f64,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Order {
    if wallet_exposure_limit_with_allowance(bot_params) == 0.0 || state_params.balance <= 0.0 {
        return Order::default();
    }
    // 3commas DCA ladder: if the position is non-flat, the next entry is a
    // safety order at a fixed-base-relative price (NOT the running average).
    let next_so_idx = dca_entry_fills as usize; // BO counts as fill 1 -> next SO index = fills
    if next_so_idx >= 1 && dca_base_price > 0.0 {
        let max_so = bot_params.dca_max_safety_orders.round() as usize;
        if next_so_idx > max_so {
            return Order::default();
        }
        let so_price = round_dn(
            dca_so_price_long(
                dca_base_price,
                next_so_idx,
                bot_params.dca_price_deviation_pct,
                bot_params.dca_step_scale,
            ),
            exchange_params.price_step,
        );
        if so_price <= exchange_params.price_step {
            return Order::default();
        }
        // base_order_cost anchored to the ORIGINAL base order, recomputed the
        // same way the BO branch sizes the first fill (at the base price).
        let bo_qty = calc_initial_entry_qty(
            exchange_params,
            bot_params,
            state_params.balance,
            dca_base_price,
        );
        let base_order_cost = bo_qty * dca_base_price;
        let so_cost = dca_so_cost(
            base_order_cost,
            next_so_idx,
            bot_params.dca_so1_ratio,
            bot_params.dca_volume_scale,
        );
        let so_qty = f64::max(
            calc_min_entry_qty(so_price, &exchange_params),
            round_(
                cost_to_qty(so_cost, so_price, exchange_params.c_mult),
                exchange_params.qty_step,
            ),
        );
        let wallet_exposure = calc_wallet_exposure(
            exchange_params.c_mult,
            state_params.balance,
            position.size,
            position.price,
        );
        let effective_wallet_exposure_limit = f64::min(
            wallet_exposure_limit_cap,
            wallet_exposure_limit_with_allowance(bot_params),
        );
        // wallet-exposure ceiling: if already at/over the limit, no more SOs.
        if wallet_exposure >= effective_wallet_exposure_limit * 0.999 {
            return Order::default();
        }
        let (_wallet_exposure_if_filled, so_qty_cropped) = calc_cropped_reentry_qty(
            exchange_params,
            bot_params,
            position,
            wallet_exposure,
            state_params.balance,
            so_qty,
            so_price,
            effective_wallet_exposure_limit,
        );
        if so_qty_cropped < so_qty {
            return Order {
                qty: so_qty_cropped,
                price: so_price,
                order_type: OrderType::EntryGridCroppedLong,
            };
        }
        return Order {
            qty: so_qty,
            price: so_price,
            order_type: OrderType::EntryGridNormalLong,
        };
    }
    let initial_entry_price = calc_ema_price_bid(
        exchange_params.price_step,
        state_params.order_book.bid,
        state_params.ema_bands.lower,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    if position.size == 0.0 {
        return Order {
            qty: initial_entry_qty,
            price: initial_entry_price,
            order_type: OrderType::EntryInitialNormalLong,
        };
    } else if position.size < initial_entry_qty * 0.8 {
        return Order {
            qty: f64::max(
                calc_min_entry_qty(initial_entry_price, &exchange_params),
                round_dn(initial_entry_qty - position.size, exchange_params.qty_step),
            ),
            price: initial_entry_price,
            order_type: OrderType::EntryInitialPartialLong,
        };
    }
    // 3commas DCA: all post-base entries are safety orders handled above via the
    // dca_base_price / dca_entry_fills ladder. No legacy re-entry grid.
    Order::default()
}

pub fn calc_next_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    _trailing_price_bundle: &TrailingPriceBundle,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Order {
    // 3commas DCA: base order (flat) or safety-order ladder. No trailing entries.
    if wallet_exposure_limit_with_allowance(bot_params) == 0.0 || state_params.balance <= 0.0 {
        return Order::default();
    }
    let allowed_wallet_exposure_limit = wallet_exposure_limit_with_allowance(bot_params);
    calc_grid_entry_long(
        exchange_params,
        state_params,
        bot_params,
        position,
        allowed_wallet_exposure_limit,
        dca_base_price,
        dca_entry_fills,
    )
}

pub fn calc_grid_entry_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    wallet_exposure_limit_cap: f64,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Order {
    if wallet_exposure_limit_with_allowance(bot_params) == 0.0 || state_params.balance <= 0.0 {
        return Order::default();
    }
    // 3commas DCA ladder (short mirror): non-flat -> safety order at a
    // fixed-base-relative price above the base.
    let next_so_idx = dca_entry_fills as usize;
    if next_so_idx >= 1 && dca_base_price > 0.0 {
        let max_so = bot_params.dca_max_safety_orders.round() as usize;
        if next_so_idx > max_so {
            return Order::default();
        }
        let so_price = round_up(
            dca_so_price_short(
                dca_base_price,
                next_so_idx,
                bot_params.dca_price_deviation_pct,
                bot_params.dca_step_scale,
            ),
            exchange_params.price_step,
        );
        if so_price <= exchange_params.price_step {
            return Order::default();
        }
        let bo_qty = calc_initial_entry_qty(
            exchange_params,
            bot_params,
            state_params.balance,
            dca_base_price,
        );
        let base_order_cost = bo_qty * dca_base_price;
        let so_cost = dca_so_cost(
            base_order_cost,
            next_so_idx,
            bot_params.dca_so1_ratio,
            bot_params.dca_volume_scale,
        );
        let so_qty = f64::max(
            calc_min_entry_qty(so_price, &exchange_params),
            round_(
                cost_to_qty(so_cost, so_price, exchange_params.c_mult),
                exchange_params.qty_step,
            ),
        );
        let wallet_exposure = calc_wallet_exposure(
            exchange_params.c_mult,
            state_params.balance,
            position.size.abs(),
            position.price,
        );
        let effective_wallet_exposure_limit = f64::min(
            wallet_exposure_limit_cap,
            wallet_exposure_limit_with_allowance(bot_params),
        );
        if wallet_exposure >= effective_wallet_exposure_limit * 0.999 {
            return Order::default();
        }
        let (_wallet_exposure_if_filled, so_qty_cropped) = calc_cropped_reentry_qty(
            exchange_params,
            bot_params,
            position,
            wallet_exposure,
            state_params.balance,
            so_qty,
            so_price,
            effective_wallet_exposure_limit,
        );
        if so_qty_cropped < so_qty {
            return Order {
                qty: -so_qty_cropped,
                price: so_price,
                order_type: OrderType::EntryGridCroppedShort,
            };
        }
        return Order {
            qty: -so_qty,
            price: so_price,
            order_type: OrderType::EntryGridNormalShort,
        };
    }
    let initial_entry_price = calc_ema_price_ask(
        exchange_params.price_step,
        state_params.order_book.ask,
        state_params.ema_bands.upper,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    let position_size_abs = position.size.abs();
    if position_size_abs == 0.0 {
        return Order {
            qty: -initial_entry_qty,
            price: initial_entry_price,
            order_type: OrderType::EntryInitialNormalShort,
        };
    } else if position_size_abs < initial_entry_qty * 0.8 {
        return Order {
            qty: -f64::max(
                calc_min_entry_qty(initial_entry_price, &exchange_params),
                round_dn(
                    initial_entry_qty - position_size_abs,
                    exchange_params.qty_step,
                ),
            ),
            price: initial_entry_price,
            order_type: OrderType::EntryInitialPartialShort,
        };
    }
    // 3commas DCA: post-base entries are safety orders handled above.
    Order::default()
}

pub fn calc_next_entry_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    _trailing_price_bundle: &TrailingPriceBundle,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Order {
    // 3commas DCA: base order (flat) or safety-order ladder. No trailing entries.
    if wallet_exposure_limit_with_allowance(bot_params) == 0.0 || state_params.balance <= 0.0 {
        return Order::default();
    }
    let allowed_wallet_exposure_limit = wallet_exposure_limit_with_allowance(bot_params);
    calc_grid_entry_short(
        exchange_params,
        state_params,
        bot_params,
        position,
        allowed_wallet_exposure_limit,
        dca_base_price,
        dca_entry_fills,
    )
}

pub fn calc_entries_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Vec<Order> {
    let mut entries = Vec::<Order>::new();
    let mut psize = position.size;
    let mut pprice = position.price;
    let mut bid = state_params.order_book.bid;
    // running DCA ladder state: base price stays FIXED, index advances per rung.
    let mut sim_base_price = dca_base_price;
    let mut sim_entry_fills = dca_entry_fills;
    for _ in 0..500 {
        let position_mod = Position {
            size: psize,
            price: pprice,
        };
        let mut state_params_mod = state_params.clone();
        state_params_mod.order_book.bid = bid;
        let mut entry = calc_next_entry_long(
            exchange_params,
            &state_params_mod,
            bot_params,
            &position_mod,
            &trailing_price_bundle,
            sim_base_price,
            sim_entry_fills,
        );
        entry.price = quantize_price(
            entry.price,
            exchange_params.price_step,
            RoundingMode::Nearest,
            "calc_entries_long::price",
        );
        entry.qty = quantize_qty(
            entry.qty,
            exchange_params.qty_step,
            RoundingMode::Nearest,
            "calc_entries_long::qty",
        );
        if entry.qty == 0.0 {
            break;
        }
        if !entries.is_empty() {
            if entry.order_type == OrderType::EntryTrailingNormalLong
                || entry.order_type == OrderType::EntryTrailingCroppedLong
            {
                break;
            }
            if entries[entries.len() - 1].price == entry.price {
                break;
            }
        }
        (psize, pprice) = calc_new_psize_pprice(
            psize,
            pprice,
            entry.qty,
            entry.price,
            exchange_params.qty_step,
        );
        bid = bid.min(entry.price);
        // advance DCA ladder state for the next rung.
        if sim_entry_fills < 1.0 {
            // this was the base order: its fill price becomes the fixed base.
            sim_base_price = entry.price;
            sim_entry_fills = 1.0;
        } else {
            sim_entry_fills += 1.0;
        }
        entries.push(entry);
    }
    entries
}

pub fn calc_entries_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Vec<Order> {
    let mut entries = Vec::<Order>::new();
    let mut psize = position.size;
    let mut pprice = position.price;
    let mut ask = state_params.order_book.ask;
    let mut sim_base_price = dca_base_price;
    let mut sim_entry_fills = dca_entry_fills;
    for _ in 0..500 {
        let position_mod = Position {
            size: psize,
            price: pprice,
        };
        let mut state_params_mod = state_params.clone();
        state_params_mod.order_book.ask = ask;
        let mut entry = calc_next_entry_short(
            exchange_params,
            &state_params_mod,
            bot_params,
            &position_mod,
            &trailing_price_bundle,
            sim_base_price,
            sim_entry_fills,
        );
        entry.price = quantize_price(
            entry.price,
            exchange_params.price_step,
            RoundingMode::Nearest,
            "calc_entries_short::price",
        );
        entry.qty = quantize_qty(
            entry.qty,
            exchange_params.qty_step,
            RoundingMode::Nearest,
            "calc_entries_short::qty",
        );
        if entry.qty == 0.0 {
            break;
        }
        if !entries.is_empty() {
            if entry.order_type == OrderType::EntryTrailingNormalShort
                || entry.order_type == OrderType::EntryTrailingCroppedShort
            {
                break;
            }
            if entries[entries.len() - 1].price == entry.price {
                break;
            }
        }
        (psize, pprice) = calc_new_psize_pprice(
            psize,
            pprice,
            entry.qty,
            entry.price,
            exchange_params.qty_step,
        );
        ask = ask.max(entry.price);
        if sim_entry_fills < 1.0 {
            sim_base_price = entry.price;
            sim_entry_fills = 1.0;
        } else {
            sim_entry_fills += 1.0;
        }
        entries.push(entry);
    }
    entries
}
