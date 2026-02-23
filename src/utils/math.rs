use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;

pub fn calculate_kelly_size(
    win_rate: f64,
    price: Decimal,
    portfolio_balance: Decimal,
) -> Decimal {
    let p = win_rate;
    let p_price = price.to_f64().unwrap_or(0.5);
    
    // Safety check for impossible odds
    if p_price <= 0.0 || p_price >= 1.0 {
        return dec!(0.0);
    }
    
    // Given the price (e.g., $0.60), the payout is $1.00.
    // The decimal odds (b) representing the net profit per $1 wagered is:
    // b = (1.0 - p_price) / p_price
    let b = (1.0 - p_price) / p_price;
    let q = 1.0 - p;
    
    // Kelly fraction f* = p - (q / b)
    let f_star = p - (q / b);
    
    if f_star <= 0.0 {
        return dec!(0.0); // Edge case or negative expected value (do not trade)
    }
    
    // Half-kelly for safety
    let half_f = f_star * 0.5;
    
    let size_f64 = half_f * portfolio_balance.to_f64().unwrap_or(0.0);
    Decimal::from_f64_retain(size_f64).unwrap_or(dec!(0.0))
}
