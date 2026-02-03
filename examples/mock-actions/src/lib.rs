pub mod bindings {
    wit_bindgen::generate!({ generate_all });
}
use bindings::exports::betty_blocks::actions::actions::{RunInput, RunOutput, Error, Guest};

struct Component;

impl Guest for Component {
    fn call(input: RunInput) -> Result<RunOutput, Error> {
        // Extract the action_id and payload from the input
        eprintln!("We are here");
        let action_id = input.action_id;
        let payload_input = input.payload.input;
        let payload_configurations = input.payload.configurations;

        // If payload input is "error", return a run failed error
        if payload_input == "\"error\"" || payload_input == "error" {
            return Err(Error::RunFailed("Simulated error from mock action".to_string()));
        }

        // Create a simple JSON response with the extracted data
        let result = format!(
            r#"{{"action_id": "{}", "payload": {{"input": {}, "configurations": {}}}}}"#,
            action_id, payload_input, payload_configurations
        );

        Ok(RunOutput { result })
    }
}

bindings::export!(Component with_types_in bindings);

