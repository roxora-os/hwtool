//! Wi-Fi regulatory country.
//!
//! cfg80211 takes the country from its ieee80211_regdom module option and
//! from `iw reg set`. The ROCK 4D's Wi-Fi, the aic8800 USB driver (DKMS
//! package aic8800-usb, module aic8800_fdrv_usb), does not follow it: in its
//! source (USB/driver_fw/drivers/aic8800/aic8800_fdrv/rwnx_mod_params.c) the
//! custregd parameter defaults to on, the wiphy is REGULATORY_WIPHY_SELF_MANAGED
//! and gets the driver's own rules for the country compiled in
//! (CONFIG_COUNTRY_CODE "00" in its Makefile), with "CAUTION: USING
//! PERMISSIVE CUSTOM REGULATORY RULES"; its country_code module parameter is
//! overwritten at load. The only switch is custregd=0, which makes it leave
//! regulation to cfg80211. hwtool sets that only when asked
//! (--aic8800-follow-country), because it has not been tried on hardware.

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::exec::{argv, Executor};
use crate::plan::{Action, Plan};
use crate::root::Root;

pub const MODPROBE: &str = "/etc/modprobe.d/hwtool-wifi.conf";
pub const AIC_MODULE: &str = "aic8800_fdrv_usb";

/// ISO 3166-1 alpha-2 codes (officially assigned), plus "00" (world).
const ISO3166: &str =
    "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL \
BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK \
DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU \
GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ \
LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY \
MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU \
RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO \
TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW";

pub fn valid_country(c: &str) -> bool {
    c == "00" || ISO3166.split_whitespace().any(|x| x == c)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    /// The global regulatory domain from `iw reg get`.
    pub country: Option<String>,
    /// Self-managed wiphys and their country: (phy, country).
    pub self_managed: Vec<(String, String)>,
    /// The country hwtool set in /etc/modprobe.d/hwtool-wifi.conf.
    pub configured: Option<String>,
    /// The aic8800 driver is installed (DKMS) or loaded.
    pub aic8800: bool,
    pub aic8800_follows_country: bool,
    pub iw: bool,
    pub notes: Vec<String>,
}

/// Parse `iw reg get`: "global" then "country XX: ..." and per-phy blocks
/// "phy#0 (self-managed)" followed by "country XX: ...".
pub fn parse_reg(out: &str) -> (Option<String>, Vec<(String, String)>) {
    let mut global = None;
    let mut phys = Vec::new();
    let mut current: Option<String> = None;
    for l in out.lines() {
        let t = l.trim();
        if t == "global" {
            current = None;
        } else if t.starts_with("phy#") {
            current = Some(t.split_whitespace().next().unwrap_or(t).to_string());
        } else if let Some(rest) = t.strip_prefix("country ") {
            let c = rest.split(':').next().unwrap_or("").trim().to_string();
            match &current {
                None if global.is_none() => global = Some(c),
                Some(p) => phys.push((p.clone(), c)),
                None => {}
            }
        }
    }
    (global, phys)
}

fn aic8800(root: &Root) -> bool {
    root.exists(&format!("/sys/module/{AIC_MODULE}"))
        || root
            .list_dir("/var/lib/dkms")
            .iter()
            .any(|d| d.starts_with("aic8800"))
}

