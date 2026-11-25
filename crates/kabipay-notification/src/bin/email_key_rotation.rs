//! No dotenv, telemetry subscriber, cleartext CLI credential or database-error output.
use kabipay_notification::email::{
    keyring::Keyring,
    rotation::{connect_for_rotation, rotate_email_credential, RotationError, RotationMode},
    rotation_cli::{parse_options, ROTATION_USAGE},
};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] {
        println!("{ROTATION_USAGE}");
        return ExitCode::SUCCESS;
    }
    let options = match parse_options(&arguments) {
        Ok(options) => options,
        Err(_) => {
            println!("{{\"completed\":false,\"code\":\"EMAIL_ROTATION_CONFIGURATION\",\"privateValuesReturned\":false}}");
            return ExitCode::from(2);
        }
    };
    let apply = options.request.mode == RotationMode::Apply;
    let result = async {
        let keys = Keyring::from_file(&options.keyring_file)
            .await
            .map_err(|_| RotationError::KeyUnavailable)?;
        let db = connect_for_rotation(&options.request, &options.database_url_file).await?;
        rotate_email_credential(&db, &options.request, &keys).await
    }
    .await;
    match result {
        Ok(report) => {
            println!(
                "{}",
                serde_json::json!({"completed":true,"report":report,"privateValuesReturned":false})
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"completed":false,"code":error.to_string(),
                "operatorReadbackRequired":apply,"privateValuesReturned":false})
            );
            ExitCode::FAILURE
        }
    }
}
