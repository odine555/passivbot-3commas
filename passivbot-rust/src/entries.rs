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
    let base = bot_params.wallet_exposure_limit;
    if base <= 0.0 {
        base
    } else {
        base * (1.0 + bot_params.risk_we_excess_allowance_pct.max(0.0))
    }
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

pub fn calc_reentry_qty(
    entry_price: f64,
    balance: f64,
    position_size: f64,
    double_down_factor: f64,
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
    wallet_exposure_limit_cap: f64,
) -> f64 {
    let effective_wallet_exposure_limit = f64::min(
        wallet_exposure_limit_cap,
        wallet_exposure_limit_with_allowance(bot_params),
    );
    f64::max(
        calc_min_entry_qty(entry_price, &exchange_params),
        round_(
            f64::max(
                position_size.abs() * double_down_factor,
                cost_to_qty(balance, entry_price, exchange_params.c_mult)
                    * effective_wallet_exposure_limit
                    * bot_params.entry_initial_qty_pct,
            ),
            exchange_params.qty_step,
        ),
    )
}

fn calc_reentry_price_bid(
    position_price: f64,
    wallet_exposure: f64,
    order_book_bid: f64,
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
    entry_volatility_logrange_ema_1h: f64,
    wallet_exposure_limit_cap: f64,
) -> f64 {
    let effective_wallet_exposure_limit = f64::min(
        wallet_exposure_limit_cap,
        wallet_exposure_limit_with_allowance(bot_params),
    );
    let we_multiplier = if effective_wallet_exposure_limit > 0.0 {
        (wallet_exposure / effective_wallet_exposure_limit)
            * bot_params.entry_grid_spacing_we_weight
    } else {
        0.0
    };
    let log_multiplier =
        entry_volatility_logrange_ema_1h * bot_params.entry_grid_spacing_volatility_weight;
    let spacing_multiplier = 1.0 + we_multiplier + log_multiplier;
    let reentry_price = f64::min(
        round_dn(
            position_price
                * (1.0 - bot_params.entry_grid_spacing_pct * spacing_multiplier.max(0.0)),
            exchange_params.price_step,
        ),
        order_book_bid,
    );
    if reentry_price <= exchange_params.price_step {
        0.0
    } else {
        reentry_price
    }
}

fn calc_reentry_price_ask(
    position_price: f64,
    wallet_exposure: f64,
    order_book_ask: f64,
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
    entry_volatility_logrange_ema_1h: f64,
    wallet_exposure_limit_cap: f64,
) -> f64 {
    let effective_wallet_exposure_limit = f64::min(
        wallet_exposure_limit_cap,
        wallet_exposure_limit_with_allowance(bot_params),
    );
    let we_multiplier = if effective_wallet_exposure_limit > 0.0 {
        (wallet_exposure / effective_wallet_exposure_limit)
            * bot_params.entry_grid_spacing_we_weight
    } else {
        0.0
    };
    let log_multiplier =
        entry_volatility_logrange_ema_1h * bot_params.entry_grid_spacing_volatility_weight;
    let spacing_multiplier = 1.0 + we_multiplier + log_multiplier;
    let reentry_price = f64::max(
        round_up(
            position_price
                * (1.0 + bot_params.entry_grid_spacing_pct * spacing_multiplier.max(0.0)),
            exchange_params.price_step,
        ),
        order_book_ask,
    );
    if reentry_price <= exchange_params.price_step {
        0.0
    } else {
        reentry_price
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
    let mut initial_entry_qty = calc_initial_entry_qty(
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
    } else if position.size < initial_entry_qty {
        initial_entry_qty = round_(position.size, exchange_params.qty_step)
            .max(calc_min_entry_qty(initial_entry_price, &exchange_params));
    }
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
    if wallet_exposure >= effective_wallet_exposure_limit * 0.999 {
        return Order::default();
    }

    // normal re-entry
    let reentry_price = calc_reentry_price_bid(
        position.price,
        wallet_exposure,
        state_params.order_book.bid,
        exchange_params,
        bot_params,
        state_params.entry_volatility_logrange_ema_1h,
        effective_wallet_exposure_limit,
    );
    if reentry_price <= 0.0 {
        return Order::default();
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position.size,
            bot_params.entry_grid_double_down_factor,
            exchange_params,
            bot_params,
            effective_wallet_exposure_limit,
        ),
        initial_entry_qty,
    );
    let (_wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
        effective_wallet_exposure_limit,
    );
    if reentry_qty_cropped < reentry_qty {
        return Order {
            qty: reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryGridCroppedLong,
        };
    }
    Order {
        qty: reentry_qty,
        price: reentry_price,
        order_type: OrderType::EntryGridNormalLong,
    }
}

