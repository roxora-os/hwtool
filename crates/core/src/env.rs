//! `/boot/armbianEnv.txt`: `key=value` lines that `boot.scr` imports with
//! `env import -t`. Comments, blank lines and unknown keys are kept as they
//! are, so a rewrite changes only the keys hwtool sets.

#[derive(Debug, Clone, PartialEq)]
pub struct BootEnv {
    lines: Vec<String>,
}

fn key_of(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    t.split_once('=').map(|(k, _)| k.trim())
}

impl BootEnv {
    pub fn parse(text: &str) -> Self {
        BootEnv {
            lines: text
                .lines()
                .map(|l| l.trim_end_matches('\r').to_string())
                .collect(),
        }
    }

    pub fn render(&self) -> String {
        let mut s = self.lines.join("\n");
        s.push('\n');
        s
    }

    /// Value of `key`. U-Boot imports the lines in order, so the last one wins.
    pub fn get(&self, key: &str) -> Option<String> {
        self.lines
            .iter()
            .rev()
            .find(|l| key_of(l) == Some(key))
            .and_then(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()))
    }

    /// Space-separated list value of `key` (overlays, overlay_prefix, ...).
    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|v| v.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Set `key`: the first line with that key is replaced and any later
    /// duplicates are dropped; a missing key is appended.
    pub fn set(&mut self, key: &str, value: &str) {
        let new_line = format!("{key}={value}");
        let mut out = Vec::with_capacity(self.lines.len() + 1);
        let mut done = false;
        for l in self.lines.drain(..) {
            if key_of(&l) == Some(key) {
                if !done {
                    out.push(new_line.clone());
                    done = true;
                }
            } else {
                out.push(l);
            }
        }
        if !done {
            out.push(new_line);
        }
        self.lines = out;
    }

    pub fn remove(&mut self, key: &str) {
        self.lines.retain(|l| key_of(l) != Some(key));
    }

    /// Set a list value; an empty list removes the key.
    pub fn set_list(&mut self, key: &str, values: &[String]) {
        if values.is_empty() {
            self.remove(key);
        } else {
            self.set(key, &values.join(" "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CM5: &str = "verbosity=1\nbootlogo=false\nconsole=both\noverlay_prefix=armsom-cm5-io rockchip-rk3576\nfdtfile=rockchip/rk3576-armsom-cm5-io.dtb\nrootdev=UUID=237fa9bf-b34e-4ece-bbc6-344533c82f1e\nrootfstype=ext4\n";

    #[test]
    fn round_trip_is_identical() {
        assert_eq!(BootEnv::parse(CM5).render(), CM5);
    }

    #[test]
    fn get_and_list() {
        let e = BootEnv::parse(CM5);
        assert_eq!(
            e.get("fdtfile").as_deref(),
            Some("rockchip/rk3576-armsom-cm5-io.dtb")
        );
        assert_eq!(
            e.list("overlay_prefix"),
            vec!["armsom-cm5-io", "rockchip-rk3576"]
        );
        assert!(e.list("overlays").is_empty());
    }

    #[test]
    fn set_appends_and_replaces() {
        let mut e = BootEnv::parse(CM5);
        e.set_list("overlays", &["uart1".into(), "i2c2".into()]);
        assert!(e
            .render()
            .ends_with("rootfstype=ext4\noverlays=uart1 i2c2\n"));
        e.set("overlays", "uart1");
        assert_eq!(e.get("overlays").as_deref(), Some("uart1"));
        e.set_list("overlays", &[]);
        assert_eq!(e.render(), CM5);
    }

    #[test]
    fn duplicates_collapse_and_comments_stay() {
        let mut e = BootEnv::parse("# keep\noverlays=a\nx=1\noverlays=b\n");
        assert_eq!(e.get("overlays").as_deref(), Some("b"));
        e.set("overlays", "c");
        assert_eq!(e.render(), "# keep\noverlays=c\nx=1\n");
    }
}
