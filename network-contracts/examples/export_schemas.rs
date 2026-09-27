use std::{fs, path::PathBuf};

use noland_network_contracts::{
    control::{RpcRequest, RpcResponse},
    events::NetworkEvent,
    state::{HostNetworkState, InstanceNetworkState},
};
use schemars::{schema_for, JsonSchema};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("schemas"));
    fs::create_dir_all(&output)?;
    write_schema::<InstanceNetworkState>(&output, "instance-network-state.schema.json")?;
    write_schema::<HostNetworkState>(&output, "host-network-state.schema.json")?;
    write_schema::<RpcRequest>(&output, "control-request.schema.json")?;
    write_schema::<RpcResponse>(&output, "control-response.schema.json")?;
    write_schema::<NetworkEvent>(&output, "network-event.schema.json")?;
    Ok(())
}

fn write_schema<T: JsonSchema>(
    output: &std::path::Path,
    file_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = serde_json::to_vec_pretty(&schema_for!(T))?;
    fs::write(output.join(file_name), body)?;
    Ok(())
}