pub fn calc_next_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Order {
    // determines whether trailing or grid order, returns Order
    let base_wallet_exposure_limit = wallet_exposure_limit_with_allowance(bot_params);
    if base_wallet_exposure_limit == 0.0 || state_params.balance <= 0.0 {
        // no orders
        return Order::default();
    }
    let allowed_wallet_exposure_limit = wallet_exposure_limit_with_allowance(bot_params);
    if bot_params.entry_trailing_grid_ratio >= 1.0 || bot_params.entry_trailing_grid_ratio <= -1.0 {
        // return trailing only
        return calc_trailing_entry_long(
            &exchange_params,
            &state_params,
            &bot_params,
            &position,
            &trailing_price_bundle,
            allowed_wallet_exposure_limit,
        );
    } else if bot_params.entry_trailing_grid_ratio == 0.0 {
        // return grid only
        return calc_grid_entry_long(
            &exchange_params,
            &state_params,
            &bot_params,
            &position,
            allowed_wallet_exposure_limit,
            dca_base_price,
            dca_entry_fills,
        );
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position.size,
        position.price,
    );
    let wallet_exposure_ratio = if base_wallet_exposure_limit > 0.0 {
        wallet_exposure / base_wallet_exposure_limit
    } else {
        0.0
    };
    if bot_params.entry_trailing_grid_ratio > 0.0 {
        // trailing first
        if wallet_exposure_ratio < bot_params.entry_trailing_grid_ratio {
            let wallet_exposure_limit_cap = if wallet_exposure == 0.0 {
                allowed_wallet_exposure_limit
            } else {
                (base_wallet_exposure_limit * bot_params.entry_trailing_grid_ratio * 1.01)
                    .min(allowed_wallet_exposure_limit)
            };
            calc_trailing_entry_long(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                &trailing_price_bundle,
                wallet_exposure_limit_cap,
            )
        } else {
            // return grid order
            calc_grid_entry_long(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                allowed_wallet_exposure_limit,
                dca_base_price,
                dca_entry_fills,
            )
        }
    } else {
        // grid first
        if wallet_exposure_ratio < 1.0 + bot_params.entry_trailing_grid_ratio {
            let wallet_exposure_limit_cap = if wallet_exposure == 0.0 {
                allowed_wallet_exposure_limit
            } else {
                (base_wallet_exposure_limit * (1.0 + bot_params.entry_trailing_grid_ratio) * 1.01)
                    .min(allowed_wallet_exposure_limit)
            };
            calc_grid_entry_long(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                wallet_exposure_limit_cap,
                dca_base_price,
                dca_entry_fills,
            )
        } else {
            calc_trailing_entry_long(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                &trailing_price_bundle,
                allowed_wallet_exposure_limit,
            )
        }
    }
}

