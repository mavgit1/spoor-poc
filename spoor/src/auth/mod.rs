//! Credential shape, issuance evidence, and OS-keychain brokering.
//!
//! The pack records what traffic showed about *how* a credential looks and
//! where it was first seen. Live values stay in the OS keychain.

pub mod acquisition;
pub mod carrier;
pub mod fingerprint;
pub mod jwt;
pub mod observe;
pub mod store;

pub use acquisition::{Acquisition, find_issuer};
pub use carrier::{Carrier, Scheme};
pub use fingerprint::{Charset, Fingerprint, fingerprint};
pub use jwt::{JwtShape, inspect as inspect_jwt};
pub use observe::{
    AuthDocument, AuthObservation, Rotation, RotationMode, collect_from_flow_for_carrier,
    observe_for_origin, scrub_secrets, session_auth_warnings,
};
pub use store::{handle_for, load as load_secret, store as store_secret};
