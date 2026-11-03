//! What AI answers cost. The desktop app reports the tokens each answer used; the server turns
//! them into money with its own price list, so a person cannot make an answer look cheaper by
//! sending a cost.

/// Most one person may spend per calendar month when the firm has not set its own limit, in cents.
/// Overridden for the whole server by `DEFAULT_MONTHLY_LIMIT_CENTS`.
pub const DEFAULT_LIMIT_CENTS: i64 = 1_500;

/// A model's prices in millionths of a dollar per million tokens (so $2 is 2,000,000).
struct Price {
    input: u128,
    output: u128,
}

/// Anthropic list prices as of November 2026. A model that is not listed is charged at the
/// highest price, so an unfamiliar name never makes answers free.
fn price(model: &str, input_tokens: u64) -> Price {
    let name = model.rsplit('.').next().unwrap_or(model);
    match name {
        // Prompts longer than 100,000 tokens cost five times as much on Haiku 5.5.
        "claude-haiku-5-5" if input_tokens <= 100_000 => Price {
            input: 100_000,
            output: 500_000,
        },
        "claude-haiku-5-5" => Price {
            input: 500_000,
            output: 2_500_000,
        },
        "claude-sonnet-5-5" | "claude-sonnet-5" => Price {
            input: 2_000_000,
            output: 10_000_000,
        },
        "claude-opus-5-5" => Price {
            input: 4_000_000,
            output: 20_000_000,
        },
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => Price {
            input: 5_000_000,
            output: 25_000_000,
        },
        _ => Price {
            input: 10_000_000,
            output: 50_000_000,
        },
    }
}

/// Cost of one answer in millionths of a dollar, rounded up.
pub fn cost_micros(model: &str, input_tokens: u64, output_tokens: u64) -> i64 {
    let price = price(model, input_tokens);
    let total = u128::from(input_tokens) * price.input + u128::from(output_tokens) * price.output;
    total.div_ceil(1_000_000).min(i64::MAX as u128) as i64
}

/// The limit when a firm has not set one, in millionths of a dollar.
pub fn default_limit_micros() -> i64 {
    std::env::var("DEFAULT_MONTHLY_LIMIT_CENTS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|cents| *cents >= 0)
        .unwrap_or(DEFAULT_LIMIT_CENTS)
        .saturating_mul(10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_priced_from_the_published_rates() {
        // 3,500 tokens in and 700 out, the app's usual answer.
        assert_eq!(cost_micros("claude-sonnet-5-5", 3_500, 700), 14_000); // 1.4 cents
        assert_eq!(cost_micros("claude-haiku-5-5", 3_500, 700), 700); // 0.07 cents
        assert_eq!(cost_micros("claude-opus-5-5", 3_500, 700), 28_000);
        // Bedrock names carry a prefix.
        assert_eq!(
            cost_micros("us.anthropic.claude-sonnet-5-5", 3_500, 700),
            14_000
        );
    }

    #[test]
    fn long_prompts_on_haiku_cost_more_and_unknown_models_cost_the_most() {
        assert_eq!(cost_micros("claude-haiku-5-5", 100_000, 0), 10_000);
        assert_eq!(cost_micros("claude-haiku-5-5", 100_001, 0), 50_001);
        assert_eq!(cost_micros("mystery-model", 1_000_000, 0), 10_000_000);
    }

    #[test]
    fn nothing_overflows_and_a_tiny_answer_is_never_free() {
        assert_eq!(cost_micros("claude-haiku-5-5", 1, 0), 1);
        assert!(cost_micros("mystery-model", u64::MAX, u64::MAX) > 0);
    }
}