pub fn calc_trailing_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    wallet_exposure_limit_cap: f64,
) -> Order {
    let initial_entry_price = calc_ema_price_bid(
        exchange_params.price_step,
        state_params.order_book.bid,
        state_params.ema_bands.lower,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let mut initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    if position.size == 0.0 {
        // normal initial entry
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
    } else if position.size < initial_entry_qty {
        initial_entry_qty = round_(position.size, exchange_params.qty_step)
            .max(calc_min_entry_qty(initial_entry_price, &exchange_params));
    }
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
    if wallet_exposure > effective_wallet_exposure_limit * 0.999 {
        return Order::default();
    }
    let threshold_multiplier = if effective_wallet_exposure_limit > 0.0 {
        (wallet_exposure / effective_wallet_exposure_limit)
            * bot_params.entry_trailing_threshold_we_weight
    } else {
        0.0
    };
    let threshold_log_multiplier = state_params.entry_volatility_logrange_ema_1h
        * bot_params.entry_trailing_threshold_volatility_weight;
    let threshold_pct = bot_params.entry_trailing_threshold_pct
        * (1.0 + threshold_multiplier + threshold_log_multiplier).max(0.0);

    let retracement_multiplier = if effective_wallet_exposure_limit > 0.0 {
        (wallet_exposure / effective_wallet_exposure_limit)
            * bot_params.entry_trailing_retracement_we_weight
    } else {
        0.0
    };
    let retracement_log_multiplier = state_params.entry_volatility_logrange_ema_1h
        * bot_params.entry_trailing_retracement_volatility_weight;
    let retracement_pct = bot_params.entry_trailing_retracement_pct
        * (1.0 + retracement_multiplier + retracement_log_multiplier).max(0.0);
    let mut entry_triggered = false;
    let mut reentry_price = 0.0;
    if threshold_pct <= 0.0 {
        // means trailing entry immediately from pos change
        if retracement_pct > 0.0
            && trailing_price_bundle.max_since_min
                > trailing_price_bundle.min_since_open * (1.0 + retracement_pct)
        {
            entry_triggered = true;
            reentry_price = state_params.order_book.bid;
        }
    } else {
        // means trailing entry will activate only after a threshold
        if retracement_pct <= 0.0 {
            // close at threshold
            entry_triggered = true;
            reentry_price = f64::min(
                state_params.order_book.bid,
                round_dn(
                    position.price * (1.0 - threshold_pct),
                    exchange_params.price_step,
                ),
            );
        } else {
            // enter if both conditions are met
            if trailing_price_bundle.min_since_open < position.price * (1.0 - threshold_pct)
                && trailing_price_bundle.max_since_min
                    > trailing_price_bundle.min_since_open * (1.0 + retracement_pct)
            {
                entry_triggered = true;
                reentry_price = f64::min(
                    state_params.order_book.bid,
                    round_dn(
                        position.price * (1.0 - threshold_pct + retracement_pct),
                        exchange_params.price_step,
                    ),
                );
            }
        }
    }
    if !entry_triggered {
        return Order {
            qty: 0.0,
            price: 0.0,
            order_type: OrderType::EntryTrailingNormalLong,
        };
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position.size,
            bot_params.entry_trailing_double_down_factor,
            &exchange_params,
            &bot_params,
            effective_wallet_exposure_limit,
        ),
        initial_entry_qty,
    );
    let (_wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
        effective_wallet_exposure_limit,
    );
    if reentry_qty_cropped < reentry_qty {
        Order {
            qty: reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryTrailingCroppedLong,
        }
    } else {
        Order {
            qty: reentry_qty,
            price: reentry_price,
            order_type: OrderType::EntryTrailingNormalLong,
        }
    }
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
    let mut initial_entry_qty = calc_initial_entry_qty(
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
    } else if position_size_abs < initial_entry_qty {
        initial_entry_qty = round_(position_size_abs, exchange_params.qty_step)
            .max(calc_min_entry_qty(initial_entry_price, &exchange_params));
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position_size_abs,
        position.price,
    );
    let effective_wallet_exposure_limit = f64::min(
        wallet_exposure_limit_cap,
        wallet_exposure_limit_with_allowance(bot_params),
    );
    if wallet_exposure >= effective_wallet_exposure_limit * 0.999 {
        return Order::default();
    }

    // normal re-entry
    let reentry_price = calc_reentry_price_ask(
        position.price,
        wallet_exposure,
        state_params.order_book.ask,
        exchange_params,
        bot_params,
        state_params.entry_volatility_logrange_ema_1h,
        effective_wallet_exposure_limit,
    );
    if reentry_price <= 0.0 {
        return Order::default();
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position_size_abs,
            bot_params.entry_grid_double_down_factor,
            exchange_params,
            bot_params,
            effective_wallet_exposure_limit,
        ),
        initial_entry_qty,
    );
    let (_wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
        effective_wallet_exposure_limit,
    );
    if reentry_qty_cropped < reentry_qty {
        return Order {
            qty: -reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryGridCroppedShort,
        };
    }
    Order {
        qty: -reentry_qty,
        price: reentry_price,
        order_type: OrderType::EntryGridNormalShort,
    }
}

