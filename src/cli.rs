//! The `happyview` binary's subcommands. Parsed by hand like this crate's
//! other binaries: there are few of them and none takes more than a path and
//! a repeated flag.

use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
usage:
  happyview                                   serve; HAPPYVIEW_PROJECT names a project to serve
  happyview check <root> [--plugin <dir>]...  build the project at <root> without serving it
  happyview dev <root>                        serve the project at <root> locally, rebuilding on save";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Serve,
    Check(CheckArgs),
    Dev { root: PathBuf },
    Help,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckArgs {
    pub root: PathBuf,
    /// Plugin directories to load interpreters from, each holding a
    /// `manifest.json`. Empty means `$PLUGIN_DIR`, then the project's own.
    pub plugin_dirs: Vec<PathBuf>,
}

/// The subcommand, or a message ending in the usage, ready to print.
pub fn parse<I, S>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    parse_inner(args.into_iter().map(Into::into)).map_err(|message| format!("{message}\n\n{USAGE}"))
}

fn parse_inner(mut args: impl Iterator<Item = OsString>) -> Result<Command, String> {
    let Some(command) = args.next() else {
        return Ok(Command::Serve);
    };
    match command.to_string_lossy().as_ref() {
        "check" => {
            let mut check = CheckArgs::default();
            let mut root = None;
            while let Some(arg) = args.next() {
                let text = arg.to_string_lossy().into_owned();
                match text.as_str() {
                    "--plugin" => {
                        let dir = args.next().ok_or("--plugin needs a directory")?;
                        check.plugin_dirs.push(dir.into());
                    }
                    flag if flag.starts_with('-') => {
                        return Err(format!("unknown option '{flag}' for check"));
                    }
                    // Paths stay `OsString`s, so a root that isn't UTF-8
                    // still works.
                    _ if root.is_none() => root = Some(PathBuf::from(arg)),
                    _ => {
                        return Err(format!(
                            "check takes one project root; '{text}' is a second"
                        ));
                    }
                }
            }
            check.root = root.ok_or("check needs a project root: happyview check <root>")?;
            Ok(Command::Check(check))
        }
        "dev" => {
            let root = args
                .next()
                .ok_or("dev needs a project root: happyview dev <root>")?;
            let text = root.to_string_lossy();
            if text.starts_with('-') {
                return Err(format!("unknown option '{text}' for dev"));
            }
            if let Some(extra) = args.next() {
                return Err(format!(
                    "dev takes one project root; '{}' is a second",
                    extra.to_string_lossy()
                ));
            }
            Ok(Command::Dev {
                root: PathBuf::from(root),
            })
        }
        "-h" | "--help" | "help" => Ok(Command::Help),
        other => Err(format!("unknown command '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().copied())
    }

    #[test]
    fn no_arguments_serves() {
        assert_eq!(parse_args(&[]), Ok(Command::Serve));
    }

    #[test]
    fn check_takes_a_root_and_plugin_directories() {
        assert_eq!(
            parse_args(&["check", "app", "--plugin", "p/lua", "--plugin", "p/ts"]),
            Ok(Command::Check(CheckArgs {
                root: "app".into(),
                plugin_dirs: vec!["p/lua".into(), "p/ts".into()],
            }))
        );
    }

    #[test]
    fn mistakes_are_named() {
        assert!(parse_args(&["check"]).unwrap_err().contains("project root"));
        assert!(
            parse_args(&["check", "a", "b"])
                .unwrap_err()
                .contains("'b'")
        );
        assert!(
            parse_args(&["check", "a", "--plugin"])
                .unwrap_err()
                .contains("--plugin")
        );
        assert!(
            parse_args(&["check", "a", "--watch"])
                .unwrap_err()
                .contains("--watch")
        );
        let err = parse_args(&["serve"]).unwrap_err();
        assert!(err.contains("'serve'"), "{err}");
        assert!(err.ends_with(USAGE), "the message carries the usage: {err}");
        assert_eq!(parse_args(&["--help"]), Ok(Command::Help));
    }

    #[test]
    fn parses_dev_with_a_root() {
        assert_eq!(
            parse_args(&["dev", "./app"]),
            Ok(Command::Dev {
                root: PathBuf::from("./app")
            })
        );
    }

    #[test]
    fn dev_needs_exactly_one_root() {
        let err = parse_args(&["dev"]).unwrap_err();
        assert!(err.starts_with("dev needs a project root"), "{err}");
        assert!(
            err.ends_with(USAGE),
            "the usage comes with the error: {err}"
        );
        assert!(parse_args(&["dev", "a", "b"]).unwrap_err().contains("'b'"));
        assert!(
            parse_args(&["dev", "--watch"])
                .unwrap_err()
                .contains("unknown option '--watch' for dev")
        );
    }
}
