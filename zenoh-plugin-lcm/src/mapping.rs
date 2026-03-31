use zenoh::{
    internal::bail,
    key_expr::KeyExpr,
    Result as ZResult,
};

/// Map an LCM channel name to a Zenoh key expression.
///
/// Result: `{prefix}/{channel}`
///
/// LCM channels typically use `UPPER_SNAKE_CASE` with no special characters.
/// If a channel name contains `/`, it becomes a hierarchical Zenoh key (desirable).
/// Zenoh-reserved characters (`#`, `$`, `*`) are rejected with an error.
pub fn lcm_channel_to_key_expr<'a>(
    channel: &'a str,
    prefix: &str,
) -> ZResult<KeyExpr<'a>> {
    // Reject channels with Zenoh wildcard/reserved characters.
    if channel.contains('*') || channel.contains('#') || channel.contains('$') {
        bail!(
            "LCM channel '{}' contains Zenoh-reserved characters (* # $)",
            channel
        );
    }
    if channel.is_empty() {
        bail!("LCM channel name is empty");
    }

    let full = format!("{prefix}/{channel}");
    let ke: KeyExpr = full.try_into()?;
    Ok(ke.into_owned().into())
}

/// Extract the LCM channel name from a Zenoh key expression by stripping the prefix.
///
/// Given key `lcm/SENSOR_DATA` and prefix `lcm`, returns `SENSOR_DATA`.
pub fn key_expr_to_lcm_channel<'a>(
    key_expr: &'a KeyExpr<'_>,
    prefix: &str,
) -> ZResult<&'a str> {
    let key_str = key_expr.as_str();
    let expected_prefix = format!("{prefix}/");

    if let Some(channel) = key_str.strip_prefix(&expected_prefix) {
        if channel.is_empty() {
            bail!(
                "Zenoh key '{}' has empty channel after prefix '{}'",
                key_str,
                prefix
            );
        }
        Ok(channel)
    } else {
        bail!(
            "Zenoh key '{}' does not start with expected prefix '{}'",
            key_str,
            expected_prefix
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_to_key_expr() {
        let ke = lcm_channel_to_key_expr("SENSOR_IMU", "lcm").unwrap();
        assert_eq!(ke.as_str(), "lcm/SENSOR_IMU");
    }

    #[test]
    fn test_channel_to_key_expr_custom_prefix() {
        let ke = lcm_channel_to_key_expr("ROBOT_STATE", "robot/lcm").unwrap();
        assert_eq!(ke.as_str(), "robot/lcm/ROBOT_STATE");
    }

    #[test]
    fn test_channel_with_slash() {
        let ke = lcm_channel_to_key_expr("sensors/imu", "lcm").unwrap();
        assert_eq!(ke.as_str(), "lcm/sensors/imu");
    }

    #[test]
    fn test_channel_with_wildcard_rejected() {
        assert!(lcm_channel_to_key_expr("SENSOR_*", "lcm").is_err());
    }

    #[test]
    fn test_empty_channel_rejected() {
        assert!(lcm_channel_to_key_expr("", "lcm").is_err());
    }

    #[test]
    fn test_key_expr_to_channel() {
        let ke: KeyExpr = "lcm/SENSOR_IMU".try_into().unwrap();
        let channel = key_expr_to_lcm_channel(&ke, "lcm").unwrap();
        assert_eq!(channel, "SENSOR_IMU");
    }

    #[test]
    fn test_key_expr_to_channel_hierarchical() {
        let ke: KeyExpr = "lcm/sensors/imu".try_into().unwrap();
        let channel = key_expr_to_lcm_channel(&ke, "lcm").unwrap();
        assert_eq!(channel, "sensors/imu");
    }

    #[test]
    fn test_key_expr_wrong_prefix() {
        let ke: KeyExpr = "mqtt/SENSOR".try_into().unwrap();
        assert!(key_expr_to_lcm_channel(&ke, "lcm").is_err());
    }

    // --- Reserved character tests ---

    #[test]
    fn test_channel_with_hash_rejected() {
        assert!(lcm_channel_to_key_expr("SENSOR#1", "lcm").is_err());
    }

    #[test]
    fn test_channel_with_dollar_rejected() {
        assert!(lcm_channel_to_key_expr("$SENSOR", "lcm").is_err());
    }

    #[test]
    fn test_channel_with_all_reserved_rejected() {
        assert!(lcm_channel_to_key_expr("*#$", "lcm").is_err());
    }

    // --- Key expression edge cases ---

    #[test]
    fn test_key_expr_prefix_only_rejected_by_zenoh() {
        // "lcm/" is not a valid Zenoh key expression (trailing slash forbidden),
        // so it's impossible to even construct — Zenoh rejects it before we do.
        let result: Result<KeyExpr, _> = "lcm/".try_into();
        assert!(result.is_err());
    }

    #[test]
    fn test_channel_numeric_only() {
        let ke = lcm_channel_to_key_expr("12345", "lcm").unwrap();
        assert_eq!(ke.as_str(), "lcm/12345");
    }

    #[test]
    fn test_channel_single_char() {
        let ke = lcm_channel_to_key_expr("A", "lcm").unwrap();
        assert_eq!(ke.as_str(), "lcm/A");
    }

    #[test]
    fn test_channel_deeply_hierarchical() {
        let ke = lcm_channel_to_key_expr("a/b/c/d/e", "lcm").unwrap();
        assert_eq!(ke.as_str(), "lcm/a/b/c/d/e");

        // Round-trip.
        let channel = key_expr_to_lcm_channel(&ke, "lcm").unwrap();
        assert_eq!(channel, "a/b/c/d/e");
    }

    #[test]
    fn test_key_expr_to_channel_prefix_mismatch_partial() {
        // Key starts with "lcm" but doesn't have the slash separator.
        let ke: KeyExpr = "lcm_extra/SENSOR".try_into().unwrap();
        assert!(key_expr_to_lcm_channel(&ke, "lcm").is_err());
    }
}