pub fn calc_trailing_entry_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    wallet_exposure_limit_cap: f64,
) -> Order {
    let initial_entry_price = calc_ema_price_ask(
        exchange_params.price_step,
        state_params.order_book.ask,
        state_params.ema_bands.upper,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let mut initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    let position_size_abs = position.size.abs();
    if position_size_abs == 0.0 {
        // normal initial entry
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
    } else if position_size_abs < initial_entry_qty {
        initial_entry_qty = round_(position_size_abs, exchange_params.qty_step)
            .max(calc_min_entry_qty(initial_entry_price, &exchange_params));
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position_size_abs,
        position.price,
    );
    let effective_wallet_exposure_limit = f64::min(
        wallet_exposure_limit_cap,
        wallet_exposure_limit_with_allowance(bot_params),
    );
    if wallet_exposure > effective_wallet_exposure_limit * 0.999 {
        return Order::default();
    }
    let threshold_multiplier = if effective_wallet_exposure_limit > 0.0 {
        (wallet_exposure / effective_wallet_exposure_limit)
            * bot_params.entry_trailing_threshold_we_weight
    } else {
        0.0
    };
    let threshold_log_multiplier = state_params.entry_volatility_logrange_ema_1h
        * bot_params.entry_trailing_threshold_volatility_weight;
    let threshold_pct = bot_params.entry_trailing_threshold_pct
        * (1.0 + threshold_multiplier + threshold_log_multiplier).max(0.0);

    let retracement_multiplier = if effective_wallet_exposure_limit > 0.0 {
        (wallet_exposure / effective_wallet_exposure_limit)
            * bot_params.entry_trailing_retracement_we_weight
    } else {
        0.0
    };
    let retracement_log_multiplier = state_params.entry_volatility_logrange_ema_1h
        * bot_params.entry_trailing_retracement_volatility_weight;
    let retracement_pct = bot_params.entry_trailing_retracement_pct
        * (1.0 + retracement_multiplier + retracement_log_multiplier).max(0.0);
    let mut entry_triggered = false;
    let mut reentry_price = 0.0;
    if threshold_pct <= 0.0 {
        // means trailing entry immediately from pos change
        if retracement_pct > 0.0
            && trailing_price_bundle.min_since_max
                < trailing_price_bundle.max_since_open * (1.0 - retracement_pct)
        {
            entry_triggered = true;
            reentry_price = state_params.order_book.ask;
        }
    } else {
        // means trailing entry will activate only after a threshold
        if retracement_pct <= 0.0 {
            // enter at threshold
            entry_triggered = true;
            reentry_price = f64::max(
                state_params.order_book.ask,
                round_up(
                    position.price * (1.0 + threshold_pct),
                    exchange_params.price_step,
                ),
            );
        } else {
            // enter if both conditions are met
            if trailing_price_bundle.max_since_open > position.price * (1.0 + threshold_pct)
                && trailing_price_bundle.min_since_max
                    < trailing_price_bundle.max_since_open * (1.0 - retracement_pct)
            {
                entry_triggered = true;
                reentry_price = f64::max(
                    state_params.order_book.ask,
                    round_up(
                        position.price * (1.0 + threshold_pct - retracement_pct),
                        exchange_params.price_step,
                    ),
                );
            }
        }
    }
    if !entry_triggered {
        return Order {
            qty: 0.0,
            price: 0.0,
            order_type: OrderType::EntryTrailingNormalShort,
        };
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position_size_abs,
            bot_params.entry_trailing_double_down_factor,
            &exchange_params,
            &bot_params,
            effective_wallet_exposure_limit,
        ),
        initial_entry_qty,
    );
    let (_wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
        effective_wallet_exposure_limit,
    );
    if reentry_qty_cropped < reentry_qty {
        Order {
            qty: -reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryTrailingCroppedShort,
        }
    } else {
        Order {
            qty: -reentry_qty,
            price: reentry_price,
            order_type: OrderType::EntryTrailingNormalShort,
        }
    }
}

