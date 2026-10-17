fn main() -> std::process::ExitCode {
    // Before anything reads configuration: `check` has no database, and
    // `Config::from_env` refuses to start without one.
    let command = match happyview::cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(usage) => {
            eprintln!("{usage}");
            return std::process::ExitCode::from(2);
        }
    };
    if command == happyview::cli::Command::Help {
        println!("{}", happyview::cli::USAGE);
        return std::process::ExitCode::SUCCESS;
    }
    // In a project, `.env` holds script variables. Loaded into the
    // environment it would become server configuration and beat
    // happyview.toml, so only a plain serve reads it.
    if command == happyview::cli::Command::Serve {
        dotenvy::dotenv().ok();
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the tokio runtime")
        .block_on(happyview::run(command, happyview::Providers::default()))
}