pub fn status(root: &Root, exec: &mut dyn Executor) -> Result<Status> {
    let iw = root.has_program("iw");
    let (country, self_managed) = if iw {
        parse_reg(
            &exec
                .capture(&argv(&["iw", "reg", "get"]))
                .unwrap_or_default(),
        )
    } else {
        (None, Vec::new())
    };
    let conf = root.read_opt(MODPROBE).unwrap_or_default();
    let configured = conf
        .lines()
        .find_map(|l| {
            l.split_whitespace()
                .find_map(|w| w.strip_prefix("ieee80211_regdom="))
        })
        .map(str::to_string);
    let follows = conf
        .lines()
        .any(|l| l.contains(AIC_MODULE) && l.contains("custregd=0"));
    let aic = aic8800(root);
    let mut notes = Vec::new();
    if !iw {
        notes.push(
            "iw is not installed (package iw); hwtool installs it when you set a country.".into(),
        );
    }
    if aic && !follows {
        notes.push(
            "The aic8800 Wi-Fi driver uses its own permissive rules (country 00) and ignores this \
             setting unless it is told to follow it (--aic8800-follow-country)."
                .into(),
        );
    }
    Ok(Status {
        country,
        self_managed,
        configured,
        aic8800: aic,
        aic8800_follows_country: follows,
        iw,
        notes,
    })
}

pub fn set_plan(
    root: &Root,
    exec: &mut dyn Executor,
    country: &str,
    aic_follow: bool,
) -> Result<Plan> {
    let c = country.to_ascii_uppercase();
    if !valid_country(&c) {
        bail!("{country} is not an ISO 3166 country code (two letters, for example DE, US, CN)");
    }
    let st = status(root, exec)?;
    if aic_follow && !st.aic8800 {
        bail!("--aic8800-follow-country: the aic8800 driver is not on this system");
    }
    let mut p = Plan::new(format!("Set the Wi-Fi country to {c}"));
    if !st.iw {
        let sim = argv(&[
            "apt-get",
            "-s",
            "-y",
            "--no-install-recommends",
            "install",
            "iw",
        ]);
        let out = exec.capture(&sim).context("apt cannot resolve iw")?;
        if out.lines().any(|l| l.starts_with("Remv ")) {
            bail!("installing iw would remove packages; hwtool stops here");
        }
        p.run(
            "Install iw",
            &["apt-get", "install", "-y", "--no-install-recommends", "iw"],
        );
    }
    let mut conf = format!(
        "# Written by hwtool wifi country. The Wi-Fi regulatory country.\noptions cfg80211 ieee80211_regdom={c}\n"
    );
    if aic_follow || st.aic8800_follows_country {
        conf.push_str(&format!(
            "# Let the aic8800 driver follow it instead of its own permissive rules.\noptions {AIC_MODULE} custregd=0\n"
        ));
    }
    p.step(
        format!("Back up {MODPROBE}"),
        Action::Backup {
            path: MODPROBE.into(),
        },
    );
    p.step(
        format!("Write {MODPROBE}"),
        Action::WriteFile {
            path: MODPROBE.into(),
            content: conf,
        },
    );
    p.run(format!("Set {c} now"), &["iw", "reg", "set", &c]);
    if st.aic8800 && !(aic_follow || st.aic8800_follows_country) {
        p.warnings.push(
            "The aic8800 Wi-Fi (ROCK 4D) keeps its own permissive rules and does not follow this \
             country. Add --aic8800-follow-country to change that (not tried on hardware yet)."
                .into(),
        );
    }
    if aic_follow {
        p.notices.push(format!(
            "{AIC_MODULE} uses the setting after the next reboot."
        ));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countries() {
        assert!(
            valid_country("DE")
                && valid_country("US")
                && valid_country("CN")
                && valid_country("00")
        );
        assert!(!valid_country("XX") && !valid_country("D") && !valid_country("DEU"));
        assert_eq!(ISO3166.split_whitespace().count(), 249);
    }

    #[test]
    fn reg_get() {
        let out = "global\ncountry DE: DFS-ETSI\n\t(2400 - 2483 @ 40), (N/A, 20), (N/A)\n\nphy#0 (self-managed)\ncountry 00: DFS-UNSET\n\t(2402 - 2482 @ 40), (6, 20), (N/A)\n";
        let (g, p) = parse_reg(out);
        assert_eq!(g.as_deref(), Some("DE"));
        assert_eq!(p, vec![("phy#0".to_string(), "00".to_string())]);
    }
}
