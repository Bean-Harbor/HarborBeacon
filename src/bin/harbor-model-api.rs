mod harbor_model_api_support;

use harbor_model_api_support::{print_startup_banner, ModelApiService};
use tiny_http::Server;

fn main() {
    let service = ModelApiService::from_env_and_args().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    print_startup_banner(&service.config());

    let server = Server::http(service.config().bind.as_str()).unwrap_or_else(|error| {
        panic!(
            "failed to bind harbor-model-api on {}: {error}",
            service.config().bind
        );
    });

    for request in server.incoming_requests() {
        service.handle_request(request);
    }
}
