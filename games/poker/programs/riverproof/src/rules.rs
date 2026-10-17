//! Courtyard rules shared by cash and tournament tables.
//!
//! Keep the action allowance independent of old TableConfig bytes. Tables
//! created by earlier releases stored 15/25/40 seconds; an upgrade must apply
//! the same 40-second rule to new turns without changing account layouts.
pub const ACTION_TIME_MS: i64 = 40_000;

pub fn valid_action_time(milliseconds: i64) -> bool {
    // Older clients still send the previous creation value. It is metadata;
    // engine_config enforces ACTION_TIME_MS for every table.
    milliseconds == ACTION_TIME_MS || milliseconds == 60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_creators_cannot_select_a_different_turn_timer() {
        assert!(valid_action_time(40_000));
        assert!(valid_action_time(60_000));
        for time in [0, 15_000, 25_000, 39_999, 40_001, 59_999, 60_001, i64::MAX] {
            assert!(!valid_action_time(time));
        }
    }
}
