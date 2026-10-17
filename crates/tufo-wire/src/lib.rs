//! Protobuf contracts shared by the process roots, the process-neutral operation contracts
//! they carry, and the explicit mappings between the two.

pub mod browser;
pub mod browser_host;
pub mod error;
pub mod library;
pub mod player;
pub mod proto;
pub mod server;
pub mod song_loops;
pub mod voice_channels;

/// Generated Protobuf and Tonic bindings.
#[allow(
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::excessive_nesting,
    clippy::match_single_binding,
    clippy::must_use_candidate,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref
)]
pub mod pb {
    tonic::include_proto!("tufo.v1");

    /// The browser host's pages and views, and its pipe to the server.
    pub mod browser {
        tonic::include_proto!("tufo.browser.v1");
    }
}

/// Encoded descriptor set used by gRPC reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("tufo_descriptor");
