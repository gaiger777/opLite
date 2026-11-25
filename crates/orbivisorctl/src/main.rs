use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let mut stdout = std::io::stdout();
    match orbivisorctl::run(std::env::args_os(), &mut stdout).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast::<clap::Error>() {
            Ok(clap_err) => clap_err.exit(),
            // 끝 코드를 정한 명령(`diff` — 1 다름, 2 오류)
            Err(e) if e.is::<orbivisorctl::Exit>() => {
                let exit = e.downcast::<orbivisorctl::Exit>().expect("checked");
                if let Some(message) = &exit.message {
                    eprintln!("error: {message}");
                }
                ExitCode::from(exit.code)
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}
