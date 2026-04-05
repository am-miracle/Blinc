//! Time formatting utility

/// Format milliseconds as "M:SS" or "H:MM:SS"
pub fn format_time_ms(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_seconds() {
        assert_eq!(format_time_ms(0), "0:00");
        assert_eq!(format_time_ms(5000), "0:05");
        assert_eq!(format_time_ms(65000), "1:05");
        assert_eq!(format_time_ms(3661000), "1:01:01");
    }
}
