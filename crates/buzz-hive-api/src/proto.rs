//! Types generated from `proto/kiln/engine/v1/engine.proto`.
//!
//! Each message serializes as protobuf JSON, the wire shape of every route.

#![allow(clippy::all, clippy::pedantic, missing_docs)]

pub mod kiln {
    pub mod engine {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/kiln.engine.v1.rs"));
            include!(concat!(env!("OUT_DIR"), "/kiln.engine.v1.serde.rs"));
        }
    }
}

pub use kiln::engine::v1::*;
