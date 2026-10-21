use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let mut stdout = std::io::stdout();
    match oplctl::run(std::env::args_os(), &mut stdout).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast::<clap::Error>() {
            Ok(clap_err) => clap_err.exit(),
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}
