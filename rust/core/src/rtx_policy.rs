//! When NVIDIA RTX HDR conversion applies and which driver tuning values
//! it uses.
use crate::config::Config;
use std::collections::BTreeMap;
pub fn marker(key: &str) -> String {
    format!("__runtime_override_{key}")
}
/// RTX HDR is an app or client opt-in, as in Vibepollo: a global `rtx_hdr`
/// alone never converts.
pub fn enabled(config: &Config) -> bool {
    config.boolean("rtx_hdr", false) && config.boolean(&marker("rtx_hdr"), false)
}
/// Driver tuning supplies defaults only. It never activates host conversion.
pub fn resolve(
    config: &Config,
    foreground_matches: bool,
    profile: &BTreeMap<String, i64>,
) -> Config {
    let mut result = config.clone();
    if !foreground_matches {
        for (key, value) in [
            ("rtx_hdr_contrast", 0),
            ("rtx_hdr_saturation", 0),
            ("rtx_hdr_middle_gray", 50),
        ] {
            result.values.insert(key.into(), value.to_string());
        }
    } else {
        for (key, value) in profile {
            if !config.boolean(&marker(key), false) {
                result.values.insert(key.clone(), value.to_string());
            }
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn driver_tuning_cannot_enable_hdr_or_override_explicit_app_settings() {
        let mut config =
            Config::parse("rtx_hdr=false\nrtx_hdr_contrast=14\nrtx_hdr_peak_brightness=1500\n")
                .unwrap();
        config
            .values
            .insert(marker("rtx_hdr_contrast"), "true".into());
        let profile = BTreeMap::from([
            ("rtx_hdr_contrast".into(), 27),
            ("rtx_hdr_saturation".into(), 51),
        ]);
        let resolved = resolve(&config, true, &profile);
        assert_eq!(resolved.integer("rtx_hdr_contrast", 0), 14);
        assert_eq!(resolved.integer("rtx_hdr_saturation", 0), 51);
        assert!(!resolved.boolean("rtx_hdr", false));
        let desktop = resolve(&resolved, false, &profile);
        assert_eq!(desktop.integer("rtx_hdr_contrast", 7), 0);
        assert_eq!(desktop.integer("rtx_hdr_middle_gray", 7), 50);
        assert_eq!(desktop.integer("rtx_hdr_peak_brightness", 0), 1500);
    }
}
