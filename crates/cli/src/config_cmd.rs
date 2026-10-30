//! `liberado config …` — kept out of `lib.rs` for the function-complexity ratchet.

pub fn run(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
        Some("check") => liberado_server::config_check(None),
        Some("explain") => run_explain(args),
        Some("vapid-keys") => run_vapid_keys(),
        _ => Err("usage: liberado config <check|explain|vapid-keys>".into()),
    }
}

fn run_explain(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let component = args.next();
    let tool = args.next();
    let path = args.next();
    match (component, tool, path) {
        (Some(c), Some(t), Some(p)) => liberado_server::explain_write(None, &c, &t, &p),
        _ => Err(
            "usage: liberado config explain <component> <mcp:tool> <vault/path.md>\n  \
             e.g. liberado config explain dispatcher turbovault:write_note Learning/x.md"
                .into(),
        ),
    }
}

fn run_vapid_keys() -> Result<(), Box<dyn std::error::Error>> {
    match liberado_notify::web_push::vapid_env_lines() {
        Ok(lines) => {
            print!("{lines}");
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}
