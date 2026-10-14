//! Secret scanner used by mise and by CI; never prints matched values.
#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("fixtures"));
    let redactor = rustovate_testkit::Redactor::default().with_literals(
        std::env::var("RUSTOVATE_REDACT")
            .unwrap_or_default()
            .split(',')
            .map(str::to_owned),
    );
    match rustovate_testkit::check_directory(&path, &redactor) {
        Ok(count) => {
            println!("Checked {count} cassettes: no possible secrets");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
