//! Local operator utility; no web server, automatic migration or payslip generation.
#[tokio::main]
async fn main() {
    let result = match kabipay_tenant_import::cli::Arguments::parse(std::env::args().skip(1)) {
        Ok(args) => kabipay_tenant_import::cli::run(args).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("{}", kabipay_tenant_import::cli::safe_error(&error));
        std::process::exit(1);
    }
}
