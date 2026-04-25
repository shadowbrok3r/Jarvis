//! Hand-wired module tree that mirrors the NVIDIA ACE `.proto` package hierarchy
//! (`nvidia_ace.<pkg>.v1`) so the generated files' `super::super::...` imports resolve.
//!
//! We cannot use `tonic::include_proto!("nvidia_ace.a2f.v1")` at the crate root because
//! prost emits each `.proto` package as a single flat file in `OUT_DIR`, so the
//! cross-package references only resolve when we wrap them in this nested layout.

pub mod audio {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/nvidia_ace.audio.v1.rs"));
    }
}

pub mod status {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/nvidia_ace.status.v1.rs"));
    }
}

pub mod animation_id {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/nvidia_ace.animation_id.v1.rs"));
    }
}

pub mod emotion_with_timecode {
    pub mod v1 {
        include!(concat!(
            env!("OUT_DIR"),
            "/nvidia_ace.emotion_with_timecode.v1.rs"
        ));
    }
}

pub mod a2f {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/nvidia_ace.a2f.v1.rs"));
    }
}

pub mod animation_data {
    pub mod v1 {
        include!(concat!(
            env!("OUT_DIR"),
            "/nvidia_ace.animation_data.v1.rs"
        ));
    }
}

pub mod controller {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/nvidia_ace.controller.v1.rs"));
    }
}

pub mod services {
    pub mod a2f_controller {
        pub mod v1 {
            include!(concat!(
                env!("OUT_DIR"),
                "/nvidia_ace.services.a2f_controller.v1.rs"
            ));
        }
    }
}
