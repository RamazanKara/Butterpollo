//! Windows Firewall rules that keep Moonlight from reaching the host. The
//! installer allows the host on every network, but a block rule (left when
//! Windows' "allow access" prompt was dismissed, for instance on a network
//! marked public) overrides any allow rule.

use anyhow::Result;
use windows::{
    Win32::{
        NetworkManagement::WindowsFirewall::*,
        System::{Com::*, Ole::IEnumVARIANT, Variant::*},
    },
    core::Interface,
};

/// What Windows Firewall does to inbound traffic for `program` on the networks
/// the PC is on now: each enabled block rule naming it, or the absence of any
/// allow rule. Empty when nothing stands in the way or the firewall is off.
pub fn problems(program: &std::path::Path) -> Result<Vec<String>> {
    // SAFETY: COM is initialised on this thread in some mode before the first COM call, and the
    // Rules enumerator yields VT_DISPATCH variants, so `pdispVal` is read before VariantClear.
    unsafe {
        // Already initialised in another mode is fine for this in-process object.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)?;
        let active = policy.CurrentProfileTypes()?;
        let mut enabled = false;
        for profile in [
            NET_FW_PROFILE2_DOMAIN,
            NET_FW_PROFILE2_PRIVATE,
            NET_FW_PROFILE2_PUBLIC,
        ] {
            if active & profile.0 != 0 && policy.get_FirewallEnabled(profile)?.as_bool() {
                enabled = true;
            }
        }
        if !enabled {
            return Ok(vec![]);
        }
        let program = program.to_string_lossy().to_lowercase();
        let items: IEnumVARIANT = policy.Rules()?._NewEnum()?.cast()?;
        let (mut problems, mut allowed) = (Vec::new(), false);
        loop {
            let mut item = [VARIANT::default()];
            let mut fetched = 0;
            if items.Next(&mut item, &mut fetched).is_err() || fetched == 0 {
                break;
            }
            let rule = item[0]
                .Anonymous
                .Anonymous
                .Anonymous
                .pdispVal
                .as_ref()
                .and_then(|dispatch| dispatch.cast::<INetFwRule>().ok());
            let _ = VariantClear(&mut item[0]);
            let Some(rule) = rule else {
                continue;
            };
            let applies = rule
                .ApplicationName()
                .is_ok_and(|name| name.to_string().to_lowercase() == program)
                && rule.Direction().is_ok_and(|d| d == NET_FW_RULE_DIR_IN)
                && rule.Enabled().is_ok_and(|e| e.as_bool())
                && rule.Profiles().is_ok_and(|p| p & active != 0);
            if !applies {
                continue;
            }
            match rule.Action() {
                Ok(NET_FW_ACTION_BLOCK) => problems.push(format!(
                    "Windows Firewall rule \"{}\" blocks incoming connections to the host",
                    rule.Name().map(|n| n.to_string()).unwrap_or_default()
                )),
                Ok(NET_FW_ACTION_ALLOW) => allowed = true,
                _ => {}
            }
        }
        if !allowed && problems.is_empty() {
            problems.push(
                "no Windows Firewall rule allows incoming connections to the host on this network"
                    .into(),
            );
        }
        Ok(problems)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_program_without_rules_is_reported_when_the_firewall_is_on() {
        let problems = super::problems(std::path::Path::new(
            r"C:\no\such\butterpollo-firewall-probe.exe",
        ))
        .unwrap();
        // Either the firewall is off on this network, or nothing allows the probe.
        assert!(problems.len() <= 1);
        let installed = std::path::Path::new(r"C:\Program Files\Butterpollo\butterpollo.exe");
        if installed.exists() {
            // The installer's rule allows the installed host.
            assert_eq!(super::problems(installed).unwrap(), Vec::<String>::new());
        }
        assert!(
            problems
                .iter()
                .all(|p| p.starts_with("no Windows Firewall rule"))
        );
    }
}
