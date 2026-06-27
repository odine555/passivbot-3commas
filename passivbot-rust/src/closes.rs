use crate::types::{
    BotParams, ExchangeParams, Order, OrderType, Position, StateParams, TrailingPriceBundle,
};
use crate::utils::{quantize_price, quantize_qty, round_, round_dn, round_up, RoundingMode};

// 3commas DCA: single full-size take-profit close at avg * (1 + dca_take_profit_pct).
pub fn calc_next_close_long(
    exchange_params: &ExchangeParams,
    _state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    _trailing_price_bundle: &TrailingPriceBundle,
) -> Order {
    if position.size <= 0.0 {
        // no position
        return Order::default();
    }
    let tp_price = round_up(
        position.price * (1.0 + bot_params.dca_take_profit_pct),
        exchange_params.price_step,
    );
    Order {
        qty: -round_(position.size, exchange_params.qty_step),
        price: tp_price,
        order_type: OrderType::CloseGridLong,
    }
}

// 3commas DCA: single full-size take-profit close at avg * (1 - dca_take_profit_pct).
pub fn calc_next_close_short(
    exchange_params: &ExchangeParams,
    _state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    _trailing_price_bundle: &TrailingPriceBundle,
) -> Order {
    let position_size_abs = position.size.abs();
    if position_size_abs == 0.0 {
        // no position
        return Order::default();
    }
    let tp_price = round_dn(
        position.price * (1.0 - bot_params.dca_take_profit_pct),
        exchange_params.price_step,
    );
    Order {
        qty: round_(position_size_abs, exchange_params.qty_step),
        price: tp_price,
        order_type: OrderType::CloseGridShort,
    }
}

// 3commas DCA: exactly one full-size take-profit close (or empty if flat).
pub fn calc_closes_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
) -> Vec<Order> {
    let mut close = calc_next_close_long(
        exchange_params,
        state_params,
        bot_params,
        position,
        trailing_price_bundle,
    );
    close.price = quantize_price(
        close.price,
        exchange_params.price_step,
        RoundingMode::Nearest,
        "calc_closes_long::price",
    );
    close.qty = quantize_qty(
        close.qty,
        exchange_params.qty_step,
        RoundingMode::Nearest,
        "calc_closes_long::qty",
    );
    if close.qty == 0.0 {
        Vec::new()
    } else {
        vec![close]
    }
}

// 3commas DCA: exactly one full-size take-profit close (or empty if flat).
pub fn calc_closes_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
) -> Vec<Order> {
    let mut close = calc_next_close_short(
        exchange_params,
        state_params,
        bot_params,
        position,
        trailing_price_bundle,
    );
    close.price = quantize_price(
        close.price,
        exchange_params.price_step,
        RoundingMode::Nearest,
        "calc_closes_short::price",
    );
    close.qty = quantize_qty(
        close.qty,
        exchange_params.qty_step,
        RoundingMode::Nearest,
        "calc_closes_short::qty",
    );
    if close.qty == 0.0 {
        Vec::new()
    } else {
        vec![close]
    }
}
