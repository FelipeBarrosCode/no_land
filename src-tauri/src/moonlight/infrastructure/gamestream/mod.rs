pub mod applist;
pub mod http_client;
pub mod launch;
pub mod pairing;
pub mod request;
pub mod response;
pub mod server_info;
pub mod xml;

pub use applist::parse_app_list_response;
pub use http_client::{GameStreamHttpClient, ReqwestGameStreamHttpClient};
pub use launch::{build_cancel_request, build_launch_or_resume_request, parse_launch_response};
pub use pairing::{pair_host_with_stage1_authorization, PairHostRequest};
pub use request::{
    ClientIdentityReference, GameStreamRequest, GameStreamScheme, PinnedCertificate,
};
pub use response::GameStreamResponse;
pub use server_info::{parse_server_info_response, PairStatus};
