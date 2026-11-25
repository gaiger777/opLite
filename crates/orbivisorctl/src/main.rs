use std::process::ExitCode;

fn main() -> ExitCode {
    // 옛 이름 OPL_X(한 릴리스 동안 받는다)를 clap이 읽는 ORBIVISOR_X로 옮기고 경고한다. set_var는 다른 스레드가
    // 환경을 읽는 동안 부르면 안전하지 않으므로 tokio 런타임(일꾼 스레드)을 만들기 전, 스레드가 하나일 때 한다.
    for (name, value, warning) in orbivisor_types::legacy::promotions(|k| std::env::var_os(k)) {
        eprintln!("warning: {warning}");
        // SAFETY: 아직 이 프로세스의 스레드는 main 하나다(런타임은 아래에서 만든다).
        unsafe { std::env::set_var(name, value) };
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("cannot start the tokio runtime")
        .block_on(run())
}

async fn run() -> ExitCode {
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
