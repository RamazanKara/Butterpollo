//! Release version ordering, shared by the host's updater, the Playnite
//! plugin installer and setup (which includes this file by path).

/// Whether release `a` is newer than `b`: dotted numbers, then a final
/// release above its pre-releases (`2.0.0` > `2.0.0-rc.2` > `2.0.0-rc.1`).
/// A leading `v` and `+build` metadata are ignored.
pub fn newer(a: &str, b: &str) -> bool {
    fn parts(version: &str) -> (Vec<u64>, Option<Vec<String>>) {
        let version = version.trim().trim_start_matches(['v', 'V']);
        let version = version.split('+').next().unwrap_or(version);
        let (release, pre) = match version.split_once('-') {
            Some((release, pre)) => (release, Some(pre.split('.').map(str::to_owned).collect())),
            None => (version, None),
        };
        (
            release.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    let ((a_release, a_pre), (b_release, b_pre)) = (parts(a), parts(b));
    for i in 0..a_release.len().max(b_release.len()) {
        let (x, y) = (
            a_release.get(i).copied().unwrap_or(0),
            b_release.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    match (a_pre, b_pre) {
        (None, Some(_)) => true,
        (Some(_), None) | (None, None) => false,
        (Some(a), Some(b)) => {
            for (x, y) in a.iter().zip(&b) {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if order != std::cmp::Ordering::Equal {
                    return order == std::cmp::Ordering::Greater;
                }
            }
            a.len() > b.len()
        }
    }
}
#[cfg(test)]
mod version_tests {
    #[test]
    fn release_versions_compare_like_semver() {
        use super::newer;
        assert!(newer("v2.0.0", "2.0.0-rc.1"));
        assert!(newer("2.0.0-rc.2", "2.0.0-rc.1"));
        assert!(newer("2.0.0-rc.10", "2.0.0-rc.9"));
        assert!(newer("2.1.0-beta", "2.0.0"));
        assert!(!newer("2.0.0-rc.1", "2.0.0-rc.1"));
        assert!(!newer("1.9.9", "2.0.0-rc.1"));
        assert!(!newer("2.0.0-rc.1", "v2.0.0"));
    }
}
