use downlint::cli;

#[tokio::main]
async fn main() {
    let code = cli::run().await;
    std::process::exit(code);
}
