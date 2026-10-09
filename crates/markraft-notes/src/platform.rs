#[cfg(target_os = "macos")]
pub mod locale;

#[cfg(not(target_os = "macos"))]
pub mod locale {
    pub fn preferred_language(supported: &[&str]) -> String {
        let requested = std::env::var("LANG").unwrap_or_else(|_| "en".to_owned());
        resolve(
            supported,
            Some(&[requested.split('.').next().unwrap_or("en")]),
        )
    }
    pub fn resolve(supported: &[&str], preferences: Option<&[&str]>) -> String {
        for preference in preferences.unwrap_or_default() {
            let normalized = preference.replace('_', "-");
            if let Some(found) = supported.iter().find(|candidate| **candidate == normalized) {
                return (*found).to_owned();
            }
            let language = normalized.split('-').next().unwrap_or("en");
            if let Some(found) = supported.iter().find(|candidate| **candidate == language) {
                return (*found).to_owned();
            }
        }
        supported.first().copied().unwrap_or("en").to_owned()
    }
}

/// Seconds this machine's clock stands ahead of UTC, including whatever daylight
/// saving is in force. Timestamps are stored in UTC; a date shown to the user has
/// to be the one on their calendar, so it is read through this.
pub fn local_utc_offset() -> i64 {
    i64::from(chrono::Local::now().offset().local_minus_utc())
}
