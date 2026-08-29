pub mod adaptive_packet_size;
pub mod application;
pub mod composition;
pub mod domain;
pub mod infrastructure;
pub mod platform;
pub mod runtime;

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
pub mod native {
    include!(concat!(env!("OUT_DIR"), "/noland_moonlight_bindings.rs"));
}
