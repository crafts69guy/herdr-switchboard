//! Parser for user-defined Git menu rows.

use super::Custom;

/// Parse a `menu.conf`: one `key|icon|label|shell command` per line. Blank lines,
/// `#` comments, and rows with an empty key or command are skipped, as the fzf
/// menu did. Only the first character of the key field is the mnemonic.
pub(super) fn parse_menu_conf(text: &str) -> Vec<Custom> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.splitn(4, '|');
            let key = parts.next()?.trim().chars().next()?;
            let icon = parts.next().unwrap_or("").trim().to_string();
            let label = parts.next().unwrap_or("").trim().to_string();
            let cmd = parts.next().unwrap_or("").trim().to_string();
            if cmd.is_empty() {
                return None;
            }
            Some(Custom {
                key,
                icon,
                label,
                cmd,
            })
        })
        .collect()
}