pub fn calc_next_entry_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    dca_base_price: f64,
    dca_entry_fills: f64,
) -> Order {
    // determines whether trailing or grid order, returns Order
    let base_wallet_exposure_limit = wallet_exposure_limit_with_allowance(bot_params);
    if base_wallet_exposure_limit == 0.0 || state_params.balance <= 0.0 {
        // no orders
        return Order::default();
    }
    let allowed_wallet_exposure_limit = wallet_exposure_limit_with_allowance(bot_params);
    if bot_params.entry_trailing_grid_ratio >= 1.0 || bot_params.entry_trailing_grid_ratio <= -1.0 {
        // return trailing only
        return calc_trailing_entry_short(
            &exchange_params,
            &state_params,
            &bot_params,
            &position,
            &trailing_price_bundle,
            allowed_wallet_exposure_limit,
        );
    } else if bot_params.entry_trailing_grid_ratio == 0.0 {
        // return grid only
        return calc_grid_entry_short(
            &exchange_params,
            &state_params,
            &bot_params,
            &position,
            allowed_wallet_exposure_limit,
            dca_base_price,
            dca_entry_fills,
        );
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position.size.abs(),
        position.price,
    );
    let wallet_exposure_ratio = if base_wallet_exposure_limit > 0.0 {
        wallet_exposure / base_wallet_exposure_limit
    } else {
        0.0
    };
    if bot_params.entry_trailing_grid_ratio > 0.0 {
        // trailing first
        if wallet_exposure_ratio < bot_params.entry_trailing_grid_ratio {
            if wallet_exposure == 0.0 {
                calc_trailing_entry_short(
                    &exchange_params,
                    &state_params,
                    &bot_params,
                    &position,
                    &trailing_price_bundle,
                    allowed_wallet_exposure_limit,
                )
            } else {
                let wallet_exposure_limit_cap =
                    (base_wallet_exposure_limit * bot_params.entry_trailing_grid_ratio * 1.01)
                        .min(allowed_wallet_exposure_limit);
                calc_trailing_entry_short(
                    &exchange_params,
                    &state_params,
                    &bot_params,
                    &position,
                    &trailing_price_bundle,
                    wallet_exposure_limit_cap,
                )
            }
        } else {
            // return grid order
            calc_grid_entry_short(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                allowed_wallet_exposure_limit,
                dca_base_price,
                dca_entry_fills,
            )
        }
    } else {
        // grid first
        if wallet_exposure_ratio < 1.0 + bot_params.entry_trailing_grid_ratio {
            if wallet_exposure == 0.0 {
                calc_grid_entry_short(
                    &exchange_params,
                    &state_params,
                    &bot_params,
                    &position,
                    allowed_wallet_exposure_limit,
                    dca_base_price,
                    dca_entry_fills,
                )
            } else {
                let wallet_exposure_limit_cap = (base_wallet_exposure_limit
                    * (1.0 + bot_params.entry_trailing_grid_ratio)
                    * 1.01)
                    .min(allowed_wallet_exposure_limit);
                calc_grid_entry_short(
                    &exchange_params,
                    &state_params,
                    &bot_params,
                    &position,
                    wallet_exposure_limit_cap,
                    dca_base_price,
                    dca_entry_fills,
                )
            }
        } else {
            calc_trailing_entry_short(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                &trailing_price_bundle,
                allowed_wallet_exposure_limit,
            )
        }
    }
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
