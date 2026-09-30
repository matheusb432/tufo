use std::path::Path;

use anyhow::{Context as _, Result, bail};

pub(super) fn format_catalogs(workspace_root: &Path, check: bool) -> Result<()> {
    for relative_path in [
        "crates/tufo-gui/lang/tufo-gui.pot",
        "crates/tufo-gui/lang/pt_BR/LC_MESSAGES/tufo-gui.po",
    ] {
        let path = workspace_root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let formatted = format_catalog(&source);
        if source == formatted {
            continue;
        }
        if check {
            bail!("{relative_path} needs formatting; run `just translations-fmt`");
        }
        std::fs::write(&path, formatted).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

fn format_catalog(source: &str) -> String {
    let mut formatted = String::with_capacity(source.len());
    for line in source.lines() {
        let Some(references) = line.strip_prefix("#:") else {
            formatted.push_str(line);
            formatted.push('\n');
            continue;
        };
        let mut filenames = Vec::new();
        for reference in references.split_whitespace() {
            let filename = reference
                .rsplit_once(':')
                .filter(|(_, number)| {
                    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                })
                .map_or(reference, |(filename, _)| filename);
            if !filenames.contains(&filename) {
                filenames.push(filename);
            }
        }
        formatted.push_str("#:");
        for filename in filenames {
            formatted.push(' ');
            formatted.push_str(filename);
        }
        formatted.push('\n');
    }
    formatted
}
